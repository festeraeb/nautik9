//! HTTP API for the agent (all behind the token, so every caller here is the owner).
//!
//!   POST   /api/agent/chat                  {message, session_id?} -> TurnResult (502 when no model answers)
//!   GET    /api/agent/sessions              -> [SessionInfo]
//!   GET    /api/agent/sessions/{id}         -> [StoredMessage]
//!   DELETE /api/agent/sessions/{id}
//!   POST   /api/agent/sessions/{id}/rate    {idx, rating: 1 | -1 | null}
//!   POST   /api/agent/sessions/{id}/retry   -> TurnResult for the last user message, answered again
//!   GET    /api/agent/export?format=openai|sharegpt&rated=up|down|any -> JSONL of stored chats
//!
//!   GET    /api/agent/playbooks             -> [{name, description, tasks, valid, approved, sha256, last_run}]
//!   GET    /api/agent/playbooks/{name}      -> {name, sha256, approved, json}
//!   POST   /api/agent/playbooks/{name}/check    -> {ok, problems, plan}  (validates, runs nothing)
//!   POST   /api/agent/playbooks/{name}/run      {vars?, skip_tags?, trigger?: "manual" | "schedule"} -> RunReport
//!                                           (200 even when a task failed: read `ok`; 403 when a
//!                                           schedule run is not approved; 422 when invalid)
//!   POST   /api/agent/playbooks/{name}/approve  -> {ok, sha256}
//!
//!   POST   /api/agent/web_search            {query, k?} -> {result}: this server's web search, for
//!                                           phones that search through the home server
//!
//! Files in the config dir: `sessions.db`, `trajectories.jsonl` (every model call), optional
//! `secrets.json` (`{"NAME": "value"}` for http_request; keep it 0600) and optional `agent.json`:
//!   max_steps, context_chars, max_tokens;
//!   search: backends in order, each "searx:<url>", "remote" or "duckduckgo"
//!     (default: "remote" if remote_url is set, then "searx:<searx_url or $NAUTIK9_SEARX_URL>"
//!     if set, then "duckduckgo");
//!   remote_url + remote_token_file: another NautiK9 server to search through (the phone uses
//!     https://<your-home-server> with a file holding that server's token);
//!   mcp: [{"name", "url", "token_file"?}] MCP servers whose tools the model gets: on t440 the
//!     fleet MCP {"name":"fleet","url":"http://127.0.0.1:7800/mcp"}; on the phone the home server's
//!     proxy {"name":"home","url":"https://<your-home-server>/api/agent/mcp","token_file":"home_token"}.
//!   POST   /api/agent/mcp                   {method: tools/list | tools/call, params} -> the same
//!                                           protocol over this server's LOCAL MCP servers (never
//!                                           forwarded to another remote)

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nautik9_agent::store::Store;
use nautik9_agent::tools::{search_once, McpServer, Registry, SearchBackend, Tool, ToolCtx};
use nautik9_agent::playbook::{self, Asker, PbError, PlaybookTool, Trigger};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::LemonadeModule;
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::AppState;

/// Build the agent from the config dir. Errors are reported, never replaced by defaults
/// (a malformed secrets.json or agent.json stops the agent, not the server).
/// Read agent.json (missing = {}; malformed = error).
fn agent_conf(config_dir: &std::path::Path) -> Result<Value, String> {
    match std::fs::read_to_string(config_dir.join("agent.json")) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("agent.json: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("agent.json: {e}")),
    }
}

/// MCP tools from agent.json `mcp`. A server that does not answer is reported and skipped
/// (the agent still starts, without its tools); a malformed entry is an error.
pub async fn load_mcp(config_dir: &std::path::Path) -> Result<(Vec<Arc<dyn Tool>>, HashMap<String, McpServer>, Vec<String>), String> {
    let conf = agent_conf(config_dir)?;
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let (mut tools, mut local, mut notes) = (Vec::new(), HashMap::new(), Vec::new());
    for e in conf["mcp"].as_array().cloned().unwrap_or_default() {
        let name = e["name"].as_str().ok_or("agent.json mcp: entry without name")?.to_string();
        let url = e["url"].as_str().ok_or_else(|| format!("agent.json mcp {name}: no url"))?.to_string();
        let token = match e["token_file"].as_str() {
            Some(tf) => {
                let p = if std::path::Path::new(tf).is_absolute() { std::path::PathBuf::from(tf) } else { config_dir.join(tf) };
                Some(std::fs::read_to_string(&p).map_err(|e| format!("mcp {name} token_file {}: {e}", p.display()))?.trim().to_string())
            }
            None => None,
        };
        let srv = McpServer { name: name.clone(), url, token };
        match srv.load(&http).await {
            Ok(ts) => {
                notes.push(format!("mcp {name}: {} tools", ts.len()));
                if srv.token.is_none() {
                    for t in &ts {
                        local.insert(t.name().to_string(), srv.clone());
                    }
                }
                tools.extend(ts);
            }
            Err(err) => notes.push(format!("mcp {name}: NOT loaded: {err}")),
        }
    }
    Ok((tools, local, notes))
}

pub fn build_agent(
    config_dir: &std::path::Path,
    vault_path: &std::path::Path,
    llm: Arc<LemonadeModule>,
    mcp: (Vec<Arc<dyn Tool>>, HashMap<String, McpServer>),
) -> Result<Arc<Agent>, String> {
    let vault = WayfinderVault::new(vault_path).map_err(|e| format!("vault {}: {e}", vault_path.display()))?;
    let store = Store::open(&config_dir.join("sessions.db"), Some(config_dir.join("trajectories.jsonl")))?;
    let secrets: HashMap<String, String> = match std::fs::read_to_string(config_dir.join("secrets.json")) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("secrets.json: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(format!("secrets.json: {e}")),
    };
    let conf = agent_conf(config_dir)?;
    let d = AgentConfig::default();
    let num = |k: &str, dv: usize| conf[k].as_u64().map(|v| v as usize).unwrap_or(dv);
    let cfg = AgentConfig {
        max_steps: num("max_steps", d.max_steps),
        context_chars: num("context_chars", d.context_chars),
        max_tokens: num("max_tokens", d.max_tokens as usize) as u32,
        ..d
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;
    let ctx = ToolCtx {
        vault: Arc::new(vault),
        store,
        http,
        secrets: Arc::new(secrets),
        search: Arc::new(search_backends(&conf, config_dir)?),
        owner: true,
        mcp_local: Arc::new(mcp.1),
    };
    // The playbook tool needs the agent's registry and its `ask` turn, so it holds the agent weakly.
    Ok(Arc::new_cyclic(|me| {
        let mut extra = mcp.0;
        extra.push(Arc::new(PlaybookTool { agent: me.clone() }));
        let (registry, skipped) = Registry::with_extra(extra);
        if !skipped.is_empty() {
            println!("Agent: tools skipped (name taken by a built-in tool): {}", skipped.join(", "));
        }
        Agent::with_tools(llm, ctx, cfg, registry)
    }))
}

/// The web_search backend list from agent.json (see the module doc).
pub fn search_backends(conf: &Value, config_dir: &std::path::Path) -> Result<Vec<SearchBackend>, String> {
    let remote = match conf["remote_url"].as_str() {
        Some(url) => {
            let tf = conf["remote_token_file"].as_str().ok_or("agent.json: remote_url needs remote_token_file")?;
            let p = if std::path::Path::new(tf).is_absolute() { std::path::PathBuf::from(tf) } else { config_dir.join(tf) };
            let token = std::fs::read_to_string(&p).map_err(|e| format!("remote_token_file {}: {e}", p.display()))?.trim().to_string();
            if token.is_empty() {
                return Err(format!("remote_token_file {} is empty", p.display()));
            }
            Some(SearchBackend::Remote { url: url.to_string(), token })
        }
        None => None,
    };
    let searx = conf["searx_url"].as_str().map(str::to_string).or_else(|| std::env::var("NAUTIK9_SEARX_URL").ok());
    let Some(list) = conf["search"].as_array() else {
        let mut v = Vec::new();
        v.extend(remote);
        v.extend(searx.map(SearchBackend::Searx));
        v.push(SearchBackend::DuckDuckGo);
        return Ok(v);
    };
    list.iter()
        .map(|e| match e.as_str() {
            Some("duckduckgo") => Ok(SearchBackend::DuckDuckGo),
            Some("remote") => remote.clone().ok_or_else(|| "agent.json: search lists \"remote\" but remote_url is not set".to_string()),
            Some(x) if x.starts_with("searx:") => Ok(SearchBackend::Searx(x[6..].to_string())),
            other => Err(format!("agent.json: unknown search backend {other:?}")),
        })
        .collect()
}

/// This server's web search for other NautiK9 instances (never forwards to a remote).
pub async fn web_search(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(q) = p["query"].as_str().filter(|q| !q.trim().is_empty()) else {
        return bad(StatusCode::BAD_REQUEST, "missing `query`");
    };
    let k = p["k"].as_u64().unwrap_or(6).clamp(1, 20) as usize;
    let mut tried = Vec::new();
    for b in a.ctx.search.iter().filter(|b| !matches!(b, SearchBackend::Remote { .. })) {
        match search_once(b, &a.ctx.http, q, k).await {
            Ok(text) => return Json(json!({"ok": true, "result": format!("(results from {} on the home server)\n{text}", b.label())})).into_response(),
            Err(e) => tried.push(format!("{}: {e}", b.label())),
        }
    }
    bad(StatusCode::BAD_GATEWAY, format!("web search failed on every backend: {}", tried.join("; ")))
}

/// The home server's MCP proxy (see the module doc).
pub async fn mcp_proxy(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let local = &a.ctx.mcp_local;
    match p["method"].as_str() {
        Some("tools/list") => {
            let mut servers: Vec<&McpServer> = local.values().collect();
            servers.sort_by(|x, y| x.name.cmp(&y.name));
            servers.dedup_by(|x, y| x.name == y.name);
            let mut tools = Vec::new();
            for srv in servers {
                match srv.call_raw(&a.ctx.http, &json!({"method": "tools/list"})).await {
                    Ok(v) => tools.extend(v["tools"].as_array().cloned().unwrap_or_default()),
                    Err(e) => return bad(StatusCode::BAD_GATEWAY, e),
                }
            }
            Json(json!({"tools": tools})).into_response()
        }
        Some("tools/call") => {
            let name = p["params"]["name"].as_str().unwrap_or("");
            match local.get(name) {
                Some(srv) => match srv.call_raw(&a.ctx.http, &p).await {
                    Ok(v) => Json(v).into_response(),
                    Err(e) => bad(StatusCode::BAD_GATEWAY, e),
                },
                None => bad(StatusCode::NOT_FOUND, format!("unknown tool: {name}")),
            }
        }
        other => bad(StatusCode::BAD_REQUEST, format!("unknown method: {other:?}")),
    }
}

fn agent_of(st: &AppState) -> Result<&Arc<Agent>, Response> {
    st.agent.as_ref().map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "error": format!("agent unavailable: {e}")}))).into_response())
}

fn bad(code: StatusCode, e: impl std::fmt::Display) -> Response {
    (code, Json(json!({"ok": false, "error": e.to_string()}))).into_response()
}

fn turn_response(r: Result<nautik9_agent::TurnResult, nautik9_lemonade::ChatFailure>) -> Response {
    match r {
        Ok(t) => Json(json!({"ok": true, "result": t})).into_response(),
        Err(f) => (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": f.error, "attempts": f.attempts}))).into_response(),
    }
}

pub async fn chat(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a.clone(),
        Err(r) => return r,
    };
    let Some(msg) = p["message"].as_str().filter(|m| !m.trim().is_empty()) else {
        return bad(StatusCode::BAD_REQUEST, "missing `message`");
    };
    turn_response(a.turn(p["session_id"].as_str(), msg, true).await)
}

pub async fn sessions(State(st): State<AppState>) -> Response {
    match agent_of(&st) {
        Ok(a) => match a.ctx.store.list_sessions().await {
            Ok(l) => Json(json!(l)).into_response(),
            Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Err(r) => r,
    }
}

pub async fn session(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match agent_of(&st) {
        Ok(a) => match a.ctx.store.exists(&id).await {
            Ok(true) => match a.ctx.store.messages(&id).await {
                Ok(m) => Json(json!(m)).into_response(),
                Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
            },
            Ok(false) => bad(StatusCode::NOT_FOUND, "no such session"),
            Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Err(r) => r,
    }
}

pub async fn delete_session(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match agent_of(&st) {
        Ok(a) => match a.ctx.store.delete_session(&id).await {
            Ok(true) => Json(json!({"ok": true})).into_response(),
            Ok(false) => bad(StatusCode::NOT_FOUND, "no such session"),
            Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Err(r) => r,
    }
}

pub async fn rate(State(st): State<AppState>, Path(id): Path<String>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(idx) = p["idx"].as_u64() else { return bad(StatusCode::BAD_REQUEST, "missing `idx`") };
    let rating = match &p["rating"] {
        Value::Null => None,
        v => match v.as_i64() {
            Some(r @ (-1 | 1)) => Some(r),
            _ => return bad(StatusCode::BAD_REQUEST, "`rating` must be 1, -1 or null"),
        },
    };
    match a.ctx.store.rate(&id, idx as usize, rating).await {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => bad(StatusCode::NOT_FOUND, "no such message"),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Retry: drop everything from the last user message on and answer that message again.
pub async fn retry(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a.clone(),
        Err(r) => return r,
    };
    let msgs = match a.ctx.store.messages(&id).await {
        Ok(m) => m,
        Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let Some(last) = msgs.iter().rev().find(|m| m.message["role"] == "user") else {
        return bad(StatusCode::NOT_FOUND, "no user message to retry");
    };
    // Stored user text carries a "[YYYY-MM-DD HH:MM] " stamp; strip it so it is not doubled.
    let raw = last.message["content"].as_str().unwrap_or("");
    let text = if raw.starts_with('[') { raw.split_once("] ").map(|x| x.1).unwrap_or(raw) } else { raw }.to_string();
    if let Err(e) = a.ctx.store.truncate(&id, last.idx).await {
        return bad(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    turn_response(a.turn(Some(&id), &text, true).await)
}

/// Stored chats as JSONL for fine-tuning: one line per session, OpenAI chat-with-tools
/// (`{"messages": [...]}`) or ShareGPT (`{"conversations": [{"from","value"}]}`). `rated=up`
/// keeps sessions with a 👍 and no 👎, `rated=down` the reverse, default every session.
pub async fn export(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let fmt = q.get("format").map(String::as_str).unwrap_or("openai");
    if fmt != "openai" && fmt != "sharegpt" {
        return bad(StatusCode::BAD_REQUEST, "format must be openai or sharegpt");
    }
    let rated = q.get("rated").map(String::as_str).unwrap_or("any");
    let mut lines = Vec::new();
    let sessions = match a.ctx.store.list_sessions().await {
        Ok(s) => s,
        Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    for s in sessions {
        let msgs = match a.ctx.store.messages(&s.id).await {
            Ok(m) => m,
            Err(e) => return bad(StatusCode::INTERNAL_SERVER_ERROR, e),
        };
        let (up, down) = (msgs.iter().any(|m| m.rating == Some(1)), msgs.iter().any(|m| m.rating == Some(-1)));
        let keep = match rated {
            "up" => up && !down,
            "down" => down,
            _ => true,
        };
        if !keep || msgs.is_empty() {
            continue;
        }
        let line = if fmt == "openai" {
            json!({"messages": msgs.iter().map(|m| m.message.clone()).collect::<Vec<_>>()})
        } else {
            let conv: Vec<Value> = msgs
                .iter()
                .filter_map(|m| {
                    let from = match m.message["role"].as_str()? {
                        "user" => "human",
                        "assistant" => "gpt",
                        "tool" => "tool",
                        _ => return None,
                    };
                    let value = m.message["content"].as_str().map(str::to_string).unwrap_or_else(|| m.message["tool_calls"].to_string());
                    Some(json!({"from": from, "value": value}))
                })
                .collect();
            json!({"conversations": conv})
        };
        lines.push(line.to_string());
    }
    ([(axum::http::header::CONTENT_TYPE, "application/x-ndjson")], lines.join("\n") + if lines.is_empty() { "" } else { "\n" }).into_response()
}

// ── playbooks ───────────────────────────────────────────────────────────────

fn pb_error(e: PbError) -> Response {
    match e {
        PbError::NotFound(m) => bad(StatusCode::NOT_FOUND, m),
        PbError::Invalid(p) => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "error": "playbook is not valid", "problems": p}))).into_response(),
        PbError::NotApproved(m) => bad(StatusCode::FORBIDDEN, m),
        PbError::Other(m) => bad(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

pub async fn playbooks(State(st): State<AppState>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match playbook::list(&a.ctx, &a.tools).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn playbook_show(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match playbook::load(&a.ctx, &name).await {
        Ok((_, raw, sha)) => {
            let approved = playbook::is_approved(&a.ctx, &name, &sha).await.unwrap_or(false);
            Json(json!({"name": name, "sha256": sha, "approved": approved, "json": raw})).into_response()
        }
        Err(e) => pb_error(e),
    }
}

pub async fn playbook_check(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match playbook::check_named(&a.ctx, &a.tools, &name).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => pb_error(e),
    }
}

pub async fn playbook_run(State(st): State<AppState>, Path(name): Path<String>, body: Option<Json<Value>>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let p = body.map(|b| b.0).unwrap_or(Value::Null);
    let vars = p["vars"].as_object().cloned().unwrap_or_default();
    let tags: Vec<String> = p["skip_tags"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
    let trigger = match p["trigger"].as_str() {
        None | Some("manual") => Trigger::Manual,
        Some("schedule") => Trigger::Schedule,
        Some(o) => return bad(StatusCode::BAD_REQUEST, format!("unknown trigger `{o}` (manual or schedule)")),
    };
    match playbook::run_named(&a.ctx, &a.tools, Some(a.as_ref() as &dyn Asker), &name, &vars, &tags, trigger).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => pb_error(e),
    }
}

pub async fn playbook_approve(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match playbook::approve(&a.ctx, &a.tools, &name).await {
        Ok(sha) => Json(json!({"ok": true, "sha256": sha})).into_response(),
        Err(e) => pb_error(e),
    }
}
