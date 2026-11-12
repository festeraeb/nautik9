//! Flows: triggers that fire a playbook or an agent prompt.
//!
//! The design follows Jade Vault's Flows (MIT upstream, intent only; no Jade code is used), cut
//! down to what can be proven here: a flow is a small JSON note `Jade/Flows/<name>.json`
//!
//! ```json
//! {"name": "inbox", "description": "...", "enabled": true,
//!  "trigger": {"type": "note_changed", "path": "Inbox/*.md"},
//!  "action":  {"playbook": "file-inbox", "vars": {"note": "{{ trigger.path }}"}}}
//! ```
//!
//! Triggers (`trigger.type`):
//!
//! * `manual`: only by hand (the owner's `run`).
//! * `schedule` (`when`: any spec the schedules accept, e.g. `daily at 08:00`): the
//!   [`FlowRunner`] fires it when due (checked every 30 s, no replay of missed runs).
//! * `webhook`: `POST /api/agent/flows/<name>/hook` with the owner's token; the JSON body is
//!   `trigger.payload`.
//! * `note_changed` (`path`: glob, `*` stays inside a folder, `**` crosses folders, `?` one
//!   character): the [`FlowRunner`] compares the vault with its last look every 30 s and fires
//!   for each note that is new or whose text changed. The first look only records a baseline.
//!   Notes changed by flows in the same tick are absorbed, so a flow that writes to a note it
//!   watches does not loop. `Jade/Flows/`, `Jade/Playbooks/` and `Jade/Logs/` never trigger.
//!   Deleted notes do not trigger.
//! * `sms_received` (optional `from`, `contains`, both case-insensitive substrings; `from`
//!   ignores punctuation): fired when the server receives a text.
//!
//! Actions: `{"playbook": name, "vars": {...}}` runs the playbook (vars are templated, and must be
//! declared by the playbook) or `{"prompt": "text"}` runs one agent turn. Templates see one name,
//! `trigger`: `{type, payload, path, change, sender, body, when}` (unused keys are empty).
//! An `sms_received` flow can only run a playbook: a text from a stranger is never put in front
//! of the model as an instruction.
//!
//! ## Approval
//!
//! Same rule as playbooks. Any trigger except `manual` fires only when the owner approved the
//! flow's exact JSON (SHA-256 of the canonical form, stored under the name `flow:<name>`); a
//! changed file needs approval again. A playbook fired by a flow needs its own approval too
//! (it runs with trigger `flow`). Approval is only on the HTTP API (owner token), never a tool.
//! A refused firing is recorded (run log and `Jade/Logs/cron.log`) and nothing runs.
//! `run` by the owner works on any flow, approved or not, like a manual playbook run.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::agent::Agent;
use crate::playbook::{self, Asker, PbError, RunReport, Scope, Trigger};
use crate::schedule::{append_log, local_resolver, parse_spec};
use crate::tools::{Registry, ToolCtx};

pub const FLOW_DIR: &str = "Jade/Flows/";
/// Notes under these folders never trigger `note_changed` (definitions and logs written by the system).
const QUIET_PREFIXES: &[&str] = &["Jade/Flows/", "Jade/Playbooks/", "Jade/Logs/"];
const MAX_EVENTS_PER_TICK: usize = 50;

pub fn note_path(name: &str) -> String {
    format!("{FLOW_DIR}{name}.json")
}

/// The name approvals and run logs use for a flow (kept apart from playbook names).
fn key(name: &str) -> String {
    format!("flow:{name}")
}

// ── the document ────────────────────────────────────────────────────────────

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub trigger: FlowTrigger,
    pub action: FlowAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FlowTrigger {
    Manual {},
    Schedule { when: String },
    Webhook {},
    NoteChanged { path: String },
    SmsReceived {
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        contains: Option<String>,
    },
}

impl FlowTrigger {
    pub fn type_name(&self) -> &'static str {
        match self {
            FlowTrigger::Manual {} => "manual",
            FlowTrigger::Schedule { .. } => "schedule",
            FlowTrigger::Webhook {} => "webhook",
            FlowTrigger::NoteChanged { .. } => "note_changed",
            FlowTrigger::SmsReceived { .. } => "sms_received",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FlowAction {
    #[serde(default)]
    pub playbook: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub vars: Map<String, Value>,
}

/// What happened, handed to the flow as `trigger`.
#[derive(Debug, Clone)]
pub enum Event {
    Manual(Value),
    Schedule { when: String },
    Webhook(Value),
    NoteChanged { path: String, change: String },
    SmsReceived { sender: String, body: String },
}

impl Event {
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::Manual(_) => "manual",
            Event::Schedule { .. } => "schedule",
            Event::Webhook(_) => "webhook",
            Event::NoteChanged { .. } => "note_changed",
            Event::SmsReceived { .. } => "sms_received",
        }
    }

    pub fn sms(sender: &str, body: &str) -> Event {
        Event::SmsReceived { sender: sender.into(), body: body.into() }
    }

    /// The `trigger` value templates see.
    fn scope_value(&self) -> Value {
        let mut v = json!({"type": self.type_name(), "payload": null, "path": "", "change": "", "sender": "", "body": "", "when": ""});
        match self {
            Event::Manual(p) | Event::Webhook(p) => v["payload"] = p.clone(),
            Event::Schedule { when } => v["when"] = json!(when),
            Event::NoteChanged { path, change } => {
                v["path"] = json!(path);
                v["change"] = json!(change);
            }
            Event::SmsReceived { sender, body } => {
                v["sender"] = json!(sender);
                v["body"] = json!(body);
            }
        }
        v
    }
}

// ── glob ────────────────────────────────────────────────────────────────────

fn glob_regex(pat: &str) -> Result<regex::Regex, String> {
    let p = pat.trim().trim_start_matches('/');
    let c: Vec<char> = p.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '*' if c.get(i + 1) == Some(&'*') => {
                if c.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
            }
            '*' => {
                re.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            ch => {
                re.push_str(&regex::escape(&ch.to_string()));
                i += 1;
            }
        }
    }
    re.push('$');
    regex::RegexBuilder::new(&re).size_limit(100_000).build().map_err(|e| format!("bad path pattern `{pat}`: {e}"))
}

/// Does the note path match the glob?
pub fn glob_match(pat: &str, path: &str) -> bool {
    glob_regex(pat).is_ok_and(|r| r.is_match(path.trim().trim_start_matches('/')))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn trigger_matches(t: &FlowTrigger, e: &Event) -> bool {
    match (t, e) {
        (FlowTrigger::NoteChanged { path }, Event::NoteChanged { path: p, .. }) => glob_match(path, p),
        (FlowTrigger::SmsReceived { from, contains }, Event::SmsReceived { sender, body }) => {
            from.as_deref().map(norm).filter(|f| !f.is_empty()).is_none_or(|f| norm(sender).contains(&f))
                && contains.as_deref().map(str::to_lowercase).filter(|c| !c.is_empty()).is_none_or(|c| body.to_lowercase().contains(&c))
        }
        (FlowTrigger::Webhook {}, Event::Webhook(_)) => true,
        (FlowTrigger::Schedule { .. }, Event::Schedule { .. }) => true,
        (FlowTrigger::Manual {}, Event::Manual(_)) => true,
        _ => false,
    }
}

// ── check ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FlowCheck {
    pub ok: bool,
    pub problems: Vec<String>,
    pub trigger: String,
    pub action: String,
}

/// The parts of a check that need no vault.
pub fn check_flow(f: &Flow, file_name: Option<&str>) -> Vec<String> {
    let mut p = Vec::new();
    if !playbook::valid_name(&f.name) {
        p.push(format!("name `{}` must be 1-64 letters, digits, `-` or `_`", f.name));
    }
    if let Some(n) = file_name {
        if n != f.name {
            p.push(format!("name `{}` does not match the note `{}`", f.name, note_path(n)));
        }
    }
    match &f.trigger {
        FlowTrigger::Schedule { when } => {
            if let Err(e) = parse_spec(when) {
                p.push(format!("trigger.when: {e}"));
            }
        }
        FlowTrigger::NoteChanged { path } => {
            if path.trim().is_empty() || path.len() > 200 {
                p.push("trigger.path must be a glob of 1-200 characters".into());
            } else if let Err(e) = glob_regex(path) {
                p.push(e);
            }
        }
        FlowTrigger::SmsReceived { from, contains } => {
            for (k, v) in [("from", from), ("contains", contains)] {
                if v.as_deref().is_some_and(|s| s.len() > 200) {
                    p.push(format!("trigger.{k} is longer than 200 characters"));
                }
            }
        }
        FlowTrigger::Manual {} | FlowTrigger::Webhook {} => {}
    }
    match (&f.action.playbook, &f.action.prompt) {
        (Some(_), Some(_)) => p.push("action needs a playbook or a prompt, not both".into()),
        (None, None) => p.push("action needs a playbook or a prompt".into()),
        (Some(name), None) => {
            if !playbook::valid_name(name) {
                p.push(format!("action.playbook `{name}` is not a valid playbook name"));
            }
        }
        (None, Some(pr)) => {
            if pr.trim().is_empty() {
                p.push("action.prompt is empty".into());
            }
            if !f.action.vars.is_empty() {
                p.push("action.vars only applies to a playbook".into());
            }
            if matches!(f.trigger, FlowTrigger::SmsReceived { .. }) {
                p.push("an sms_received flow can only run a playbook: a text from outside is never given to the model as a prompt".into());
            }
        }
    }
    let mut names = Vec::new();
    let mut tv = Value::Object(f.action.vars.clone());
    if let Some(pr) = &f.action.prompt {
        tv = json!([tv, pr]);
    }
    match playbook::template_names(&tv, &mut names) {
        Ok(()) => {
            for n in names.into_iter().collect::<HashSet<_>>() {
                if n != "trigger" {
                    p.push(format!("action: undeclared variable `{n}` (the only name is `trigger`)"));
                }
            }
        }
        Err(e) => p.push(format!("action: bad template: {e}")),
    }
    p
}

// ── vault + approval ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FlowError {
    NotFound(String),
    Invalid(Vec<String>),
    NotApproved(String),
    /// Disabled, or the event is not this flow's trigger.
    Refused(String),
    Other(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::NotFound(m) | FlowError::NotApproved(m) | FlowError::Refused(m) | FlowError::Other(m) => write!(f, "{m}"),
            FlowError::Invalid(p) => write!(f, "flow is not valid: {}", p.join("; ")),
        }
    }
}

/// Load a flow note: (parsed, raw text, hash).
pub async fn load(ctx: &ToolCtx, name: &str) -> Result<(Flow, String, String), FlowError> {
    if !playbook::valid_name(name) {
        return Err(FlowError::Invalid(vec![format!("`{name}` is not a valid flow name")]));
    }
    let n = crate::tools::note_by_path(&ctx.vault, &note_path(name)).await.map_err(FlowError::Other)?.ok_or_else(|| FlowError::NotFound(format!("no flow `{name}` (expected the note {})", note_path(name))))?;
    let f: Flow = serde_json::from_str(&n.content).map_err(|e| FlowError::Invalid(vec![format!("{}: {e}", note_path(name))]))?;
    let sha = playbook::playbook_hash(&n.content).map_err(FlowError::Other)?;
    Ok((f, n.content, sha))
}

pub async fn is_approved(ctx: &ToolCtx, name: &str, sha: &str) -> Result<bool, String> {
    Ok(ctx.store.playbook_approval(&key(name)).await?.is_some_and(|(s, _)| s == sha))
}

/// Problems of the flow itself plus its playbook (missing or invalid).
pub async fn check_named(ctx: &ToolCtx, reg: &Registry, name: &str) -> Result<FlowCheck, FlowError> {
    let (f, _, _) = match load(ctx, name).await {
        Ok(x) => x,
        Err(FlowError::Invalid(p)) if playbook::valid_name(name) => return Ok(FlowCheck { ok: false, problems: p, trigger: String::new(), action: String::new() }),
        Err(e) => return Err(e),
    };
    let mut problems = check_flow(&f, Some(name));
    if let Some(pb) = f.action.playbook.as_deref().filter(|p| playbook::valid_name(p)) {
        match playbook::check_named(ctx, reg, pb).await {
            Ok(r) if r.ok => {
                // Vars the playbook does not declare would fail at run time.
                if let Ok((p, _, _)) = playbook::load(ctx, pb).await {
                    for k in f.action.vars.keys().filter(|k| !p.vars.contains_key(*k)) {
                        problems.push(format!("action.vars: `{k}` is not declared in the vars of playbook `{pb}`"));
                    }
                }
            }
            Ok(r) => problems.push(format!("playbook `{pb}` is not valid: {}", r.problems.join("; "))),
            Err(e) => problems.push(e.to_string()),
        }
    }
    let action = match (&f.action.playbook, &f.action.prompt) {
        (Some(p), _) => format!("playbook {p}"),
        (_, Some(_)) => "prompt".to_string(),
        _ => String::new(),
    };
    Ok(FlowCheck { ok: problems.is_empty(), problems, trigger: f.trigger.type_name().into(), action })
}

/// Approve the flow as it is now (it must pass the check). Owner action: no tool calls this.
pub async fn approve(ctx: &ToolCtx, reg: &Registry, name: &str) -> Result<String, FlowError> {
    let rep = check_named(ctx, reg, name).await?;
    if !rep.ok {
        return Err(FlowError::Invalid(rep.problems));
    }
    let (_, _, sha) = load(ctx, name).await?;
    ctx.store.approve_playbook(&key(name), &sha).await.map_err(FlowError::Other)?;
    Ok(sha)
}

async fn names(ctx: &ToolCtx) -> Result<Vec<String>, String> {
    let mut n: Vec<String> = ctx.vault.list_items().await.map_err(|e| e.to_string())?.into_iter().filter_map(|n| n.title.trim_start_matches('/').strip_prefix(FLOW_DIR).and_then(|r| r.strip_suffix(".json")).map(str::to_string)).collect();
    n.sort();
    Ok(n)
}

/// All flows with their state.
pub async fn list(ctx: &ToolCtx, reg: &Registry) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for name in names(ctx).await? {
        let last = ctx.store.last_playbook_run(&key(&name)).await?.map(|(at, trig, ok)| json!({"at": at, "trigger": trig, "ok": ok}));
        match load(ctx, &name).await {
            Ok((f, _, sha)) => {
                let chk = check_named(ctx, reg, &name).await.map_err(|e| e.to_string())?;
                let pb_approved = match f.action.playbook.as_deref() {
                    Some(p) if playbook::valid_name(p) => match playbook::load(ctx, p).await {
                        Ok((_, _, psha)) => Some(playbook::is_approved(ctx, p, &psha).await?),
                        Err(_) => Some(false),
                    },
                    _ => None,
                };
                out.push(json!({"name": name, "description": f.description, "enabled": f.enabled, "trigger": serde_json::to_value(&f.trigger).unwrap_or(Value::Null), "action": chk.action,
                    "valid": chk.ok, "problems": chk.problems, "approved": is_approved(ctx, &name, &sha).await?, "playbook_approved": pb_approved, "sha256": sha, "last_run": last}));
            }
            Err(e) => out.push(json!({"name": name, "valid": false, "approved": false, "problems": [e.to_string()], "last_run": last})),
        }
    }
    Ok(out)
}

// ── firing ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FireOutcome {
    pub flow: String,
    pub trigger: String,
    pub ok: bool,
    /// One line: what ran and how it ended.
    pub detail: String,
    pub report: Option<RunReport>,
    pub reply: Option<String>,
}

fn scope_of(e: &Event) -> Scope {
    let mut s = Scope::new();
    s.insert("trigger".into(), e.scope_value());
    s
}

/// Fire one flow for an event. `Manual` events (the owner's `run`) need no approval; every other
/// event needs the flow enabled, the trigger to match, and the flow's current JSON approved.
/// `Err` = nothing ran; `Ok` with `ok: false` = it ran (or tried to) and failed.
pub async fn fire(ctx: &ToolCtx, reg: &Registry, asker: Option<&dyn Asker>, name: &str, event: &Event) -> Result<FireOutcome, FlowError> {
    let (f, _, sha) = load(ctx, name).await?;
    let problems = check_flow(&f, Some(name));
    if !problems.is_empty() {
        return Err(FlowError::Invalid(problems));
    }
    let manual = matches!(event, Event::Manual(_));
    if !manual {
        if !f.enabled {
            return Err(FlowError::Refused(format!("flow `{name}` is disabled")));
        }
        if !trigger_matches(&f.trigger, event) {
            return Err(FlowError::Refused(format!("flow `{name}` does not take `{}` events (its trigger is `{}`)", event.type_name(), f.trigger.type_name())));
        }
        match ctx.store.playbook_approval(&key(name)).await.map_err(FlowError::Other)? {
            Some((s, _)) if s == sha => {}
            Some(_) => return Err(FlowError::NotApproved(format!("flow `{name}` changed since it was approved; approve it again before it fires"))),
            None => return Err(FlowError::NotApproved(format!("flow `{name}` is not approved; approve it before it fires"))),
        }
    }
    let scope = scope_of(event);
    let tname = event.type_name();
    let done = |ok: bool, detail: String, report: Option<RunReport>, reply: Option<String>| FireOutcome { flow: name.into(), trigger: tname.into(), ok, detail, report, reply };
    let out = if let Some(pb) = &f.action.playbook {
        let vars = match playbook::render_value(&Value::Object(f.action.vars.clone()), &scope) {
            Ok(Value::Object(o)) => o,
            Ok(_) => Map::new(),
            Err(e) => return Ok(finish(ctx, name, tname, done(false, format!("vars: {e}"), None, None)).await),
        };
        let trig = if manual { Trigger::Manual } else { Trigger::Flow };
        match playbook::run_named(ctx, reg, asker, pb, &vars, &[], trig).await {
            Ok(r) => done(r.ok, format!("playbook `{pb}`: {}", r.status), Some(r), None),
            Err(PbError::NotApproved(m)) => done(false, format!("playbook `{pb}` did not run: {m}"), None, None),
            Err(e) => done(false, format!("playbook `{pb}` did not run: {e}"), None, None),
        }
    } else {
        let prompt = match playbook::render_str(f.action.prompt.as_deref().unwrap_or(""), &scope) {
            Ok(v) => playbook::to_text(&v),
            Err(e) => return Ok(finish(ctx, name, tname, done(false, format!("prompt: {e}"), None, None)).await),
        };
        match asker {
            None => done(false, "no agent attached to answer the prompt".into(), None, None),
            Some(a) => match a.ask(&prompt).await {
                Ok(t) => done(true, "prompt answered".into(), None, Some(t)),
                Err(e) => done(false, format!("prompt failed: {e}"), None, None),
            },
        }
    };
    Ok(finish(ctx, name, tname, out).await)
}

/// Record a firing in the run log and `Jade/Logs/cron.log`.
async fn finish(ctx: &ToolCtx, name: &str, trigger: &str, out: FireOutcome) -> FireOutcome {
    let rec = json!({"flow": name, "detail": out.detail, "report": out.report, "reply": out.reply});
    if let Err(e) = ctx.store.log_playbook_run(&key(name), trigger, out.ok, &rec).await {
        tracing::warn!("flow log {name}: {e}");
    }
    log_line(ctx, if out.ok { "ok" } else { "ERROR" }, name, &out.detail).await;
    out
}

async fn log_line(ctx: &ToolCtx, status: &str, name: &str, text: &str) {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
    let line = format!("{} {status} flow:{name}: {one}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    if let Err(e) = append_log(&ctx.vault, &line).await {
        tracing::warn!("cron.log: {e}");
    }
}

/// [`fire`], and a refusal of an unattended event (not approved, disabled ...) is written to the
/// run log and `Jade/Logs/cron.log`, so it is never silent.
pub async fn fire_logged(ctx: &ToolCtx, reg: &Registry, asker: Option<&dyn Asker>, name: &str, event: &Event) -> Result<FireOutcome, FlowError> {
    let r = fire(ctx, reg, asker, name, event).await;
    if let (Err(e), false) = (&r, matches!(event, Event::Manual(_))) {
        let msg = format!("not fired: {e}");
        let _ = ctx.store.log_playbook_run(&key(name), event.type_name(), false, &json!({"flow": name, "detail": msg})).await;
        log_line(ctx, "ERROR", name, &msg).await;
    }
    r
}

/// Fire every enabled flow whose trigger matches `event` (note_changed, sms_received). A flow
/// that is refused (not approved ...) is recorded, not run.
pub async fn fire_matching(ctx: &ToolCtx, reg: &Registry, asker: Option<&dyn Asker>, event: &Event) -> Vec<(String, Result<FireOutcome, FlowError>)> {
    let mut out = Vec::new();
    for name in names(ctx).await.unwrap_or_default() {
        let Ok((f, _, _)) = load(ctx, &name).await else { continue };
        if !f.enabled || !trigger_matches(&f.trigger, event) {
            continue;
        }
        let r = fire_logged(ctx, reg, asker, &name, event).await;
        out.push((name, r));
    }
    out
}

/// Fire matching flows in the background (used by the SMS routes).
pub fn spawn_event(agent: Arc<Agent>, event: Event) {
    tokio::spawn(async move {
        fire_matching(&agent.ctx, &agent.tools, Some(agent.as_ref() as &dyn Asker), &event).await;
    });
}

/// What a `playbook` schedule (and crontab line) runs: the playbook with trigger `schedule`.
/// An unapproved or changed playbook is an error result and nothing runs.
pub async fn playbook_job(agent: &Agent, name: &str) -> (bool, String) {
    match playbook::run_named(&agent.ctx, &agent.tools, Some(agent as &dyn Asker), name, &Map::new(), &[], Trigger::Schedule).await {
        Ok(r) => (r.ok, r.recap()),
        Err(e) => (false, e.to_string()),
    }
}

// ── the runner: schedule triggers and note changes ──────────────────────────

#[derive(Default)]
struct State {
    next: HashMap<(String, String), DateTime<FixedOffset>>,
    /// note path -> hash of its text; None until a flow watches notes.
    notes: Option<HashMap<String, String>>,
}

pub struct FlowRunner {
    agent: Arc<Agent>,
    st: std::sync::Mutex<State>,
}

fn quiet(path: &str) -> bool {
    QUIET_PREFIXES.iter().any(|p| path.starts_with(p))
}

impl FlowRunner {
    pub fn new(agent: Arc<Agent>) -> Arc<Self> {
        Arc::new(FlowRunner { agent, st: Default::default() })
    }

    /// Tick every 30 s until the process ends.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(30));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                me.tick(chrono::Local::now().fixed_offset()).await;
            }
        })
    }

    async fn snapshot(&self) -> Result<HashMap<String, String>, String> {
        Ok(self
            .agent
            .ctx
            .vault
            .list_items()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|n| (n.title.trim_start_matches('/').to_string(), Sha256::digest(n.content.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect::<String>()))
            .filter(|(p, _)| !quiet(p))
            .collect())
    }

    /// One pass. Returns how many flows were fired (including refused firings that were recorded).
    pub async fn tick(&self, now: DateTime<FixedOffset>) -> usize {
        let a = &self.agent;
        let (ctx, reg) = (&a.ctx, &a.tools);
        let asker = Some(a.as_ref() as &dyn Asker);
        let mut flows = Vec::new();
        for name in names(ctx).await.unwrap_or_default() {
            if let Ok((f, _, _)) = load(ctx, &name).await {
                if f.enabled {
                    flows.push((name, f));
                }
            }
        }
        let mut fired = 0;

        // schedule triggers
        let mut due = Vec::new();
        let mut live = HashSet::new();
        for (name, f) in &flows {
            let FlowTrigger::Schedule { when } = &f.trigger else { continue };
            let Ok(spec) = parse_spec(when) else { continue };
            let k = (name.clone(), when.clone());
            live.insert(k.clone());
            let mut st = self.st.lock().unwrap();
            match st.next.get(&k).copied() {
                None => {
                    if let Some(nx) = spec.next_after(now, &local_resolver) {
                        st.next.insert(k, nx);
                    }
                }
                Some(t) if t <= now => due.push((name.clone(), when.clone(), spec)),
                _ => {}
            }
        }
        self.st.lock().unwrap().next.retain(|k, _| live.contains(k));
        for (name, when, spec) in due {
            let _ = fire_logged(ctx, reg, asker, &name, &Event::Schedule { when: when.clone() }).await;
            fired += 1;
            let nx = spec.next_after(chrono::Local::now().fixed_offset().max(now), &local_resolver);
            let mut st = self.st.lock().unwrap();
            match nx {
                Some(nx) => st.next.insert((name, when), nx),
                None => st.next.remove(&(name, when)),
            };
        }

        // note_changed triggers
        if !flows.iter().any(|(_, f)| matches!(f.trigger, FlowTrigger::NoteChanged { .. })) {
            self.st.lock().unwrap().notes = None;
            return fired;
        }
        let new = match self.snapshot().await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("flows: reading notes: {e}");
                return fired;
            }
        };
        let old = self.st.lock().unwrap().notes.replace(new.clone());
        let Some(old) = old else { return fired };
        let mut changed: Vec<(&String, &str)> = new.iter().filter(|(p, h)| old.get(*p) != Some(*h)).map(|(p, _)| (p, if old.contains_key(p) { "modified" } else { "created" })).collect();
        changed.sort();
        if changed.len() > MAX_EVENTS_PER_TICK {
            log_line(ctx, "ERROR", "-", &format!("{} notes changed at once; only the first {MAX_EVENTS_PER_TICK} were offered to flows", changed.len())).await;
            changed.truncate(MAX_EVENTS_PER_TICK);
        }
        for (path, change) in changed {
            fired += fire_matching(ctx, reg, asker, &Event::NoteChanged { path: path.clone(), change: change.into() }).await.len();
        }
        if fired > 0 {
            // Absorb what the flows themselves wrote.
            if let Ok(n) = self.snapshot().await {
                self.st.lock().unwrap().notes = Some(n);
            }
        }
        fired
    }
}
