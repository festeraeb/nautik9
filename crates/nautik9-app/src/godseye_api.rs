//! God's Eye in the app: two ways to see the globe, and the phone's position track.
//!
//!   GET  /api/godseye/settings   {mode, thresholds, track_url, stream_url, has_token, label}
//!   POST /api/godseye/settings   {mode?, thresholds?, track_url?, stream_url?, label?, token?}  (token is write-only)
//!   GET  /api/godseye/status     where the globe, the stream and the track service are and whether each answers
//!   POST /api/godseye/fix        {lat, lon, alt_m?, accuracy_m?, heading_deg?, speed_mps?, time_ms?}
//!                                forwarded to the home track service with this device's token; nothing is stored here
//!
//! LOCAL mode renders the globe in the phone's WebView from the home track service's `/globe` page
//! (CesiumJS and aerial imagery); STREAM mode shows the GPU-rendered kiosk over noVNC, view-only.
//! Which one is used is decided by the page from what the device measures (see index.html `geDecide`);
//! the thresholds are run-time settings kept here, never constants in the page. A fix only ever goes
//! to the tailnet address of the home machine; with Tailscale off the call fails with a plain message.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::buzz_api::{derive_with_port, precheck};
use crate::webxdc_api::Runtime;
use crate::AppState;

const TRACK_PORT: u16 = 4176;
const STREAM_PORT: u16 = 6081;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/godseye/settings", get(settings_get).post(settings_set))
        .route("/api/godseye/status", get(status))
        .route("/api/godseye/fix", post(fix))
}

fn fail(status: StatusCode, kind: &str, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "kind": kind, "error": msg.into()}))).into_response()
}

pub fn default_thresholds() -> Value {
    // Run-time settings, not constants: edit them in Settings (or godseye.json) as the probe numbers come in.
    json!({"min_fps": 30.0, "min_texture": 8192, "min_memory_gb": 4.0, "min_cores": 6, "probe_ms": 2000, "allow_save_data": false})
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub mode: String,
    pub thresholds: Value,
    pub track_url: Option<String>,
    pub stream_url: Option<String>,
    pub label: String,
    /// Which GPU's kiosk to watch: a name in the home websockify token file (default the t440 P100).
    pub stream_target: String,
}

pub fn load(dir: &Path) -> Settings {
    let v: Value = std::fs::read_to_string(dir.join("godseye.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let mut th = default_thresholds();
    if let (Some(o), Some(u)) = (th.as_object_mut(), v["thresholds"].as_object()) {
        for (k, val) in u {
            if o.contains_key(k) {
                o.insert(k.clone(), val.clone());
            }
        }
    }
    let s = |k: &str| v[k].as_str().map(|x| x.trim().trim_end_matches('/').to_string()).filter(|x| !x.is_empty());
    Settings {
        mode: match v["mode"].as_str() { Some(m @ ("local" | "stream")) => m.to_string(), _ => "auto".to_string() },
        thresholds: th,
        track_url: s("track_url"),
        stream_url: s("stream_url"),
        label: s("label").unwrap_or_else(|| "phone".to_string()),
        stream_target: s("stream_target").filter(|t| t.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')).unwrap_or_else(|| "t440-p100".to_string()),
    }
}

fn token(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("godseye.token")).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

fn urls(st: &AppState, dir: &Path) -> (Option<String>, Option<String>) {
    let s = load(dir);
    (s.track_url.or_else(|| derive_with_port(&st.fleet_url, TRACK_PORT)), s.stream_url.or_else(|| derive_with_port(&st.fleet_url, STREAM_PORT)))
}

fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://")) && u.len() < 200 && !u.contains(char::is_whitespace)
}

async fn settings_get(State(st): State<AppState>, Extension(rt): Extension<Runtime>) -> Json<Value> {
    let s = load(&rt.config_dir);
    let (t, v) = urls(&st, &rt.config_dir);
    Json(json!({"mode": s.mode, "thresholds": s.thresholds, "track_url": t, "stream_url": v, "label": s.label, "stream_target": s.stream_target, "has_token": token(&rt.config_dir).is_some(),
                "track_url_source": if s.track_url.is_some() { "setting" } else { "derived from the fleet address (same host, port 4176)" },
                "stream_url_source": if s.stream_url.is_some() { "setting" } else { "derived from the fleet address (same host, port 6081)" }}))
}

async fn settings_set(Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let mut cur: Value = std::fs::read_to_string(rt.config_dir.join("godseye.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if let Some(m) = p["mode"].as_str() {
        if !matches!(m, "auto" | "local" | "stream") {
            return fail(StatusCode::BAD_REQUEST, "bad_request", "mode must be auto, local or stream");
        }
        cur["mode"] = json!(m);
    }
    for k in ["track_url", "stream_url"] {
        if let Some(v) = p[k].as_str() {
            let v = v.trim().trim_end_matches('/');
            if v.is_empty() {
                cur.as_object_mut().map(|o| o.remove(k));
            } else if valid_url(v) {
                cur[k] = json!(v);
            } else {
                return fail(StatusCode::BAD_REQUEST, "bad_request", format!("{k} must be an http(s) URL"));
            }
        }
    }
    if let Some(t) = p["stream_target"].as_str() {
        let t = t.trim();
        if t.is_empty() || !t.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') || t.len() > 40 {
            return fail(StatusCode::BAD_REQUEST, "bad_request", "stream_target is a name like t440-p100");
        }
        cur["stream_target"] = json!(t);
    }
    if let Some(l) = p["label"].as_str() {
        let l: String = l.chars().filter(|c| c.is_alphanumeric() || "-_ .".contains(*c)).take(32).collect();
        if !l.trim().is_empty() {
            cur["label"] = json!(l.trim());
        }
    }
    if let Some(t) = p["thresholds"].as_object() {
        let defaults = default_thresholds();
        let mut th = cur["thresholds"].as_object().cloned().unwrap_or_default();
        for (k, v) in t {
            let ok = match (defaults.get(k), v) {
                (Some(Value::Bool(_)), Value::Bool(_)) => true,
                (Some(Value::Number(_)), Value::Number(n)) => n.as_f64().map_or(false, |x| x.is_finite() && x >= 0.0 && x <= 1.0e6),
                _ => false,
            };
            if !ok {
                return fail(StatusCode::BAD_REQUEST, "bad_request", format!("threshold {k} is not a known setting or not a sensible value"));
            }
            th.insert(k.clone(), v.clone());
        }
        cur["thresholds"] = Value::Object(th);
    }
    if let Some(k) = p["token"].as_str() {
        let k = k.trim();
        let path = rt.config_dir.join("godseye.token");
        let r = if k.is_empty() { std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }) } else { crate::buzz_api::write_private_pub(&path, k) };
        if let Err(e) = r {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "io", format!("could not save the track token: {e}"));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&rt.config_dir).and_then(|_| std::fs::write(rt.config_dir.join("godseye.json"), serde_json::to_string_pretty(&cur).unwrap_or_default())) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "io", format!("could not save settings: {e}"));
    }
    Json(json!({"ok": true, "has_token": token(&rt.config_dir).is_some()})).into_response()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(6)).connect_timeout(Duration::from_secs(4)).build().unwrap_or_default()
}

async fn one(name: &str, base: &Option<String>, path: &str) -> Value {
    let Some(b) = base else { return json!({"name": name, "ok": false, "kind": "not_configured", "message": "no address (set one in Settings, or give the app a fleet address)"}) };
    if let Some((kind, msg)) = precheck(b).await {
        return json!({"name": name, "ok": false, "kind": kind, "message": msg});
    }
    match client().get(format!("{b}{path}")).send().await {
        Ok(r) if r.status().is_success() => json!({"name": name, "ok": true, "url": b}),
        Ok(r) => json!({"name": name, "ok": false, "kind": "refused", "message": format!("answered HTTP {}", r.status().as_u16())}),
        Err(_) => json!({"name": name, "ok": false, "kind": "unreachable", "message": "no answer"}),
    }
}

async fn status(State(st): State<AppState>, Extension(rt): Extension<Runtime>) -> Json<Value> {
    let (t, v) = urls(&st, &rt.config_dir);
    let target = load(&rt.config_dir).stream_target;
    let (track, stream) = tokio::join!(one("track", &t, "/healthz"), one("stream", &v, "/vnc_lite.html"));
    Json(json!({"track": track, "stream": stream, "globe_url": t.as_ref().map(|u| format!("{u}/globe")),
                "stream_view_url": v.as_ref().map(|u| format!("{u}/vnc_lite.html?autoconnect=true&resize=scale&reconnect=true&view_only=true&path=websockify%3Ftoken%3D{target}"))}))
}

async fn fix(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let Some(tok) = token(&rt.config_dir) else {
        return fail(StatusCode::UNAUTHORIZED, "no_token", "enter the track token in Settings first");
    };
    let (Some(lat), Some(lon)) = (p["lat"].as_f64(), p["lon"].as_f64()) else {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "lat and lon are required");
    };
    let (track, _) = urls(&st, &rt.config_dir);
    let Some(base) = track else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "not_configured", "no track address");
    };
    if let Some((kind, msg)) = precheck(&base).await {
        return fail(StatusCode::SERVICE_UNAVAILABLE, kind, msg);
    }
    let body = json!({"lat": lat, "lon": lon, "alt": p["alt_m"], "accuracy": p["accuracy_m"], "heading": p["heading_deg"], "speed": p["speed_mps"], "ts": p["time_ms"]});
    match client().post(format!("{base}/api/track/fix")).header("X-Device-Token", tok).json(&body).send().await {
        Ok(r) if r.status().is_success() => Json(json!({"ok": true})).into_response(),
        Ok(r) if r.status() == StatusCode::UNAUTHORIZED => fail(StatusCode::UNAUTHORIZED, "bad_token", "the track service did not accept the token; enter it again in Settings"),
        Ok(r) => fail(StatusCode::BAD_GATEWAY, "refused", format!("the track service answered HTTP {}", r.status().as_u16())),
        Err(_) => fail(StatusCode::BAD_GATEWAY, "unreachable", "no answer from the track service"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    type Log = Arc<Mutex<Vec<(String, Value)>>>;

    /// A stand-in for the home track service: records the token header and body of each fix.
    async fn mock() -> (String, Log) {
        use axum::extract::State as S;
        use axum::http::HeaderMap;
        let log: Log = Default::default();
        async fn health() -> &'static str { "ok" }
        async fn vnc() -> &'static str { "<html>noVNC</html>" }
        async fn fixh(S(l): S<Log>, h: HeaderMap, Json(b): Json<Value>) -> Response {
            let t = h.get("x-device-token").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            l.lock().unwrap().push((t.clone(), b));
            if t != "good-token-0123456789abcdef" {
                return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false}))).into_response();
            }
            Json(json!({"ok": true})).into_response()
        }
        let app = Router::new().route("/healthz", get(health)).route("/vnc_lite.html", get(vnc)).route("/api/track/fix", post(fixh)).with_state(log.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, log)
    }

    fn app(dir: &std::path::Path) -> Router {
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let state = crate::AppState::test_state(dir, hw, "http://100.64.1.2:9122");
        routes().with_state(state).layer(Extension(Runtime { config_dir: dir.to_path_buf(), secret: "t".into() }))
    }
    async fn call(a: &Router, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let b = Request::builder().method(m).uri(uri);
        let req = match body { Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(), None => b.body(Body::empty()).unwrap() };
        let r = a.clone().oneshot(req).await.unwrap();
        let st = r.status();
        (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn settings_thresholds_are_run_time_validated_and_the_token_is_write_only() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (_, g) = call(&a, "GET", "/api/godseye/settings", None).await;
        assert_eq!((g["mode"].as_str(), g["thresholds"]["min_fps"].as_f64(), g["has_token"].as_bool()), (Some("auto"), Some(30.0), Some(false)));
        assert_eq!(g["track_url"], "http://100.64.1.2:4176", "derived from the fleet host, no address in the repo");
        assert_eq!(g["stream_url"], "http://100.64.1.2:6081");
        let (s, _) = call(&a, "POST", "/api/godseye/settings", Some(json!({"mode": "warp"}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(&a, "POST", "/api/godseye/settings", Some(json!({"thresholds": {"min_fps": "fast"}}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(&a, "POST", "/api/godseye/settings", Some(json!({"thresholds": {"warp_factor": 9}}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "unknown settings are refused, not stored");
        let (s, w) = call(&a, "POST", "/api/godseye/settings", Some(json!({"mode": "stream", "thresholds": {"min_fps": 45, "allow_save_data": true}, "token": "good-token-0123456789abcdef", "label": "s25-ultra"}))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!w.to_string().contains("good-token"));
        let a2 = app(d.path());
        let (_, g) = call(&a2, "GET", "/api/godseye/settings", None).await;
        assert_eq!((g["mode"].as_str(), g["thresholds"]["min_fps"].as_f64(), g["thresholds"]["allow_save_data"].as_bool(), g["label"].as_str(), g["has_token"].as_bool()), (Some("stream"), Some(45.0), Some(true), Some("s25-ultra"), Some(true)), "survives a restart");
        assert!(!g.to_string().contains("good-token"), "the token is never returned");
        assert!(!std::fs::read_to_string(d.path().join("godseye.json")).unwrap().contains("good-token"), "and not in the settings file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.path().join("godseye.token")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn a_fix_goes_to_the_track_service_with_the_token_and_only_then() {
        let d = tempfile::tempdir().unwrap();
        let (url, log) = mock().await;
        let a = app(d.path());
        call(&a, "POST", "/api/godseye/settings", Some(json!({"track_url": url}))).await;
        let fix = json!({"lat": 41.9, "lon": -87.6, "alt_m": 180.0, "accuracy_m": 9.0, "heading_deg": 270.0, "speed_mps": 1.2, "time_ms": 1794000000000u64});
        let (s, v) = call(&a, "POST", "/api/godseye/fix", Some(fix.clone())).await;
        assert!(s == StatusCode::UNAUTHORIZED && v["kind"] == "no_token", "{v}");
        assert!(log.lock().unwrap().is_empty(), "nothing is sent without a token");
        call(&a, "POST", "/api/godseye/settings", Some(json!({"token": "wrong-token-0123456789abcdef"}))).await;
        let (s, v) = call(&a, "POST", "/api/godseye/fix", Some(fix.clone())).await;
        assert!(s == StatusCode::UNAUTHORIZED && v["kind"] == "bad_token", "{v}");
        call(&a, "POST", "/api/godseye/settings", Some(json!({"token": "good-token-0123456789abcdef"}))).await;
        let (s, v) = call(&a, "POST", "/api/godseye/fix", Some(fix)).await;
        assert!(s == StatusCode::OK && v["ok"] == true, "{v}");
        let (tok, body) = log.lock().unwrap().last().unwrap().clone();
        assert_eq!(tok, "good-token-0123456789abcdef");
        assert_eq!((body["lat"].as_f64(), body["heading"].as_f64(), body["accuracy"].as_f64(), body["ts"].as_u64()), (Some(41.9), Some(270.0), Some(9.0), Some(1794000000000)));
        let (s, _) = call(&a, "POST", "/api/godseye/fix", Some(json!({"lat": "x"}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(!d.path().join("track.json").exists() && std::fs::read_dir(d.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("fix")), "no fix is written to disk");
    }

    #[tokio::test]
    async fn status_reports_each_door_and_a_tailnet_address_without_tailscale_fails_plainly() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let (url, _) = mock().await;
        let a = app(d.path());
        call(&a, "POST", "/api/godseye/settings", Some(json!({"track_url": url, "stream_url": url}))).await;
        let (_, st) = call(&a, "GET", "/api/godseye/status", None).await;
        assert!(st["track"]["ok"] == true && st["stream"]["ok"] == true, "{st}");
        assert!(st["stream_view_url"].as_str().unwrap().contains("view_only=true"), "the stream is view-only");
        assert!(st["stream_view_url"].as_str().unwrap().ends_with("token%3Dt440-p100"), "default target is the t440 P100 kiosk");
        let (s, _) = call(&a, "POST", "/api/godseye/settings", Some(json!({"stream_target": "bad target;x"}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(st["globe_url"].as_str().unwrap().ends_with("/globe"));
        let b = app(tempfile::tempdir().unwrap().path());
        nautik9_lemonade::set_tailscale(false);
        let (_, st) = call(&b, "GET", "/api/godseye/status", None).await;
        nautik9_lemonade::set_tailscale(true);
        assert!(st["track"]["kind"] == "no_tailscale" && st["stream"]["kind"] == "no_tailscale", "{st}");
    }
}
