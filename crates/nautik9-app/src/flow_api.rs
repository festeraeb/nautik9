//! HTTP API for flows (all behind the owner's token; approval is only here, never a tool).
//!
//!   GET    /api/agent/flows                    -> [{name, description, enabled, trigger, action, valid,
//!                                               problems, approved, playbook_approved, sha256, last_run}]
//!   GET    /api/agent/flows/{name}             -> {name, sha256, approved, json}
//!   POST   /api/agent/flows/{name}/check       -> {ok, problems, trigger, action}  (runs nothing)
//!   POST   /api/agent/flows/{name}/run         {payload?} -> FireOutcome: the owner runs it by hand
//!                                               (no approval needed; its playbook runs as manual)
//!   POST   /api/agent/flows/{name}/approve     -> {ok, sha256}
//!   POST   /api/agent/flows/{name}/hook        any JSON body -> FireOutcome. For flows whose trigger is
//!                                               `webhook`; 403 when the flow is not approved (or changed
//!                                               since), 409 when it is disabled or not a webhook flow.
//!                                               200 also when the playbook failed: read `ok`.
//!   POST   /api/agent/schedules                {spec, playbook} adds a schedule that runs an approved
//!                                               playbook (see agent_api)

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nautik9_agent::flow::{self, Event, FlowError};
use nautik9_agent::playbook::Asker;
use serde_json::{json, Value};

use crate::AppState;

fn bad(code: StatusCode, e: impl std::fmt::Display) -> Response {
    (code, Json(json!({"ok": false, "error": e.to_string()}))).into_response()
}

fn agent_of(st: &AppState) -> Result<&std::sync::Arc<nautik9_agent::Agent>, Response> {
    st.agent.as_ref().map_err(|e| bad(StatusCode::SERVICE_UNAVAILABLE, format!("agent unavailable: {e}")))
}

fn flow_error(e: FlowError) -> Response {
    match e {
        FlowError::NotFound(m) => bad(StatusCode::NOT_FOUND, m),
        FlowError::Invalid(p) => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "error": "flow is not valid", "problems": p}))).into_response(),
        FlowError::NotApproved(m) => bad(StatusCode::FORBIDDEN, m),
        FlowError::Refused(m) => bad(StatusCode::CONFLICT, m),
        FlowError::Other(m) => bad(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

pub async fn flows(State(st): State<AppState>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match flow::list(&a.ctx, &a.tools).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn flow_show(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match flow::load(&a.ctx, &name).await {
        Ok((_, raw, sha)) => {
            let approved = flow::is_approved(&a.ctx, &name, &sha).await.unwrap_or(false);
            Json(json!({"name": name, "sha256": sha, "approved": approved, "json": raw})).into_response()
        }
        Err(e) => flow_error(e),
    }
}

pub async fn flow_check(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match flow::check_named(&a.ctx, &a.tools, &name).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => flow_error(e),
    }
}

pub async fn flow_run(State(st): State<AppState>, Path(name): Path<String>, body: Option<Json<Value>>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let payload = body.map(|b| b.0["payload"].clone()).unwrap_or(Value::Null);
    match flow::fire_logged(&a.ctx, &a.tools, Some(a.as_ref() as &dyn Asker), &name, &Event::Manual(payload)).await {
        Ok(o) => Json(o).into_response(),
        Err(e) => flow_error(e),
    }
}

pub async fn flow_hook(State(st): State<AppState>, Path(name): Path<String>, body: Option<Json<Value>>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let payload = body.map(|b| b.0).unwrap_or(Value::Null);
    match flow::fire_logged(&a.ctx, &a.tools, Some(a.as_ref() as &dyn Asker), &name, &Event::Webhook(payload)).await {
        Ok(o) => Json(o).into_response(),
        Err(e) => flow_error(e),
    }
}

pub async fn flow_approve(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let a = match agent_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match flow::approve(&a.ctx, &a.tools, &name).await {
        Ok(sha) => Json(json!({"ok": true, "sha256": sha})).into_response(),
        Err(e) => flow_error(e),
    }
}

/// `POST /api/agent/schedules {spec, playbook}`.
pub async fn add_playbook_schedule(a: &nautik9_agent::Agent, spec: Option<&str>, playbook: &str) -> Response {
    let Some(spec) = spec else { return bad(StatusCode::BAD_REQUEST, "need string `spec`") };
    match nautik9_agent::schedule::Schedules::new(&a.ctx.store).add_playbook(spec, playbook, chrono::Local::now().fixed_offset()).await {
        Ok(s) => Json(json!(s)).into_response(),
        Err(e) => bad(StatusCode::BAD_REQUEST, e),
    }
}
