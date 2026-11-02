//! HTTP routes for note scripts, templates and the tree-of-thought reasoner (all behind the token).
//!
//!   GET  /api/agent/templates                  -> {templates: [path]}
//!   POST /api/agent/templates/{name}/create    {title, values?, dry_run?} -> {path, text, cursor, prompts}
//!                                              (422 when a prompt has no value or the note exists)
//!   GET  /api/agent/scripts                    -> {scripts: [path]}  (notes under Scripts/)
//!   POST /api/agent/scripts/run                {path, args?} -> {ok, result: text}; 422 + {error} on failure
//!   POST /api/tot                              {problem, branch?, beam?, depth?, budget?} -> the tree
//!                                              (502 + attempts when no model answers; 400 without a problem)

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nautik9_agent::tot::{self, TotError, TotParams};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::AppState;

fn bad(code: StatusCode, e: impl std::fmt::Display) -> Response {
    (code, Json(json!({"ok": false, "error": e.to_string()}))).into_response()
}

fn agent_of(st: &AppState) -> Result<&std::sync::Arc<nautik9_agent::Agent>, Response> {
    st.agent.as_ref().map_err(|e| bad(StatusCode::SERVICE_UNAVAILABLE, format!("agent unavailable: {e}")))
}

pub async fn templates(State(st): State<AppState>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match nautik9_agent::template::list(&a.ctx).await {
        Ok(l) => Json(json!({"templates": l})).into_response(),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn template_create(State(st): State<AppState>, Path(name): Path<String>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(title) = p["title"].as_str() else { return bad(StatusCode::BAD_REQUEST, "missing `title`") };
    let values: HashMap<String, String> = p["values"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default();
    match nautik9_agent::template::create(&a.ctx, &name, title, &values, p["dry_run"].as_bool().unwrap_or(false)).await {
        Ok((path, r)) => Json(json!({"ok": true, "path": path, "text": r.text, "cursor": r.cursor, "prompts": r.prompts})).into_response(),
        Err(e) if e.starts_with("no template") => bad(StatusCode::NOT_FOUND, e),
        Err(e) => bad(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

pub async fn scripts(State(st): State<AppState>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match a.ctx.vault.list_items().await {
        Ok(items) => {
            let mut v: Vec<String> = items.into_iter().map(|n| n.title).filter(|t| t.trim_start_matches('/').starts_with("Scripts/")).collect();
            v.sort();
            Json(json!({"scripts": v})).into_response()
        }
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn script_run(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(path) = p["path"].as_str() else { return bad(StatusCode::BAD_REQUEST, "missing `path`") };
    let Some(tool) = a.tools.get("script") else { return bad(StatusCode::SERVICE_UNAVAILABLE, "the script tool is not loaded") };
    match tool.call(&json!({"action": "run", "path": path, "args": p.get("args").cloned().unwrap_or(Value::Null)}), &a.ctx).await {
        Ok(t) => Json(json!({"ok": true, "result": t})).into_response(),
        Err(e) if e.starts_with("no note at") => bad(StatusCode::NOT_FOUND, e),
        Err(e) => bad(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

/// Tree-of-thought reasoning through the provider chain (see `nautik9_agent::tot`).
pub async fn tot(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let Some(problem) = p["problem"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return bad(StatusCode::BAD_REQUEST, "missing `problem`");
    };
    match tot::reason(&st.lemonade, problem, &TotParams::from_json(&p)).await {
        Ok(r) => Json(r).into_response(),
        Err(TotError::NoModel(f)) => (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": f.error, "attempts": f.attempts}))).into_response(),
        Err(TotError::Unusable(e)) => bad(StatusCode::BAD_GATEWAY, e),
    }
}
