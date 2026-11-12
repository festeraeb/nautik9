//! Button debugging on demand: Wireless debugging stays OFF, the app turns it on only when the
//! owner says Yes to a prompt the fleet asked for, or when the app's own button self-test fails,
//! and turns it off again after an idle time (state, not code: `debug.json`).
//!
//! The fleet side (token-gated, reachable over Tailscale while remote care is on):
//!   GET  /api/admin/debug            what the app last reported: permission, Wireless debugging on/off,
//!                                    the adb TLS port (or why it could not be read), the prompt's answer
//!   POST /api/admin/debug/session    ask for a button-debugging session: the phone shows "Turn on Wireless
//!                                    debugging for a button test? Yes / Not now". Called again while it is on,
//!                                    it extends the idle timer (an adb session itself cannot be seen by an app).
//!   POST /api/admin/debug/off        turn it off now
//!   GET/POST /api/admin/debug/config {"idle_minutes": 30, "prompt_minutes": 10}
//!   GET  /api/admin/selftest         the last internal button self-test result
//!   POST /api/admin/selftest         run it now (the Android app picks the request up)
//!
//! The app's side (loopback only, the Java bridge posts these):
//!   POST /api/admin/device/debug_state   merge a status patch; {"announce": true} reports to Buzz #ops
//!   POST /api/admin/device/selftest      the hardware checks; this server adds its own and stores the result
//!
//! Turning it on needs android.permission.WRITE_SECURE_SETTINGS, granted once over adb. Without it the
//! app says so (409 here, and in the prompt's answer); it never pretends.
//!
//! The self-test result can only turn Wireless debugging on from the app's own test run. There is no
//! way to force a failure through this API: that would be a way around the Yes prompt.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::AppState;

pub const CONFIG_FILE: &str = "debug.json";
pub const SELFTEST_FILE: &str = "selftest.json";
pub const DEFAULT_IDLE_MINUTES: u64 = 30;
pub const DEFAULT_PROMPT_MINUTES: u64 = 10;
pub const PACKAGE: &str = "com.festeraeb.nautik9";

pub const NO_PERMISSION: &str = "WRITE_SECURE_SETTINGS is not granted to the app, so it cannot turn Wireless debugging on. \
    Grant it once over adb (it survives reboots and updates): adb shell pm grant com.festeraeb.nautik9 android.permission.WRITE_SECURE_SETTINGS";

#[derive(Default)]
pub struct DebugState {
    /// What the Android app last reported (whitelisted keys only, see `merge_device`).
    pub device: Map<String, Value>,
    pub selftest: Option<Value>,
    /// Buzz report progress: {ok, attempts, error}.
    pub announce: Value,
}

const DEVICE_KEYS: &[&str] = &[
    "permission_granted", "wireless_debugging", "on_reason", "on_since", "idle_off_at", "adb_port", "adb_port_note",
    "prompt", "last_error", "last_event", "wifi_connected",
];

/// Merge an app status patch into the stored state. Unknown keys are dropped; `null` clears a key.
pub fn merge_device(dev: &mut Map<String, Value>, patch: &Value) {
    let Some(p) = patch.as_object() else { return };
    for k in DEVICE_KEYS {
        if let Some(v) = p.get(*k) {
            if v.is_null() {
                dev.remove(*k);
            } else {
                dev.insert((*k).to_string(), v.clone());
            }
        }
    }
    dev.insert("reported_at".into(), json!(now_secs()));
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load_config(dir: &Path) -> Value {
    std::fs::read_to_string(dir.join(CONFIG_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

pub fn idle_minutes(dir: &Path) -> u64 {
    load_config(dir)["idle_minutes"].as_u64().filter(|m| (1..=1440).contains(m)).unwrap_or(DEFAULT_IDLE_MINUTES)
}

pub fn prompt_minutes(dir: &Path) -> u64 {
    load_config(dir)["prompt_minutes"].as_u64().filter(|m| (1..=240).contains(m)).unwrap_or(DEFAULT_PROMPT_MINUTES)
}

pub fn routes() -> Router<AppState> {
    let device = Router::new()
        .route("/api/admin/device/debug_state", post(device_state))
        .route("/api/admin/device/selftest", post(device_selftest))
        .route_layer(axum::middleware::from_fn(loopback_only));
    Router::new()
        .route("/api/admin/debug", get(debug_get))
        .route("/api/admin/debug/session", post(debug_session))
        .route("/api/admin/debug/off", post(debug_off))
        .route("/api/admin/debug/config", get(config_get).post(config_set))
        .route("/api/admin/selftest", get(selftest_get).post(selftest_request))
        .merge(device)
}

/// The routes only the app's own bridge may call (a tailnet peer with the token may not write device state).
async fn loopback_only(req: Request, next: Next) -> Response {
    match crate::source_gate::peer(&req) {
        Some(ip) if ip.is_loopback() => next.run(req).await,
        _ => fail(StatusCode::FORBIDDEN, "these routes are for the app on the phone itself"),
    }
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

/// Queue a device action for the Android app, replacing an unhandled one of the same kind.
fn queue(st: &AppState, kind: &str) -> Value {
    let action = json!({"id": uuid::Uuid::new_v4().to_string(), "kind": kind,
                        "created": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)});
    if let Ok(mut p) = st.admin.pending.lock() {
        p.retain(|x| x["kind"] != kind);
        p.push(action.clone());
    }
    action
}

fn debug_view(st: &AppState) -> Value {
    let (dev, announce) = st.admin.debug.lock().map(|d| (d.device.clone(), d.announce.clone())).unwrap_or_default();
    let permission = dev.get("permission_granted").and_then(|v| v.as_bool());
    let lan: Vec<String> = crate::admin_api::interface_addrs()
        .into_iter()
        .filter(|(_, ip)| crate::source_gate::classify(*ip) == Some(crate::source_gate::Class::Lan) && ip.is_ipv4())
        .map(|(n, ip)| format!("{n} {ip}"))
        .collect();
    let mut v = json!({
        "idle_minutes": idle_minutes(&st.admin.config_dir),
        "prompt_minutes": prompt_minutes(&st.admin.config_dir),
        "device": Value::Object(dev),
        "lan_addrs": lan,
        "announce": announce,
        "now": now_secs(),
        "port_fallback": "if adb_port is absent the app could not read it: scan the phone's LAN address over 1024-65535 (phone-adb rearm does)",
    });
    if permission == Some(false) {
        v["error"] = json!(NO_PERMISSION);
    }
    v
}

async fn debug_get(State(st): State<AppState>) -> Json<Value> {
    Json(debug_view(&st))
}

async fn debug_session(State(st): State<AppState>) -> Response {
    let permission = st.admin.debug.lock().ok().and_then(|d| d.device.get("permission_granted").and_then(|v| v.as_bool()));
    if permission == Some(false) {
        return (StatusCode::CONFLICT, Json(json!({"ok": false, "kind": "no_permission", "error": NO_PERMISSION,
            "grant_command": format!("adb shell pm grant {PACKAGE} android.permission.WRITE_SECURE_SETTINGS")}))).into_response();
    }
    let action = queue(&st, "wireless_debug_prompt");
    if let Ok(mut d) = st.admin.debug.lock() {
        d.device.insert("prompt".into(), json!({"id": action["id"], "answer": "waiting", "asked_at": now_secs()}));
    }
    println!("Debug: fleet asked for a button-debugging session; waiting for the owner's answer");
    Json(json!({"ok": true, "queued": true, "id": action["id"], "permission_granted": permission,
        "note": "the phone shows the prompt within a few seconds; read GET /api/admin/debug for the answer (prompt.answer: waiting, yes, not_now, expired, no_permission, already_on)"}))
    .into_response()
}

async fn debug_off(State(st): State<AppState>) -> Json<Value> {
    let action = queue(&st, "wireless_debug_off");
    Json(json!({"ok": true, "queued": true, "id": action["id"]}))
}

async fn config_get(State(st): State<AppState>) -> Json<Value> {
    Json(json!({"idle_minutes": idle_minutes(&st.admin.config_dir), "prompt_minutes": prompt_minutes(&st.admin.config_dir)}))
}

async fn config_set(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let dir = &st.admin.config_dir;
    let mut cur = load_config(dir);
    for (k, lo, hi) in [("idle_minutes", 1u64, 1440u64), ("prompt_minutes", 1, 240)] {
        if let Some(v) = b.get(k) {
            match v.as_u64().filter(|m| (lo..=hi).contains(m)) {
                Some(m) => cur[k] = json!(m),
                None => return fail(StatusCode::BAD_REQUEST, format!("{k} must be a whole number from {lo} to {hi}")),
            }
        }
    }
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(dir.join(CONFIG_FILE), serde_json::to_string_pretty(&cur).unwrap_or_default())) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("could not save {CONFIG_FILE}: {e}"));
    }
    Json(json!({"ok": true, "idle_minutes": idle_minutes(dir), "prompt_minutes": prompt_minutes(dir)})).into_response()
}

async fn device_state(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    if let Ok(mut d) = st.admin.debug.lock() {
        merge_device(&mut d.device, &b);
    }
    if b["announce"].as_bool().unwrap_or(false) {
        let st2 = st.clone();
        tokio::spawn(async move { announce(st2).await });
    }
    Json(json!({"ok": true, "idle_minutes": idle_minutes(&st.admin.config_dir), "prompt_minutes": prompt_minutes(&st.admin.config_dir)})).into_response()
}

// ---- the self-test ----

async fn selftest_get(State(st): State<AppState>) -> Json<Value> {
    let mem = st.admin.debug.lock().ok().and_then(|d| d.selftest.clone());
    let res = mem.or_else(|| std::fs::read_to_string(st.admin.config_dir.join(SELFTEST_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()));
    let queued = st.admin.pending.lock().map(|p| p.iter().any(|x| x["kind"] == "self_test")).unwrap_or(false);
    Json(json!({"result": res, "run_requested": queued}))
}

async fn selftest_request(State(st): State<AppState>) -> Json<Value> {
    let action = queue(&st, "self_test");
    Json(json!({"ok": true, "queued": true, "id": action["id"], "note": "read GET /api/admin/selftest in a few seconds (GPS may wait up to 12 s)"}))
}

/// The checks this server can make itself: it is answering, a theme can be applied and saved, the Nexus SDK is loaded.
fn server_checks(dir: &Path) -> Vec<Value> {
    let mut out = vec![json!({"name": "local server health", "ok": true, "detail": "answered this request"})];
    let ui = dir.join("ui.json");
    let before = std::fs::read_to_string(&ui).ok();
    let cur: Value = before.as_deref().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(json!({}));
    let was = cur["theme"].as_str().filter(|t| crate::apps_api::valid_theme(t)).unwrap_or("dark").to_string();
    let probe = if was == "lakes" { "dead" } else { "lakes" };
    let write = |t: &str| std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&ui, serde_json::to_string_pretty(&json!({"theme": t, "plain": cur["plain"].as_bool().unwrap_or(false)})).unwrap_or_default()));
    let read = || std::fs::read_to_string(&ui).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["theme"].as_str().map(str::to_string));
    let ok = write(probe).is_ok() && read().as_deref() == Some(probe);
    // Put the owner's theme back exactly as it was (or remove the file we created).
    match before {
        Some(t) => { let _ = std::fs::write(&ui, t); }
        None => { let _ = std::fs::remove_file(&ui); }
    }
    out.push(json!({"name": "theme apply", "ok": ok, "detail": if ok { format!("{probe} saved and read back; {was} restored") } else { "could not save the theme".to_string() }}));
    let (loaded, what) = crate::nexus_api::sdk_state();
    out.push(json!({"name": "Nexus SDK loaded", "ok": loaded, "detail": what}));
    out
}

async fn device_selftest(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let mut checks: Vec<Value> = b["checks"].as_array().cloned().unwrap_or_default();
    checks.extend(server_checks(&st.admin.config_dir));
    let failed: Vec<String> = checks.iter().filter(|c| c["ok"] != true).map(|c| c["name"].as_str().unwrap_or("?").to_string()).collect();
    let result = json!({
        "ok": failed.is_empty(), "failed": failed, "checks": checks,
        "trigger": b["trigger"].as_str().unwrap_or("request"), "app": b["app"].as_str(),
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    if let Ok(mut d) = st.admin.debug.lock() {
        d.selftest = Some(result.clone());
    }
    let _ = std::fs::write(st.admin.config_dir.join(SELFTEST_FILE), serde_json::to_string_pretty(&result).unwrap_or_default());
    println!("Self-test ({}): {}", result["trigger"].as_str().unwrap_or(""), if failed.is_empty() { "all passed".to_string() } else { format!("FAILED: {}", failed.join(", ")) });
    Json(json!({"ok": true, "result": result})).into_response()
}

// ---- telling the fleet ----

/// The one-line Buzz message for the current state. Pure, so the wording is tested.
pub fn announce_text(dev: &Map<String, Value>, selftest: Option<&Value>, lan: &[String], tailnet: &[String], idle_min: u64) -> String {
    let why = match dev.get("on_reason").and_then(|v| v.as_str()) {
        Some("self_test_failed") => {
            let names = selftest.and_then(|s| s["failed"].as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            format!("its button self-test failed ({})", if names.is_empty() { "see GET /api/admin/selftest" } else { &names })
        }
        Some("fleet_request") => "the owner said Yes to the button-debugging prompt".to_string(),
        _ => "a request".to_string(),
    };
    let mut s = format!("NautiK9 on the phone: {why}. ");
    if dev.get("wireless_debugging").and_then(|v| v.as_bool()) == Some(true) {
        s += "Wireless debugging is ON. ";
        match dev.get("adb_port").and_then(|v| v.as_u64()) {
            Some(p) => s += &format!("adb TLS port {p} (read from the phone). "),
            None => s += &format!("The adb TLS port could not be read by the app ({}); scan 1024-65535 on the LAN address (phone-adb rearm). ", dev.get("adb_port_note").and_then(|v| v.as_str()).unwrap_or("no reason given")),
        }
        if !lan.is_empty() || !tailnet.is_empty() {
            s += &format!("Addresses: {}. ", lan.iter().chain(tailnet.iter()).cloned().collect::<Vec<_>>().join(", "));
        }
        s += &format!("It turns itself off after {idle_min} idle minutes (extend: POST /api/admin/debug/session).");
    } else {
        s += &format!("Wireless debugging could NOT be turned on: {}", dev.get("last_error").and_then(|v| v.as_str()).unwrap_or("reason not reported"));
    }
    s
}

async fn announce(st: AppState) {
    use crate::source_gate::{classify, Class};
    let sender = std::env::var("NAUTIK9_NEXUS_DEVICE").unwrap_or_else(|_| "nautik9-phone".into());
    let mut last_err = String::new();
    for attempt in 1..=6u32 {
        let (dev, test) = st.admin.debug.lock().map(|d| (d.device.clone(), d.selftest.clone())).unwrap_or_default();
        let all = crate::admin_api::interface_addrs();
        let lan: Vec<String> = all.iter().filter(|(_, ip)| ip.is_ipv4() && classify(*ip) == Some(Class::Lan)).map(|(_, ip)| format!("LAN {ip}")).collect();
        let ts: Vec<String> = crate::admin_api::tailnet_addrs_from(&all).into_iter().filter(|ip| ip.is_ipv4()).map(|ip| format!("tailnet {ip}")).collect();
        let body = announce_text(&dev, test.as_ref(), &lan, &ts, idle_minutes(&st.admin.config_dir));
        let req = json!({"sender": sender, "channel": "ops", "body": body});
        match crate::buzz_api::buzz_req(&st, &st.admin.config_dir, reqwest::Method::POST, "/board/message", Some(req)).await {
            Ok(_) => {
                if let Ok(mut d) = st.admin.debug.lock() {
                    d.announce = json!({"ok": true, "attempts": attempt});
                }
                println!("Debug: reported to the fleet (Buzz #ops)");
                return;
            }
            Err(r) => last_err = format!("HTTP {}", r.status().as_u16()),
        }
        if let Ok(mut d) = st.admin.debug.lock() {
            d.announce = json!({"ok": false, "attempts": attempt, "error": last_err});
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
    println!("Debug: could not report to the fleet after 6 tries ({last_err})");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    const TOKEN: &str = "debug-test-token-0123456789abcdef";

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
        let s = r.status();
        (s, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
    }

    async fn call(st: &AppState, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        call_from(st, "127.0.0.1", m, uri, body).await
    }

    fn pending_kinds(st: &AppState) -> Vec<String> {
        st.admin.pending.lock().unwrap().iter().map(|a| a["kind"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn a_session_request_queues_a_prompt_and_reads_back_as_waiting() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let (s, v) = call(&st, "POST", "/api/admin/debug/session", None).await;
        assert_eq!((s, v["queued"].as_bool()), (StatusCode::OK, Some(true)), "{v}");
        assert_eq!(pending_kinds(&st), vec!["wireless_debug_prompt"]);
        let (_, g) = call(&st, "GET", "/api/admin/debug", None).await;
        assert_eq!(g["device"]["prompt"]["answer"], "waiting");
        assert_eq!(g["idle_minutes"], 30, "default idle time");
        // asking twice leaves one prompt, not two
        call(&st, "POST", "/api/admin/debug/session", None).await;
        assert_eq!(pending_kinds(&st), vec!["wireless_debug_prompt"]);
    }

    #[tokio::test]
    async fn a_missing_permission_is_said_plainly_and_nothing_is_queued() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let (s, _) = call(&st, "POST", "/api/admin/device/debug_state", Some(json!({"permission_granted": false}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, v) = call(&st, "POST", "/api/admin/debug/session", None).await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(v["kind"], "no_permission");
        assert!(v["error"].as_str().unwrap().contains("adb shell pm grant com.festeraeb.nautik9 android.permission.WRITE_SECURE_SETTINGS"), "{v}");
        assert!(pending_kinds(&st).is_empty());
        assert!(call(&st, "GET", "/api/admin/debug", None).await.1["error"].is_string());
    }

    #[tokio::test]
    async fn only_the_app_itself_writes_device_state_and_unknown_keys_are_dropped() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        let from_tailnet = call_from(&st, "100.64.1.2", "POST", "/api/admin/device/debug_state", Some(json!({"wireless_debugging": true}))).await.0;
        // remote care is off in the test state, so the gate answers first; with it on the loopback rule still holds
        assert_eq!(from_tailnet, StatusCode::FORBIDDEN);
        crate::admin_api::RemoteCare { enabled: true }.save(d.path()).unwrap();
        *st.admin.gate.remote_care.write().unwrap() = crate::admin_api::RemoteCare { enabled: true };
        let (s, _) = call_from(&st, "100.64.1.2", "POST", "/api/admin/device/debug_state", Some(json!({"wireless_debugging": true}))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "a tailnet peer cannot write the phone's state");
        let (s, _) = call_from(&st, "100.64.1.2", "POST", "/api/admin/device/selftest", Some(json!({"checks": []}))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(call_from(&st, "100.64.1.2", "GET", "/api/admin/debug", None).await.0, StatusCode::OK, "but it may read it");
        call(&st, "POST", "/api/admin/device/debug_state", Some(json!({"wireless_debugging": true, "adb_port": 41613, "evil": 1}))).await;
        let (_, g) = call(&st, "GET", "/api/admin/debug", None).await;
        assert_eq!((g["device"]["wireless_debugging"].as_bool(), g["device"]["adb_port"].as_u64()), (Some(true), Some(41613)));
        assert!(g["device"].get("evil").is_none());
        call(&st, "POST", "/api/admin/device/debug_state", Some(json!({"adb_port": null}))).await;
        assert!(call(&st, "GET", "/api/admin/debug", None).await.1["device"].get("adb_port").is_none(), "null clears");
    }

    #[tokio::test]
    async fn idle_time_is_state_with_a_default_and_bounds() {
        let d = tempfile::tempdir().unwrap();
        let st = state(d.path());
        assert_eq!(call(&st, "GET", "/api/admin/debug/config", None).await.1["idle_minutes"], 30);
        let (s, v) = call(&st, "POST", "/api/admin/debug/config", Some(json!({"idle_minutes": 5}))).await;
        assert_eq!((s, v["idle_minutes"].as_u64()), (StatusCode::OK, Some(5)));
        assert_eq!(idle_minutes(d.path()), 5, "kept in debug.json for the app and the next start");
        for bad in [json!({"idle_minutes": 0}), json!({"idle_minutes": 5000}), json!({"idle_minutes": "soon"})] {
            assert_eq!(call(&st, "POST", "/api/admin/debug/config", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        }
        assert_eq!(idle_minutes(d.path()), 5);
    }

    #[tokio::test]
    async fn the_selftest_adds_server_checks_stores_the_result_and_lists_failures() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("ui.json"), r#"{"theme":"dead","plain":true}"#).unwrap();
        let st = state(d.path());
        let (s, q) = call(&st, "POST", "/api/admin/selftest", None).await;
        assert_eq!((s, q["queued"].as_bool()), (StatusCode::OK, Some(true)));
        assert_eq!(pending_kinds(&st), vec!["self_test"]);
        let body = json!({"trigger": "request", "app": "0.11.0 (56)", "checks": [
            {"name": "haptic", "ok": true, "detail": "vibrated"}, {"name": "torch on/off", "ok": false, "detail": "no flash unit"}]});
        let (s, v) = call(&st, "POST", "/api/admin/device/selftest", Some(body)).await;
        assert_eq!(s, StatusCode::OK);
        let r = &v["result"];
        assert_eq!(r["ok"], false);
        let failed: Vec<&str> = r["failed"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert!(failed.contains(&"torch on/off"), "{r}");
        let names: Vec<&str> = r["checks"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        for n in ["haptic", "local server health", "theme apply", "Nexus SDK loaded"] {
            assert!(names.contains(&n), "{n} in {names:?}");
        }
        assert_eq!(std::fs::read_to_string(d.path().join("ui.json")).unwrap(), r#"{"theme":"dead","plain":true}"#, "the owner's theme is put back byte for byte");
        let (_, g) = call(&st, "GET", "/api/admin/selftest", None).await;
        assert_eq!(g["result"]["trigger"], "request");
        assert!(d.path().join(SELFTEST_FILE).exists(), "survives a restart");
        // A request body cannot force a failure: unknown fields are ignored.
        let (_, v) = call(&st, "POST", "/api/admin/device/selftest", Some(json!({"checks": [], "force_fail": true}))).await;
        assert!(v["result"]["failed"].as_array().unwrap().iter().all(|f| f != "force_fail"));
    }

    #[test]
    fn the_buzz_line_says_port_reason_and_when_it_turns_off() {
        let mut dev = Map::new();
        merge_device(&mut dev, &json!({"wireless_debugging": true, "on_reason": "self_test_failed", "adb_port": 41613}));
        let t = json!({"failed": ["torch on/off", "gps"]});
        let s = announce_text(&dev, Some(&t), &["LAN 192.168.1.50".into()], &["tailnet 100.64.1.3".into()], 30);
        assert!(s.contains("self-test failed (torch on/off, gps)") && s.contains("port 41613") && s.contains("192.168.1.50") && s.contains("30 idle minutes"), "{s}");
        merge_device(&mut dev, &json!({"adb_port": null, "adb_port_note": "mDNS did not show this phone"}));
        let s = announce_text(&dev, None, &[], &[], 30);
        assert!(s.contains("could not be read") && s.contains("mDNS did not show this phone") && s.contains("1024-65535"), "{s}");
        merge_device(&mut dev, &json!({"wireless_debugging": false, "last_error": "WRITE_SECURE_SETTINGS is not granted"}));
        let s = announce_text(&dev, None, &[], &[], 30);
        assert!(s.contains("could NOT be turned on") && s.contains("WRITE_SECURE_SETTINGS"), "{s}");
    }
}
