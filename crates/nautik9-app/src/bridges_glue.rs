//! Wires the messenger bridges (`nautik9-bridges`) to this server: the full agent for the
//! owner, the SMS-style no-tools drafter for everyone else, and the Work SMS list as the inbox.
//! Both bridges are OFF unless `agent.json` enables them (see the crate docs for the keys).
//! A malformed `agent.json` or a missing token/key is printed and that bridge does not start;
//! it never stops the server.

use async_trait::async_trait;
use nautik9_agent::Agent;
use nautik9_bridges::{discord, vector, Bridge, Brain, Draft, Inbox, InboxItem};
use nautik9_core::sms::{SmsMessage, SmsStatus, WorkModeConfig};
use nautik9_lemonade::{LemonadeModule, SmsDraftRequest};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub struct AppBrain {
    agent: Arc<Agent>,
    lemonade: Arc<LemonadeModule>,
    work: Arc<RwLock<WorkModeConfig>>,
    /// One agent conversation at a time across all bridges.
    turn_lock: Arc<Mutex<()>>,
}

#[async_trait]
impl Brain for AppBrain {
    async fn owner_turn(&self, session: Option<&str>, text: &str) -> Result<(String, String), String> {
        let _g = self.turn_lock.lock().await;
        match self.agent.turn(session, text, true).await {
            Ok(t) => match t.reply {
                Some(r) if !r.trim().is_empty() => Ok((r, t.session_id)),
                Some(_) => Err("the model answered with nothing".into()),
                None => Err(t.stopped.unwrap_or_else(|| "stopped without an answer".into())),
            },
            Err(f) => Err(format!("{} ({})", f.error, f.attempts.join("; "))),
        }
    }

    /// The same prompt as an SMS from someone who is not the owner: no vault notes, no tools.
    async fn other_draft(&self, _platform: &str, sender: &str, text: &str) -> Result<Draft, String> {
        let cfg = self.work.read().await.clone();
        let req = SmsDraftRequest { owner_name: &cfg.owner_name, instructions: &cfg.custom_instructions, sender, body: text, is_owner: false, vault_context: None };
        match self.lemonade.draft_sms_reply(&req, cfg.model_target == "offline").await {
            Ok(a) => Ok(Draft { reply: a.reply.clone(), model: a.label() }),
            Err(f) => Err(format!("{}: {}", f.error, f.attempts.join("; "))),
        }
    }
}

/// Messages for the owner are listed with the Work SMS messages (sender "discord:<id> (name)").
pub struct SmsInbox(pub Arc<RwLock<Vec<SmsMessage>>>);

#[async_trait]
impl Inbox for SmsInbox {
    async fn push(&self, item: InboxItem) {
        let (status, draft_reply, reply_model, draft_error) = match (&item.draft, &item.error) {
            (Some(d), _) => (if item.sent { SmsStatus::Approved } else { SmsStatus::Drafted }, Some(d.reply.clone()), Some(d.model.clone()), None),
            (None, e) => (SmsStatus::NoModel, None, None, Some(e.clone().unwrap_or_else(|| "no draft".into()))),
        };
        let msg = SmsMessage {
            id: format!("{}-{}", item.platform, &uuid::Uuid::new_v4().to_string()[..8]),
            sender: item.sender,
            body: item.body,
            timestamp: chrono::Local::now().format("%I:%M %p").to_string(),
            draft_reply,
            draft_error,
            status,
            reply_model,
            latency_ms: None,
        };
        self.0.write().await.insert(0, msg);
    }
}

/// Start the enabled bridges. Returns one status line per bridge that is enabled or broken.
pub fn spawn(config_dir: &Path, agent: Arc<Agent>, lemonade: Arc<LemonadeModule>, work: Arc<RwLock<WorkModeConfig>>, sms: Arc<RwLock<Vec<SmsMessage>>>) -> Vec<String> {
    let conf: serde_json::Value = match std::fs::read_to_string(config_dir.join("agent.json")) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(v) => v,
            Err(e) => return vec![format!("agent.json: {e} (bridges not started)")],
        },
        Err(_) => return vec![],
    };
    let brain: Arc<dyn Brain> = Arc::new(AppBrain { agent, lemonade, work, turn_lock: Arc::new(Mutex::new(())) });
    let inbox: Arc<dyn Inbox> = Arc::new(SmsInbox(sms));
    let env = |k: &str| std::env::var(k).ok();
    let mut notes = Vec::new();
    match discord::DiscordConfig::from_agent_json(&conf, env) {
        Ok(Some(cfg)) => {
            let bridge = Bridge::new(cfg.policy.clone(), brain.clone(), inbox.clone());
            let t = discord::DiscordTransport::start(&cfg);
            tokio::spawn(async move { bridge.run(t).await });
            notes.push(format!("discord: running ({} owner id(s); replies to others are {})", cfg.policy.owner_ids.len(), if cfg.policy.auto_reply_others { "sent automatically" } else { "drafts in the inbox only" }));
        }
        Ok(None) => {}
        Err(e) => notes.push(format!("discord: NOT started: {e}")),
    }
    match vector::VectorConfig::from_agent_json(&conf, env, config_dir) {
        Ok(Some(cfg)) => {
            let bridge = Bridge::new(cfg.policy.clone(), brain, inbox);
            let t = vector::VectorTransport::start(&cfg);
            tokio::spawn(async move { bridge.run(t).await });
            notes.push(format!("vector: running as {} on {} relay(s)", cfg.npub(), cfg.relays.len()));
        }
        Ok(None) => {}
        Err(e) => notes.push(format!("vector: NOT started: {e}")),
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inbox_lists_drafts_and_no_model_honestly() {
        let list: Arc<RwLock<Vec<SmsMessage>>> = Default::default();
        let inbox = SmsInbox(list.clone());
        let item = |draft: Option<(&str, &str)>, err: Option<&str>, sent: bool| InboxItem {
            platform: "discord".into(),
            sender: "discord:2 (sam)".into(),
            channel: "c".into(),
            body: "hi".into(),
            draft: draft.map(|(r, m)| nautik9_bridges::Draft2 { reply: r.into(), model: m.into() }),
            error: err.map(str::to_string),
            sent,
        };
        inbox.push(item(Some(("hello", "m via p")), None, false)).await;
        inbox.push(item(None, Some("nothing answered"), false)).await;
        inbox.push(item(Some(("sent text", "m")), None, true)).await;
        let l = list.read().await;
        assert_eq!(l.len(), 3);
        assert_eq!((l[2].status, l[2].draft_reply.as_deref()), (SmsStatus::Drafted, Some("hello")));
        assert_eq!((l[1].status, l[1].draft_reply.is_none(), l[1].draft_error.as_deref()), (SmsStatus::NoModel, true, Some("nothing answered")));
        assert_eq!(l[0].status, SmsStatus::Approved);
        assert!(l[2].id.starts_with("discord-") && l[2].sender == "discord:2 (sam)");
    }
}
