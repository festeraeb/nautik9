//! Schedules: Jade's cron / crontab / heartbeat, in Rust.
//!
//! * A schedule is `spec` + `prompt`: when due, the runner runs one owner agent turn with the
//!   prompt in a session of its own ("⏰ <spec>"). Kind `prompt` is the default.
//!   Kind `playbook` (payload = playbook name) runs that playbook with trigger `schedule`
//!   (see `flow::playbook_job`): only an approved playbook runs; an unapproved or changed one is
//!   a recorded ERROR in `last_result` and the log, and nothing runs. Flows (`flow.rs`) are
//!   separate: they have their own triggers, including a schedule trigger.
//! * Specs are standard 5-field cron (`minute hour day month weekday`, with `*`, `*/n`, `a-b`,
//!   `a-b/n`, lists; weekday 0 or 7 = Sunday; numbers only, no month/day names) or simple English:
//!   `every N minutes|hours` (also `every minute|hour`), `hourly`, `daily at HH:MM`,
//!   `weekdays at HH:MM`, `every monday at HH:MM`. Anything else is an error; nothing is guessed.
//! * Time zone: the server's local zone. Cron specs are wall-clock. Day-of-month and weekday
//!   combine like Vixie cron: if both are restricted a day matches when EITHER does.
//!   `every N minutes|hours` counts real elapsed time from the previous run.
//! * DST: a wall-clock time that does not exist (spring forward gap) runs once, at the first
//!   instant after the gap; a wall-clock time that happens twice (fall back) runs only at its
//!   first occurrence. So a daily job runs exactly once every day, and wildcard jobs do not
//!   fire in the repeated hour a second time. Interval specs are unaffected.
//! * Missed runs (server down) are not replayed: a schedule that is overdue at start runs once
//!   on the next tick, then continues from "now".
//! * `Jade/crontab` (lines `<5-field cron> prompt <text>` or `<5-field cron> playbook <name>`,
//!   `#` comments) is re-read every tick.
//!   A bad line is written to the log once (again if it changes), never silently skipped.
//!   Crontab entries live in memory only (next run is computed from when the line is first seen).
//! * `Jade/HEARTBEAT.md`: when it has content and `heartbeat_minutes` > 0, every N minutes its
//!   text is run as one turn in the session "♥ Heartbeat".
//! * Every run appends a line to the note `Jade/Logs/cron.log` (newest last, 500 lines kept).
//! * One run at a time: a tick that finds a run in progress does nothing.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::{strip_html_comments, Agent};
use crate::store::Store;
use crate::tools::{note_by_path, write_note, Tool, ToolCtx};

pub const CRONTAB_PATH: &str = "Jade/crontab";
pub const HEARTBEAT_PATH: &str = "Jade/HEARTBEAT.md";
pub const LOG_PATH: &str = "Jade/Logs/cron.log";
pub const HEARTBEAT_TITLE: &str = "♥ Heartbeat";
const LOG_LINES: usize = 500;

// ── spec ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: u64,
    hour: u32,
    dom: u32,
    month: u16,
    dow: u8,
    dom_star: bool,
    dow_star: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Spec {
    Cron(Cron),
    /// Every this many seconds after the previous run.
    Every(i64),
}

fn parse_field(text: &str, min: u32, max: u32, name: &str) -> Result<u64, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        let (base, step) = match item.split_once('/') {
            Some((b, s)) => (b, Some(s.parse::<u32>().map_err(|_| format!("{name}: bad step `{s}`"))?)),
            None => (item, None),
        };
        if step == Some(0) {
            return Err(format!("{name}: step must be at least 1"));
        }
        let num = |t: &str| t.parse::<u32>().map_err(|_| format!("{name}: `{t}` is not a number"));
        let (lo, hi) = if base == "*" {
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            (num(a)?, num(b)?)
        } else {
            let v = num(base)?;
            (v, if step.is_some() { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("{name}: `{item}` is outside {min}-{max}"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step.unwrap_or(1);
        }
    }
    Ok(bits)
}

/// Parse 5 cron fields.
pub fn parse_cron(text: &str) -> Result<Cron, String> {
    let f: Vec<&str> = text.split_whitespace().collect();
    if f.len() != 5 {
        return Err(format!("a cron spec has 5 fields (minute hour day month weekday), got {}", f.len()));
    }
    let dow_bits = parse_field(f[4], 0, 7, "weekday")?;
    let dow = ((dow_bits & 0x7f) | ((dow_bits >> 7) & 1)) as u8; // 7 = Sunday = 0
    Ok(Cron {
        minute: parse_field(f[0], 0, 59, "minute")?,
        hour: parse_field(f[1], 0, 23, "hour")? as u32,
        dom: parse_field(f[2], 1, 31, "day")? as u32,
        month: parse_field(f[3], 1, 12, "month")? as u16,
        dow,
        dom_star: f[2].starts_with('*'),
        dow_star: f[4].starts_with('*'),
    })
}

fn parse_hhmm(t: &str) -> Result<(u32, u32), String> {
    let (h, m) = t.split_once(':').ok_or_else(|| format!("time `{t}` must look like HH:MM"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad hour in `{t}`"))?;
    let m: u32 = m.parse().map_err(|_| format!("bad minute in `{t}`"))?;
    if h > 23 || m > 59 {
        return Err(format!("time `{t}` is out of range"));
    }
    Ok((h, m))
}

const DAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

/// Parse a cron line or the simple English forms. Unknown text is an error.
pub fn parse_spec(text: &str) -> Result<Spec, String> {
    let low = text.trim().to_lowercase();
    let w: Vec<&str> = low.split_whitespace().collect();
    let unknown = || format!("unknown schedule `{}`: use 5 cron fields, `every N minutes|hours`, `hourly`, `daily at HH:MM`, `weekdays at HH:MM` or `every monday at HH:MM`", text.trim());
    match w.as_slice() {
        [] => Err("empty schedule".into()),
        ["hourly"] => parse_cron("0 * * * *").map(Spec::Cron),
        ["every", "minute"] => Ok(Spec::Every(60)),
        ["every", "hour"] => Ok(Spec::Every(3600)),
        ["every", n, unit @ ("minutes" | "hours")] if n.parse::<i64>().is_ok() => {
            let n: i64 = n.parse().unwrap();
            let max = if *unit == "minutes" { 10_080 } else { 168 };
            if n < 1 || n > max {
                return Err(format!("every N {unit}: N must be 1-{max}"));
            }
            Ok(Spec::Every(n * if *unit == "minutes" { 60 } else { 3600 }))
        }
        ["daily", "at", t] => parse_hhmm(t).and_then(|(h, m)| parse_cron(&format!("{m} {h} * * *"))).map(Spec::Cron),
        ["weekdays", "at", t] => parse_hhmm(t).and_then(|(h, m)| parse_cron(&format!("{m} {h} * * 1-5"))).map(Spec::Cron),
        ["every", d, "at", t] if DAYS.contains(d) => {
            let dow = DAYS.iter().position(|x| x == d).unwrap();
            parse_hhmm(t).and_then(|(h, m)| parse_cron(&format!("{m} {h} * * {dow}"))).map(Spec::Cron)
        }
        _ if w.len() == 5 && w.iter().all(|f| f.chars().all(|c| c.is_ascii_digit() || "*/,-".contains(c))) => parse_cron(&low).map(Spec::Cron),
        _ => Err(unknown()),
    }
}

/// Wall-clock to instant. `local` (the server zone) or a test zone.
pub type Resolver<'a> = &'a dyn Fn(NaiveDateTime) -> LocalResult<DateTime<FixedOffset>>;

pub fn local_resolver(n: NaiveDateTime) -> LocalResult<DateTime<FixedOffset>> {
    match Local.from_local_datetime(&n) {
        LocalResult::Single(a) => LocalResult::Single(a.fixed_offset()),
        LocalResult::Ambiguous(a, b) => LocalResult::Ambiguous(a.fixed_offset(), b.fixed_offset()),
        LocalResult::None => LocalResult::None,
    }
}

/// A wall-clock time as one instant under the DST policy in the module doc.
fn resolve_wall(n: NaiveDateTime, resolve: Resolver) -> Option<DateTime<FixedOffset>> {
    let mut n = n;
    for _ in 0..(24 * 60) {
        match resolve(n) {
            LocalResult::Single(a) => return Some(a),
            LocalResult::Ambiguous(a, b) => return Some(a.min(b)),
            LocalResult::None => n += Duration::minutes(1),
        }
    }
    None
}

impl Cron {
    fn day_matches(&self, d: NaiveDate) -> bool {
        if self.month & (1 << d.month()) == 0 {
            return false;
        }
        let dom = self.dom & (1 << d.day()) != 0;
        let dow = self.dow & (1 << d.weekday().num_days_from_sunday()) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    fn next_after(&self, after: DateTime<FixedOffset>, resolve: Resolver) -> Option<DateTime<FixedOffset>> {
        let start = after.naive_local().with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        for k in 0..(366 * 9) {
            let d = start.date() + Duration::days(k);
            if !self.day_matches(d) {
                continue;
            }
            for h in (0..24).filter(|h| self.hour & (1 << h) != 0) {
                for m in (0..60).filter(|m| self.minute & (1 << m) != 0) {
                    let n = d.and_hms_opt(h, m, 0)?;
                    if n < start {
                        continue;
                    }
                    if let Some(t) = resolve_wall(n, resolve) {
                        if t > after {
                            return Some(t);
                        }
                    }
                }
            }
        }
        None
    }
}

impl Spec {
    /// The first run strictly after `after`; None when the spec can never fire (e.g. `0 0 31 2 *`).
    pub fn next_after(&self, after: DateTime<FixedOffset>, resolve: Resolver) -> Option<DateTime<FixedOffset>> {
        match self {
            Spec::Every(s) => Some(after + Duration::seconds(*s)),
            Spec::Cron(c) => c.next_after(after, resolve),
        }
    }
}

fn now_local() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

fn next_local(spec: &Spec, after: DateTime<FixedOffset>) -> Result<DateTime<FixedOffset>, String> {
    spec.next_after(after, &local_resolver).ok_or_else(|| "that schedule never fires".to_string())
}

// ── storage ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Schedule {
    pub id: String,
    pub spec: String,
    pub kind: String,
    pub payload: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub last_result: Option<String>,
    pub session_id: Option<String>,
}

/// Called by `Store` when it opens the sessions DB.
pub fn init_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedules (id TEXT PRIMARY KEY, spec TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
            enabled INTEGER NOT NULL, last_run TEXT, next_run TEXT, last_result TEXT, session_id TEXT, created_at TEXT NOT NULL);",
    )
}

/// A view of the schedules table in the sessions DB.
#[derive(Clone)]
pub struct Schedules {
    conn: Arc<Mutex<Connection>>,
}

const COLS: &str = "id, spec, kind, payload, enabled, last_run, next_run, last_result, session_id";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: r.get(0)?,
        spec: r.get(1)?,
        kind: r.get(2)?,
        payload: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        last_run: r.get(5)?,
        next_run: r.get(6)?,
        last_result: r.get(7)?,
        session_id: r.get(8)?,
    })
}

fn e2s(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Schedules {
    pub fn new(store: &Store) -> Self {
        Schedules { conn: store.shared_conn() }
    }

    pub async fn add(&self, spec: &str, prompt: &str, now: DateTime<FixedOffset>) -> Result<Schedule, String> {
        if prompt.trim().is_empty() {
            return Err("the prompt is empty".into());
        }
        self.insert("prompt", spec, prompt, now).await
    }

    /// A schedule that runs the playbook `name` (trigger `schedule`; it must be approved when it runs).
    pub async fn add_playbook(&self, spec: &str, name: &str, now: DateTime<FixedOffset>) -> Result<Schedule, String> {
        if !crate::playbook::valid_name(name.trim()) {
            return Err(format!("`{name}` is not a valid playbook name"));
        }
        self.insert("playbook", spec, name.trim(), now).await
    }

    async fn insert(&self, kind: &str, spec: &str, prompt: &str, now: DateTime<FixedOffset>) -> Result<Schedule, String> {
        let parsed = parse_spec(spec)?;
        let next = next_local(&parsed, now)?.to_rfc3339();
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        self.conn
            .lock()
            .await
            .execute(
                "INSERT INTO schedules (id, spec, kind, payload, enabled, next_run, created_at) VALUES (?1, ?2, ?6, ?3, 1, ?4, ?5)",
                params![id, spec.trim(), prompt.trim(), next, now.to_rfc3339(), kind],
            )
            .map_err(e2s)?;
        self.get(&id).await?.ok_or_else(|| "schedule vanished".into())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Schedule>, String> {
        let c = self.conn.lock().await;
        c.query_row(&format!("SELECT {COLS} FROM schedules WHERE id = ?1"), params![id], row).optional().map_err(e2s)
    }

    pub async fn list(&self) -> Result<Vec<Schedule>, String> {
        let c = self.conn.lock().await;
        let mut st = c.prepare(&format!("SELECT {COLS} FROM schedules ORDER BY created_at, id")).map_err(e2s)?;
        let rows = st.query_map([], row).map_err(e2s)?;
        rows.collect::<Result<_, _>>().map_err(e2s)
    }

    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        self.conn.lock().await.execute("DELETE FROM schedules WHERE id = ?1", params![id]).map(|n| n == 1).map_err(e2s)
    }

    /// Enable (next run recomputed from `now`) or disable (next run cleared).
    pub async fn set_enabled(&self, id: &str, on: bool, now: DateTime<FixedOffset>) -> Result<bool, String> {
        let Some(s) = self.get(id).await? else { return Ok(false) };
        let next = if on { Some(next_local(&parse_spec(&s.spec)?, now)?.to_rfc3339()) } else { None };
        self.conn.lock().await.execute("UPDATE schedules SET enabled = ?2, next_run = ?3 WHERE id = ?1", params![id, on as i64, next]).map_err(e2s)?;
        Ok(true)
    }

    /// Make a schedule due now (the runner picks it up on its next tick).
    pub async fn queue_now(&self, id: &str, now: DateTime<FixedOffset>) -> Result<bool, String> {
        self.conn.lock().await.execute("UPDATE schedules SET next_run = ?2 WHERE id = ?1", params![id, now.to_rfc3339()]).map(|n| n == 1).map_err(e2s)
    }

    /// Enabled schedules whose next run is at or before `now`.
    pub async fn due(&self, now: DateTime<FixedOffset>) -> Result<Vec<Schedule>, String> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|s| s.enabled && s.next_run.as_deref().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).is_some_and(|t| t <= now))
            .collect())
    }

    async fn record(&self, id: &str, ran: DateTime<FixedOffset>, result: &str, next: Option<DateTime<FixedOffset>>) -> Result<(), String> {
        self.conn
            .lock()
            .await
            .execute(
                "UPDATE schedules SET last_run = ?2, last_result = ?3, next_run = ?4 WHERE id = ?1",
                params![id, ran.to_rfc3339(), result, next.map(|n| n.to_rfc3339())],
            )
            .map(|_| ())
            .map_err(e2s)
    }

    async fn set_session(&self, id: &str, sid: &str) -> Result<(), String> {
        self.conn.lock().await.execute("UPDATE schedules SET session_id = ?2 WHERE id = ?1", params![id, sid]).map(|_| ()).map_err(e2s)
    }
}

// ── crontab note ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CronLine {
    pub spec_text: String,
    pub spec: Spec,
    /// The prompt text, or the playbook name when `kind` is `playbook`.
    pub prompt: String,
    /// `prompt` or `playbook`.
    pub kind: String,
}

impl CronLine {
    /// Identity of a line between ticks (a playbook line never equals a prompt line).
    fn key(&self) -> (String, String) {
        (self.spec_text.clone(), if self.kind == "playbook" { format!("\u{1}playbook {}", self.prompt) } else { self.prompt.clone() })
    }
}

/// Parse the crontab note: good lines, and `line N: ...` errors for the bad ones.
pub fn parse_crontab(text: &str) -> (Vec<CronLine>, Vec<String>) {
    let (mut good, mut bad) = (Vec::new(), Vec::new());
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        let kind = match toks.get(5) {
            Some(&"prompt") if toks.len() >= 7 => "prompt",
            Some(&"playbook") if toks.len() == 7 && crate::playbook::valid_name(toks[6]) => "playbook",
            _ => {
                bad.push(format!("line {}: expected `<5 cron fields> prompt <text>` or `<5 cron fields> playbook <name>`: {l}", i + 1));
                continue;
            }
        };
        let spec_text = toks[..5].join(" ");
        let prompt = toks[6..].join(" ");
        match parse_cron(&spec_text) {
            Ok(c) => {
                let spec = Spec::Cron(c);
                if spec.next_after(now_local(), &local_resolver).is_none() {
                    bad.push(format!("line {}: that schedule never fires: {l}", i + 1));
                } else {
                    good.push(CronLine { spec_text, spec, prompt, kind: kind.to_string() });
                }
            }
            Err(e) => bad.push(format!("line {}: {e}: {l}", i + 1)),
        }
    }
    (good, bad)
}

// ── runner ──────────────────────────────────────────────────────────────────

#[derive(Default)]
struct RunState {
    cron: HashMap<(String, String), DateTime<FixedOffset>>,
    bad_reported: HashSet<String>,
    heartbeat_next: Option<DateTime<FixedOffset>>,
}

pub struct Runner {
    agent: Arc<Agent>,
    sched: Schedules,
    heartbeat_minutes: u64,
    guard: Mutex<()>,
    st: std::sync::Mutex<RunState>,
}

fn one_line(s: &str, n: usize) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect()
}

impl Runner {
    pub fn new(agent: Arc<Agent>, heartbeat_minutes: u64) -> Arc<Self> {
        let sched = Schedules::new(&agent.ctx.store);
        Arc::new(Runner { agent, sched, heartbeat_minutes, guard: Mutex::new(()), st: Default::default() })
    }

    pub fn schedules(&self) -> &Schedules {
        &self.sched
    }

    /// Tick every 30 s until the process ends.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(30));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                me.tick(now_local()).await;
            }
        })
    }

    async fn session_titled(&self, title: &str) -> Result<String, String> {
        let store = &self.agent.ctx.store;
        if let Some(s) = store.list_sessions().await?.into_iter().find(|s| s.title == title) {
            return Ok(s.id);
        }
        store.create_session(title).await
    }

    /// One agent turn as the owner; (ok, text).
    async fn run_turn(&self, sid: &str, prompt: &str) -> (bool, String) {
        match self.agent.turn(Some(sid), prompt, true).await {
            Ok(r) => match (r.reply, r.stopped) {
                (Some(t), _) => (true, t),
                (None, s) => (false, s.unwrap_or_else(|| "no answer".into())),
            },
            Err(f) => (false, format!("{} ({})", f.error, f.attempts.join("; "))),
        }
    }

    async fn log(&self, now: DateTime<FixedOffset>, status: &str, label: &str, text: &str) {
        let line = format!("{} {status} {label}: {}", now.format("%Y-%m-%d %H:%M"), one_line(text, 300));
        if let Err(e) = append_log(&self.agent.ctx.vault, &line).await {
            tracing::warn!("cron.log: {e}");
        }
    }

    async fn run_db(&self, s: &Schedule, now: DateTime<FixedOffset>) {
        let started = now_local().max(now);
        let title = format!("⏰ {}", s.spec);
        if s.kind == "playbook" {
            let (ok, text) = crate::flow::playbook_job(&self.agent, &s.payload).await;
            let result = if ok { text } else { format!("ERROR: {text}") };
            let finished = now_local().max(now);
            let next = parse_spec(&s.spec).ok().and_then(|p| p.next_after(finished, &local_resolver));
            if let Err(e) = self.sched.record(&s.id, started, &one_line(&result, 500), next).await {
                tracing::warn!("schedule {}: {e}", s.id);
            }
            self.log(started, if ok { "ok" } else { "ERROR" }, &format!("{title} playbook {}", s.payload), &result).await;
            return;
        }
        let sid = match &s.session_id {
            Some(id) if self.agent.ctx.store.exists(id).await.unwrap_or(false) => Ok(id.clone()),
            _ => match self.agent.ctx.store.create_session(&title).await {
                Ok(id) => {
                    let _ = self.sched.set_session(&s.id, &id).await;
                    Ok(id)
                }
                Err(e) => Err(e),
            },
        };
        let (ok, text) = match sid {
            Ok(sid) => self.run_turn(&sid, &s.payload).await,
            Err(e) => (false, format!("no session: {e}")),
        };
        let result = if ok { text } else { format!("ERROR: {text}") };
        let finished = now_local().max(now);
        let next = parse_spec(&s.spec).ok().and_then(|p| p.next_after(finished, &local_resolver));
        if let Err(e) = self.sched.record(&s.id, started, &one_line(&result, 500), next).await {
            tracing::warn!("schedule {}: {e}", s.id);
        }
        self.log(started, if ok { "ok" } else { "ERROR" }, &title, &result).await;
    }

    async fn run_titled(&self, title: &str, prompt: &str) {
        let started = now_local();
        let (ok, text) = match self.session_titled(title).await {
            Ok(sid) => self.run_turn(&sid, prompt).await,
            Err(e) => (false, format!("no session: {e}")),
        };
        self.log(started, if ok { "ok" } else { "ERROR" }, title, &text).await;
    }

    /// Run one schedule now (HTTP `run`). Err when another run is in progress or the id is unknown.
    pub async fn run_now(&self, id: &str) -> Result<Schedule, String> {
        let Ok(_g) = self.guard.try_lock() else { return Err("busy: a scheduled run is in progress".into()) };
        let s = self.sched.get(id).await?.ok_or_else(|| format!("no schedule `{id}`"))?;
        self.run_db(&s, now_local()).await;
        self.sched.get(id).await?.ok_or_else(|| "schedule vanished".into())
    }

    /// One pass: due stored schedules, due crontab lines, the heartbeat. Returns how many runs
    /// started; 0 also when a run is already in progress.
    pub async fn tick(&self, now: DateTime<FixedOffset>) -> usize {
        let Ok(_g) = self.guard.try_lock() else { return 0 };
        let mut n = 0;
        match self.sched.due(now).await {
            Ok(due) => {
                for s in due {
                    self.run_db(&s, now).await;
                    n += 1;
                }
            }
            Err(e) => tracing::warn!("schedules: {e}"),
        }
        n + self.tick_crontab(now).await + self.tick_heartbeat(now).await
    }

    async fn tick_crontab(&self, now: DateTime<FixedOffset>) -> usize {
        let text = match note_by_path(&self.agent.ctx.vault, CRONTAB_PATH).await {
            Ok(Some(n)) => n.content,
            _ => String::new(),
        };
        let (good, bad) = parse_crontab(&text);
        let fresh: Vec<String> = {
            let mut st = self.st.lock().unwrap();
            let fresh = bad.iter().filter(|b| !st.bad_reported.contains(*b)).cloned().collect();
            st.bad_reported = bad.iter().cloned().collect();
            fresh
        };
        for b in fresh {
            self.log(now, "ERROR", CRONTAB_PATH, &b).await;
        }
        let keys: HashSet<(String, String)> = good.iter().map(CronLine::key).collect();
        let mut due = Vec::new();
        {
            let mut st = self.st.lock().unwrap();
            st.cron.retain(|k, _| keys.contains(k));
            for g in &good {
                let k = g.key();
                match st.cron.get(&k).copied() {
                    None => {
                        if let Some(nx) = g.spec.next_after(now, &local_resolver) {
                            st.cron.insert(k, nx);
                        }
                    }
                    Some(t) if t <= now && !due.iter().any(|d: &CronLine| d.key() == g.key()) => due.push(g.clone()),
                    _ => {}
                }
            }
        }
        for g in &due {
            if g.kind == "playbook" {
                let started = now_local();
                let (ok, text) = crate::flow::playbook_job(&self.agent, &g.prompt).await;
                self.log(started, if ok { "ok" } else { "ERROR" }, &format!("⏰ {} (crontab playbook {})", g.spec_text, g.prompt), &text).await;
            } else {
                self.run_titled(&format!("⏰ {} (crontab)", g.spec_text), &g.prompt).await;
            }
            let nx = g.spec.next_after(now_local().max(now), &local_resolver);
            let mut st = self.st.lock().unwrap();
            match nx {
                Some(nx) => st.cron.insert(g.key(), nx),
                None => st.cron.remove(&g.key()),
            };
        }
        due.len()
    }

    async fn tick_heartbeat(&self, now: DateTime<FixedOffset>) -> usize {
        if self.heartbeat_minutes == 0 {
            return 0;
        }
        let every = Duration::minutes(self.heartbeat_minutes as i64);
        {
            let mut st = self.st.lock().unwrap();
            match st.heartbeat_next {
                None => {
                    st.heartbeat_next = Some(now + every);
                    return 0;
                }
                Some(t) if t > now => return 0,
                _ => st.heartbeat_next = Some(now + every),
            }
        }
        let text = note_by_path(&self.agent.ctx.vault, HEARTBEAT_PATH).await.ok().flatten().map(|n| strip_html_comments(&n.content).trim().to_string()).unwrap_or_default();
        if text.is_empty() {
            return 0;
        }
        self.run_titled(HEARTBEAT_TITLE, &text).await;
        1
    }
}

/// Append a line to the log note, keeping the newest `LOG_LINES`.
pub async fn append_log(vault: &nautik9_wayfinder::WayfinderVault, line: &str) -> Result<(), String> {
    let old = note_by_path(vault, LOG_PATH).await?.map(|n| n.content).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().collect();
    lines.push(line);
    let from = lines.len().saturating_sub(LOG_LINES);
    write_note(vault, LOG_PATH, &lines[from..].join("\n")).await.map(|_| ())
}

// ── tool ────────────────────────────────────────────────────────────────────

pub struct ScheduleTool;

fn fmt(s: &Schedule) -> String {
    format!(
        "{} | {} | {} | next {} | last {} | {}",
        s.id,
        s.spec,
        if s.enabled { "on" } else { "off" },
        s.next_run.as_deref().unwrap_or("-"),
        s.last_run.as_deref().unwrap_or("never"),
        one_line(&s.payload, 80)
    )
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }
    fn description(&self) -> &str {
        "Schedule a prompt to run later as an agent turn. Actions: add (spec, prompt), list, remove (id), enable (id), disable (id), run_now (id: queued, runs within 30 s). \
Spec: 5 cron fields (`0 9 * * 1-5`), `every 30 minutes`, `every 2 hours`, `hourly`, `daily at 08:00`, `weekdays at 07:30` or `every monday at 09:00`. Local time. Unknown specs are rejected."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["add","list","remove","enable","disable","run_now"]},"spec":{"type":"string"},"prompt":{"type":"string"},"id":{"type":"string"}},"required":["action"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let sc = Schedules::new(&c.store);
        let id = || a["id"].as_str().ok_or_else(|| "missing string argument `id`".to_string());
        let now = now_local();
        let missing = |id: &str| format!("no schedule `{id}`");
        match a["action"].as_str().ok_or("missing string argument `action`")? {
            "add" => {
                let spec = a["spec"].as_str().ok_or("missing string argument `spec`")?;
                let prompt = a["prompt"].as_str().ok_or("missing string argument `prompt`")?;
                Ok(format!("added: {}", fmt(&sc.add(spec, prompt, now).await?)))
            }
            "list" => {
                let l = sc.list().await?;
                Ok(if l.is_empty() { "no schedules".into() } else { l.iter().map(fmt).collect::<Vec<_>>().join("\n") })
            }
            "remove" => sc.remove(id()?).await?.then(|| format!("removed {}", id().unwrap())).ok_or_else(|| missing(id().unwrap())),
            "enable" | "disable" => {
                let on = a["action"] == "enable";
                sc.set_enabled(id()?, on, now).await?.then(|| format!("{} {}", if on { "enabled" } else { "disabled" }, id().unwrap())).ok_or_else(|| missing(id().unwrap()))
            }
            "run_now" => sc.queue_now(id()?, now).await?.then(|| "queued: it runs on the next 30 s tick".to_string()).ok_or_else(|| missing(id().unwrap())),
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(n: NaiveDateTime) -> LocalResult<DateTime<FixedOffset>> {
        FixedOffset::east_opt(0).unwrap().from_local_datetime(&n)
    }
    fn t(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }
    fn next(spec: &str, after: &str) -> String {
        parse_spec(spec).unwrap().next_after(t(after), &utc).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "never".into())
    }

    #[test]
    fn cron_basics_and_fields() {
        assert_eq!(next("*/15 * * * *", "2026-10-29T10:07:30+00:00"), "2026-10-29 10:15");
        assert_eq!(next("*/15 * * * *", "2026-10-29T10:45:00+00:00"), "2026-10-29 11:00", "strictly after");
        assert_eq!(next("30 9 * * *", "2026-10-29T09:30:00+00:00"), "2026-10-30 09:30");
        assert_eq!(next("0 9-17/4 * * *", "2026-10-29T10:00:00+00:00"), "2026-10-29 13:00");
        assert_eq!(next("5,35 * * * *", "2026-10-29T10:06:00+00:00"), "2026-10-29 10:35");
        assert_eq!(next("10-20/5 8 * * *", "2026-10-29T08:11:00+00:00"), "2026-10-29 08:15");
        assert_eq!(next("0 0 * * *", "2026-10-29T23:59:59+00:00"), "2026-10-30 00:00");
    }

    #[test]
    fn month_ends_years_and_leap_days() {
        assert_eq!(next("0 0 1 * *", "2026-01-31T12:00:00+00:00"), "2026-02-01 00:00");
        assert_eq!(next("0 12 31 * *", "2026-04-30T12:00:00+00:00"), "2026-05-31 12:00", "April has no 31st");
        assert_eq!(next("0 12 30 * *", "2026-01-30T13:00:00+00:00"), "2026-03-30 12:00", "February has no 30th");
        assert_eq!(next("59 23 31 12 *", "2026-12-31T23:59:00+00:00"), "2027-12-31 23:59", "year rollover");
        assert_eq!(next("0 0 29 2 *", "2026-03-01T00:00:00+00:00"), "2028-02-29 00:00", "leap day");
        assert_eq!(next("0 0 29 2 *", "2096-03-01T00:00:00+00:00"), "2104-02-29 00:00", "2100 is not a leap year");
        assert_eq!(next("0 0 31 2 *", "2026-03-01T00:00:00+00:00"), "never");
        assert_eq!(next("0 0 1 1 *", "2026-12-31T23:59:00+00:00"), "2027-01-01 00:00");
    }

    #[test]
    fn weekday_rules() {
        // 2026-10-29 is a Thursday.
        assert_eq!(next("0 9 * * 1", "2026-10-29T10:00:00+00:00"), "2026-11-02 09:00");
        assert_eq!(next("0 9 * * 7", "2026-10-29T10:00:00+00:00"), "2026-11-01 09:00", "7 is Sunday");
        assert_eq!(next("0 9 * * 0", "2026-10-29T10:00:00+00:00"), "2026-11-01 09:00");
        assert_eq!(next("0 9 * * 1-5", "2026-10-30T10:00:00+00:00"), "2026-11-02 09:00", "Friday after 9 goes to Monday");
        // both restricted: either matches (Vixie)
        assert_eq!(next("0 9 1 * 1", "2026-10-29T10:00:00+00:00"), "2026-11-01 09:00", "1st OR Monday: the 1st (Sunday) comes first");
        assert_eq!(next("0 9 5 * 1", "2026-10-29T10:00:00+00:00"), "2026-11-02 09:00");
    }

    #[test]
    fn english_forms() {
        assert_eq!(parse_spec("every 5 minutes").unwrap(), Spec::Every(300));
        assert_eq!(parse_spec("Every 2 hours").unwrap(), Spec::Every(7200));
        assert_eq!(parse_spec("every minute").unwrap(), Spec::Every(60));
        assert_eq!(next("every 90 minutes", "2026-10-29T10:00:00+00:00"), "2026-10-29 11:30");
        assert_eq!(next("hourly", "2026-10-29T23:10:00+00:00"), "2026-10-30 00:00");
        assert_eq!(next("daily at 08:05", "2026-10-29T08:05:00+00:00"), "2026-10-30 08:05");
        assert_eq!(next("daily at 8:05", "2026-10-29T07:00:00+00:00"), "2026-10-29 08:05");
        assert_eq!(next("weekdays at 07:30", "2026-10-30T08:00:00+00:00"), "2026-11-02 07:30");
        assert_eq!(next("every monday at 09:00", "2026-10-29T08:00:00+00:00"), "2026-11-02 09:00");
        assert_eq!(next("every sunday at 23:59", "2026-10-29T08:00:00+00:00"), "2026-11-01 23:59");
        assert_eq!(parse_spec("0 9 * * 1-5").unwrap(), parse_spec("weekdays at 09:00").unwrap());
    }

    #[test]
    fn unknown_text_is_an_error() {
        for bad in ["", "sometimes", "every 0 minutes", "every 5 parsecs", "every 99999 minutes", "daily at 25:00", "daily at 9", "daily at 09:61", "every funday at 09:00", "* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 8", "*/0 * * * *", "5-1 * * * *", "a b c d e", "weekdays 09:00", "at 09:00 daily"] {
            assert!(parse_spec(bad).is_err(), "`{bad}` must be rejected");
        }
    }

    /// New York 2026: clocks jump 02:00 -> 03:00 on Mar 8 and fall back 02:00 -> 01:00 on Nov 1.
    fn nyc(n: NaiveDateTime) -> LocalResult<DateTime<FixedOffset>> {
        let at = |h: i32| FixedOffset::west_opt(h * 3600).unwrap().from_local_datetime(&n).unwrap();
        let d = |m, dd, h| NaiveDate::from_ymd_opt(2026, m, dd).unwrap().and_hms_opt(h, 0, 0).unwrap();
        if n >= d(3, 8, 2) && n < d(3, 8, 3) {
            LocalResult::None
        } else if n >= d(11, 1, 1) && n < d(11, 1, 2) {
            LocalResult::Ambiguous(at(4), at(5))
        } else if n >= d(3, 8, 3) && n < d(11, 1, 1) {
            LocalResult::Single(at(4))
        } else {
            LocalResult::Single(at(5))
        }
    }
    fn nxt(spec: &str, after: &str) -> String {
        parse_spec(spec).unwrap().next_after(t(after), &nyc).map(|d| d.to_rfc3339()).unwrap_or_else(|| "never".into())
    }

    #[test]
    fn dst_gap_runs_once_after_the_gap_and_overlap_runs_at_first_occurrence() {
        // Gap: a daily 02:30 job on Mar 8 runs once at 03:00 EDT, then back to 02:30 on Mar 9.
        assert_eq!(nxt("30 2 * * *", "2026-03-07T02:30:00-05:00"), "2026-03-08T03:00:00-04:00");
        assert_eq!(nxt("30 2 * * *", "2026-03-08T03:00:00-04:00"), "2026-03-09T02:30:00-04:00");
        // Two gap minutes map to the same instant: only one run.
        assert_eq!(nxt("15,45 2 * * *", "2026-03-08T03:00:00-04:00"), "2026-03-09T02:15:00-04:00");
        // Overlap: 01:30 on Nov 1 runs at the first 01:30 (EDT) only.
        assert_eq!(nxt("30 1 * * *", "2026-10-31T01:30:00-04:00"), "2026-11-01T01:30:00-04:00");
        assert_eq!(nxt("30 1 * * *", "2026-11-01T01:30:00-04:00"), "2026-11-02T01:30:00-05:00");
        // A normal job across the change keeps its wall-clock time.
        assert_eq!(nxt("0 9 * * *", "2026-03-07T09:00:00-05:00"), "2026-03-08T09:00:00-04:00");
        // Interval specs are real elapsed time.
        assert_eq!(nxt("every 2 hours", "2026-03-08T01:00:00-05:00"), "2026-03-08T03:00:00-05:00");
    }

    #[test]
    fn crontab_lines() {
        let text = "# my jobs\n\n0 9 * * 1-5 prompt Summarise my inbox notes\n*/10 * * * * prompt\n61 * * * * prompt x\nevery day prompt y\n0 0 31 2 * prompt never\n30 8 * * * prompt Plan   the  day\n";
        let (good, bad) = parse_crontab(text);
        assert_eq!(good.len(), 2);
        assert_eq!(good[1].prompt, "Plan the day");
        assert_eq!(bad.len(), 4, "{bad:?}");
        assert!(bad[0].starts_with("line 4:") && bad[1].starts_with("line 5:") && bad[2].starts_with("line 6:") && bad[3].starts_with("line 7:"));
    }

    #[tokio::test]
    async fn storage_roundtrip() {
        let store = Store::in_memory().unwrap();
        let sc = Schedules::new(&store);
        let now = t("2026-10-29T10:00:00+00:00");
        let a = sc.add("every 10 minutes", "check the tide", now).await.unwrap();
        assert!(sc.add("whenever", "x", now).await.is_err());
        assert!(sc.add("hourly", "  ", now).await.is_err());
        assert_eq!(a.kind, "prompt");
        assert_eq!(t(a.next_run.as_deref().unwrap()), now + Duration::minutes(10));
        assert!(sc.due(now).await.unwrap().is_empty());
        assert_eq!(sc.due(now + Duration::minutes(10)).await.unwrap().len(), 1);
        assert!(sc.set_enabled(&a.id, false, now).await.unwrap());
        assert!(sc.due(now + Duration::hours(5)).await.unwrap().is_empty());
        assert!(sc.queue_now(&a.id, now).await.unwrap());
        assert!(sc.due(now).await.unwrap().is_empty(), "queued but disabled");
        assert!(sc.set_enabled(&a.id, true, now).await.unwrap());
        assert!(sc.remove(&a.id).await.unwrap() && !sc.remove(&a.id).await.unwrap());
    }
}
