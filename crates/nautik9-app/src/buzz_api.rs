//! The Buzz tab (the fleet's shared message board) and the Board tab (the Lemonade #1221 shared
//! page). The phone page never talks to either service: it asks this server, which applies the
//! network checks (offline, Tailscale off, a 2 s TCP pre-check for tailnet addresses), fixes the
//! sender, and keeps the board key on the server side only.
//!
//! Buzz (plain JSON, no auth, tailnet only; see the fleet manifest's `_note_tailnet_buzz`):
//!   GET  /api/buzz/settings            {buzz_url, buzz_url_source, sender, board_url, has_board_key}
//!   POST /api/buzz/settings            {buzz_url?, sender?, board_url?, board_key?}  (key is write-only)
//!   GET  /api/buzz/rooms               rooms with counts and open-question counts
//!   GET  /api/buzz/messages?channel=&limit=
//!   POST /api/buzz/message             {channel, body, reply_to?, kind?}  (sender is the setting, never the caller)
//!   GET  /api/buzz/questions?room=     open questions (kind=question without an answer)
//!   GET  /api/buzz/tasks?state=        board tasks, each with `mine`
//!   POST /api/buzz/task/complete       {task_id}  only a task owned by the sender
//! Board (cesarops-feedback, key in header X-Notes-Key):
//!   GET  /api/board/status, GET /api/board/notes, POST /api/board/note {text}
//!
//! Settings are in `buzz.json` (no secrets). The board key is in `board.key` (mode 0600, the same
//! app-private config dir as the access token); no route returns it and nothing logs it.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use crate::webxdc_api::Runtime;
use crate::AppState;

pub const DEFAULT_BOARD_URL: &str = "https://cesarops.com/lemonade/1221/api";
const BUZZ_TAILNET_PORT: u16 = 5561;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/buzz/settings", get(settings_get).post(settings_set))
        .route("/api/buzz/rooms", get(rooms))
        .route("/api/buzz/messages", get(messages))
        .route("/api/buzz/message", post(post_message))
        .route("/api/buzz/questions", get(questions))
        .route("/api/buzz/tasks", get(tasks))
        .route("/api/buzz/task/complete", post(task_complete))
        .route("/api/board/status", get(board_status))
        .route("/api/board/notes", get(board_notes))
        .route("/api/board/note", post(board_note))
}

fn fail(status: StatusCode, kind: &str, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "kind": kind, "error": msg.into()}))).into_response()
}

// ---- settings ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub buzz_url: Option<String>,
    pub sender: String,
    pub board_url: String,
}

pub(crate) fn write_private_pub(path: &Path, text: &str) -> std::io::Result<()> {
    write_private(path, text)
}

pub fn load_settings(dir: &Path) -> Settings {
    let v: Value = std::fs::read_to_string(dir.join("buzz.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let s = |k: &str| v[k].as_str().map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    Settings {
        buzz_url: s("buzz_url").or_else(|| std::fs::read_to_string(dir.join("buzz_url")).ok().map(|x| x.trim().to_string()).filter(|x| !x.is_empty())),
        sender: s("sender").unwrap_or_else(|| "Thom".to_string()),
        board_url: s("board_url").unwrap_or_else(|| DEFAULT_BOARD_URL.to_string()),
    }
}

/// `http://host:port` of the fleet aggregator -> `http://host:5561`: Buzz's tailnet door is on the
/// same machine as the fleet's, so no address has to be written down twice.
pub fn derive_buzz_url(fleet_url: &str) -> Option<String> {
    derive_with_port(fleet_url, BUZZ_TAILNET_PORT)
}

/// The fleet address's host on another port (the home machine's other tailnet doors).
pub fn derive_with_port(fleet_url: &str, port: u16) -> Option<String> {
    let (scheme, rest) = fleet_url.split_once("://")?;
    let host = rest.split('/').next()?.rsplit_once(':').map(|(h, _)| h).unwrap_or_else(|| rest.split('/').next().unwrap_or(""));
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}:{port}"))
}

fn effective_buzz_url(st: &AppState, dir: &Path) -> (Option<String>, &'static str) {
    match load_settings(dir).buzz_url {
        Some(u) => (Some(u.trim_end_matches('/').to_string()), "setting"),
        None => (derive_buzz_url(&st.fleet_url), "derived from the fleet address (same host, port 5561)"),
    }
}

fn board_key(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("board.key")).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    o.open(path)?.write_all(text.as_bytes())
}

async fn settings_get(State(st): State<AppState>, Extension(rt): Extension<Runtime>) -> Json<Value> {
    let s = load_settings(&rt.config_dir);
    let (url, src) = effective_buzz_url(&st, &rt.config_dir);
    Json(json!({"buzz_url": url, "buzz_url_source": src, "sender": s.sender, "board_url": s.board_url, "has_board_key": board_key(&rt.config_dir).is_some()}))
}

fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://")) && u.len() < 200 && !u.contains(char::is_whitespace)
}

async fn settings_set(Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let mut cur: Value = std::fs::read_to_string(rt.config_dir.join("buzz.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    for k in ["buzz_url", "board_url"] {
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
    if let Some(v) = p["sender"].as_str() {
        let v: String = v.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(48).collect();
        if v.is_empty() {
            return fail(StatusCode::BAD_REQUEST, "bad_request", "sender cannot be empty");
        }
        cur["sender"] = json!(v);
    }
    if let Some(k) = p["board_key"].as_str() {
        let k = k.trim();
        let path = rt.config_dir.join("board.key");
        let r = if k.is_empty() { std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }) } else { write_private(&path, k) };
        if let Err(e) = r {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "io", format!("could not save the board key: {e}"));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&rt.config_dir).and_then(|_| std::fs::write(rt.config_dir.join("buzz.json"), serde_json::to_string_pretty(&cur).unwrap_or_default())) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "io", format!("could not save settings: {e}"));
    }
    Json(json!({"ok": true, "has_board_key": board_key(&rt.config_dir).is_some()})).into_response()
}

// ---- network checks and the HTTP helper ----

/// The plain-language refusal for a URL we should not even try, or None when it may be tried.
pub async fn precheck(url: &str) -> Option<(&'static str, String)> {
    if !nautik9_lemonade::is_online() {
        return Some(("offline", nautik9_lemonade::OFFLINE_MESSAGE.to_string()));
    }
    if nautik9_lemonade::is_tailnet_url(url) {
        if !nautik9_lemonade::is_tailscale() {
            return Some(("no_tailscale", nautik9_lemonade::HOME_UNREACHABLE_MESSAGE.to_string()));
        }
        if !nautik9_lemonade::tailnet_reachable(url).await {
            return Some(("unreachable", "no answer from home in 5 s: Tailscale is on, but that machine or its Buzz door did not answer".to_string()));
        }
    }
    None
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(10)).connect_timeout(Duration::from_secs(4)).build().unwrap_or_default()
}

fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() { "timed out".into() } else if e.is_connect() { "connection refused or unreachable".into() } else { "request failed".to_string() }
}

pub(crate) async fn buzz_req(st: &AppState, dir: &Path, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, Response> {
    let (url, _) = effective_buzz_url(st, dir);
    let Some(base) = url else {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, "not_configured", "no Buzz address: set one in Settings, or give the app a fleet address"));
    };
    if let Some((kind, msg)) = precheck(&base).await {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, kind, msg));
    }
    let mut rb = client().request(method, format!("{base}{path}"));
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.map_err(|e| fail(StatusCode::BAD_GATEWAY, "unreachable", format!("Buzz: {}", describe(&e))))?;
    let status = r.status();
    let v: Value = r.json().await.map_err(|e| fail(StatusCode::BAD_GATEWAY, "bad_reply", format!("Buzz answered HTTP {} with no JSON ({})", status.as_u16(), describe(&e))))?;
    if !status.is_success() || v["status"] == "error" {
        return Err(fail(StatusCode::BAD_GATEWAY, "refused", v["error"].as_str().unwrap_or("Buzz refused the request").to_string()));
    }
    Ok(v)
}

fn q_str(q: &HashMap<String, String>, k: &str) -> String {
    q.get(k).map(|v| v.chars().filter(|c| c.is_alphanumeric() || "-_.".contains(*c)).take(64).collect()).unwrap_or_default()
}

// ---- Buzz routes ----

async fn rooms(State(st): State<AppState>, Extension(rt): Extension<Runtime>) -> Response {
    match buzz_req(&st, &rt.config_dir, reqwest::Method::GET, "/board/rooms", None).await {
        Ok(v) => {
            let mut out: Vec<Value> = v["rooms"]
                .as_object()
                .map(|o| o.iter().map(|(name, r)| json!({"name": name, "purpose": r["purpose"].as_str().or_else(|| r["charter"].as_str()).unwrap_or(""), "count": r["count"].as_u64().unwrap_or(0), "last_at": r["last_at"], "open_questions": r["open_questions"].as_u64().unwrap_or(0)})).collect())
                .unwrap_or_default();
            out.sort_by(|a, b| b["last_at"].as_u64().unwrap_or(0).cmp(&a["last_at"].as_u64().unwrap_or(0)).then(a["name"].as_str().cmp(&b["name"].as_str())));
            Json(json!({"rooms": out})).into_response()
        }
        Err(r) => r,
    }
}

async fn messages(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Query(q): Query<HashMap<String, String>>) -> Response {
    let ch = q_str(&q, "channel");
    let limit: u32 = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(60).clamp(1, 300);
    let path = format!("/board/messages?channel={}&limit={limit}", if ch.is_empty() { "ops" } else { &ch });
    match buzz_req(&st, &rt.config_dir, reqwest::Method::GET, &path, None).await {
        Ok(v) => {
            // The board returns newest first or last depending on version; sort oldest to newest.
            let mut m = v["messages"].as_array().cloned().unwrap_or_default();
            m.sort_by_key(|x| x["created_at"].as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| x["created_at"].as_u64()).unwrap_or(0));
            Json(json!({"channel": v["channel"], "messages": m})).into_response()
        }
        Err(r) => r,
    }
}

async fn post_message(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let body = p["body"].as_str().unwrap_or("").trim();
    if body.is_empty() || body.len() > 4000 {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "write a message (up to 4000 characters)");
    }
    let sender = load_settings(&rt.config_dir).sender;
    let mut req = json!({"sender": sender, "channel": p["channel"].as_str().unwrap_or("general"), "body": body});
    if let Some(k) = p["kind"].as_str().filter(|k| ["note", "question", "finding", "proposal", "claim", "review", "result", "status"].contains(k)) {
        req["kind"] = json!(k);
    }
    if let Some(r) = p["reply_to"].as_str().filter(|r| !r.is_empty()) {
        req["reply_to"] = json!(r);
    }
    match buzz_req(&st, &rt.config_dir, reqwest::Method::POST, "/board/message", Some(req)).await {
        Ok(v) => Json(json!({"ok": true, "message": v["message"]})).into_response(),
        Err(r) => r,
    }
}

async fn questions(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Query(q): Query<HashMap<String, String>>) -> Response {
    let room = q_str(&q, "room");
    let path = if room.is_empty() { "/board/questions".to_string() } else { format!("/board/questions?room={room}") };
    match buzz_req(&st, &rt.config_dir, reqwest::Method::GET, &path, None).await {
        Ok(v) => Json(json!({"questions": v["open_questions"]})).into_response(),
        Err(r) => r,
    }
}

fn is_mine(owner: Option<&str>, sender: &str) -> bool {
    owner.map_or(false, |o| o.eq_ignore_ascii_case(sender) || o.eq_ignore_ascii_case("human-operator"))
}

async fn tasks(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Query(q): Query<HashMap<String, String>>) -> Response {
    let state = q_str(&q, "state");
    let path = if state.is_empty() { "/board/tasks".to_string() } else { format!("/board/tasks?state={state}") };
    let sender = load_settings(&rt.config_dir).sender;
    match buzz_req(&st, &rt.config_dir, reqwest::Method::GET, &path, None).await {
        Ok(v) => {
            let list: Vec<Value> = v["tasks"].as_array().cloned().unwrap_or_default().into_iter().map(|mut t| {
                let mine = is_mine(t["owner_agent"].as_str(), &sender);
                t["mine"] = json!(mine);
                // keep descriptions short for a phone list; the full text is one tap away on Buzz itself
                if let Some(d) = t["description"].as_str() {
                    t["description"] = json!(d.chars().take(600).collect::<String>());
                }
                t
            }).collect();
            Json(json!({"tasks": list})).into_response()
        }
        Err(r) => r,
    }
}

/// Complete a task the sender owns. The board has no `claimed -> completed` edge, so the real
/// steps are taken in order (claimed -> in_progress -> ready_for_review -> completed), each one a
/// recorded transition by the owner with a note; a task someone else owns is refused.
async fn task_complete(State(st): State<AppState>, Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let id = p["task_id"].as_str().unwrap_or("");
    if id.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "task_id is required");
    }
    let sender = load_settings(&rt.config_dir).sender;
    let list = match buzz_req(&st, &rt.config_dir, reqwest::Method::GET, "/board/tasks", None).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(t) = list["tasks"].as_array().and_then(|a| a.iter().find(|t| t["task_id"] == id)) else {
        return fail(StatusCode::NOT_FOUND, "not_found", "no such task");
    };
    if !is_mine(t["owner_agent"].as_str(), &sender) {
        return fail(StatusCode::FORBIDDEN, "not_yours", format!("this task is owned by {}; only your own tasks can be completed here", t["owner_agent"].as_str().unwrap_or("nobody")));
    }
    let path: &[&str] = match t["state"].as_str().unwrap_or("") {
        "claimed" => &["in_progress", "ready_for_review", "completed"],
        "in_progress" => &["ready_for_review", "completed"],
        "ready_for_review" => &["completed"],
        other => return fail(StatusCode::CONFLICT, "bad_state", format!("a task in state {other} cannot be completed from here")),
    };
    let mut last = Value::Null;
    for to in path {
        let body = json!({"task_id": id, "to": to, "owner_agent": sender, "note": "completed from the NautiK9 app"});
        match buzz_req(&st, &rt.config_dir, reqwest::Method::POST, "/board/task/transition", Some(body)).await {
            Ok(v) => last = v["task"].clone(),
            Err(r) => return r,
        }
    }
    Json(json!({"ok": true, "task": last, "steps": path})).into_response()
}

// ---- Board (Lemonade #1221 shared page) ----

async fn board_req(rt: &Runtime, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, Response> {
    let s = load_settings(&rt.config_dir);
    let Some(key) = board_key(&rt.config_dir) else {
        return Err(fail(StatusCode::UNAUTHORIZED, "no_key", "enter the board key in Settings first"));
    };
    let base = s.board_url.trim_end_matches('/').to_string();
    if let Some((kind, msg)) = precheck(&base).await {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, kind, msg));
    }
    let mut rb = client().request(method, format!("{base}{path}")).header("X-Notes-Key", key);
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.map_err(|e| fail(StatusCode::BAD_GATEWAY, "unreachable", format!("the board: {}", describe(&e))))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(json!({}));
    if status == StatusCode::UNAUTHORIZED {
        return Err(fail(StatusCode::UNAUTHORIZED, "bad_key", "the board did not accept the key; enter it again in Settings"));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(fail(StatusCode::TOO_MANY_REQUESTS, "rate_limited", v["error"].as_str().unwrap_or("too many notes in a few minutes; try again shortly").to_string()));
    }
    if !status.is_success() {
        return Err(fail(StatusCode::BAD_GATEWAY, "refused", v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("the board answered HTTP {}", status.as_u16()))));
    }
    Ok(v)
}

async fn board_status(Extension(rt): Extension<Runtime>) -> Response {
    match board_req(&rt, reqwest::Method::GET, "/status", None).await {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

async fn board_notes(Extension(rt): Extension<Runtime>) -> Response {
    match board_req(&rt, reqwest::Method::GET, "/notes", None).await {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

async fn board_note(Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let text = p["text"].as_str().unwrap_or("").trim();
    if text.is_empty() || text.chars().count() > 4000 {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "write a note (up to 4000 characters)");
    }
    let name = load_settings(&rt.config_dir).sender;
    match board_req(&rt, reqwest::Method::POST, "/notes", Some(json!({"name": name, "text": text}))).await {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(r) => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buzz_url_follows_the_fleet_address() {
        assert_eq!(derive_buzz_url("http://100.1.2.3:9122").as_deref(), Some("http://100.1.2.3:5561"));
        assert_eq!(derive_buzz_url("http://fleet.local:9122/").as_deref(), Some("http://fleet.local:5561"));
        assert_eq!(derive_buzz_url("not a url"), None);
    }
    #[test]
    fn ownership_matches_the_sender_or_the_operator_alias() {
        assert!(is_mine(Some("thom"), "Thom") && is_mine(Some("Human-Operator"), "Thom"));
        assert!(!is_mine(Some("claude"), "Thom") && !is_mine(None, "Thom"));
    }

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    type Log = Arc<Mutex<Vec<(String, Value)>>>;

    /// A stand-in for Buzz and for the board service, recording what it is sent.
    async fn mock() -> (String, Log) {
        use axum::extract::State as S;
        use axum::http::HeaderMap;
        let log: Log = Default::default();
        async fn msgs() -> Json<Value> {
            Json(json!({"channel":"ops","messages":[
                {"id":"b","sender":"fable","body":"second","created_at":"1700000002000","kind":"question","reply_to":"a"},
                {"id":"a","sender":"claude","body":"first","created_at":"1700000001000"}]}))
        }
        async fn rooms() -> Json<Value> {
            Json(json!({"_doc":"x","rooms":{"ops":{"purpose":"work","count":5,"last_at":9,"open_questions":0},"human-questions":{"purpose":"asks","count":2,"last_at":10,"open_questions":1}}}))
        }
        async fn qs() -> Json<Value> {
            Json(json!({"open_questions":[{"id":"q1","sender":"claude","body":"which key?","kind":"question","created_at":"1"}]}))
        }
        async fn tasks() -> Json<Value> {
            Json(json!({"tasks":[
                {"task_id":"t1","title":"mine","state":"claimed","owner_agent":"Thom","channel":"ops","description":"d"},
                {"task_id":"t2","title":"theirs","state":"claimed","owner_agent":"claude","channel":"ops","description":"d"}]}))
        }
        async fn post_msg(S(l): S<Log>, Json(b): Json<Value>) -> Json<Value> {
            l.lock().unwrap().push(("message".into(), b.clone()));
            Json(json!({"status":"posted","message":b}))
        }
        async fn trans(S(l): S<Log>, Json(b): Json<Value>) -> Json<Value> {
            l.lock().unwrap().push(("transition".into(), b.clone()));
            Json(json!({"status":"ok","task":{"task_id":b["task_id"],"state":b["to"]}}))
        }
        async fn status(S(l): S<Log>, h: HeaderMap) -> Response {
            l.lock().unwrap().push(("status-key".into(), json!(h.get("x-notes-key").and_then(|v| v.to_str().ok()).unwrap_or(""))));
            if h.get("x-notes-key").and_then(|v| v.to_str().ok()) != Some("sekrit-key-123456") {
                return (StatusCode::UNAUTHORIZED, Json(json!({"ok":false,"error":"this page needs its full link"}))).into_response();
            }
            Json(json!({"title":"Lemonade #1221","features":[{"name":"x","state":"built","note":""}],"log":[],"tests":[]})).into_response()
        }
        async fn note(S(l): S<Log>, Json(b): Json<Value>) -> Json<Value> {
            l.lock().unwrap().push(("note".into(), b));
            Json(json!({"ok":true}))
        }
        let app = Router::new()
            .route("/board/messages", get(msgs)).route("/board/rooms", get(rooms)).route("/board/questions", get(qs))
            .route("/board/tasks", get(tasks)).route("/board/message", post(post_msg)).route("/board/task/transition", post(trans))
            .route("/status", get(status)).route("/notes", post(note))
            .with_state(log.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, log)
    }

    const TOKEN: &str = "t";
    fn app(dir: &std::path::Path, fleet_url: &str) -> Router {
        use crate::webxdc_api::Runtime;
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let state = crate::AppState::test_state(dir, hw, fleet_url);
        routes().with_state(state).layer(Extension(Runtime { config_dir: dir.to_path_buf(), secret: TOKEN.into() }))
    }
    async fn call(a: &Router, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let _flags = crate::NET_FLAGS.read().await;
        call_raw(a, m, uri, body).await
    }
    /// For the tests that flip the network flags (they hold crate::NET_FLAGS for writing).
    async fn call_raw(a: &Router, m: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let b = Request::builder().method(m).uri(uri);
        let req = match body { Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(), None => b.body(Body::empty()).unwrap() };
        let r = a.clone().oneshot(req).await.unwrap();
        let st = r.status();
        (st, serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn buzz_rooms_messages_threads_questions_and_sender_are_real() {
        let d = tempfile::tempdir().unwrap();
        let (url, log) = mock().await;
        let a = app(d.path(), "http://127.0.0.1:1");
        let (s, v) = call(&a, "POST", "/api/buzz/settings", Some(json!({"buzz_url": url}))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let (_, r) = call(&a, "GET", "/api/buzz/rooms", None).await;
        assert_eq!(r["rooms"][0]["name"], "human-questions", "newest room first: {r}");
        assert_eq!(r["rooms"][0]["open_questions"], 1);
        let (_, m) = call(&a, "GET", "/api/buzz/messages?channel=ops", None).await;
        assert_eq!(m["messages"][0]["id"], "a", "oldest first, newest at the bottom");
        assert_eq!(m["messages"][1]["reply_to"], "a");
        let (_, q) = call(&a, "GET", "/api/buzz/questions?room=human-questions", None).await;
        assert_eq!(q["questions"][0]["id"], "q1");
        // the caller cannot choose the sender
        let (s, _) = call(&a, "POST", "/api/buzz/message", Some(json!({"channel":"ops","body":"hello","sender":"mallory","reply_to":"a","kind":"question"}))).await;
        assert_eq!(s, StatusCode::OK);
        let sent = log.lock().unwrap().iter().find(|(k, _)| k == "message").unwrap().1.clone();
        assert_eq!((sent["sender"].as_str(), sent["reply_to"].as_str(), sent["kind"].as_str()), (Some("Thom"), Some("a"), Some("question")));
        let (s, _) = call(&a, "POST", "/api/buzz/message", Some(json!({"channel":"ops","body":"  "}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn only_your_own_tasks_can_be_completed_and_each_step_is_a_real_transition() {
        let d = tempfile::tempdir().unwrap();
        let (url, log) = mock().await;
        let a = app(d.path(), "http://127.0.0.1:1");
        call(&a, "POST", "/api/buzz/settings", Some(json!({"buzz_url": url}))).await;
        let (_, t) = call(&a, "GET", "/api/buzz/tasks", None).await;
        assert_eq!((t["tasks"][0]["mine"].clone(), t["tasks"][1]["mine"].clone()), (json!(true), json!(false)));
        let (s, v) = call(&a, "POST", "/api/buzz/task/complete", Some(json!({"task_id":"t2"}))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
        assert!(log.lock().unwrap().iter().all(|(k, _)| k != "transition"), "nothing was sent for someone else's task");
        let (s, v) = call(&a, "POST", "/api/buzz/task/complete", Some(json!({"task_id":"t1"}))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let steps: Vec<String> = log.lock().unwrap().iter().filter(|(k, _)| k == "transition").map(|(_, b)| b["to"].as_str().unwrap().to_string()).collect();
        assert_eq!(steps, ["in_progress", "ready_for_review", "completed"]);
    }

    #[tokio::test]
    async fn the_board_key_is_write_only_sent_as_a_header_and_never_returned() {
        let d = tempfile::tempdir().unwrap();
        let (url, log) = mock().await;
        let a = app(d.path(), "http://127.0.0.1:1");
        call(&a, "POST", "/api/buzz/settings", Some(json!({"board_url": url}))).await;
        let (s, v) = call(&a, "GET", "/api/board/status", None).await;
        assert!(s == StatusCode::UNAUTHORIZED && v["kind"] == "no_key", "{v}");
        let (_, w) = call(&a, "POST", "/api/buzz/settings", Some(json!({"board_key": "wrong-key"}))).await;
        assert!(!w.to_string().contains("wrong-key"));
        let (s, v) = call(&a, "GET", "/api/board/status", None).await;
        assert!(s == StatusCode::UNAUTHORIZED && v["kind"] == "bad_key", "{v}");
        let (_, w) = call(&a, "POST", "/api/buzz/settings", Some(json!({"board_key": "sekrit-key-123456"}))).await;
        let (_, g) = call(&a, "GET", "/api/buzz/settings", None).await;
        assert!(g["has_board_key"] == true && !g.to_string().contains("sekrit") && !w.to_string().contains("sekrit"), "{g}");
        let (s, st) = call(&a, "GET", "/api/board/status", None).await;
        assert!(s == StatusCode::OK && st["title"] == "Lemonade #1221", "{st}");
        let (s, _) = call(&a, "POST", "/api/board/note", Some(json!({"text":"hi from the phone","name":"mallory"}))).await;
        assert_eq!(s, StatusCode::OK);
        let note = log.lock().unwrap().iter().find(|(k, _)| k == "note").unwrap().1.clone();
        assert_eq!((note["name"].as_str(), note["text"].as_str()), (Some("Thom"), Some("hi from the phone")), "signed Thom whatever the caller sends");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.path().join("board.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(!std::fs::read_to_string(d.path().join("buzz.json")).unwrap().contains("sekrit"), "the key is not in the settings file");
        let (_, _) = call(&a, "POST", "/api/buzz/settings", Some(json!({"board_key": ""}))).await;
        assert!(!d.path().join("board.key").exists(), "forgetting the key deletes it");
    }

    #[tokio::test]
    async fn a_tailnet_address_without_tailscale_is_a_plain_message_not_a_timeout() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path(), "http://100.64.1.2:9122");
        let flags = crate::NET_FLAGS.write().await;
        nautik9_lemonade::set_tailscale(false);
        let t = std::time::Instant::now();
        let (s, v) = call_raw(&a, "GET", "/api/buzz/rooms", None).await;
        nautik9_lemonade::set_tailscale(true);
        drop(flags);
        assert!(s == StatusCode::SERVICE_UNAVAILABLE && v["kind"] == "no_tailscale", "{v}");
        assert!(t.elapsed().as_millis() < 500, "no connect attempt was made");
        // derived address: same host as the fleet, port 5561
        let (_, g) = call(&a, "GET", "/api/buzz/settings", None).await;
        assert_eq!(g["buzz_url"], "http://100.64.1.2:5561");
        let flags = crate::NET_FLAGS.write().await;
        nautik9_lemonade::set_online(false);
        let (_, v) = call_raw(&a, "GET", "/api/buzz/rooms", None).await;
        nautik9_lemonade::set_online(true);
        drop(flags);
        assert_eq!(v["kind"], "offline");
    }
}
