//! The agent turn: steering, context guard and the tool-calling loop.
//!
//! System prompt (owner): the base introduction (or `Jade/SYSTEM.md` instead of it), the
//! persona `Jade/SOUL.md`, standing instructions `AGENTS.md` + `Jade/AGENTS.md`, the memory
//! bullets from `Jade/Memory.md` (all of them up to `memory_k`, else the ones sharing most words
//! with the last two user messages, or ranked by hybrid search when an embedding endpoint is configured), the skill list, and the local date, time and time zone. HTML
//! comments in steering text are removed. Other callers get the base text and SOUL only: no
//! memory, no notes, no tools.
//!
//! Context guard: the prompt is kept under `context_chars` (about 4 characters per token) by
//! dropping the oldest whole turns (a turn starts at a user message, so tool results are never
//! orphaned) and by cutting tool results longer than `max_tool_chars`. If the endpoint still
//! answers "context" too long, the budget (and the per-tool-result cap, a quarter of it) is halved
//! and the call retried, up to twice, and the learned
//! budget is kept for the rest of the process.
//!
//! Loop: up to `max_steps` model calls; each tool call is run (tools marked `parallel` run at
//! the same time when they are asked for together) and its result goes back as a `tool`
//! message. A turn that hits the step limit says so; it never invents a final answer.
//! `delegate` runs a sub-agent with read-only tools in a clean context and returns its report.

use futures_util::future::join_all;
use nautik9_lemonade::{ChatFailure, LemonadeModule};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::tools::{list_skills, memory_bullets, note_by_path, redact, Registry, ToolCtx, MEMORY_PATH};

pub const BASE_INTRO: &str = "You are Mighty Mack, Thom's NautiDog and helpful companion (sometimes). You work inside NautiK9, a private markdown vault. \
Use your tools to look things up instead of guessing: search and read notes before answering questions about them, and cite the note paths you used. \
Ask before deleting or replacing notes the user did not ask you to change. If a tool fails, say so plainly. Never invent facts, results or sources. \
Your own knowledge stops before today (the date is at the end of this prompt): for weather, news, prices or anything current, use the weather or web_search tool and trust what it returns over what you remember.";

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_steps: usize,
    pub context_chars: usize,
    pub max_tool_chars: usize,
    pub memory_k: usize,
    pub max_tokens: u32,
    pub delegate_steps: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig { max_steps: 8, context_chars: 48_000, max_tool_chars: 12_000, memory_k: 8, max_tokens: 2048, delegate_steps: 6 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolTrace {
    pub name: String,
    pub arguments: Value,
    pub ok: bool,
    pub result_preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnResult {
    pub session_id: String,
    /// The final answer text; None when the turn stopped without one (see `stopped`).
    pub reply: Option<String>,
    pub stopped: Option<String>,
    pub model: String,
    pub provider: String,
    pub endpoint: String,
    pub steps: usize,
    pub tool_calls: Vec<ToolTrace>,
    /// Messages left out of the prompt by the context guard on the last call.
    pub trimmed: usize,
    /// Index of the user message of this turn in the session (for Retry).
    pub first_idx: usize,
}

pub struct Agent {
    pub llm: Arc<LemonadeModule>,
    pub tools: Registry,
    pub ctx: ToolCtx,
    pub cfg: AgentConfig,
    learned_budget: AtomicUsize,
}

pub fn strip_html_comments(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find("-->").map(|e| &rest[i + e + 3..]).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// `<think>...</think>` removed from what the user sees.
pub fn visible_text(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find("</think>").map(|e| &rest[i + e + 8..]).unwrap_or("");
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn msg_chars(m: &Value) -> usize {
    m["content"].as_str().map(str::len).unwrap_or(0) + m["tool_calls"].to_string().len().min(4000) + 16
}

/// Context guard: system + the newest whole turns that fit `budget` characters. Returns the
/// messages to send and how many history messages were left out.
pub fn fit_context(system: &Value, history: &[Value], budget: usize, max_tool_chars: usize) -> (Vec<Value>, usize) {
    let clipped: Vec<Value> = history
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if m["role"] == "tool" {
                if let Some(t) = m["content"].as_str() {
                    if t.len() > max_tool_chars {
                        let cut: String = t.chars().take(max_tool_chars).collect();
                        m["content"] = json!(format!("{cut}\n[cut to {max_tool_chars} characters by the context guard]"));
                    }
                }
            }
            m
        })
        .collect();
    let turn_starts: Vec<usize> = clipped.iter().enumerate().filter(|(_, m)| m["role"] == "user").map(|(i, _)| i).collect();
    let mut used = msg_chars(system);
    let mut start = clipped.len();
    // Walk turns from the newest; always keep the newest turn even if it alone is over budget.
    for (k, &ts) in turn_starts.iter().enumerate().rev() {
        let end = turn_starts.get(k + 1).copied().unwrap_or(clipped.len());
        let size: usize = clipped[ts..end].iter().map(msg_chars).sum();
        if used + size > budget && start != clipped.len() {
            break;
        }
        used += size;
        start = ts;
    }
    if turn_starts.is_empty() {
        start = 0;
    }
    let mut out = vec![system.clone()];
    out.extend_from_slice(&clipped[start..]);
    (out, start)
}

impl Agent {
    pub fn new(llm: Arc<LemonadeModule>, ctx: ToolCtx, cfg: AgentConfig) -> Self {
        Self::with_tools(llm, ctx, cfg, Registry::standard())
    }

    pub fn with_tools(llm: Arc<LemonadeModule>, ctx: ToolCtx, cfg: AgentConfig, tools: Registry) -> Self {
        let b = cfg.context_chars;
        Agent { llm, tools, ctx, cfg, learned_budget: AtomicUsize::new(b) }
    }

    async fn note_text(&self, path: &str) -> String {
        note_by_path(&self.ctx.vault, path).await.ok().flatten().map(|n| strip_html_comments(&n.content).trim().to_string()).unwrap_or_default()
    }

    /// The `memory_k` bullets to show when there are more: hybrid (keywords + meaning) search on
    /// the recent user messages when an embedding endpoint is configured, else (or if that finds
    /// nothing) the word-overlap ranking.
    async fn rank_memory(&self, note: &str, bullets: &[String], query: &str) -> Vec<String> {
        let k = self.cfg.memory_k;
        let m = &self.ctx.memory;
        if m.semantic_on() {
            if m.sync_memory(note, bullets).await.is_err() {
                // Endpoint down: the bullets are still synced and keyword search still works.
            }
            if let Ok(out) = m.search(query, "hybrid", &["memory".to_string()], k).await {
                let picked: Vec<String> = out.hits.into_iter().map(|h| h.text).collect();
                if !picked.is_empty() {
                    return picked;
                }
            }
        }
        let words: Vec<String> = query.split_whitespace().filter(|w| w.len() > 2).map(|w| w.to_lowercase()).collect();
        let mut scored: Vec<(usize, &String)> = bullets.iter().map(|b| (words.iter().filter(|w| b.to_lowercase().contains(w.as_str())).count(), b)).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(k).map(|(_, b)| b.clone()).collect()
    }

    /// The system prompt for this caller (see the module doc).
    pub async fn system_prompt(&self, owner: bool, last_user: &str) -> String {
        let system_md = self.note_text("Jade/SYSTEM.md").await;
        let mut p = if owner && !system_md.is_empty() { system_md } else { BASE_INTRO.to_string() };
        let soul = self.note_text("Jade/SOUL.md").await;
        if !soul.is_empty() {
            p.push_str(&format!("\n\n## Persona (SOUL.md)\n{soul}"));
        }
        if owner {
            for path in ["AGENTS.md", "Jade/AGENTS.md"] {
                let a = self.note_text(path).await;
                if !a.is_empty() {
                    p.push_str(&format!("\n\n## Standing instructions ({path})\n{a}"));
                }
            }
            let memory_text = self.note_text(MEMORY_PATH).await;
            let bullets = memory_bullets(&memory_text);
            if !bullets.is_empty() {
                let picked: Vec<String> = if bullets.len() <= self.cfg.memory_k { bullets.clone() } else { self.rank_memory(&memory_text, &bullets, last_user).await };
                p.push_str("\n\n## What you remember about the user (Jade/Memory.md)\n");
                p.push_str(&picked.iter().map(|b| format!("- {b}")).collect::<Vec<_>>().join("\n"));
            }
            if let Ok(skills) = list_skills(&self.ctx.vault).await {
                if !skills.is_empty() {
                    p.push_str("\n\n## Skills (load one with the skill tool when it fits)\n");
                    p.push_str(&skills.iter().map(|(n, d, _)| format!("- {n}: {d}")).collect::<Vec<_>>().join("\n"));
                }
            }
        }
        let now = chrono::Local::now();
        p.push_str(&format!("\n\nNow: {} ({}).", now.format("%A %Y-%m-%d %H:%M"), now.format("%Z %:z")));
        p
    }

    fn delegate_schema() -> Value {
        json!({"type":"function","function":{"name":"delegate","description":"Hand a side job (research, reading many notes or pages) to a helper with read-only tools in its own clean context. It returns one report; its pages and search results never fill your context.","parameters":{"type":"object","properties":{"task":{"type":"string","description":"what the helper must find out and report"}},"required":["task"]}}})
    }

    /// One model call with the context guard and the learned budget.
    async fn call_model(&self, system: &Value, history: &[Value], tools: &Value) -> Result<(nautik9_lemonade::RawAnswer, usize), ChatFailure> {
        let mut budget = self.learned_budget.load(Ordering::Relaxed);
        for attempt in 0..3 {
            // Tool results shrink with the budget too: the newest turn is always kept, so on a
            // small-context model (4k on a phone) its page text is what has to give.
            let tool_cap = self.cfg.max_tool_chars.min(budget / 4).max(800);
            let (msgs, trimmed) = fit_context(system, history, budget, tool_cap);
            match self.llm.chat_messages(&msgs, Some(tools), Some(self.cfg.max_tokens), false).await {
                Ok(a) => {
                    self.ctx.store.log_trajectory(&json!({"ts": chrono::Local::now().to_rfc3339(), "model": a.model, "provider": a.provider, "messages": msgs, "tools": tools, "response": a.message, "finish_reason": a.finish_reason}));
                    return Ok((a, trimmed));
                }
                Err(f) if attempt < 2 && f.attempts.iter().any(|x| x.to_lowercase().contains("context")) => {
                    budget /= 2;
                    self.learned_budget.store(budget, Ordering::Relaxed);
                }
                Err(f) => return Err(f),
            }
        }
        unreachable!()
    }

    /// Run one tool call; returns (tool message content, trace).
    async fn run_tool(&self, call: &Value, owner: bool, read_only: bool) -> (String, ToolTrace) {
        let name = call["function"]["name"].as_str().unwrap_or("").to_string();
        let raw = call["function"]["arguments"].clone();
        let args: Value = match &raw {
            Value::String(s) if s.trim().is_empty() => json!({}),
            Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            v => v.clone(),
        };
        let shown = serde_json::from_str::<Value>(&redact(&args.to_string(), &self.ctx.secrets)).unwrap_or(Value::Null);
        let res: Result<String, String> = if args.is_null() {
            Err(format!("arguments are not valid JSON: {raw}"))
        } else if name == "delegate" && owner && !read_only {
            match args["task"].as_str() {
                Some(task) => Box::pin(self.delegate(task)).await,
                None => Err("missing string argument `task`".into()),
            }
        } else {
            match self.tools.allowed(&name, owner, read_only) {
                Some(t) => t.call(&args, &self.ctx).await,
                None => Err(format!("no tool `{name}` is available here")),
            }
        };
        let (ok, text) = match res {
            Ok(t) => (true, t),
            Err(e) => (false, format!("ERROR: {e}")),
        };
        let text = redact(&text, &self.ctx.secrets);
        let preview: String = text.chars().take(300).collect();
        (text, ToolTrace { name, arguments: shown, ok, result_preview: preview })
    }

    /// Model + tools until a final answer or the step limit. Appends to `history`.
    async fn run_loop(&self, system: &Value, history: &mut Vec<Value>, owner: bool, read_only: bool, max_steps: usize) -> Result<(Option<String>, Option<String>, nautik9_lemonade::RawAnswer, usize, Vec<ToolTrace>, usize), ChatFailure> {
        let extra = if owner && !read_only { vec![Self::delegate_schema()] } else { vec![] };
        let tools = self.tools.schema(owner, read_only, &extra);
        let mut traces = Vec::new();
        let mut last = None;
        let mut trimmed = 0;
        for step in 1..=max_steps {
            let (ans, t) = self.call_model(system, history, &tools).await?;
            trimmed = t;
            let mut msg = ans.message.clone();
            msg.as_object_mut().map(|o| o.retain(|k, _| ["role", "content", "tool_calls"].contains(&k.as_str())));
            msg["role"] = json!("assistant");
            history.push(msg.clone());
            let calls: Vec<Value> = msg["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                let text = visible_text(msg["content"].as_str().unwrap_or(""));
                return Ok((Some(text), None, ans, step, traces, trimmed));
            }
            // Parallel-safe calls asked for together run at the same time; others in order.
            let all_parallel = calls.iter().all(|c| self.tools.get(c["function"]["name"].as_str().unwrap_or("")).is_some_and(|t| t.parallel()));
            let results: Vec<(String, ToolTrace)> = if all_parallel && calls.len() > 1 {
                join_all(calls.iter().map(|c| self.run_tool(c, owner, read_only))).await
            } else {
                let mut v = Vec::new();
                for c in &calls {
                    v.push(self.run_tool(c, owner, read_only).await);
                }
                v
            };
            for (c, (text, trace)) in calls.iter().zip(results) {
                history.push(json!({"role": "tool", "tool_call_id": c["id"], "name": trace.name, "content": text}));
                traces.push(trace);
            }
            last = Some(ans);
        }
        let ans = last.expect("max_steps >= 1");
        Ok((None, Some(format!("stopped after {max_steps} model calls without a final answer; the tool results so far are in the chat")), ans, max_steps, traces, trimmed))
    }

    /// `delegate`: a sub-agent with read-only tools and its own context; returns its report.
    pub async fn delegate(&self, task: &str) -> Result<String, String> {
        let system = json!({"role":"system","content": format!("{BASE_INTRO}\n\nYou are a helper on a side job. Use your read-only tools, then answer with ONE concise report of what you found, with note paths or URLs as sources. Say plainly what you could not find.\n\nNow: {}.", chrono::Local::now().format("%Y-%m-%d %H:%M %Z"))});
        let mut h = vec![json!({"role":"user","content": task})];
        match self.run_loop(&system, &mut h, true, true, self.cfg.delegate_steps).await {
            Ok((Some(r), _, a, steps, traces, _)) => Ok(format!("{r}\n\n[helper: {} via {}, {} steps, {} tool calls]", a.model, a.provider, steps, traces.len())),
            Ok((None, stopped, ..)) => Err(format!("helper {}", stopped.unwrap_or_default())),
            Err(f) => Err(format!("helper could not reach a model: {} ({})", f.error, f.attempts.join("; "))),
        }
    }

    /// One user turn in a session (created when `session` is None or unknown).
    pub async fn turn(&self, session: Option<&str>, user_text: &str, owner: bool) -> Result<TurnResult, ChatFailure> {
        let store = &self.ctx.store;
        let err = |e: String| ChatFailure { error: e, attempts: vec![] };
        let sid = match session {
            Some(s) if store.exists(s).await.map_err(err)? => s.to_string(),
            _ => store.create_session(&user_text.chars().take(60).collect::<String>()).await.map_err(err)?,
        };
        let mut history: Vec<Value> = store.messages(&sid).await.map_err(err)?.into_iter().map(|m| m.message).collect();
        let before = history.len();
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
        history.push(json!({"role":"user","content": format!("[{stamp}] {user_text}")}));
        // Memory ranking looks at the last two user messages (the one before this turn's, then this one).
        let prev_user = history[..before].iter().rev().find(|m| m["role"] == "user").and_then(|m| m["content"].as_str()).map(|t| if t.starts_with('[') { t.split_once("] ").map(|x| x.1).unwrap_or(t) } else { t }.to_string());
        let query = match prev_user {
            Some(p) => format!("{p}\n{user_text}"),
            None => user_text.to_string(),
        };
        let system = json!({"role":"system","content": self.system_prompt(owner, &query).await});
        let out = self.run_loop(&system, &mut history, owner, false, self.cfg.max_steps).await;
        // Persist what happened, even when the model failed after some tool calls.
        let first_idx = store.append(&sid, &history[before..]).await.map_err(err)?;
        let (reply, stopped, ans, steps, tool_calls, trimmed) = out?;
        Ok(TurnResult { session_id: sid, reply, stopped, model: ans.model, provider: ans.provider, endpoint: ans.endpoint, steps, tool_calls, trimmed, first_idx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_guard_keeps_whole_newest_turns() {
        let sys = json!({"role":"system","content":"s"});
        let big = "x".repeat(400);
        let h = vec![
            json!({"role":"user","content": big}),
            json!({"role":"assistant","content":"a1","tool_calls":[{"id":"1","function":{"name":"read","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"1","content": big}),
            json!({"role":"assistant","content":"a1 done"}),
            json!({"role":"user","content":"second"}),
            json!({"role":"assistant","content":"a2"}),
        ];
        let (out, start) = fit_context(&sys, &h, 300, 10_000);
        assert_eq!(start, 4, "the first turn (user..tool..assistant) is dropped whole");
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["content"], "second");
        let (out, start) = fit_context(&sys, &h, 100_000, 50);
        assert_eq!(start, 0);
        assert!(out[3]["content"].as_str().unwrap().contains("cut to 50 characters"));
    }

    #[test]
    fn think_blocks_hidden_and_comments_stripped() {
        assert_eq!(visible_text("<think>plan</think>\nAnswer"), "Answer");
        assert_eq!(strip_html_comments("a<!-- hidden -->b"), "ab");
    }
}
