//! Note scripts: JavaScript with a `jade.*` API, run by an embedded pure-Rust engine (Boa).
//!
//! Jade Vault documents `jade.*` in JavaScript with top-level `await`, so scripts are JavaScript
//! and the engine is `boa_engine` (see PORTING.md for why not Rhai). A script is a note: its
//! fenced ```js / ```javascript / ```jade-js blocks joined together, or the whole note when it
//! has no fence. The text is the body of an `async` function: use `return` for a result and
//! `console.log` for output. The host calls are synchronous, so `await` works but is optional.
//!
//! API (`args` is the JSON given to the run):
//!   jade.read(path) -> string | null        jade.list(prefix?) -> [path]
//!   jade.search(query, limit?) -> [{title, score, snippet}]
//!   jade.write(path, content) -> bool       (needs `scripts.write`; true = created)
//!   jade.tool(name, args) -> string         jade.tools.<name>(args)   (needs `scripts.tools`)
//!   jade.ask(prompt, {system?}) -> string   one model answer, no tools (needs `scripts.tools`)
//!   console.log(...), console.error(...)
//! Like Jade, notes can read by default but cannot change anything or call tools until the owner
//! turns that on in agent.json: `scripts: {write, tools, max_calls (50), max_loop (1000000),
//! timeout_secs (20)}`. The `script` tool and `sandbox`-style recursion are refused: a script
//! cannot call `script`. Limits: loop iterations and recursion depth are capped inside the
//! engine; host calls are counted and refused after `max_calls`; the wall clock is checked at
//! every host call and the run is abandoned after `timeout_secs` (a tight loop that makes no
//! host call is bounded by `max_loop`, not by the clock, because Boa has no interrupt).

use async_trait::async_trait;
use boa_engine::{js_string, native_function::NativeFunction, property::Attribute, Context, JsError, JsString, JsValue, Source};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::agent::Agent;
use crate::tools::{note_by_path, write_note, Tool, ToolCtx};

#[derive(Clone, Debug)]
pub struct ScriptConfig {
    pub write: bool,
    pub tools: bool,
    pub max_calls: u32,
    pub max_loop: u64,
    pub timeout_secs: u64,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        ScriptConfig { write: false, tools: false, max_calls: 50, max_loop: 1_000_000, timeout_secs: 20 }
    }
}

impl ScriptConfig {
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let mut c = ScriptConfig::default();
        if v.is_null() {
            return Ok(c);
        }
        let o = v.as_object().ok_or("agent.json scripts: must be an object")?;
        c.write = o.get("write").and_then(Value::as_bool).unwrap_or(false);
        c.tools = o.get("tools").and_then(Value::as_bool).unwrap_or(false);
        let num = |k: &str, d: u64| -> Result<u64, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(d),
                Some(x) => x.as_u64().filter(|n| *n > 0).ok_or_else(|| format!("agent.json scripts.{k}: must be a positive integer")),
            }
        };
        c.max_calls = num("max_calls", c.max_calls as u64)?.min(10_000) as u32;
        c.max_loop = num("max_loop", c.max_loop)?;
        c.timeout_secs = num("timeout_secs", c.timeout_secs)?.min(300);
        Ok(c)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScriptReport {
    pub ok: bool,
    pub logs: Vec<String>,
    /// JSON text of the returned value.
    pub result: Option<String>,
    pub error: Option<String>,
    pub host_calls: u32,
    pub millis: u128,
}

impl ScriptReport {
    pub fn text(&self) -> String {
        let mut s = String::new();
        if !self.logs.is_empty() {
            s.push_str(&format!("output:\n{}\n", self.logs.join("\n")));
        }
        if let Some(r) = &self.result {
            s.push_str(&format!("returned: {r}\n"));
        }
        if let Some(e) = &self.error {
            s.push_str(&format!("ERROR: {e}\n"));
        }
        s.push_str(&format!("[{} host calls, {} ms]", self.host_calls, self.millis));
        s
    }
}

/// The script text of a note: its js fences, or the whole note.
pub fn extract_script(note: &str) -> String {
    let mut blocks = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for line in note.lines() {
        let t = line.trim_start();
        match &mut cur {
            None => {
                if let Some(info) = t.strip_prefix("```") {
                    if ["js", "javascript", "jade-js"].contains(&info.trim().to_lowercase().as_str()) {
                        cur = Some(Vec::new());
                    }
                }
            }
            Some(b) => {
                if t.starts_with("```") {
                    blocks.push(b.join("\n"));
                    cur = None;
                } else {
                    b.push(line);
                }
            }
        }
    }
    if blocks.is_empty() {
        note.to_string()
    } else {
        blocks.join("\n")
    }
}

const PRELUDE: &str = r#"
globalThis.jade = (function () {
  function host(op, a) {
    var r = JSON.parse(__host(op, JSON.stringify(a === undefined ? null : a)));
    if (r.error !== undefined) throw new Error(r.error);
    return r.value;
  }
  var tools = new Proxy({}, { get: function (_, name) { return function (args) { return host("tool", { name: String(name), args: args || {} }); }; } });
  return {
    read: function (p) { return host("read", { path: p }); },
    list: function (prefix) { return host("list", { prefix: prefix || "" }); },
    search: function (q, limit) { return host("search", { query: q, limit: limit || 8 }); },
    write: function (p, c) { return host("write", { path: p, content: String(c) }); },
    tool: function (name, args) { return host("tool", { name: name, args: args || {} }); },
    ask: function (prompt, opts) { return host("ask", { prompt: String(prompt), system: opts && opts.system }); },
    tools: tools
  };
})();
globalThis.console = {
  log: function () { __host("log", JSON.stringify(Array.prototype.map.call(arguments, function (x) { return typeof x === "string" ? x : JSON.stringify(x); }).join(" "))); },
  error: function () { __host("log", JSON.stringify("[error] " + Array.prototype.map.call(arguments, function (x) { return typeof x === "string" ? x : JSON.stringify(x); }).join(" "))); }
};
"#;

struct HostState {
    agent: Arc<Agent>,
    cfg: ScriptConfig,
    handle: tokio::runtime::Handle,
    deadline: Instant,
    calls: Mutex<u32>,
    logs: Mutex<Vec<String>>,
}

impl HostState {
    fn dispatch(&self, op: &str, a: &Value) -> Result<Value, String> {
        if op == "log" {
            let line = a.as_str().unwrap_or("").to_string();
            let mut l = self.logs.lock().unwrap();
            if l.len() < 500 {
                l.push(line.chars().take(4000).collect());
            }
            return Ok(Value::Null);
        }
        if Instant::now() > self.deadline {
            return Err(format!("script stopped: more than {}s of wall-clock time", self.cfg.timeout_secs));
        }
        {
            let mut c = self.calls.lock().unwrap();
            if *c >= self.cfg.max_calls {
                return Err(format!("script stopped: more than {} host calls (scripts.max_calls)", self.cfg.max_calls));
            }
            *c += 1;
        }
        let ctx = &self.agent.ctx;
        let s = |k: &str| a[k].as_str().ok_or_else(|| format!("jade.{op}: missing string `{k}`"));
        let h = &self.handle;
        match op {
            "read" => Ok(h.block_on(note_by_path(&ctx.vault, s("path")?))?.map(|n| json!(n.content)).unwrap_or(Value::Null)),
            "list" => {
                let prefix = a["prefix"].as_str().unwrap_or("").trim_start_matches('/').to_string();
                let items = h.block_on(ctx.vault.list_items()).map_err(|e| e.to_string())?;
                let mut v: Vec<String> = items.into_iter().map(|n| n.title).filter(|t| t.trim_start_matches('/').starts_with(&prefix)).collect();
                v.sort();
                Ok(json!(v))
            }
            "search" => {
                let k = a["limit"].as_u64().unwrap_or(8).clamp(1, 30) as usize;
                let hits = h.block_on(ctx.vault.search(s("query")?, k)).map_err(|e| e.to_string())?;
                Ok(json!(hits.iter().map(|x| json!({"title": x.title, "score": x.score, "snippet": x.snippet})).collect::<Vec<_>>()))
            }
            "write" => {
                if !self.cfg.write {
                    return Err("jade.write is off: the owner must set scripts.write in agent.json".into());
                }
                Ok(json!(h.block_on(write_note(&ctx.vault, s("path")?, s("content")?))?))
            }
            "tool" => {
                if !self.cfg.tools {
                    return Err("jade.tool is off: the owner must set scripts.tools in agent.json".into());
                }
                let name = s("name")?;
                if name == "script" || name == "delegate" {
                    return Err(format!("a script cannot call `{name}`"));
                }
                let t = self.agent.tools.allowed(name, true, false).ok_or_else(|| format!("no tool `{name}` is available"))?;
                h.block_on(t.call(&a["args"], ctx)).map(Value::String)
            }
            "ask" => {
                if !self.cfg.tools {
                    return Err("jade.ask is off: the owner must set scripts.tools in agent.json".into());
                }
                let mut msgs = Vec::new();
                if let Some(sys) = a["system"].as_str() {
                    msgs.push(json!({"role": "system", "content": sys}));
                }
                msgs.push(json!({"role": "user", "content": s("prompt")?}));
                let ans = h.block_on(self.agent.llm.chat_messages(&msgs, None, Some(self.agent.cfg.max_tokens), false)).map_err(|f| format!("no model answered: {}", f.error))?;
                Ok(json!(crate::agent::visible_text(ans.message["content"].as_str().unwrap_or(""))))
            }
            other => Err(format!("unknown host call `{other}`")),
        }
    }
}

fn run_blocking(agent: Arc<Agent>, cfg: ScriptConfig, body: String, args: Value, handle: tokio::runtime::Handle) -> ScriptReport {
    let started = Instant::now();
    let st = Arc::new(HostState { agent, cfg: cfg.clone(), handle, deadline: started + Duration::from_secs(cfg.timeout_secs), calls: Mutex::new(0), logs: Mutex::new(vec![]) });
    let outcome = eval_script(&st, &cfg, &body, &args);
    let (result, error) = match outcome {
        Ok(r) => (r, None),
        Err(e) => (None, Some(e)),
    };
    let calls = *st.calls.lock().unwrap();
    let logs = st.logs.lock().unwrap().clone();
    ScriptReport { ok: error.is_none(), logs, result, error, host_calls: calls, millis: started.elapsed().as_millis() }
}

fn eval_script(st: &Arc<HostState>, cfg: &ScriptConfig, body: &str, args: &Value) -> Result<Option<String>, String> {
    let mut ctx = Context::default();
    {
        let l = ctx.runtime_limits_mut();
        l.set_loop_iteration_limit(cfg.max_loop);
        l.set_recursion_limit(200);
        l.set_stack_size_limit(4 * 1024);
    }
    let st2 = st.clone();
    // SAFETY: the closure captures only an `Arc` of plain Rust data, nothing the GC must trace.
    let host = unsafe {
        NativeFunction::from_closure(move |_this, a, c| {
            let op = a.first().map(|v| v.to_string(c)).transpose()?.map(|s| s.to_std_string_escaped()).unwrap_or_default();
            let payload = a.get(1).map(|v| v.to_string(c)).transpose()?.map(|s| s.to_std_string_escaped()).unwrap_or_default();
            let parsed: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let reply = match st2.dispatch(&op, &parsed) {
                Ok(v) => json!({"value": v}),
                Err(e) => json!({"error": e}),
            };
            Ok(JsValue::from(JsString::from(reply.to_string().as_str())))
        })
    };
    ctx.register_global_callable(js_string!("__host"), 2, host).map_err(|e| e.to_string())?;
    let args_js = JsString::from(args.to_string().as_str());
    ctx.register_global_property(js_string!("__args_json"), args_js, Attribute::all()).map_err(|e| e.to_string())?;
    let err_text = |e: JsError, _c: &mut Context| e.to_string();
    ctx.eval(Source::from_bytes(PRELUDE)).map_err(|e| format!("prelude: {}", err_text(e, &mut ctx)))?;
    let wrapped = format!(
        "globalThis.args = JSON.parse(__args_json);\n(async function () {{\n{body}\n}})().then(function (v) {{ globalThis.__result = v === undefined ? null : JSON.stringify(v); globalThis.__done = true; }}, function (e) {{ globalThis.__error = String(e && e.message !== undefined ? (e.name ? e.name + ': ' : '') + e.message : e); globalThis.__done = true; }});"
    );
    if let Err(e) = ctx.eval(Source::from_bytes(&wrapped)) {
        return Err(err_text(e, &mut ctx));
    }
    if let Err(e) = ctx.run_jobs() {
        return Err(err_text(e, &mut ctx));
    }
    let g = ctx.global_object();
    let get = |k: JsString, c: &mut Context| g.get(k, c).ok().filter(|v| !v.is_undefined() && !v.is_null());
    if let Some(e) = get(js_string!("__error"), &mut ctx) {
        return Err(e.to_string(&mut ctx).map(|s| s.to_std_string_escaped()).unwrap_or_default());
    }
    if get(js_string!("__done"), &mut ctx).is_none() {
        return Err("the script never finished (it is waiting on something that cannot complete here; timers and the network do not exist)".into());
    }
    Ok(get(js_string!("__result"), &mut ctx).and_then(|v| v.to_string(&mut ctx).ok()).map(|s| s.to_std_string_escaped()))
}

/// Run script `source` (a note's text) with `args`.
pub async fn run_source(agent: &Arc<Agent>, cfg: &ScriptConfig, source: &str, args: Value) -> ScriptReport {
    let body = extract_script(source);
    let args = if args.is_null() { json!({}) } else { args };
    let (a, c, h) = (agent.clone(), cfg.clone(), tokio::runtime::Handle::current());
    let started = Instant::now();
    let join = tokio::task::spawn_blocking(move || run_blocking(a, c, body, args, h));
    // The engine cannot be interrupted, so the caller stops waiting after the limit (plus slack).
    match tokio::time::timeout(Duration::from_secs(cfg.timeout_secs + 5), join).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => ScriptReport { ok: false, logs: vec![], result: None, error: Some(format!("the script thread failed: {e}")), host_calls: 0, millis: started.elapsed().as_millis() },
        Err(_) => ScriptReport { ok: false, logs: vec![], result: None, error: Some(format!("script abandoned after {}s (it is still winding down in the background)", cfg.timeout_secs + 5)), host_calls: 0, millis: started.elapsed().as_millis() },
    }
}

/// Run the script note at `path`.
pub async fn run_note(agent: &Arc<Agent>, cfg: &ScriptConfig, path: &str, args: Value) -> Result<ScriptReport, String> {
    let n = note_by_path(&agent.ctx.vault, path).await?.ok_or_else(|| format!("no note at `{path}`"))?;
    Ok(run_source(agent, cfg, &n.content, args).await)
}

pub struct ScriptTool {
    pub agent: Weak<Agent>,
    pub cfg: ScriptConfig,
}

#[async_trait]
impl Tool for ScriptTool {
    fn name(&self) -> &str {
        "script"
    }
    fn description(&self) -> &str {
        "Run a note script: a note holding JavaScript (fenced ```js blocks, or the whole note) with the `jade.*` API (read, list, search; write and tool when the owner allows). Actions: run (path, args?), check (path: show the script text that would run, runs nothing). Scripts have step, call and time limits."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["run","check"]},"path":{"type":"string","description":"the script note, e.g. Scripts/count.md"},"args":{"description":"JSON passed to the script as `args`"}},"required":["action","path"]})
    }
    async fn call(&self, a: &Value, ctx: &ToolCtx) -> Result<String, String> {
        let agent = self.agent.upgrade().ok_or("the agent is shutting down")?;
        let path = a["path"].as_str().ok_or("missing string argument `path`")?;
        match a["action"].as_str().ok_or("missing string argument `action`")? {
            "check" => {
                let n = note_by_path(&ctx.vault, path).await?.ok_or_else(|| format!("no note at `{path}`"))?;
                Ok(format!("write {} / tools {}\n{}", self.cfg.write, self.cfg.tools, extract_script(&n.content)))
            }
            "run" => {
                let r = run_note(&agent, &self.cfg, path, a.get("args").cloned().unwrap_or(Value::Null)).await?;
                if r.ok {
                    Ok(r.text())
                } else {
                    Err(r.text())
                }
            }
            other => Err(format!("unknown action `{other}`")),
        }
    }
}
