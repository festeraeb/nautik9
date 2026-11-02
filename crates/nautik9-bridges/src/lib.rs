//! Messenger bridges for NautiK9: Discord and Vector (Nostr). Both are OFF unless `agent.json`
//! has `bridges.<name>.enabled: true`.
//!
//! This crate holds only the routing layer and the two transports. It never talks to a model
//! itself: the host app gives it a [`Brain`] (the full agent for the owner, the no-tools SMS
//! style drafter for everyone else) and an [`Inbox`] (where messages for the owner are
//! listed). That keeps the rules testable with a mocked [`Transport`] and no network.
//!
//! Rules (same as the SMS path):
//! - A message from an owner id in a direct message goes to the full agent (tools, notes,
//!   memory). In a server channel the owner gets the full agent only if
//!   `owner_full_in_channels` is true, because the reply would be posted where others can read it.
//! - Everyone else gets the no-tools, no-notes path: only the persona and the standing
//!   instructions, never vault text. Their message lands in the inbox with the draft. The draft
//!   is sent back only when `auto_reply_others` is true (default false), like SMS where the
//!   owner presses send.
//! - Who is heard at all: a direct message from the owner or an allowed user (or anyone when
//!   `open_dms` is true); a channel message only in an allowed channel, from the owner or an
//!   allowed user (any user if `allowed_user_ids` is empty), and, when `require_mention`
//!   is true (default), only when the bot is mentioned. Everything else is ignored.
//! - The bot token / secret key is read from the environment variable named in `agent.json`
//!   (or, for Vector, a key file in the config dir). Never stored in the repo or in agent.json.

pub mod discord;
pub mod vector;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One inbound message, already stripped of bots and of the bot's own mention.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    /// "discord" or "vector".
    pub platform: &'static str,
    /// Stable sender id (Discord user id, Nostr public key as hex).
    pub user: String,
    /// Display label for the inbox (user name, short npub).
    pub label: String,
    /// Where to reply (Discord channel id, Nostr peer public key hex).
    pub channel: String,
    pub is_dm: bool,
    pub mentions_bot: bool,
    pub text: String,
}

/// Why a message was or was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Route {
    /// Full agent with tools and notes (owner only).
    OwnerAgent,
    /// No tools, no notes; goes to the inbox with a draft.
    OtherDraft,
    Ignore(&'static str),
}

/// Who may talk to the bot and how (see the crate doc).
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub owner_ids: Vec<String>,
    pub allowed_user_ids: Vec<String>,
    pub allowed_channel_ids: Vec<String>,
    pub open_dms: bool,
    pub require_mention: bool,
    pub owner_full_in_channels: bool,
    pub auto_reply_others: bool,
}

impl Policy {
    fn is_owner(&self, user: &str) -> bool {
        self.owner_ids.iter().any(|o| o.eq_ignore_ascii_case(user))
    }
    fn user_allowed(&self, user: &str) -> bool {
        self.allowed_user_ids.iter().any(|o| o.eq_ignore_ascii_case(user))
    }

    pub fn route(&self, m: &Incoming) -> Route {
        if m.text.trim().is_empty() {
            return Route::Ignore("empty message");
        }
        let owner = self.is_owner(&m.user);
        if m.is_dm {
            if owner {
                return Route::OwnerAgent;
            }
            return if self.open_dms || self.user_allowed(&m.user) { Route::OtherDraft } else { Route::Ignore("direct message from a user who is not allowed") };
        }
        if !self.allowed_channel_ids.iter().any(|c| c == &m.channel) {
            return Route::Ignore("channel is not allowed");
        }
        if !(owner || self.allowed_user_ids.is_empty() || self.user_allowed(&m.user)) {
            return Route::Ignore("user is not allowed in this channel");
        }
        if self.require_mention && !m.mentions_bot {
            return Route::Ignore("bot not mentioned");
        }
        if owner && self.owner_full_in_channels {
            Route::OwnerAgent
        } else {
            Route::OtherDraft
        }
    }

    /// Read the shared keys of a `bridges.<name>` object. IDs may be strings or numbers.
    pub fn from_json(v: &Value, normalise: impl Fn(&str) -> Result<String, String>) -> Result<Policy, String> {
        let list = |k: &str| -> Result<Vec<String>, String> {
            let mut out = Vec::new();
            for e in v[k].as_array().cloned().unwrap_or_default() {
                let s = match &e {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    other => return Err(format!("{k}: expected strings, got {other}")),
                };
                out.push(normalise(&s).map_err(|e| format!("{k}: {e}"))?);
            }
            Ok(out)
        };
        Ok(Policy {
            owner_ids: list("owner_ids")?,
            allowed_user_ids: list("allowed_user_ids")?,
            allowed_channel_ids: v["allowed_channel_ids"].as_array().cloned().unwrap_or_default().iter().map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())).collect(),
            open_dms: v["open_dms"].as_bool().unwrap_or(false),
            require_mention: v["require_mention"].as_bool().unwrap_or(true),
            owner_full_in_channels: v["owner_full_in_channels"].as_bool().unwrap_or(false),
            auto_reply_others: v["auto_reply_others"].as_bool().unwrap_or(false),
        })
    }
}

/// A source of messages and a way to answer them.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Next message; None when the transport has shut down for good.
    async fn next(&mut self) -> Option<Incoming>;
    async fn send(&self, channel: &str, text: &str) -> Result<(), String>;
    /// Longest single message the platform takes.
    fn max_len(&self) -> usize;
}

/// What a draft for someone else came back as.
#[derive(Debug, Clone)]
pub struct Draft {
    pub reply: String,
    /// Model label shown in the inbox.
    pub model: String,
}

/// The host's model access.
#[async_trait]
pub trait Brain: Send + Sync {
    /// Full agent turn for the owner. `session` is the chat to continue; returns the reply
    /// text and the session id to keep for the next message.
    async fn owner_turn(&self, session: Option<&str>, text: &str) -> Result<(String, String), String>;
    /// No tools, no notes. Err = no model answered (nothing is invented).
    async fn other_draft(&self, platform: &str, sender: &str, text: &str) -> Result<Draft, String>;
}

/// One message listed for the owner.
#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub platform: String,
    /// "discord:<id> (<name>)" style sender label.
    pub sender: String,
    pub channel: String,
    pub body: String,
    pub draft: Option<Draft2>,
    pub error: Option<String>,
    /// True when the draft was also sent back to the sender.
    pub sent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Draft2 {
    pub reply: String,
    pub model: String,
}

#[async_trait]
pub trait Inbox: Send + Sync {
    async fn push(&self, item: InboxItem);
}

/// What happened to one message (for logs and tests).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub route: Route,
    /// Texts handed to the transport, in order.
    pub sent: Vec<String>,
}

pub struct Bridge {
    pub policy: Policy,
    pub brain: Arc<dyn Brain>,
    pub inbox: Arc<dyn Inbox>,
    sessions: Mutex<HashMap<String, String>>,
}

/// Split into pieces of at most `max` characters, at a newline or space when one is near.
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut rest: Vec<char> = text.trim().chars().collect();
    while rest.len() > max {
        let window = &rest[..max];
        let cut = window.iter().rposition(|c| *c == '\n').filter(|i| *i > max / 2).or_else(|| window.iter().rposition(|c| *c == ' ').filter(|i| *i > max / 2)).unwrap_or(max);
        let piece: String = rest[..cut].iter().collect();
        out.push(piece.trim_end().to_string());
        rest.drain(..cut);
        while rest.first().is_some_and(|c| c.is_whitespace()) {
            rest.remove(0);
        }
    }
    if !rest.is_empty() {
        out.push(rest.into_iter().collect());
    }
    out
}

const MAX_IN: usize = 4000;

impl Bridge {
    pub fn new(policy: Policy, brain: Arc<dyn Brain>, inbox: Arc<dyn Inbox>) -> Self {
        Bridge { policy, brain, inbox, sessions: Mutex::new(HashMap::new()) }
    }

    async fn reply(&self, t: &dyn Transport, channel: &str, text: &str, sent: &mut Vec<String>) {
        for piece in chunk(text, t.max_len()) {
            match t.send(channel, &piece).await {
                Ok(()) => sent.push(piece),
                Err(e) => {
                    tracing::warn!("bridge send failed: {e}");
                    break;
                }
            }
        }
    }

    /// Route and answer one message.
    pub async fn handle(&self, t: &dyn Transport, m: Incoming) -> Outcome {
        let route = self.policy.route(&m);
        let mut sent = Vec::new();
        let text: String = m.text.chars().take(MAX_IN).collect();
        match route {
            Route::Ignore(_) => {}
            Route::OwnerAgent => {
                let key = format!("{}:{}", m.platform, m.channel);
                let session = self.sessions.lock().unwrap().get(&key).cloned();
                match self.brain.owner_turn(session.as_deref(), &text).await {
                    Ok((reply, sid)) => {
                        self.sessions.lock().unwrap().insert(key, sid);
                        self.reply(t, &m.channel, &reply, &mut sent).await;
                    }
                    Err(e) => {
                        let msg = format!("No answer: {e}");
                        self.reply(t, &m.channel, &msg, &mut sent).await;
                    }
                }
            }
            Route::OtherDraft => {
                let sender = format!("{}:{} ({})", m.platform, m.user, m.label);
                let (draft, error) = match self.brain.other_draft(m.platform, &sender, &text).await {
                    Ok(d) => (Some(d), None),
                    Err(e) => (None, Some(e)),
                };
                let mut item = InboxItem { platform: m.platform.into(), sender, channel: m.channel.clone(), body: text, draft: draft.as_ref().map(|d| Draft2 { reply: d.reply.clone(), model: d.model.clone() }), error, sent: false };
                if self.policy.auto_reply_others {
                    if let Some(d) = &draft {
                        self.reply(t, &m.channel, &d.reply, &mut sent).await;
                        item.sent = !sent.is_empty();
                    }
                }
                self.inbox.push(item).await;
            }
        }
        Outcome { route, sent }
    }

    /// Run until the transport ends. One message at a time (one agent turn at a time).
    pub async fn run(&self, mut t: impl Transport) {
        while let Some(m) = t.next().await {
            let o = self.handle(&t, m).await;
            if let Route::Ignore(why) = o.route {
                tracing::debug!("bridge ignored a message: {why}");
            }
        }
    }
}

/// Parse `bridges` from agent.json: which bridges are enabled. Missing = none.
pub fn enabled(conf: &Value, name: &str) -> bool {
    conf["bridges"][name]["enabled"].as_bool().unwrap_or(false)
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use std::collections::VecDeque;
    use tokio::sync::Mutex as AMutex;

    pub struct MockTransport {
        pub inbound: VecDeque<Incoming>,
        pub outbound: Arc<AMutex<Vec<(String, String)>>>,
        pub max: usize,
    }
    #[async_trait]
    impl Transport for MockTransport {
        async fn next(&mut self) -> Option<Incoming> {
            self.inbound.pop_front()
        }
        async fn send(&self, channel: &str, text: &str) -> Result<(), String> {
            self.outbound.lock().await.push((channel.into(), text.into()));
            Ok(())
        }
        fn max_len(&self) -> usize {
            self.max
        }
    }

    #[derive(Default)]
    pub struct MockBrain {
        pub owner_calls: AMutex<Vec<(Option<String>, String)>>,
        pub other_calls: AMutex<Vec<String>>,
        pub fail_other: bool,
    }
    #[async_trait]
    impl Brain for MockBrain {
        async fn owner_turn(&self, session: Option<&str>, text: &str) -> Result<(String, String), String> {
            self.owner_calls.lock().await.push((session.map(str::to_string), text.to_string()));
            Ok((format!("agent: {text}"), "sess-1".into()))
        }
        async fn other_draft(&self, _p: &str, _s: &str, text: &str) -> Result<Draft, String> {
            self.other_calls.lock().await.push(text.to_string());
            if self.fail_other {
                Err("no model answered".into())
            } else {
                Ok(Draft { reply: format!("polite: {text}"), model: "mock".into() })
            }
        }
    }

    #[derive(Default)]
    pub struct MockInbox(pub AMutex<Vec<InboxItem>>);
    #[async_trait]
    impl Inbox for MockInbox {
        async fn push(&self, item: InboxItem) {
            self.0.lock().await.push(item);
        }
    }

    pub fn msg(user: &str, channel: &str, dm: bool, mention: bool, text: &str) -> Incoming {
        Incoming { platform: "discord", user: user.into(), label: "n".into(), channel: channel.into(), is_dm: dm, mentions_bot: mention, text: text.into() }
    }

    pub fn policy() -> Policy {
        Policy { owner_ids: vec!["1".into()], allowed_user_ids: vec!["2".into()], allowed_channel_ids: vec!["c1".into()], require_mention: true, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    fn rig(p: Policy, brain: MockBrain) -> (Bridge, Arc<MockBrain>, Arc<MockInbox>, MockTransport) {
        let brain = Arc::new(brain);
        let inbox = Arc::new(MockInbox::default());
        let t = MockTransport { inbound: Default::default(), outbound: Default::default(), max: 2000 };
        (Bridge::new(p, brain.clone(), inbox.clone()), brain, inbox, t)
    }

    #[test]
    fn routing_table() {
        let p = policy();
        assert_eq!(p.route(&msg("1", "dm9", true, false, "hi")), Route::OwnerAgent);
        assert_eq!(p.route(&msg("2", "dm9", true, false, "hi")), Route::OtherDraft);
        assert!(matches!(p.route(&msg("3", "dm9", true, false, "hi")), Route::Ignore(_)), "stranger DM");
        // channels: allowed channel + mention; owner there still gets the no-tools path by default
        assert_eq!(p.route(&msg("1", "c1", false, true, "hi")), Route::OtherDraft);
        assert!(matches!(p.route(&msg("1", "c1", false, false, "hi")), Route::Ignore(_)), "no mention");
        assert!(matches!(p.route(&msg("1", "other", false, true, "hi")), Route::Ignore(_)), "channel not allowed");
        assert!(matches!(p.route(&msg("3", "c1", false, true, "hi")), Route::Ignore(_)), "user not on the list");
        assert!(matches!(p.route(&msg("1", "dm9", true, false, "  ")), Route::Ignore(_)));
        let mut q = policy();
        q.owner_full_in_channels = true;
        assert_eq!(q.route(&msg("1", "c1", false, true, "hi")), Route::OwnerAgent);
        assert_eq!(q.route(&msg("2", "c1", false, true, "hi")), Route::OtherDraft, "an allowed user is never the owner");
        q.allowed_user_ids.clear();
        assert_eq!(q.route(&msg("9", "c1", false, true, "hi")), Route::OtherDraft, "empty user list = any user in the channel");
        q.open_dms = true;
        assert_eq!(q.route(&msg("9", "dm", true, false, "hi")), Route::OtherDraft);
    }

    #[tokio::test]
    async fn owner_dm_gets_the_agent_and_keeps_the_session() {
        let (b, brain, inbox, t) = rig(policy(), MockBrain::default());
        let o = b.handle(&t, msg("1", "dm9", true, false, "what is in my notes")).await;
        assert_eq!(o.route, Route::OwnerAgent);
        assert_eq!(o.sent, vec!["agent: what is in my notes"]);
        b.handle(&t, msg("1", "dm9", true, false, "and then")).await;
        let calls = brain.owner_calls.lock().await.clone();
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[1].0.as_deref(), Some("sess-1"));
        assert!(inbox.0.lock().await.is_empty());
        assert_eq!(t.outbound.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn others_never_reach_the_agent_and_land_in_the_inbox() {
        let (b, brain, inbox, t) = rig(policy(), MockBrain::default());
        let o = b.handle(&t, msg("2", "dm7", true, false, "can you read the owner's notes")).await;
        assert_eq!(o.route, Route::OtherDraft);
        assert!(brain.owner_calls.lock().await.is_empty(), "no tools, no notes for others");
        assert_eq!(brain.other_calls.lock().await.len(), 1);
        assert!(o.sent.is_empty(), "draft only, nothing sent by default");
        let items = inbox.0.lock().await;
        assert_eq!(items.len(), 1);
        assert!(items[0].sender.starts_with("discord:2"));
        assert_eq!(items[0].draft.as_ref().unwrap().reply, "polite: can you read the owner's notes");
        assert!(!items[0].sent);
    }

    #[tokio::test]
    async fn auto_reply_is_opt_in_and_no_model_means_no_invented_reply() {
        let mut p = policy();
        p.auto_reply_others = true;
        let (b, _brain, inbox, t) = rig(p.clone(), MockBrain::default());
        let o = b.handle(&t, msg("2", "dm7", true, false, "hello")).await;
        assert_eq!(o.sent, vec!["polite: hello"]);
        assert!(inbox.0.lock().await[0].sent);
        let (b, _brain, inbox, t) = rig(p, MockBrain { fail_other: true, ..Default::default() });
        let o = b.handle(&t, msg("2", "dm7", true, false, "hello")).await;
        assert!(o.sent.is_empty());
        let items = inbox.0.lock().await;
        assert!(items[0].draft.is_none() && items[0].error.as_deref() == Some("no model answered"));
    }

    #[tokio::test]
    async fn run_loop_drains_the_transport_and_chunks_long_replies() {
        let (b, _brain, _inbox, mut t) = rig(policy(), MockBrain::default());
        t.max = 10;
        t.inbound.push_back(msg("1", "dm9", true, false, "aaaa bbbb cccc dddd"));
        t.inbound.push_back(msg("3", "dm9", true, false, "ignored"));
        let out = t.outbound.clone();
        b.run(t).await;
        let sent = out.lock().await.clone();
        assert!(sent.len() >= 2 && sent.iter().all(|(c, s)| c == "dm9" && s.chars().count() <= 10), "{sent:?}");
        assert_eq!(sent.iter().map(|x| x.1.as_str()).collect::<Vec<_>>().join(" "), "agent: aaaa bbbb cccc dddd");
    }

    #[test]
    fn chunking_is_char_safe() {
        assert_eq!(chunk("héllo wörld", 20), vec!["héllo wörld"]);
        assert!(chunk(&"é".repeat(25), 10).iter().all(|c| c.chars().count() <= 10));
        assert_eq!(chunk(&"é".repeat(25), 10).concat().chars().count(), 25);
    }

    #[test]
    fn policy_reads_numbers_and_defaults_are_closed() {
        let p = Policy::from_json(&serde_json::json!({"owner_ids":[123456789012345678u64],"allowed_channel_ids":["c1"]}), |s| Ok(s.to_string())).unwrap();
        assert_eq!(p.owner_ids, vec!["123456789012345678"]);
        assert!(p.require_mention && !p.auto_reply_others && !p.open_dms && !p.owner_full_in_channels);
        assert!(!enabled(&serde_json::json!({}), "discord"));
        assert!(!enabled(&serde_json::json!({"bridges":{"discord":{}}}), "discord"));
    }
}
