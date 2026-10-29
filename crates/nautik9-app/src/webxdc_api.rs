//! Webxdc mini apps over HTTP (storage and rules live in the `nautik9-webxdc` crate).
//!
//!   GET    /api/webxdc/apps                      -> [AppInfo]
//!   POST   /api/webxdc/apps                      raw .xdc bytes (header `X-Filename` optional) -> AppInfo
//!                                                (400 with the reason for a bad/oversized/unsafe archive)
//!   DELETE /api/webxdc/apps/{id}
//!   GET    /api/webxdc/apps/{id}/host            the page that frames the app (needs ?token=)
//!   GET    /api/webxdc/apps/{id}/updates?since=N -> {updates, max_serial}
//!   POST   /api/webxdc/apps/{id}/updates         {update: {payload, info?, summary?}} -> {serial}
//!   GET    /xdc/{id}/{view_key}/{*path}          the app's own files, for the sandboxed frame only
//!
//! The app frame is `<iframe sandbox="allow-scripts">` (no same-origin: opaque origin, no
//! token, no cookies) and every file is served with a CSP that blocks all network. The
//! `view_key` in the path is derived from the access token and the app id, so only a page
//! that was given it by `/host` can load the files; it opens nothing else. The frame talks to
//! the host page by postMessage; the host page does the authenticated API calls.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use nautik9_webxdc as xdc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::AppState;

/// Set once at server start (`Extension`): where the config dir is and the secret the view
/// keys come from (the access token).
#[derive(Clone)]
pub struct Runtime {
    pub config_dir: PathBuf,
    pub secret: String,
}

pub const SELF_ADDR: &str = "local-owner";
pub const SELF_NAME: &str = "You";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/webxdc/apps", get(list_apps).post(install_app).layer(DefaultBodyLimit::max(xdc::Limits::default().max_zip_bytes + 4096)))
        .route("/api/webxdc/apps/{id}", delete(delete_app))
        .route("/api/webxdc/apps/{id}/host", get(host))
        .route("/api/webxdc/apps/{id}/updates", get(updates_get).post(updates_post))
        .route("/xdc/{id}/{key}/{*path}", get(app_file))
}

fn err(code: StatusCode, e: impl std::fmt::Display) -> Response {
    (code, Json(json!({"ok": false, "error": e.to_string()}))).into_response()
}

fn view_key(secret: &str, id: &str) -> String {
    let h = Sha256::digest(format!("{secret}\0webxdc-view\0{id}").as_bytes());
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Run `f` on the sessions database connection (tables created on first use).
async fn with_db<T>(st: &AppState, f: impl FnOnce(&rusqlite::Connection) -> Result<T, String>) -> Result<T, Response> {
    let a = st.agent.as_ref().map_err(|e| err(StatusCode::SERVICE_UNAVAILABLE, format!("agent unavailable: {e}")))?;
    let conn = a.ctx.store.shared_conn();
    let g = conn.lock().await;
    xdc::init(&g).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    f(&g).map_err(|e| err(StatusCode::BAD_REQUEST, e))
}

async fn list_apps(State(st): State<AppState>) -> Response {
    match with_db(&st, xdc::list).await {
        Ok(v) => Json(json!(v)).into_response(),
        Err(r) => r,
    }
}

async fn install_app(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()).unwrap_or_else(|| "app.xdc".into());
    match with_db(&st, |c| xdc::install(c, &body, &name, &xdc::Limits::default())).await {
        Ok(v) => Json(json!(v)).into_response(),
        Err(r) => r,
    }
}

async fn delete_app(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match with_db(&st, |c| xdc::delete(c, &id)).await {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "no such app"),
        Err(r) => r,
    }
}

async fn updates_get(State(st): State<AppState>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let since = q.get("since").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    match with_db(&st, |c| if xdc::exists(c, &id)? { xdc::updates_since(c, &id, since, 500).map(Some) } else { Ok(None) }).await {
        Ok(Some(u)) => Json(json!({"ok": true, "updates": u})).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such app"),
        Err(r) => r,
    }
}

async fn updates_post(State(st): State<AppState>, Path(id): Path<String>, Json(p): Json<Value>) -> Response {
    match with_db(&st, |c| xdc::send_update(c, &xdc::Limits::default(), &id, SELF_ADDR, &p["update"])).await {
        Ok(serial) => Json(json!({"ok": true, "serial": serial})).into_response(),
        Err(r) => r,
    }
}

async fn host(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Path(id): Path<String>) -> Response {
    let found: Result<Option<String>, Response> = with_db(&st, |c| {
        let apps = xdc::list(c)?;
        Ok(apps.into_iter().find(|x| x.id == id).map(|x| x.name))
    })
    .await;
    let name: String = match found {
        Ok(Some(n)) => n,
        Ok(None) => return err(StatusCode::NOT_FOUND, "no such app"),
        Err(r) => return r,
    };
    let html = xdc::host_html(&id, &name, &view_key(&rt.secret, &id));
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-src 'self'"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

async fn app_file(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Path((id, key, path)): Path<(String, String, String)>) -> Response {
    if !crate::auth::ct_eq(key.as_bytes(), view_key(&rt.secret, &id).as_bytes()) {
        return err(StatusCode::NOT_FOUND, "not found");
    }
    let (ctype, body): (&str, Vec<u8>) = if path == "webxdc.js" {
        ("text/javascript; charset=utf-8", xdc::runtime_js(SELF_ADDR, SELF_NAME).into_bytes())
    } else {
        match with_db(&st, |c| xdc::file(c, &id, &path)).await {
            Ok(Some((m, d))) => (m, d),
            Ok(None) => return err(StatusCode::NOT_FOUND, "not found"),
            Err(r) => return r,
        }
    };
    let h = |s: &'static str| HeaderValue::from_static(s);
    let mut resp = (StatusCode::OK, [(header::CONTENT_TYPE, ctype)], body).into_response();
    let hs = resp.headers_mut();
    hs.insert(header::CONTENT_SECURITY_POLICY, h(xdc::APP_CSP));
    hs.insert(header::X_CONTENT_TYPE_OPTIONS, h("nosniff"));
    hs.insert(header::REFERRER_POLICY, h("no-referrer"));
    hs.insert(header::CACHE_CONTROL, h("no-store"));
    resp
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    pub const TOKEN: &str = "t0ken-for-webxdc-tests-0123456789abcdef";

    /// A router with just the routes of this crate, behind the same token layer.
    pub(crate) fn test_app(dir: &std::path::Path) -> Router {
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let lemonade = std::sync::Arc::new(nautik9_lemonade::LemonadeModule::new(nautik9_lemonade::LemonadeConfig::default()));
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(nautik9_core::module::ModuleRegistry::new(hw.clone()))),
            hardware: std::sync::Arc::new(hw),
            vault_path: dir.join("vault.db"),
            sms_messages: Default::default(),
            work_config: Default::default(),
            lemonade: lemonade.clone(),
            fleet_url: "http://127.0.0.1:1".into(),
            sms_bridge: crate::SmsBridge::App,
            agent: crate::agent_api::build_agent(dir, &dir.join("vault.db"), lemonade, Default::default()),
            scheduler: None,
        };
        routes()
            .merge(crate::tts::routes())
            .layer(axum::middleware::from_fn_with_state(std::sync::Arc::new(TOKEN.to_string()), crate::auth::require_token))
            .layer(Extension(Runtime { config_dir: dir.to_path_buf(), secret: TOKEN.into() }))
            .with_state(state)
    }

    async fn raw(app: &Router, method: &str, uri: &str, bearer: bool, body: Vec<u8>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut b = Request::builder().method(method).uri(uri);
        if bearer {
            b = b.header("authorization", format!("Bearer {TOKEN}"));
        }
        let r = app.clone().oneshot(b.header("content-type", "application/json").body(Body::from(body)).unwrap()).await.unwrap();
        let (s, h) = (r.status(), r.headers().clone());
        (s, h, r.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    fn make_xdc(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, d) in entries {
            w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[tokio::test]
    async fn upload_host_serve_with_csp_and_updates() {
        let d = tempfile::tempdir().unwrap();
        let app = test_app(d.path());
        let (s, _, _) = raw(&app, "GET", "/api/webxdc/apps", false, vec![]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let z = make_xdc(&[("index.html", b"<script src=\"webxdc.js\"></script>"), ("manifest.toml", b"name = \"Poll\"")]);
        let (s, _, b) = raw(&app, "POST", "/api/webxdc/apps", true, z).await;
        assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let info: Value = serde_json::from_slice(&b).unwrap();
        let id = info["id"].as_str().unwrap().to_string();
        assert_eq!(info["name"], "Poll");

        // host page: needs the token, and hands out the view key
        let (s, _, _) = raw(&app, "GET", &format!("/api/webxdc/apps/{id}/host"), false, vec![]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, h, b) = raw(&app, "GET", &format!("/api/webxdc/apps/{id}/host"), true, vec![]).await;
        assert_eq!(s, StatusCode::OK);
        let html = String::from_utf8(b).unwrap();
        let key = view_key(TOKEN, &id);
        assert!(html.contains(&format!("/xdc/{id}/{key}/index.html")) && html.contains("sandbox=\"allow-scripts\""));
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("frame-src 'self'"));

        // app files: key required (no bearer: the frame has none), CSP blocks network
        let (s, h, b) = raw(&app, "GET", &format!("/xdc/{id}/{key}/index.html"), false, vec![]).await;
        assert_eq!(s, StatusCode::OK);
        assert!(String::from_utf8_lossy(&b).contains("webxdc.js"));
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("connect-src 'none'") && csp.contains("sandbox allow-scripts"), "{csp}");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let (_, h, b) = raw(&app, "GET", &format!("/xdc/{id}/{key}/webxdc.js"), false, vec![]).await;
        assert!(String::from_utf8_lossy(&b).contains("window.webxdc") && h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/javascript"));
        let (s, _, _) = raw(&app, "GET", &format!("/xdc/{id}/wrongkey/index.html"), false, vec![]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = raw(&app, "GET", &format!("/xdc/{id}/{key}/nope.js"), false, vec![]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        for evil in ["..%2f..%2fetc%2fpasswd", "%2e%2e/%2e%2e/etc/passwd", "a/../index.html"] {
            let (s, _, b) = raw(&app, "GET", &format!("/xdc/{id}/{key}/{evil}"), false, vec![]).await;
            assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND, "{evil}: {s} {}", String::from_utf8_lossy(&b));
        }

        // updates: ordered, owner-stamped, readable from a serial
        let up = |p: Value| serde_json::to_vec(&json!({"update": {"payload": p, "info": "i"}})).unwrap();
        let (s, _, b) = raw(&app, "POST", &format!("/api/webxdc/apps/{id}/updates"), true, up(json!({"vote": 1}))).await;
        assert_eq!((s, serde_json::from_slice::<Value>(&b).unwrap()["serial"].as_i64()), (StatusCode::OK, Some(1)));
        raw(&app, "POST", &format!("/api/webxdc/apps/{id}/updates"), true, up(json!({"vote": 2}))).await;
        let (_, _, b) = raw(&app, "GET", &format!("/api/webxdc/apps/{id}/updates?since=1"), true, vec![]).await;
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!((v["updates"].as_array().unwrap().len(), v["updates"][0]["payload"]["vote"].as_i64(), v["updates"][0]["sender"].as_str()), (1, Some(2), Some(SELF_ADDR)));
        let (s, _, _) = raw(&app, "POST", "/api/webxdc/apps/nope/updates", true, up(json!(1))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = raw(&app, "GET", "/api/webxdc/apps/nope/updates", true, vec![]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // delete
        let (s, _, _) = raw(&app, "DELETE", &format!("/api/webxdc/apps/{id}"), true, vec![]).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = raw(&app, "GET", &format!("/xdc/{id}/{key}/index.html"), false, vec![]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bad_archives_are_refused_over_http() {
        let d = tempfile::tempdir().unwrap();
        let app = test_app(d.path());
        let (s, _, b) = raw(&app, "POST", "/api/webxdc/apps", true, make_xdc(&[("index.html", b"x"), ("../evil.html", b"x")])).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&b).contains("unsafe path"));
        let (s, _, b) = raw(&app, "POST", "/api/webxdc/apps", true, make_xdc(&[("index.html", &vec![0u8; 17 << 20])])).await;
        assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::PAYLOAD_TOO_LARGE, "{s} {}", String::from_utf8_lossy(&b)); // 17 MB: over the upload or per-file cap
        let (s, _, _) = raw(&app, "POST", "/api/webxdc/apps", true, b"nope".to_vec()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (_, _, b) = raw(&app, "GET", "/api/webxdc/apps", true, vec![]).await;
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap().as_array().unwrap().len(), 0);
    }
}
