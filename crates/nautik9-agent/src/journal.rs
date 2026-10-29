//! The journal: Jade's daily notes that summarise earlier chats, rewritten for NautiK9.
//!
//! `Agent::journal(date)` (default yesterday) takes the chats stored on that day, asks the model
//! for one short summary per chat from the stored user and assistant messages only, and writes
//! `Jade/Journal/YYYY-MM-DD.md` (one `##` entry per chat, with its session id). `Jade/Journal.md`
//! links the day. If any chat cannot be summarised (no model answers), nothing is written and
//! the error is returned. The entries are also put in the `journal` search collection.

use serde_json::{json, Value};

use crate::agent::{visible_text, Agent};
use crate::tools::write_note;

pub const JOURNAL_INDEX: &str = "Jade/Journal.md";

pub fn journal_path(date: &str) -> String {
    format!("Jade/Journal/{date}.md")
}

const JOURNAL_SYSTEM: &str = "You write one journal entry for a private notebook. Summarise the chat below in 3 to 6 plain sentences, past tense: what was asked or decided, facts learned about the user, and anything left open. Use only what is in the chat. Do not add facts, advice or guesses. Reply with the entry text only.";

fn transcript(msgs: &[(String, String)], max: usize) -> String {
    let full: String = msgs.iter().map(|(r, t)| format!("{}: {}\n\n", if r == "user" { "User" } else { "Assistant" }, t)).collect();
    if full.chars().count() <= max {
        return full;
    }
    let head: String = full.chars().take(max / 2).collect();
    let tail: String = full.chars().rev().take(max / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n[... middle of the chat left out ...]\n{tail}")
}

impl Agent {
    /// Write the journal for `date` (YYYY-MM-DD; None = yesterday). Returns a JSON report.
    pub async fn journal(&self, date: Option<&str>) -> Result<Value, String> {
        let date = match date {
            Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| format!("date `{d}` is not YYYY-MM-DD"))?,
            None => chrono::Local::now().date_naive().pred_opt().ok_or("no yesterday")?,
        }
        .format("%Y-%m-%d")
        .to_string();
        let chats = self.ctx.store.day_chats(&date).await?;
        let chats: Vec<_> = chats.into_iter().filter(|c| c.messages.iter().any(|(r, _)| r == "user")).collect();
        if chats.is_empty() {
            return Ok(json!({"date": date, "entries": 0, "written": false, "message": format!("no chats on {date}; nothing written")}));
        }
        let mut entries = Vec::new();
        let mut models = Vec::new();
        for c in &chats {
            let msgs = vec![
                json!({"role": "system", "content": JOURNAL_SYSTEM}),
                json!({"role": "user", "content": format!("Chat titled \"{}\":\n\n{}", c.title, transcript(&c.messages.iter().map(|(r, t)| (r.clone(), visible_text(t))).collect::<Vec<_>>(), 12_000))}),
            ];
            let ans = self.llm.chat_messages(&msgs, None, Some(400), false).await.map_err(|f| format!("journal not written, no model answered: {} ({})", f.error, f.attempts.join("; ")))?;
            let text = visible_text(ans.message["content"].as_str().unwrap_or(""));
            if text.is_empty() {
                return Err(format!("journal not written: the model returned an empty summary for session {}", c.id));
            }
            if !models.contains(&ans.model) {
                models.push(ans.model.clone());
            }
            entries.push((c, text));
        }
        let path = journal_path(&date);
        let mut body = format!("# Journal {date}\n\nSummaries written by the model from the stored chats of the day (session ids below); not a transcript.\n");
        for (c, text) in &entries {
            body.push_str(&format!("\n## {} (session {})\n\n{}\n", c.title.replace('\n', " "), c.id, text));
        }
        write_note(&self.ctx.vault, &path, &body).await?;
        let index = crate::tools::note_by_path(&self.ctx.vault, JOURNAL_INDEX).await?.map(|n| n.content).unwrap_or_else(|| "# Journal\n".to_string());
        let link = format!("[[{path}]]");
        let linked = !index.contains(&link);
        if linked {
            write_note(&self.ctx.vault, JOURNAL_INDEX, &format!("{}\n- {date}: {link}\n", index.trim_end())).await?;
        }
        // Search collection: one chunk per chat, replaced when the day is regenerated.
        let items: Vec<(String, String)> = entries.iter().map(|(c, t)| (format!("{} (session {})", c.title.replace('\n', " "), c.id), t.clone())).collect();
        let (index_error, indexed) = match self.ctx.memory.replace_source("journal", &path, &items) {
            Ok(_) => match self.ctx.memory.embed_pending().await {
                Ok(_) => (None, true),
                Err(e) => (Some(e), true),
            },
            Err(e) => (Some(e), false),
        };
        Ok(json!({"date": date, "entries": entries.len(), "written": true, "note": path, "linked_in_index": linked, "models": models,
                  "search_index": {"indexed": indexed, "semantic": if self.ctx.memory.semantic_on() { "on" } else { "off: keyword only" }, "error": index_error}}))
    }
}
