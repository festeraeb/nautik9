//! Nexus: join the fleet's Lemonade Nexus network from the app, show it, leave it.
//!
//!   GET    /api/nexus/status      what the Nexus screen shows (nexus-join status: joined, device name,
//!                                 path to the server, peers with their path, handshake, bytes, stale)
//!   POST   /api/nexus/offer       an invite (the bundle JSON, the sidecar's invite file, or an 8-digit
//!                                 code for a network already known here); stored for the user to confirm
//!   DELETE /api/nexus/offer       drop the pending invite
//!   POST   /api/nexus/join        {device_name?} join with the pending invite
//!   POST   /api/nexus/leave       {force?} leave (force = forget here when the server is not reached)
//!   POST   /api/nexus/network     {what} the phone's network changed (Wi-Fi <-> cellular): new socket,
//!                                 fresh handshake, path check; no rejoin
//!   POST   /api/nexus/lifecycle   {state: "background" | "foreground"}
//!
//! An invite is a single-use secret: it is stored 0600 until the join and never returned by any route.
//! The SDK (liblemonade_nexus_sdk.so) is looked for in $NAUTIK9_NEXUS_SDK, next to this program (the
//! Android app's native library folder), then on the loader path. Without it the screen says so.

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nexus_join::{Config, Nexus};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::AppState;

static NEXUS: OnceLock<Nexus> = OnceLock::new();

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/nexus/status", get(status))
        .route("/api/nexus/offer", post(offer).delete(discard))
        .route("/api/nexus/join", post(join))
        .route("/api/nexus/leave", post(leave))
        .route("/api/nexus/network", post(network))
        .route("/api/nexus/lifecycle", post(lifecycle))
}

fn sdk_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("NAUTIK9_NEXUS_SDK") {
        v.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(dir.join("liblemonade_nexus_sdk.so"));
    }
    v.push(PathBuf::from("liblemonade_nexus_sdk.so"));
    v
}

/// Start the module (once, at serve start). Rejoins in the background when this device was joined.
pub fn init(config_dir: &Path, device_hint: &str) {
    let name = std::env::var("NAUTIK9_NEXUS_DEVICE").unwrap_or_else(|_| {
        let d: String = device_hint.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        format!("nautik9-{}", if d.is_empty() { "device".into() } else { d })
    });
    let n = Nexus::open(Config { dir: config_dir.join("nexus"), sdk_paths: sdk_paths(), device_name: name, poll_secs: 5 });
    if NEXUS.set(n).is_ok() {
        nautik9_lemonade::set_mesh_resolver(resolve_route);
    }
}

fn nexus() -> Result<Nexus, Response> {
    NEXUS.get().cloned().ok_or_else(|| fail(StatusCode::SERVICE_UNAVAILABLE, "Nexus is not started in this server"))
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

/// Provider chain hook: a provider with `"nexus": {"peer", "port", "path"?}` is reached through the
/// mesh (a loopback port the SDK bridges to the peer's tunnel IP), or skipped with the reason.
fn resolve_route(route: &nautik9_lemonade::NexusRoute) -> Result<String, String> {
    let n = NEXUS.get().ok_or("Nexus is not started")?;
    let s = n.status();
    if s["joined"] != true {
        return Err("not on the Nexus".into());
    }
    if s["tunnel_up"] != true {
        return Err("the Nexus is not connected right now".into());
    }
    let ip = n.peer_ip(&route.peer).ok_or_else(|| format!("no Nexus peer named {}", route.peer))?;
    let lp = n.egress(&ip, route.port)?;
    Ok(format!("http://127.0.0.1:{lp}{}", route.path.as_deref().unwrap_or("/v1")))
}

async fn blocking<F: FnOnce(Nexus) -> Response + Send + 'static>(f: F) -> Response {
    let n = match nexus() {
        Ok(n) => n,
        Err(r) => return r,
    };
    tokio::task::spawn_blocking(move || f(n)).await.unwrap_or_else(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn status() -> Response {
    blocking(|n| Json(n.status()).into_response()).await
}

async fn offer(body: Bytes) -> Response {
    let text = String::from_utf8_lossy(&body).to_string();
    // The UI may wrap pasted text as {"text": "..."}; a bundle or invite file is sent as is.
    let text = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["text"].as_str().map(str::to_string)).unwrap_or(text);
    blocking(move |n| match n.offer(&text) {
        Ok(summary) => Json(json!({"ok": true, "offer": summary})).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    })
    .await
}

async fn discard() -> Response {
    blocking(|n| {
        n.discard_offer();
        Json(json!({"ok": true})).into_response()
    })
    .await
}

async fn join(body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = v["device_name"].as_str().map(str::to_string);
    blocking(move |n| match n.join(name.as_deref()) {
        Ok(m) => Json(json!({"ok": true, "membership": m})).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    })
    .await
}

async fn leave(body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let force = v["force"].as_bool().unwrap_or(false);
    blocking(move |n| match n.leave(force) {
        Ok(r) => Json(json!({"ok": true, "result": r})).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    })
    .await
}

async fn network(body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let what = v["what"].as_str().unwrap_or("changed").chars().take(40).collect::<String>();
    blocking(move |n| Json(json!({"ok": true, "event": n.network_changed(&what)})).into_response()).await
}

async fn lifecycle(body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let st = v["state"].as_str().unwrap_or("").to_string();
    blocking(move |n| {
        match st.as_str() {
            "background" => n.suspend(),
            "foreground" => n.resume(),
            _ => return fail(StatusCode::BAD_REQUEST, "state: background or foreground"),
        }
        Json(json!({"ok": true})).into_response()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_search_order_starts_with_the_env_override() {
        std::env::set_var("NAUTIK9_NEXUS_SDK", "/x/liblemonade_nexus_sdk.so");
        let p = sdk_paths();
        assert_eq!(p[0], PathBuf::from("/x/liblemonade_nexus_sdk.so"));
        assert_eq!(p.last().unwrap(), &PathBuf::from("liblemonade_nexus_sdk.so"));
        std::env::remove_var("NAUTIK9_NEXUS_SDK");
    }
}
