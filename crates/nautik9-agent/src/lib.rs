//! NautiK9 agent: Jade Vault's agent abilities, rewritten in Rust for the NautiK9 server.
//!
//! Design follows Jade Vault (MIT, lucky-edgelord/jade-vault v2.20.0) docs for intent; no Jade
//! source is copied. Vault paths keep Jade's names (`Jade/Memory.md`, `Jade/SOUL.md`,
//! `AGENTS.md`, `Jade/SYSTEM.md`, `Jade/skills/<name>/SKILL.md`); in NautiK9 a note's path is
//! its title.
//!
//! - [`store`]: chat sessions and the trajectory log (SQLite + JSONL).
//! - [`tools`]: the tool registry and the tools (notes, memory, web, http, chat history, skills).
//! - [`agent`]: steering, context guard and the tool-calling loop (with `delegate`).
//! - [`schedule`]: cron-style schedules, `Jade/crontab`, the heartbeat and their runner.
//! - [`html2md`]: page HTML to Markdown for `web_fetch`.

pub mod agent;
pub mod html2md;
pub mod schedule;
pub mod store;
pub mod tools;

pub use agent::{Agent, AgentConfig, TurnResult};
