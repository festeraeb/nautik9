//! Discord bot: Gateway (websocket, JSON) in, REST out. No Discord library; the parts of the
//! protocol used are small: HELLO, heartbeat, IDENTIFY, MESSAGE_CREATE, and one POST to send.
//!
//! `agent.json`:
//! ```json
//! "bridges": { "discord": {
//!   "enabled": true,
//!   "token_env": "NAUTIK9_DISCORD_TOKEN",
//!   "owner_ids": ["<your user id>"],
//!   "allowed_user_ids": [], "allowed_channel_ids": ["<channel id>"],
//!   "require_mention": true, "owner_full_in_channels": false, "auto_reply_others": false
//! } }
//! ```
//! The token is read from the environment variable named by `token_env`; it is never in the
//! file. Needs the Message Content intent (Developer Portal, Bot page) for ordinary channel
//! text; without it Discord closes with 4014 and the bridge reconnects without that intent
//! (direct messages and @mentions still arrive with text).
//! Not covered by a live test (no Discord account in the test suite): the protocol is tested
//! against a local mock gateway and a mock REST server.

use crate::{Incoming, Policy, Transport};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
pub const API: &str = "https://discord.com/api/v10";
const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
const INTENT_DIRECT_MESSAGES: u64 = 1 << 12;
const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;

#[derive(Clone)]
pub struct DiscordConfig {
    token: String,
    pub policy: Policy,
    pub gateway_url: String,
    pub api_base: String,
}

impl std::fmt::Debug for DiscordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordConfig").field("token", &"<hidden>").field("policy", &self.policy).finish()
    }
}

impl DiscordConfig {
    /// None when the bridge is not enabled. Err when enabled but unusable (no env var set).
    pub fn from_agent_json(conf: &Value, env: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        if !crate::enabled(conf, "discord") {
            return Ok(None);
        }
        let c = &conf["bridges"]["discord"];
        let var = c["token_env"].as_str().ok_or("bridges.discord: token_env (the NAME of the environment variable holding the bot token) is required")?;
        let token = env(var).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).ok_or_else(|| format!("bridges.discord: environment variable {var} is not set"))?;
        let policy = Policy::from_json(c, |s| if s.chars().all(|ch| ch.is_ascii_digit()) && !s.is_empty() { Ok(s.to_string()) } else { Err(format!("`{s}` is not a Discord numeric id")) })?;
        if policy.owner_ids.is_empty() {
            return Err("bridges.discord: owner_ids is empty; nobody could get the full agent".into());
        }
        Ok(Some(DiscordConfig { token, policy, gateway_url: GATEWAY.into(), api_base: API.into() }))
    }

    pub fn with_endpoints(mut self, gateway_url: &str, api_base: &str) -> Self {
        self.gateway_url = gateway_url.into();
        self.api_base = api_base.into();
        self
    }
}

pub struct DiscordTransport {
    rx: mpsc::Receiver<Incoming>,
    http: reqwest::Client,
    token: String,
    api_base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DiscordTransport {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DiscordTransport {
    /// Connect in the background (reconnects with back-off until dropped).
    pub fn start(cfg: &DiscordConfig) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(gateway_loop(cfg.token.clone(), cfg.gateway_url.clone(), tx));
        DiscordTransport { rx, http: reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client"), token: cfg.token.clone(), api_base: cfg.api_base.clone(), task }
    }
}

#[async_trait]
impl Transport for DiscordTransport {
    async fn next(&mut self) -> Option<Incoming> {
        self.rx.recv().await
    }

    async fn send(&self, channel: &str, text: &str) -> Result<(), String> {
        if !channel.chars().all(|c| c.is_ascii_digit()) || channel.is_empty() {
            return Err("bad channel id".into());
        }
        let url = format!("{}/channels/{}/messages", self.api_base, channel);
        for attempt in 0..2 {
            let r = self
                .http
                .post(&url)
                .header("Authorization", format!("Bot {}", self.token))
                .json(&json!({"content": text, "allowed_mentions": {"parse": []}}))
                .send()
                .await
                .map_err(|e| format!("discord send: {e}"))?;
            if r.status().as_u16() == 429 && attempt == 0 {
                let v: Value = r.json().await.unwrap_or(Value::Null);
                tokio::time::sleep(Duration::from_secs_f64(v["retry_after"].as_f64().unwrap_or(1.0).clamp(0.1, 30.0))).await;
                continue;
            }
            if !r.status().is_success() {
                return Err(format!("discord send: HTTP {}", r.status()));
            }
            return Ok(());
        }
        Err("discord send: rate limited".into())
    }

    fn max_len(&self) -> usize {
        2000
    }
}

/// Turn a MESSAGE_CREATE payload into an [`Incoming`]. None for bots, our own messages and
/// messages without text.
pub fn parse_message(d: &Value, bot_id: &str) -> Option<Incoming> {
    let author = &d["author"];
    let user = author["id"].as_str()?;
    if author["bot"].as_bool().unwrap_or(false) || user == bot_id {
        return None;
    }
    let mut text = d["content"].as_str().unwrap_or("").to_string();
    let mentions_bot = d["mentions"].as_array().is_some_and(|m| m.iter().any(|x| x["id"].as_str() == Some(bot_id)));
    for pat in [format!("<@{bot_id}>"), format!("<@!{bot_id}>")] {
        text = text.replace(&pat, "");
    }
    Some(Incoming {
        platform: "discord",
        user: user.to_string(),
        label: author["username"].as_str().unwrap_or("?").to_string(),
        channel: d["channel_id"].as_str()?.to_string(),
        is_dm: d["guild_id"].is_null(),
        mentions_bot,
        text: text.trim().to_string(),
    })
}

async fn gateway_loop(token: String, url: String, tx: mpsc::Sender<Incoming>) {
    let mut content_intent = true;
    let mut backoff = 2u64;
    loop {
        match session(&token, &url, content_intent, &tx).await {
            Ok(SessionEnd::NoContentIntent) => {
                tracing::warn!("discord: Message Content intent is not enabled for this bot; reconnecting without it (DMs and mentions still work)");
                content_intent = false;
                backoff = 2;
            }
            Ok(SessionEnd::Closed) => backoff = 2,
            Ok(SessionEnd::ReceiverGone) => return,
            Err(e) => tracing::warn!("discord gateway: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

enum SessionEnd {
    Closed,
    NoContentIntent,
    ReceiverGone,
}

async fn session(token: &str, url: &str, content_intent: bool, tx: &mpsc::Sender<Incoming>) -> Result<SessionEnd, String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| format!("connect: {e}"))?;
    let mut seq: Option<u64> = None;
    let mut bot_id = String::new();
    let mut beat = tokio::time::interval(Duration::from_secs(3600));
    beat.reset();
    loop {
        tokio::select! {
            _ = beat.tick() => {
                ws.send(Message::text(json!({"op": 1, "d": seq}).to_string())).await.map_err(|e| e.to_string())?;
            }
            frame = ws.next() => {
                let Some(frame) = frame else { return Ok(SessionEnd::Closed) };
                let frame = frame.map_err(|e| e.to_string())?;
                let text = match frame {
                    Message::Text(t) => t.to_string(),
                    Message::Close(c) => {
                        let code = c.map(|c| u16::from(c.code)).unwrap_or(0);
                        return Ok(if code == 4014 && content_intent { SessionEnd::NoContentIntent } else { SessionEnd::Closed });
                    }
                    _ => continue,
                };
                let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if let Some(s) = v["s"].as_u64() {
                    seq = Some(s);
                }
                match v["op"].as_u64() {
                    Some(10) => {
                        let ms = v["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250).clamp(100, 120_000);
                        beat = tokio::time::interval(Duration::from_millis(ms));
                        beat.reset();
                        let mut intents = INTENT_GUILD_MESSAGES | INTENT_DIRECT_MESSAGES;
                        if content_intent {
                            intents |= INTENT_MESSAGE_CONTENT;
                        }
                        let ident = json!({"op": 2, "d": {"token": token, "intents": intents, "properties": {"os": "linux", "browser": "nautik9", "device": "nautik9"}}});
                        ws.send(Message::text(ident.to_string())).await.map_err(|e| e.to_string())?;
                    }
                    Some(1) => ws.send(Message::text(json!({"op": 1, "d": seq}).to_string())).await.map_err(|e| e.to_string())?,
                    Some(7) | Some(9) => return Ok(SessionEnd::Closed), // reconnect / invalid session: start over
                    Some(0) => match v["t"].as_str() {
                        Some("READY") => bot_id = v["d"]["user"]["id"].as_str().unwrap_or("").to_string(),
                        Some("MESSAGE_CREATE") => {
                            if let Some(m) = parse_message(&v["d"], &bot_id) {
                                if tx.send(m).await.is_err() {
                                    return Ok(SessionEnd::ReceiverGone);
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[test]
    fn config_is_off_by_default_and_needs_the_env_var() {
        let none = |_: &str| None;
        assert!(DiscordConfig::from_agent_json(&json!({}), none).unwrap().is_none());
        assert!(DiscordConfig::from_agent_json(&json!({"bridges":{"discord":{"enabled":false}}}), none).unwrap().is_none());
        let on = json!({"bridges":{"discord":{"enabled":true,"token_env":"TOK","owner_ids":["42"]}}});
        assert!(DiscordConfig::from_agent_json(&on, none).unwrap_err().contains("TOK is not set"));
        let c = DiscordConfig::from_agent_json(&on, |k| (k == "TOK").then(|| " sekret \n".to_string())).unwrap().unwrap();
        assert_eq!(c.token, "sekret");
        assert!(!format!("{c:?}").contains("sekret"), "Debug never prints the token");
        let no_owner = json!({"bridges":{"discord":{"enabled":true,"token_env":"TOK"}}});
        assert!(DiscordConfig::from_agent_json(&no_owner, |_| Some("x".into())).unwrap_err().contains("owner_ids"));
        let bad = json!({"bridges":{"discord":{"enabled":true,"token_env":"TOK","owner_ids":["not-a-number"]}}});
        assert!(DiscordConfig::from_agent_json(&bad, |_| Some("x".into())).is_err());
    }

    #[test]
    fn message_parsing() {
        let d = json!({"id":"1","channel_id":"55","guild_id":"9","content":"<@777> hello there","author":{"id":"42","username":"sam"},"mentions":[{"id":"777"}]});
        let m = parse_message(&d, "777").unwrap();
        assert_eq!((m.user.as_str(), m.channel.as_str(), m.text.as_str(), m.is_dm, m.mentions_bot), ("42", "55", "hello there", false, true));
        let dm = json!({"channel_id":"56","content":"hi","author":{"id":"42","username":"sam"},"mentions":[]});
        assert!(parse_message(&dm, "777").unwrap().is_dm);
        assert!(parse_message(&json!({"channel_id":"5","content":"x","author":{"id":"8","bot":true}}), "777").is_none());
        assert!(parse_message(&json!({"channel_id":"5","content":"x","author":{"id":"777"}}), "777").is_none());
    }

    /// A local gateway that says HELLO, expects IDENTIFY, sends READY and two messages.
    async fn mock_gateway() -> (String, Arc<Mutex<Vec<Value>>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        tokio::spawn(async move {
            let (sock, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(Message::text(json!({"op":10,"d":{"heartbeat_interval":200}}).to_string())).await.unwrap();
            let ident = ws.next().await.unwrap().unwrap();
            s2.lock().unwrap().push(serde_json::from_str(ident.to_text().unwrap()).unwrap());
            ws.send(Message::text(json!({"op":0,"s":1,"t":"READY","d":{"user":{"id":"777"},"session_id":"s"}}).to_string())).await.unwrap();
            let msgs = [
                json!({"op":0,"s":2,"t":"MESSAGE_CREATE","d":{"channel_id":"55","content":"from a bot","author":{"id":"8","bot":true},"mentions":[]}}),
                json!({"op":0,"s":3,"t":"MESSAGE_CREATE","d":{"channel_id":"56","content":"hello mack","author":{"id":"42","username":"sam"},"mentions":[]}}),
            ];
            for m in msgs {
                ws.send(Message::text(m.to_string())).await.unwrap();
            }
            while let Some(Ok(f)) = ws.next().await {
                if let Message::Text(t) = f {
                    s2.lock().unwrap().push(serde_json::from_str(&t).unwrap());
                }
            }
        });
        (url, seen)
    }

    #[tokio::test]
    async fn gateway_identify_heartbeat_and_message_flow_and_rest_send() {
        let (gw, seen) = mock_gateway().await;
        // mock REST
        let posted: Arc<Mutex<Vec<(String, String, Value)>>> = Default::default();
        let p2 = posted.clone();
        let app = axum::Router::new().route(
            "/channels/{id}/messages",
            axum::routing::post(move |axum::extract::Path(id): axum::extract::Path<String>, h: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
                let p = p2.clone();
                async move {
                    p.lock().unwrap().push((id, h["authorization"].to_str().unwrap().to_string(), b));
                    axum::Json(json!({"id":"1"}))
                }
            }),
        );
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let cfg = DiscordConfig::from_agent_json(&json!({"bridges":{"discord":{"enabled":true,"token_env":"T","owner_ids":["42"]}}}), |_| Some("tok123".into())).unwrap().unwrap().with_endpoints(&gw, &api);
        let mut t = DiscordTransport::start(&cfg);
        let m = tokio::time::timeout(Duration::from_secs(5), t.next()).await.unwrap().unwrap();
        assert_eq!((m.user.as_str(), m.text.as_str(), m.is_dm), ("42", "hello mack", true), "the bot's message was skipped");
        t.send("56", "hi @everyone").await.unwrap();
        assert!(t.send("56/../x", "no").await.is_err());
        tokio::time::sleep(Duration::from_millis(500)).await; // let a heartbeat or two through
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0]["op"], 2);
        assert_eq!(seen[0]["d"]["token"], "tok123");
        let intents = seen[0]["d"]["intents"].as_u64().unwrap();
        assert!(intents & INTENT_MESSAGE_CONTENT != 0 && intents & INTENT_DIRECT_MESSAGES != 0);
        assert!(seen.iter().any(|v| v["op"] == 1 && v["d"] == 3), "heartbeat carries the last sequence number");
        let posted = posted.lock().unwrap().clone();
        assert_eq!(posted[0].0, "56");
        assert_eq!(posted[0].1, "Bot tok123");
        assert_eq!(posted[0].2["allowed_mentions"]["parse"], json!([]), "replies never ping anyone");
    }
}
