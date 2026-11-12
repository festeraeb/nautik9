//! Nexus admission from the app: accept or reject a device that asks to join, revoke and k-line one,
//! un-ban. The app does not decide anything: it forwards to the admission gate (nexus-gate) and shows its answer.
//!
//!   GET  /api/nexus/admin/state     {configured:false} without the admin file, else the gate's state
//!   POST /api/nexus/admin/accept    {request_id}
//!   POST /api/nexus/admin/reject    {request_id}
//!   POST /api/nexus/admin/revoke    {key, label?}
//!   POST /api/nexus/admin/unban     {key}
//!
//! The gate's address and bearer token are in `<config dir>/nexus/nexus-admin.json` (`{"url": "...", "token": "..."}`,
//! mode 0600), written by the operator on the device that may administer. They are not in this repository, are
//! never returned by any route, and without the file every route answers "not configured".

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use crate::AppState;

static FILE: OnceLock<PathBuf> = OnceLock::new();

pub fn init(config_dir: &Path) {
    let _ = FILE.set(config_dir.join("nexus").join("nexus-admin.json"));
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/nexus/admin/state", get(state))
        .route("/api/nexus/admin/{op}", post(act))
}

struct Gate {
    url: String,
    token: String,
}

fn load() -> Option<Gate> {
    let text = std::fs::read_to_string(FILE.get()?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let url = v["url"].as_str()?.trim_end_matches('/').to_string();
    let token = v["token"].as_str()?.to_string();
    let url_ok = url.starts_with("http://") || url.starts_with("https://");
    let token_ok = token.len() >= 32 && !token.contains(char::is_whitespace);
    (url_ok && token_ok).then_some(Gate { url, token })
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(8)).connect_timeout(Duration::from_secs(4)).build().unwrap_or_default()
}

fn reply(status: StatusCode, v: Value) -> Response {
    (status, Json(v)).into_response()
}

async fn forward(req: reqwest::RequestBuilder, g: &Gate) -> Response {
    match req.bearer_auth(&g.token).send().await {
        Ok(r) => {
            let st = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body: Value = r.json().await.unwrap_or(Value::Null);
            if st.is_success() {
                let mut b = body;
                if let Some(o) = b.as_object_mut() {
                    o.insert("configured".into(), json!(true));
                    o.entry("ok").or_insert(json!(true));
                }
                reply(StatusCode::OK, b)
            } else {
                reply(st, json!({"ok": false, "configured": true, "error": body["error"].as_str().unwrap_or("the admission service refused")}))
            }
        }
        Err(e) => reply(StatusCode::BAD_GATEWAY, json!({"ok": false, "configured": true, "error": format!("admission service not reached: {}", e.without_url())})),
    }
}

async fn state() -> Response {
    let Some(g) = load() else { return reply(StatusCode::OK, json!({"ok": true, "configured": false})) };
    forward(client().get(format!("{}/admin/state", g.url)), &g).await
}

async fn act(axum::extract::Path(op): axum::extract::Path<String>, body: Bytes) -> Response {
    if !matches!(op.as_str(), "accept" | "reject" | "revoke" | "unban") {
        return reply(StatusCode::NOT_FOUND, json!({"ok": false, "error": "unknown action"}));
    }
    let Some(g) = load() else { return reply(StatusCode::OK, json!({"ok": false, "configured": false, "error": "not configured on this device"})) };
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    // Only the fields the gate reads are passed on.
    let mut out = serde_json::Map::new();
    for k in ["request_id", "key", "label"] {
        if let Some(x) = v[k].as_str() {
            out.insert(k.into(), json!(x));
        }
    }
    let out = Value::Object(out);
    forward(client().post(format!("{}/admin/{}", g.url, op)).json(&out), &g).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_file_nothing_is_configured() {
        let d = std::env::temp_dir().join(format!("n9-admin-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join("nexus")).unwrap();
        init(&d);
        assert!(load().is_none());
        std::fs::write(d.join("nexus").join("nexus-admin.json"), r#"{"url":"http://127.0.0.1:1/","token":"short"}"#).unwrap();
        assert!(load().is_none(), "a short token is not accepted");
        std::fs::write(d.join("nexus").join("nexus-admin.json"), format!(r#"{{"url":"http://127.0.0.1:1/","token":"{}"}}"#, "x".repeat(40))).unwrap();
        let g = load().unwrap();
        assert_eq!(g.url, "http://127.0.0.1:1");
        let _ = std::fs::remove_dir_all(d);
    }
}
