//! Webxdc mini apps for NautiK9 (the format is an open spec, webxdc.org; Jade Vault runs the
//! same kind of `.xdc` packages). An `.xdc` is a zip with `index.html`, optional
//! `manifest.toml` and `icon.png`/`icon.jpg`, and whatever files the app needs.
//!
//! This crate is the safe, storage side, with no web server in it:
//! - [`install`] unpacks a zip into SQLite tables (so there is no file system path to escape)
//!   with caps on size, entry count and per-file size, enforced on the bytes actually read
//!   (a zip header that lies about its size does not help); unsafe names (`..`, absolute
//!   paths, backslashes, NUL) reject the whole archive.
//! - [`file`] looks a file up by normalised path; there is no directory walk.
//! - [`send_update`] / [`updates_since`] keep the ordered update log per app (serials 1, 2, 3
//!   per app), the thing `window.webxdc.sendUpdate` / `setUpdateListener` are built on.
//! - [`runtime_js`] and [`host_html`] produce the `window.webxdc` shim (runs inside the
//!   sandboxed frame) and the page that frames the app and relays its updates to the server.
//!
//! The tables are created in whatever SQLite connection the caller gives (NautiK9 passes the
//! sessions database).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_zip_bytes: usize,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
    pub max_update_bytes: usize,
    pub max_updates_per_app: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_zip_bytes: 16 << 20, max_files: 1000, max_total_bytes: 48 << 20, max_file_bytes: 16 << 20, max_update_bytes: 128 << 10, max_updates_per_app: 50_000 }
    }
}

/// The Content-Security-Policy every app file is served with: scripts and styles from the
/// app itself, no network of any kind (`connect-src 'none'`, no forms, no frames, no base).
pub const APP_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' data: blob:; connect-src 'none'; frame-src 'none'; \
form-action 'none'; base-uri 'none'; object-src 'none'; worker-src 'none'; sandbox allow-scripts";

#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    pub has_icon: bool,
    pub installed_at: String,
}

pub fn init(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS webxdc_apps (id TEXT PRIMARY KEY, name TEXT NOT NULL, files INTEGER NOT NULL, bytes INTEGER NOT NULL, installed_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS webxdc_files (app_id TEXT NOT NULL, path TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (app_id, path));
         CREATE TABLE IF NOT EXISTS webxdc_updates (app_id TEXT NOT NULL, serial INTEGER NOT NULL, sender TEXT NOT NULL, payload TEXT NOT NULL,
             info TEXT, summary TEXT, document TEXT, at TEXT NOT NULL, PRIMARY KEY (app_id, serial));",
    )
    .map_err(|e| e.to_string())
}

/// A safe, normalised archive path (`a/b.js`), or why not.
pub fn clean_path(raw: &str) -> Result<String, String> {
    if raw.is_empty() || raw.contains('\0') || raw.contains('\\') || raw.starts_with('/') {
        return Err(format!("unsafe path {raw:?}"));
    }
    let mut parts = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(format!("unsafe path {raw:?} (..)")),
            s if s.contains(':') && parts.is_empty() && s.len() == 2 => return Err(format!("unsafe path {raw:?} (drive letter)")),
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return Err(format!("unsafe path {raw:?}"));
    }
    Ok(parts.join("/"))
}

fn manifest_name(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "name").then(|| v.trim().trim_matches('"').trim_matches('\'').chars().take(80).collect::<String>())
    })
}

/// Unpack and store an `.xdc`. Replaces an app with the same content hash (same id).
pub fn install(conn: &Connection, zip_bytes: &[u8], fallback_name: &str, lim: &Limits) -> Result<AppInfo, String> {
    if zip_bytes.len() > lim.max_zip_bytes {
        return Err(format!("the .xdc is {} bytes; the limit is {}", zip_bytes.len(), lim.max_zip_bytes));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| format!("not a zip file: {e}"))?;
    if zip.len() > lim.max_files {
        return Err(format!("{} entries; the limit is {}", zip.len(), lim.max_files));
    }
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
        let name = f.name().to_string();
        if f.is_dir() {
            clean_path(name.trim_end_matches('/'))?;
            continue;
        }
        let path = clean_path(&name)?;
        if f.size() > lim.max_file_bytes {
            return Err(format!("{path}: declared size {} is over the {} limit", f.size(), lim.max_file_bytes));
        }
        // Read at most one byte over the limit: the declared size is not trusted.
        let mut buf = Vec::new();
        f.by_ref().take(lim.max_file_bytes + 1).read_to_end(&mut buf).map_err(|e| format!("{path}: {e}"))?;
        if buf.len() as u64 > lim.max_file_bytes {
            return Err(format!("{path}: unpacks to more than {} bytes", lim.max_file_bytes));
        }
        total += buf.len() as u64;
        if total > lim.max_total_bytes {
            return Err(format!("the archive unpacks to more than {} bytes", lim.max_total_bytes));
        }
        if files.iter().any(|(p, _)| *p == path) {
            return Err(format!("{path}: listed twice"));
        }
        files.push((path, buf));
    }
    if !files.iter().any(|(p, _)| p == "index.html") {
        return Err("no index.html in the archive".into());
    }
    let manifest = files.iter().find(|(p, _)| p == "manifest.toml").map(|(_, d)| String::from_utf8_lossy(d).to_string());
    let name = manifest.as_deref().and_then(manifest_name).filter(|n| !n.is_empty()).unwrap_or_else(|| fallback_name.chars().take(80).collect());
    let id = hex_prefix(&Sha256::digest(zip_bytes), 16);
    let has_icon = files.iter().any(|(p, _)| p == "icon.png" || p == "icon.jpg");
    let now = chrono::Local::now().to_rfc3339();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM webxdc_files WHERE app_id = ?1", [&id]).map_err(|e| e.to_string())?;
    for (p, d) in &files {
        tx.execute("INSERT INTO webxdc_files (app_id, path, data) VALUES (?1, ?2, ?3)", params![id, p, d]).map_err(|e| e.to_string())?;
    }
    tx.execute(
        "INSERT INTO webxdc_apps (id, name, files, bytes, installed_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, files = excluded.files, bytes = excluded.bytes, installed_at = excluded.installed_at",
        params![id, name, files.len() as i64, total as i64, now],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(AppInfo { id, name, files: files.len(), bytes: total, has_icon, installed_at: now })
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().chars().take(n).collect()
}

pub fn list(conn: &Connection) -> Result<Vec<AppInfo>, String> {
    let mut st = conn
        .prepare("SELECT id, name, files, bytes, installed_at, EXISTS(SELECT 1 FROM webxdc_files f WHERE f.app_id = a.id AND f.path IN ('icon.png','icon.jpg')) FROM webxdc_apps a ORDER BY installed_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| Ok(AppInfo { id: r.get(0)?, name: r.get(1)?, files: r.get::<_, i64>(2)? as usize, bytes: r.get::<_, i64>(3)? as u64, installed_at: r.get(4)?, has_icon: r.get::<_, i64>(5)? != 0 }))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub fn exists(conn: &Connection, app: &str) -> Result<bool, String> {
    conn.query_row("SELECT 1 FROM webxdc_apps WHERE id = ?1", [app], |_| Ok(())).optional().map(|o| o.is_some()).map_err(|e| e.to_string())
}

pub fn delete(conn: &Connection, app: &str) -> Result<bool, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for t in ["webxdc_files", "webxdc_updates"] {
        tx.execute(&format!("DELETE FROM {t} WHERE app_id = ?1"), [app]).map_err(|e| e.to_string())?;
    }
    let n = tx.execute("DELETE FROM webxdc_apps WHERE id = ?1", [app]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// Content type by extension (the few web types an app uses).
pub fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "txt" | "toml" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// One file of an app by request path (`""` = index.html). Err for an unsafe path, None if missing.
pub fn file(conn: &Connection, app: &str, path: &str) -> Result<Option<(&'static str, Vec<u8>)>, String> {
    let p = if path.is_empty() || path == "/" { "index.html".to_string() } else { clean_path(path)? };
    let data: Option<Vec<u8>> = conn.query_row("SELECT data FROM webxdc_files WHERE app_id = ?1 AND path = ?2", params![app, p], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
    Ok(data.map(|d| (mime(&p), d)))
}

/// Append an update from `sender`. `update` is `{payload, info?, summary?, document?}`;
/// returns the new serial.
pub fn send_update(conn: &Connection, lim: &Limits, app: &str, sender: &str, update: &Value) -> Result<i64, String> {
    if !exists(conn, app)? {
        return Err("no such app".into());
    }
    let payload = update.get("payload").ok_or("update has no `payload`")?;
    let text = payload.to_string();
    if text.len() > lim.max_update_bytes {
        return Err(format!("payload is {} bytes; the limit is {}", text.len(), lim.max_update_bytes));
    }
    let short = |k: &str| update[k].as_str().map(|s| s.chars().take(300).collect::<String>());
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let next: i64 = tx.query_row("SELECT COALESCE(MAX(serial), 0) + 1 FROM webxdc_updates WHERE app_id = ?1", [app], |r| r.get(0)).map_err(|e| e.to_string())?;
    if next > lim.max_updates_per_app {
        return Err(format!("this app already has {} updates", lim.max_updates_per_app));
    }
    tx.execute(
        "INSERT INTO webxdc_updates (app_id, serial, sender, payload, info, summary, document, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![app, next, sender, text, short("info"), short("summary"), short("document"), chrono::Local::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(next)
}

/// Updates with serial > `since`, oldest first, as the objects `setUpdateListener` hands to
/// the app: `{payload, serial, max_serial, info?, summary?, document?, sender}`.
pub fn updates_since(conn: &Connection, app: &str, since: i64, limit: usize) -> Result<Vec<Value>, String> {
    let max: i64 = conn.query_row("SELECT COALESCE(MAX(serial), 0) FROM webxdc_updates WHERE app_id = ?1", [app], |r| r.get(0)).map_err(|e| e.to_string())?;
    let mut st = conn.prepare("SELECT serial, sender, payload, info, summary, document FROM webxdc_updates WHERE app_id = ?1 AND serial > ?2 ORDER BY serial LIMIT ?3").map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params![app, since.max(0), limit as i64], |r| {
            let payload: String = r.get(2)?;
            let mut u = json!({"serial": r.get::<_, i64>(0)?, "max_serial": max, "sender": r.get::<_, String>(1)?, "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)});
            for (i, k) in [(3, "info"), (4, "summary"), (5, "document")] {
                if let Some(s) = r.get::<_, Option<String>>(i)? {
                    u[k] = json!(s);
                }
            }
            Ok(u)
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// `window.webxdc` for the sandboxed frame. `self_addr` and `self_name` are baked in because
/// apps read them synchronously. Talks to the parent (host page) by `postMessage`; the frame
/// has no network.
pub fn runtime_js(self_addr: &str, self_name: &str) -> String {
    let addr = serde_json::to_string(self_addr).unwrap().replace('<', "\\u003c");
    let name = serde_json::to_string(self_name).unwrap().replace('<', "\\u003c");
    format!(
        r#"(function () {{
  'use strict';
  var listener = null, readyResolve = null, delivered = 0;
  function post(m) {{ parent.postMessage(Object.assign({{ n9xdc: true }}, m), '*'); }}
  window.addEventListener('message', function (e) {{
    if (e.source !== parent || !e.data || e.data.n9xdc !== 'updates') return;
    var list = e.data.updates || [];
    for (var i = 0; i < list.length; i++) {{
      var u = list[i];
      if (u.serial <= delivered) continue;
      delivered = u.serial;
      if (listener) {{ try {{ listener(u); }} catch (err) {{ console.error(err); }} }}
    }}
    if (readyResolve) {{ var r = readyResolve; readyResolve = null; r(); }}
  }});
  window.webxdc = {{
    selfAddr: {addr},
    selfName: {name},
    sendUpdate: function (update, descr) {{
      if (!update || typeof update !== 'object' || !('payload' in update)) throw new Error('sendUpdate: update.payload is required');
      post({{ kind: 'send', update: {{ payload: update.payload, info: update.info, summary: update.summary, document: update.document }}, descr: descr || '' }});
    }},
    setUpdateListener: function (cb, serial) {{
      listener = cb; delivered = serial || 0;
      return new Promise(function (resolve) {{ readyResolve = resolve; post({{ kind: 'listen', serial: delivered }}); }});
    }},
    getAllUpdates: function () {{ return Promise.reject(new Error('getAllUpdates is deprecated; use setUpdateListener')); }},
    sendToChat: function () {{ return Promise.reject(new Error('sendToChat is not supported in NautiK9')); }},
    importFiles: function () {{ return Promise.reject(new Error('importFiles is not supported in NautiK9')); }}
  }};
}})();
"#
    )
}

/// The page that frames an app. It lives on the server's own origin and holds the access
/// token (from its own URL) to talk to the update API; the app frame is sandboxed WITHOUT
/// `allow-same-origin`, so it has an opaque origin, no token, no cookies and no network.
pub fn host_html(app_id: &str, name: &str, view_key: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
    let id_js = serde_json::to_string(app_id).unwrap().replace('<', "\\u003c");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>html,body{{margin:0;height:100%;background:#0b1220;color:#cbd5e1;font-family:sans-serif}}iframe{{border:0;width:100%;height:100%;background:#fff}}#err{{position:fixed;left:0;right:0;bottom:0;background:#7f1d1d;color:#fff;padding:6px 10px;font-size:13px;display:none}}</style></head>
<body><iframe id="app" sandbox="allow-scripts" referrerpolicy="no-referrer" src="/xdc/{app}/{key}/index.html"></iframe><div id="err"></div>
<script>
(function () {{
  var APP = {id_js}, token = new URLSearchParams(location.search).get('token') || '';
  var frame = document.getElementById('app'), err = document.getElementById('err');
  var base = '/api/webxdc/apps/' + encodeURIComponent(APP) + '/updates';
  var since = 0, timer = null, listening = false;
  function fail(m) {{ err.style.display = 'block'; err.textContent = m; }}
  function api(path, init) {{
    init = init || {{}};
    init.headers = Object.assign({{ Authorization: 'Bearer ' + token }}, init.headers || {{}});
    return fetch(path, init).then(function (r) {{ return r.json().then(function (j) {{ if (!r.ok) throw new Error(j.error || r.status); return j; }}); }});
  }}
  function pull() {{
    api(base + '?since=' + since).then(function (j) {{
      var list = j.updates || [];
      if (list.length) since = list[list.length - 1].serial;
      if (list.length || !listening) {{ listening = true; frame.contentWindow.postMessage({{ n9xdc: 'updates', updates: list }}, '*'); }}
    }}).catch(function (e) {{ fail('updates: ' + e.message); }});
  }}
  window.addEventListener('message', function (e) {{
    if (e.source !== frame.contentWindow || !e.data || e.data.n9xdc !== true) return;
    if (e.data.kind === 'listen') {{
      since = Math.max(0, e.data.serial | 0); listening = false;
      pull(); clearInterval(timer); timer = setInterval(pull, 1500);
    }} else if (e.data.kind === 'send') {{
      api(base, {{ method: 'POST', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify({{ update: e.data.update }}) }}).then(pull).catch(function (x) {{ fail('sendUpdate: ' + x.message); }});
    }}
  }});
}})();
</script></body></html>"#,
        title = esc(name),
        app = esc(app_id),
        key = esc(view_key),
        id_js = id_js
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, d) in entries {
            w.start_file(*n, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }

    #[test]
    fn install_serve_list_delete() {
        let c = db();
        let z = zip_of(&[("index.html", b"<h1>hi</h1>"), ("manifest.toml", b"name = \"Poll\"\nsource_code_url = \"x\"\n"), ("js/app.js", b"1"), ("icon.png", b"png")]);
        let a = install(&c, &z, "fallback.xdc", &Limits::default()).unwrap();
        assert_eq!((a.name.as_str(), a.files, a.has_icon), ("Poll", 4, true));
        assert_eq!(list(&c).unwrap().len(), 1);
        let (m, d) = file(&c, &a.id, "").unwrap().unwrap();
        assert_eq!((m, d.as_slice()), ("text/html; charset=utf-8", b"<h1>hi</h1>".as_slice()));
        assert_eq!(file(&c, &a.id, "js//./app.js").unwrap().unwrap().0, "text/javascript; charset=utf-8");
        assert!(file(&c, &a.id, "missing.js").unwrap().is_none());
        // same bytes again = same app, not a second copy
        assert_eq!(install(&c, &z, "x", &Limits::default()).unwrap().id, a.id);
        assert_eq!(list(&c).unwrap().len(), 1);
        assert!(delete(&c, &a.id).unwrap());
        assert!(file(&c, &a.id, "").unwrap().is_none() && !delete(&c, &a.id).unwrap());
        // no manifest name: the fallback
        let z2 = zip_of(&[("index.html", b"x")]);
        assert_eq!(install(&c, &z2, "my-game.xdc", &Limits::default()).unwrap().name, "my-game.xdc");
    }

    #[test]
    fn path_traversal_is_refused_everywhere() {
        for bad in ["../x", "a/../../x", "/etc/passwd", "a\\b", "C:/x", "a/\0b", "", "..", "a/.."] {
            assert!(clean_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!(clean_path("./a//b/c.js").unwrap(), "a/b/c.js");
        let c = db();
        for evil in ["../evil.html", "a/../../evil.html", "/abs.html", "dir\\evil.html"] {
            let z = zip_of(&[("index.html", b"x"), (evil, b"boom")]);
            let e = install(&c, &z, "x", &Limits::default()).unwrap_err();
            assert!(e.contains("unsafe path"), "{evil}: {e}");
        }
        assert!(list(&c).unwrap().is_empty(), "a rejected archive stores nothing");
        let z = zip_of(&[("index.html", b"x")]);
        let a = install(&c, &z, "x", &Limits::default()).unwrap();
        assert!(file(&c, &a.id, "../../etc/passwd").is_err());
        assert!(file(&c, &a.id, "a/../index.html").is_err());
    }

    #[test]
    fn size_caps_hold_even_when_the_header_lies_or_the_ratio_is_huge() {
        let c = db();
        let lim = Limits { max_zip_bytes: 10_000, max_files: 5, max_total_bytes: 100_000, max_file_bytes: 50_000, ..Limits::default() };
        // zip bomb: 10 MB of zeros compresses to ~10 KB
        let bomb = vec![0u8; 10 << 20];
        let z = zip_of(&[("index.html", b"x"), ("big.bin", &bomb)]);
        let e = install(&c, &z, "x", &Limits { max_zip_bytes: 1 << 20, ..lim.clone() }).unwrap_err();
        assert!(e.contains("big.bin") && e.contains("limit"), "{e}");
        // total cap across many small files
        let chunk = vec![1u8; 40_000];
        let z = zip_of(&[("index.html", b"x"), ("a", &chunk), ("b", &chunk), ("c", &chunk)]);
        let e = install(&c, &z, "x", &Limits { max_zip_bytes: 1 << 20, ..lim.clone() }).unwrap_err();
        assert!(e.contains("more than 100000"), "{e}");
        // too many entries
        let many: Vec<(String, Vec<u8>)> = (0..6).map(|i| (format!("f{i}"), vec![1])).collect();
        let mut entries: Vec<(&str, &[u8])> = many.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        entries.push(("index.html", b"x"));
        let e = install(&c, &zip_of(&entries), "x", &lim).unwrap_err();
        assert!(e.contains("entries"), "{e}");
        // zip itself too large; not a zip; no index.html
        assert!(install(&c, &vec![0u8; 20_000], "x", &lim).unwrap_err().contains("limit"));
        assert!(install(&c, b"not a zip at all", "x", &lim).unwrap_err().contains("not a zip"));
        assert!(install(&c, &zip_of(&[("readme.txt", b"x")]), "x", &lim).unwrap_err().contains("index.html"));
        assert!(list(&c).unwrap().is_empty());
        // the cap is applied to the bytes actually read, whatever the header says
        let z = zip_of(&[("index.html", b"x"), ("f.bin", &vec![7u8; 200])]);
        let e = install(&c, &z, "x", &Limits { max_file_bytes: 100, ..Limits::default() }).unwrap_err();
        assert!(e.contains("f.bin"), "{e}");
    }

    #[test]
    fn updates_are_ordered_per_app_and_capped() {
        let c = db();
        let lim = Limits { max_update_bytes: 100, max_updates_per_app: 3, ..Limits::default() };
        let a = install(&c, &zip_of(&[("index.html", b"a")]), "a", &lim).unwrap().id;
        let b = install(&c, &zip_of(&[("index.html", b"b")]), "b", &lim).unwrap().id;
        assert_eq!(send_update(&c, &lim, &a, "me@x", &json!({"payload": {"n": 1}, "info": "first"})).unwrap(), 1);
        assert_eq!(send_update(&c, &lim, &a, "me@x", &json!({"payload": "two", "summary": "s"})).unwrap(), 2);
        assert_eq!(send_update(&c, &lim, &b, "me@x", &json!({"payload": 0})).unwrap(), 1, "serials are per app");
        let all = updates_since(&c, &a, 0, 100).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0]["serial"].as_i64(), all[0]["max_serial"].as_i64(), all[0]["payload"]["n"].as_i64()), (Some(1), Some(2), Some(1)));
        assert_eq!(all[0]["info"], "first");
        assert_eq!(all[1]["summary"], "s");
        assert_eq!(updates_since(&c, &a, 1, 100).unwrap().len(), 1);
        assert!(updates_since(&c, &a, 2, 100).unwrap().is_empty());
        assert!(send_update(&c, &lim, &a, "me", &json!({"payload": "x".repeat(200)})).unwrap_err().contains("limit"));
        assert!(send_update(&c, &lim, &a, "me", &json!({"info": "no payload"})).is_err());
        assert!(send_update(&c, &lim, "nope", "me", &json!({"payload": 1})).is_err());
        send_update(&c, &lim, &a, "me", &json!({"payload": 3})).unwrap();
        assert!(send_update(&c, &lim, &a, "me", &json!({"payload": 4})).unwrap_err().contains("already has"));
        delete(&c, &a).unwrap();
        assert!(updates_since(&c, &a, 0, 10).unwrap().is_empty());
        assert_eq!(updates_since(&c, &b, 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn runtime_and_host_pages_are_escaped_and_shaped() {
        let js = runtime_js("local-owner", "Na\"me</script>");
        assert!(js.contains("selfAddr: \"local-owner\"") && !js.contains("</script>"));
        for api in ["sendUpdate", "setUpdateListener", "selfAddr", "selfName"] {
            assert!(js.contains(api));
        }
        let h = host_html("abc123", "<b>x</b>", "key");
        assert!(h.contains("sandbox=\"allow-scripts\"") && !h.contains("allow-same-origin"));
        assert!(h.contains("/xdc/abc123/key/index.html") && !h.contains("<b>x</b>"));
        assert!(APP_CSP.contains("connect-src 'none'") && APP_CSP.contains("form-action 'none'"));
    }
}
