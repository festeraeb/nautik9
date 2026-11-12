//! Remote care: the switch that lets the home fleet reach this server over Tailscale, and what
//! the fleet can do through it. All routes need the token (and pass the source gate first).
//!
//!   GET  /api/admin/remote_care        {enabled, tailnet_addrs, port, version, git}
//!   POST /api/admin/remote_care        {"enabled": bool}
//!   GET  /api/admin/version            {version, git, built, app}
//!   POST /api/admin/update             {"url", "sha256"}: download, verify, queue an install for the app
//!   GET  /api/admin/pending            {actions: [...]}   (the Android app polls this)
//!   POST /api/admin/pending/{id}/done  {"ok": bool, "detail": str}
//!
//! `<config_dir>/remote_care.json` {"enabled": bool}, default off. The Android app reads the same
//! file at boot (BootReceiver) to decide whether to start itself.
//!
//! An update is only a download here: installing needs the phone's own confirmation (Android's
//! installer dialog), and Android refuses an APK signed with a different key than the installed app.

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::AppState;

use crate::source_gate::{Class, Gate};

pub const FILE: &str = "remote_care.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCare {
    #[serde(default)]
    pub enabled: bool,
}

impl RemoteCare {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Write via a temp file and rename, so a reader (the app at boot) never sees half a file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_string(self).unwrap_or_default() + "\n")?;
        std::fs::rename(tmp, dir.join(FILE))
    }
}

/// Everything the admin routes and the source gate share. Cheap to clone.
#[derive(Clone)]
pub struct Admin {
    pub config_dir: PathBuf,
    pub update_dir: PathBuf,
    pub port: u16,
    pub gate: Gate,
    /// Device actions waiting for the Android app (it polls /api/admin/pending).
    pub pending: Arc<Mutex<Vec<Value>>>,
    /// Largest update the server will download (a run-time cap, not a constant in the handler).
    pub max_update_bytes: u64,
    /// One download at a time.
    pub update_lock: Arc<tokio::sync::Mutex<()>>,
    /// What the Android app reports about Wireless debugging and the self-test (debug_api.rs).
    pub debug: Arc<Mutex<crate::debug_api::DebugState>>,
}

pub const DEFAULT_MAX_UPDATE_BYTES: u64 = 200 * 1024 * 1024;

impl Admin {
    pub fn new(config_dir: &Path, update_dir: Option<PathBuf>, port: u16, allow: Vec<Class>) -> Self {
        Admin {
            config_dir: config_dir.to_path_buf(),
            update_dir: update_dir.unwrap_or_else(|| default_update_dir(config_dir)),
            port,
            gate: Gate { allow, remote_care: Arc::new(RwLock::new(RemoteCare::load(config_dir))) },
            pending: Default::default(),
            max_update_bytes: DEFAULT_MAX_UPDATE_BYTES,
            update_lock: Default::default(),
            debug: Default::default(),
        }
    }

    pub fn remote_care(&self) -> RemoteCare {
        self.gate.remote_care.read().map(|r| *r).unwrap_or_default()
    }

    #[cfg(test)]
    pub fn for_test(dir: &Path) -> Self {
        Admin::new(dir, Some(dir.join("update")), 8340, vec![Class::Loopback, Class::Tailnet])
    }
}

/// `<config_dir>/../update` (on the phone the app passes its own files dir explicitly).
pub fn default_update_dir(config_dir: &Path) -> PathBuf {
    config_dir.parent().map(|p| p.join("update")).unwrap_or_else(|| config_dir.join("update"))
}

/// Wipe what an earlier run left in the update folder: a half download, or an APK that was
/// installed (the install replaced this process before it could report back). Nothing refers to
/// either once the server restarts, since pending actions live in memory.
pub fn clean_update_dir(dir: &Path) {
    for f in ["update.apk.part", "update.apk"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/remote_care", get(remote_care_get).post(remote_care_set))
        .route("/api/admin/version", get(version))
        .route("/api/admin/update", post(update))
        .route("/api/admin/pending", get(pending))
        .route("/api/admin/pending/{id}/done", post(pending_done))
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

pub const GIT: &str = match option_env!("NAUTIK9_GIT_SHA") { Some(s) => s, None => "unknown" };
pub const BUILT: &str = match option_env!("NAUTIK9_BUILT") { Some(s) => s, None => "unknown" };

fn version_json() -> Value {
    // `app` is the Android app's versionName/versionCode, passed in by Engines.java (absent elsewhere).
    json!({"version": env!("CARGO_PKG_VERSION"), "git": GIT, "built": BUILT, "app": std::env::var("NAUTIK9_APP_VERSION").ok()})
}

async fn version() -> Json<Value> {
    Json(version_json())
}

/// Every (interface, address) on this device, from getifaddrs(3) (bionic has it since API 24).
#[cfg(unix)]
pub(crate) fn interface_addrs() -> Vec<(String, IpAddr)> {
    let mut out = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we walk read-only and free once; every pointer is checked.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut p = ifap;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null() && !ifa.ifa_name.is_null() {
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                match (*ifa.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        out.push((name, IpAddr::from(u32::from_be(sin.sin_addr.s_addr).to_be_bytes())));
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        out.push((name, IpAddr::from(sin6.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

#[cfg(not(unix))]
pub(crate) fn interface_addrs() -> Vec<(String, IpAddr)> {
    Vec::new()
}

/// This device's Tailscale addresses. A phone on cellular can also hold a carrier CGNAT address in
/// 100.64.0.0/10, so an IPv4 address counts only when it sits on the interface that also carries a
/// Tailscale fd7a:115c:a1e0:: address (Tailscale's tun). If no interface has one, tun/tailscale
/// interfaces are trusted by name, and failing that every 100.64/10 address is listed.
pub fn tailnet_addrs_from(all: &[(String, IpAddr)]) -> Vec<IpAddr> {
    use crate::source_gate::{classify, Class};
    let is_ts6 = |ip: &IpAddr| matches!(ip, IpAddr::V6(_)) && classify(*ip) == Some(Class::Tailnet);
    let mut ifaces: Vec<&str> = all.iter().filter(|(_, ip)| is_ts6(ip)).map(|(n, _)| n.as_str()).collect();
    if ifaces.is_empty() {
        ifaces = all.iter().map(|(n, _)| n.as_str()).filter(|n| n.starts_with("tun") || n.starts_with("tailscale")).collect();
    }
    let mut out: Vec<IpAddr> = all
        .iter()
        .filter(|(n, ip)| classify(*ip) == Some(Class::Tailnet) && (ifaces.is_empty() || ifaces.contains(&n.as_str())))
        .map(|(_, ip)| *ip)
        .collect();
    out.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    out.dedup();
    out
}

fn remote_care_json(a: &Admin) -> Value {
    let addrs: Vec<String> = tailnet_addrs_from(&interface_addrs()).iter().map(|ip| ip.to_string()).collect();
    json!({"enabled": a.remote_care().enabled, "tailnet_addrs": addrs, "port": a.port, "version": env!("CARGO_PKG_VERSION"), "git": GIT,
           "tailnet_allowed": a.gate.allow.contains(&crate::source_gate::Class::Tailnet)})
}

async fn remote_care_get(State(st): State<AppState>) -> Json<Value> {
    Json(remote_care_json(&st.admin))
}

async fn remote_care_set(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let Some(on) = b["enabled"].as_bool() else { return fail(StatusCode::BAD_REQUEST, "send {\"enabled\": true|false}") };
    let rc = RemoteCare { enabled: on };
    if let Err(e) = rc.save(&st.admin.config_dir) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("could not save {FILE}: {e}"));
    }
    if let Ok(mut w) = st.admin.gate.remote_care.write() {
        *w = rc;
    }
    println!("Remote care: switched {}", if on { "on" } else { "off" });
    Json(remote_care_json(&st.admin)).into_response()
}

/// https anywhere; plain http only to this machine or a tailnet address (Tailscale encrypts that hop).
pub fn update_url_allowed(u: &reqwest::Url) -> Result<(), String> {
    use crate::source_gate::{classify, Class};
    match u.scheme() {
        "https" => Ok(()),
        "http" => {
            let ok = match u.host() {
                Some(url::Host::Ipv4(v)) => matches!(classify(IpAddr::V4(v)), Some(Class::Loopback | Class::Tailnet)),
                Some(url::Host::Ipv6(v)) => matches!(classify(IpAddr::V6(v)), Some(Class::Loopback | Class::Tailnet)),
                Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
                None => false,
            };
            if ok { Ok(()) } else { Err("plain http is only allowed to loopback or a tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48); use https".into()) }
        }
        other => Err(format!("{other}: only https, or http to a tailnet address")),
    }
}

async fn update(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let a = &st.admin;
    let Some(url) = b["url"].as_str().and_then(|u| reqwest::Url::parse(u.trim()).ok()) else { return fail(StatusCode::BAD_REQUEST, "send {\"url\": \"...\", \"sha256\": \"<64 hex>\"}") };
    let want = b["sha256"].as_str().unwrap_or("").trim().to_ascii_lowercase();
    if want.len() != 64 || !want.chars().all(|c| c.is_ascii_hexdigit()) {
        return fail(StatusCode::BAD_REQUEST, "sha256 must be 64 hex digits");
    }
    if let Err(e) = update_url_allowed(&url) {
        return fail(StatusCode::BAD_REQUEST, e);
    }
    let Ok(_one) = a.update_lock.try_lock() else { return fail(StatusCode::CONFLICT, "an update is already downloading") };
    if let Err(e) = tokio::fs::create_dir_all(&a.update_dir).await {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("update folder: {e}"));
    }
    let part = a.update_dir.join("update.apk.part");
    let done = a.update_dir.join("update.apk");
    match download(&url, &part, a.max_update_bytes).await {
        Err((status, msg)) => {
            let _ = tokio::fs::remove_file(&part).await;
            fail(status, msg)
        }
        Ok((got, bytes)) if got != want => {
            let _ = tokio::fs::remove_file(&part).await;
            println!("Remote care: update from {} refused, sha256 mismatch ({bytes} bytes)", redact(&url));
            (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "error": "sha256 mismatch", "got": got, "bytes": bytes}))).into_response()
        }
        Ok((_, bytes)) => {
            if let Err(e) = tokio::fs::rename(&part, &done).await {
                let _ = tokio::fs::remove_file(&part).await;
                return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("could not keep the download: {e}"));
            }
            let action = json!({"id": uuid::Uuid::new_v4().to_string(), "kind": "install_apk", "path": done.to_string_lossy(), "sha256": want,
                                "bytes": bytes, "created": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)});
            if let Ok(mut p) = a.pending.lock() {
                // update.apk was just replaced: an older install request would point at the new file.
                p.retain(|x| x["kind"] != "install_apk");
                p.push(action.clone());
            }
            println!("Remote care: update downloaded from {} ({bytes} bytes, sha256 ok); waiting for the app to install it", redact(&url));
            Json(json!({"ok": true, "action": action})).into_response()
        }
    }
}

/// The URL without query or credentials, for the log.
fn redact(u: &reqwest::Url) -> String {
    format!("{}://{}{}", u.scheme(), u.host_str().unwrap_or(""), u.port().map(|p| format!(":{p}")).unwrap_or_default()) + u.path()
}

/// Stream `url` to `part` with a running sha256. Redirects are followed only to URLs the same rule allows.
async fn download(url: &reqwest::Url, part: &Path, cap: u64) -> Result<(String, u64), (StatusCode, String)> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|at| {
            if at.previous().len() >= 5 {
                at.error("too many redirects")
            } else if let Err(e) = update_url_allowed(at.url()) {
                at.error(e)
            } else {
                at.follow()
            }
        }))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let bad = |e: reqwest::Error| (StatusCode::BAD_GATEWAY, if e.is_redirect() { format!("redirect refused: {e}") } else if e.is_timeout() { "download timed out".into() } else if e.is_connect() { "could not connect to the update server".into() } else { "download failed".into() });
    let mut resp = client.get(url.clone()).send().await.map_err(bad)?;
    if !resp.status().is_success() {
        return Err((StatusCode::BAD_GATEWAY, format!("the update server answered HTTP {}", resp.status().as_u16())));
    }
    let too_big = || (StatusCode::PAYLOAD_TOO_LARGE, format!("larger than the {} MB cap", cap / (1024 * 1024)));
    if resp.content_length().is_some_and(|n| n > cap) {
        return Err(too_big());
    }
    let mut f = tokio::fs::File::create(part).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("update file: {e}")))?;
    let mut h = Sha256::new();
    let mut n: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(bad)? {
        n += chunk.len() as u64;
        if n > cap {
            return Err(too_big());
        }
        h.update(&chunk);
        f.write_all(&chunk).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("writing the update: {e}")))?;
    }
    f.flush().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    f.sync_all().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((h.finalize().iter().map(|b| format!("{b:02x}")).collect(), n))
}

async fn pending(State(st): State<AppState>) -> Json<Value> {
    Json(json!({"actions": st.admin.pending.lock().map(|p| p.clone()).unwrap_or_default()}))
}

async fn pending_done(State(st): State<AppState>, UrlPath(id): UrlPath<String>, Json(b): Json<Value>) -> Response {
    let removed = st.admin.pending.lock().ok().and_then(|mut p| p.iter().position(|x| x["id"] == id.as_str()).map(|i| p.remove(i)));
    let Some(action) = removed else { return fail(StatusCode::NOT_FOUND, "no pending action with that id") };
    let ok = b["ok"].as_bool().unwrap_or(false);
    let detail: String = b["detail"].as_str().unwrap_or("").chars().take(300).collect();
    println!("Remote care: action {id} ({}) done: {} {detail}", action["kind"].as_str().unwrap_or("?"), if ok { "ok" } else { "FAILED" });
    if action["kind"] == "install_apk" {
        // Installed or given up: either way the copy here is no longer needed (a retry downloads again).
        let _ = std::fs::remove_file(st.admin.update_dir.join("update.apk"));
    }
    Json(json!({"ok": true, "removed": action})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_care_defaults_off_and_survives_a_restart() {
        let d = tempfile::tempdir().unwrap();
        assert!(!RemoteCare::load(d.path()).enabled);
        std::fs::write(d.path().join(FILE), "not json").unwrap();
        assert!(!RemoteCare::load(d.path()).enabled, "a broken file means off");
        RemoteCare { enabled: true }.save(d.path()).unwrap();
        assert!(RemoteCare::load(d.path()).enabled);
        assert!(Admin::new(d.path(), None, 8340, vec![Class::Loopback]).remote_care().enabled);
        assert_eq!(default_update_dir(Path::new("/data/files/config")), PathBuf::from("/data/files/update"));
    }

    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    const TOKEN: &str = "admin-test-token-0123456789abcdef";
    const APK: &[u8] = b"PK\x03\x04 not really an apk, but bytes with a known hash";

    fn sha(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn state(dir: &Path) -> AppState {
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        AppState::test_state(dir, hw, "http://127.0.0.1:1")
    }

    async fn call_from(st: &AppState, from: &str, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let app = crate::build_router(st.clone(), TOKEN.into()).layer(MockConnectInfo(SocketAddr::new(from.parse().unwrap(), 50000)));
        let b = axum::http::Request::builder().method(m).uri(uri).header("authorization", format!("Bearer {TOKEN}"));
        let req = match body {
            Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let r = app.oneshot(req).await.unwrap();
        let st = r.status();
        (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
    }
    async fn call(st: &AppState, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        call_from(st, "127.0.0.1", m, uri, body).await
    }

    /// A stand-in for t440 serving the APK: /app.apk, and /away redirecting to a Wi-Fi address.
    async fn apk_server() -> String {
        let app = Router::new()
            .route("/app.apk", get(|| async { APK.to_vec() }))
            .route("/away", get(|| async { (StatusCode::FOUND, [("location", "http://192.168.4.20:9/app.apk")]) }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn version_has_the_crate_version_the_commit_and_the_build_time() {
        let d = tempfile::tempdir().unwrap();
        let (s, v) = call(&state(d.path()), "GET", "/api/admin/version", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["git"].as_str().is_some_and(|g| !g.is_empty()), "{v}");
        assert!(v["built"].as_str().is_some_and(|b| b == "unknown" || b.ends_with('Z')), "{v}");
        assert!(v.get("app").is_some());
    }

    #[tokio::test]
    async fn the_switch_is_saved_and_opens_and_closes_the_tailnet() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let (s, v) = call(&st, "GET", "/api/admin/remote_care", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((v["enabled"].as_bool(), v["port"].as_u64()), (Some(false), Some(8340)));
        assert!(v["tailnet_addrs"].is_array() && v["version"].is_string(), "{v}");
        assert_eq!(call_from(&st, "100.72.182.77", "GET", "/api/admin/remote_care", None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&st, "POST", "/api/admin/remote_care", Some(json!({"enabled": "yes"}))).await.0, StatusCode::BAD_REQUEST);

        let (s, v) = call(&st, "POST", "/api/admin/remote_care", Some(json!({"enabled": true}))).await;
        assert_eq!((s, v["enabled"].as_bool()), (StatusCode::OK, Some(true)));
        assert!(RemoteCare::load(d.path()).enabled, "written to remote_care.json for the app's boot check");
        let (s, v) = call_from(&st, "100.72.182.77", "GET", "/api/admin/remote_care", None).await;
        assert_eq!((s, v["enabled"].as_bool()), (StatusCode::OK, Some(true)), "the fleet gets in at once, no restart");
        assert_eq!(call_from(&st, "192.168.4.20", "GET", "/api/admin/remote_care", None).await.0, StatusCode::FORBIDDEN, "Wi-Fi does not");

        let (s, _) = call_from(&st, "100.72.182.77", "POST", "/api/admin/remote_care", Some(json!({"enabled": false}))).await;
        assert_eq!(s, StatusCode::OK, "the fleet may switch it off");
        assert_eq!(call_from(&st, "100.72.182.77", "GET", "/api/admin/version", None).await.0, StatusCode::FORBIDDEN, "and is then out");
        assert!(!RemoteCare::load(d.path()).enabled);
    }

    #[tokio::test]
    async fn a_good_update_is_kept_and_queued_until_the_app_reports_back() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let url = apk_server().await;
        let (s, v) = call(&st, "POST", "/api/admin/update", Some(json!({"url": format!("{url}/app.apk"), "sha256": sha(APK).to_uppercase()}))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let apk = d.path().join("update").join("update.apk");
        assert_eq!(std::fs::read(&apk).unwrap(), APK);
        assert!(!d.path().join("update").join("update.apk.part").exists());
        let a = &v["action"];
        assert_eq!((a["kind"].as_str(), a["sha256"].as_str(), a["bytes"].as_u64()), (Some("install_apk"), Some(sha(APK).as_str()), Some(APK.len() as u64)));
        assert_eq!(a["path"].as_str(), Some(apk.to_string_lossy().as_ref()));
        assert!(a["created"].as_str().is_some_and(|c| c.ends_with('Z')));

        // A second push replaces the first request instead of queueing two installs of one file.
        let (_, v2) = call(&st, "POST", "/api/admin/update", Some(json!({"url": format!("{url}/app.apk"), "sha256": sha(APK)}))).await;
        let (_, p) = call(&st, "GET", "/api/admin/pending", None).await;
        assert_eq!(p["actions"].as_array().map(|x| x.len()), Some(1), "{p}");
        let id = v2["action"]["id"].as_str().unwrap().to_string();
        assert_eq!(p["actions"][0]["id"], id.as_str());

        let (s, _) = call(&st, "POST", "/api/admin/pending/nope/done", Some(json!({"ok": true}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, r) = call(&st, "POST", &format!("/api/admin/pending/{id}/done"), Some(json!({"ok": false, "detail": "STATUS_FAILURE_ABORTED: user said no"}))).await;
        assert_eq!((s, r["removed"]["id"].as_str()), (StatusCode::OK, Some(id.as_str())));
        let (_, p) = call(&st, "GET", "/api/admin/pending", None).await;
        assert_eq!(p["actions"], json!([]));
        assert!(!apk.exists(), "the APK is removed once the app reported back");
        assert_eq!(call(&st, "POST", &format!("/api/admin/pending/{id}/done"), Some(json!({"ok": true}))).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_wrong_hash_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let url = apk_server().await;
        let (s, v) = call(&st, "POST", "/api/admin/update", Some(json!({"url": format!("{url}/app.apk"), "sha256": "0".repeat(64)}))).await;
        assert_eq!((s, v["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("sha256 mismatch")));
        assert_eq!(v["got"].as_str(), Some(sha(APK).as_str()));
        let dir = d.path().join("update");
        assert!(!dir.join("update.apk").exists() && !dir.join("update.apk.part").exists());
        assert_eq!(call(&st, "GET", "/api/admin/pending", None).await.1["actions"], json!([]));
    }

    #[tokio::test]
    async fn plain_http_only_to_loopback_or_the_tailnet_and_the_cap_holds() {
        let d = tempfile::tempdir().unwrap();
        let mut st = state(d.path());
        let h = sha(APK);
        for bad in ["http://192.168.4.20:8000/app.apk", "http://example.com/app.apk", "http://8.8.8.8/app.apk", "ftp://100.72.182.77/app.apk", "file:///etc/passwd", "not a url"] {
            let (s, v) = call(&st, "POST", "/api/admin/update", Some(json!({"url": bad, "sha256": h}))).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
        }
        let (s, _) = call(&st, "POST", "/api/admin/update", Some(json!({"url": "https://example.com/a.apk", "sha256": "abc"}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "short hash");
        for good in ["http://100.72.182.77:8000/a.apk", "http://[fd7a:115c:a1e0::1]/a.apk", "http://127.0.0.1/a.apk", "http://localhost/a.apk", "https://example.com/a.apk"] {
            assert!(update_url_allowed(&reqwest::Url::parse(good).unwrap()).is_ok(), "{good}");
        }
        let url = apk_server().await;
        let (s, v) = call(&st, "POST", "/api/admin/update", Some(json!({"url": format!("{url}/away"), "sha256": h}))).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "a redirect to a Wi-Fi address is not followed: {v}");
        assert!(v["error"].as_str().unwrap().contains("redirect"), "{v}");
        st.admin.max_update_bytes = 10;
        let (s, v) = call(&st, "POST", "/api/admin/update", Some(json!({"url": format!("{url}/app.apk"), "sha256": h}))).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
        assert!(!d.path().join("update").join("update.apk.part").exists());
        assert_eq!(call(&st, "GET", "/api/admin/pending", None).await.1["actions"], json!([]));
    }

    #[test]
    fn only_the_tailscale_interface_counts_not_a_carrier_cgnat_address() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let phone = vec![
            ("lo".to_string(), ip("127.0.0.1")),
            ("rmnet_data1".to_string(), ip("100.88.10.20")),
            ("wlan0".to_string(), ip("192.168.4.31")),
            ("tun0".to_string(), ip("100.72.127.73")),
            ("tun0".to_string(), ip("fd7a:115c:a1e0::1234")),
        ];
        assert_eq!(tailnet_addrs_from(&phone), vec![ip("100.72.127.73"), ip("fd7a:115c:a1e0::1234")]);
        let no_v6 = vec![("rmnet_data1".to_string(), ip("100.88.10.20")), ("tun0".to_string(), ip("100.72.127.73"))];
        assert_eq!(tailnet_addrs_from(&no_v6), vec![ip("100.72.127.73")], "by name when there is no fd7a: address");
        let bare = vec![("eth0".to_string(), ip("100.99.1.1"))];
        assert_eq!(tailnet_addrs_from(&bare), vec![ip("100.99.1.1")], "nothing better to go on: listed");
        assert!(tailnet_addrs_from(&[("wlan0".to_string(), ip("192.168.4.31"))]).is_empty());
        assert!(interface_addrs().iter().any(|(_, a)| a.is_loopback()), "getifaddrs works on this machine");
    }
}
