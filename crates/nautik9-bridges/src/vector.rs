//! Vector bridge. Jade Vault's "Vector" is the Vector private messenger (vectorapp.io), which
//! runs on Nostr: Jade's bridge runs Vector's own `vector-agent` and carries its DMs over
//! HTTP. NautiK9 speaks the Nostr side directly: NIP-17 private direct messages (kind 14
//! rumors inside NIP-59 gift wraps, sealed with NIP-44 encryption), over plain relay
//! websockets, with the `nostr` crate for the keys and cryptography.
//!
//! What is built: receive and send one-to-one encrypted DMs, owner versus other callers
//! (shared routing layer in the crate root). What is not: Vector's Concord communities
//! (group channels), Mini Apps over Iroh, attachments (Blossom), typing indicators,
//! reactions and edits, and the recipient's own DM-relay list (kind 10050) is not looked up:
//! messages go to the relays you configure, so both sides must share a relay.
//! Whether a given Vector build reads plain NIP-17 DMs from this identity was not tested
//! against the real app (no Vector account in the test suite); the protocol is tested end to
//! end against a local mock relay.
//!
//! `agent.json`:
//! ```json
//! "bridges": { "vector": {
//!   "enabled": true,
//!   "relays": ["wss://<a relay you and the sender both use>"],
//!   "nsec_env": "NAUTIK9_VECTOR_NSEC",
//!   "owner_ids": ["npub1..."], "allowed_user_ids": [], "open_dms": false,
//!   "auto_reply_others": false
//! } }
//! ```
//! The bot's secret key comes from the environment variable named by `nsec_env` if set,
//! else from `<config dir>/vector_nsec` (created with mode 0600 on first start; use a
//! dedicated bot identity, not your own key). The bot's npub is printed at start.
//! Only messages sent after the bridge started (minus one minute) are answered, so a restart
//! never replays old DMs.

use crate::{Incoming, Policy, Transport};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use nostr::prelude::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone)]
pub struct VectorConfig {
    keys: Keys,
    pub relays: Vec<String>,
    pub policy: Policy,
}

impl std::fmt::Debug for VectorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorConfig").field("npub", &self.npub()).field("relays", &self.relays).field("policy", &self.policy).finish()
    }
}

fn to_hex(s: &str) -> Result<String, String> {
    PublicKey::parse(s.trim()).map(|p| p.to_hex()).map_err(|e| format!("`{s}` is not an npub or hex public key: {e}"))
}

impl VectorConfig {
    pub fn npub(&self) -> String {
        self.keys.public_key().to_bech32().unwrap_or_default()
    }

    /// None when not enabled. Err when enabled but unusable.
    pub fn from_agent_json(conf: &Value, env: impl Fn(&str) -> Option<String>, config_dir: &Path) -> Result<Option<Self>, String> {
        if !crate::enabled(conf, "vector") {
            return Ok(None);
        }
        let c = &conf["bridges"]["vector"];
        let relays: Vec<String> = c["relays"].as_array().cloned().unwrap_or_default().iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
        if relays.is_empty() {
            return Err("bridges.vector: relays is empty (list the wss:// relay(s) you and your contacts share)".into());
        }
        if let Some(bad) = relays.iter().find(|r| !(r.starts_with("wss://") || r.starts_with("ws://127.0.0.1") || r.starts_with("ws://localhost"))) {
            return Err(format!("bridges.vector: relay {bad} must be wss:// (ws:// only for localhost)"));
        }
        let mut policy = Policy::from_json(c, to_hex)?;
        policy.require_mention = false; // every Vector message is a direct message
        if policy.owner_ids.is_empty() {
            return Err("bridges.vector: owner_ids is empty; nobody could get the full agent".into());
        }
        let keys = match c["nsec_env"].as_str().and_then(&env).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(sk) => Keys::parse(&sk).map_err(|e| format!("bridges.vector: the key in the environment is not a valid nsec or hex secret: {e}"))?,
            None => load_or_create_key(&config_dir.join("vector_nsec"))?,
        };
        Ok(Some(VectorConfig { keys, relays, policy }))
    }
}

fn load_or_create_key(path: &Path) -> Result<Keys, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Keys::parse(t.trim()).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let keys = Keys::generate();
            let sk = keys.secret_key().to_bech32().map_err(|e| e.to_string())?;
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            write_private(path, &sk)?;
            Ok(keys)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

pub struct VectorTransport {
    keys: Keys,
    inbound: mpsc::Receiver<Event>,
    outbound: broadcast::Sender<Event>,
    seen: HashSet<EventId>,
    started: Timestamp,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for VectorTransport {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl VectorTransport {
    /// Connect to every relay in the background (each reconnects with back-off).
    pub fn start(cfg: &VectorConfig) -> Self {
        let (itx, irx) = mpsc::channel(256);
        let (otx, _) = broadcast::channel(64);
        let started = Timestamp::now();
        // Gift wraps carry a randomised timestamp up to two days in the past (NIP-59).
        let since = started.as_u64().saturating_sub(3 * 86_400);
        let tasks = cfg.relays.iter().map(|r| tokio::spawn(relay_loop(r.clone(), cfg.keys.public_key(), since, itx.clone(), otx.subscribe()))).collect();
        VectorTransport { keys: cfg.keys.clone(), inbound: irx, outbound: otx, seen: HashSet::new(), started, tasks }
    }
}

async fn relay_loop(url: String, me: PublicKey, since: u64, itx: mpsc::Sender<Event>, mut out: broadcast::Receiver<Event>) {
    let mut backoff = 2u64;
    loop {
        match relay_session(&url, &me, since, &itx, &mut out).await {
            Ok(()) => return, // receiver gone
            Err(e) => tracing::warn!("vector relay {url}: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn relay_session(url: &str, me: &PublicKey, since: u64, itx: &mpsc::Sender<Event>, out: &mut broadcast::Receiver<Event>) -> Result<(), String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| format!("connect: {e}"))?;
    let req = json!(["REQ", "n9", {"kinds": [1059], "#p": [me.to_hex()], "since": since}]);
    ws.send(Message::text(req.to_string())).await.map_err(|e| e.to_string())?;
    loop {
        tokio::select! {
            frame = ws.next() => {
                let frame = frame.ok_or("closed")?.map_err(|e| e.to_string())?;
                let Message::Text(t) = frame else { continue };
                let v: Value = match serde_json::from_str(&t) { Ok(v) => v, Err(_) => continue };
                if v[0] == "EVENT" && v[1] == "n9" {
                    if let Ok(ev) = Event::from_json(v[2].to_string()) {
                        if ev.verify().is_ok() && itx.send(ev).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            ev = out.recv() => match ev {
                Ok(ev) => ws.send(Message::text(json!(["EVENT", serde_json::from_str::<Value>(&ev.as_json()).unwrap_or(Value::Null)]).to_string())).await.map_err(|e| e.to_string())?,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

fn short(pk: &PublicKey) -> String {
    let n = pk.to_bech32().unwrap_or_default();
    format!("{}…{}", &n[..n.len().min(12)], &n[n.len().saturating_sub(4)..])
}

#[async_trait]
impl Transport for VectorTransport {
    async fn next(&mut self) -> Option<Incoming> {
        loop {
            let ev = self.inbound.recv().await?;
            if !self.seen.insert(ev.id) {
                continue; // the same wrap from a second relay
            }
            let Ok(gift) = UnwrappedGift::from_gift_wrap(&self.keys, &ev).await else { continue };
            if gift.rumor.kind != Kind::PrivateDirectMessage || gift.sender == self.keys.public_key() {
                continue;
            }
            if gift.rumor.created_at.as_u64() + 60 < self.started.as_u64() {
                continue; // before this run: never replay old DMs
            }
            return Some(Incoming {
                platform: "vector",
                user: gift.sender.to_hex(),
                label: short(&gift.sender),
                channel: gift.sender.to_hex(),
                is_dm: true,
                mentions_bot: true,
                text: gift.rumor.content.clone(),
            });
        }
    }

    async fn send(&self, channel: &str, text: &str) -> Result<(), String> {
        let to = PublicKey::from_hex(channel).map_err(|e| format!("bad recipient: {e}"))?;
        let wrap = EventBuilder::private_msg(&self.keys, to, text, []).await.map_err(|e| format!("encrypt: {e}"))?;
        self.outbound.send(wrap).map(|_| ()).map_err(|_| "no relay connection to send through".to_string())
    }

    fn max_len(&self) -> usize {
        8000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    fn conf(owner: &str, relay: &str) -> Value {
        json!({"bridges":{"vector":{"enabled":true,"relays":[relay],"owner_ids":[owner],"nsec_env":"K"}}})
    }

    #[test]
    fn config_off_by_default_validates_relays_and_keeps_keys_private() {
        let d = tempfile::tempdir().unwrap();
        assert!(VectorConfig::from_agent_json(&json!({}), |_| None, d.path()).unwrap().is_none());
        let friend = Keys::generate();
        let npub = friend.public_key().to_bech32().unwrap();
        let mut c = conf(&npub, "wss://relay.example");
        let cfg = VectorConfig::from_agent_json(&c, |_| None, d.path()).unwrap().unwrap();
        assert_eq!(cfg.policy.owner_ids, vec![friend.public_key().to_hex()], "npub is normalised to hex");
        // the key file was created once with mode 0600 and is reused
        let p = d.path().join("vector_nsec");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let again = VectorConfig::from_agent_json(&c, |_| None, d.path()).unwrap().unwrap();
        assert_eq!(cfg.npub(), again.npub());
        assert!(!format!("{cfg:?}").contains("nsec1"));
        // env var wins
        let other = Keys::generate();
        let sk = other.secret_key().to_bech32().unwrap();
        let from_env = VectorConfig::from_agent_json(&c, |k| (k == "K").then(|| sk.clone()), d.path()).unwrap().unwrap();
        assert_eq!(from_env.npub(), other.public_key().to_bech32().unwrap());
        c["bridges"]["vector"]["relays"] = json!(["http://plain.example"]);
        assert!(VectorConfig::from_agent_json(&c, |_| None, d.path()).is_err());
        c["bridges"]["vector"]["relays"] = json!([]);
        assert!(VectorConfig::from_agent_json(&c, |_| None, d.path()).unwrap_err().contains("relays"));
        assert!(VectorConfig::from_agent_json(&conf("garbage", "wss://r"), |_| None, d.path()).is_err());
    }

    /// A one-connection mock relay: stores what it is sent, answers the REQ with `deliver`.
    async fn mock_relay(deliver: Vec<Event>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        tokio::spawn(async move {
            let (sock, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            while let Some(Ok(f)) = ws.next().await {
                let Message::Text(t) = f else { continue };
                let v: Value = serde_json::from_str(&t).unwrap();
                g2.lock().unwrap().push(v.clone());
                if v[0] == "REQ" {
                    for ev in &deliver {
                        let frame = json!(["EVENT", v[1], serde_json::from_str::<Value>(&ev.as_json()).unwrap()]);
                        ws.send(Message::text(frame.to_string())).await.unwrap();
                        // a duplicate, as a second relay would send
                        ws.send(Message::text(frame.to_string())).await.unwrap();
                    }
                }
            }
        });
        (url, got)
    }

    #[tokio::test]
    async fn nip17_dm_roundtrip_over_a_mock_relay() {
        let d = tempfile::tempdir().unwrap();
        let bot = Keys::generate();
        let friend = Keys::generate();
        let old = Keys::generate();
        // an old DM (before this run) and a fresh one, both gift wrapped to the bot
        let old_rumor = EventBuilder::private_msg_rumor(bot.public_key(), "stale").custom_created_at(Timestamp::from_secs(Timestamp::now().as_u64() - 7200)).build(old.public_key());
        let old_wrap = EventBuilder::gift_wrap(&old, &bot.public_key(), old_rumor, []).await.unwrap();
        let wrap = EventBuilder::private_msg(&friend, bot.public_key(), "hello bot", []).await.unwrap();
        assert_ne!(wrap.pubkey, friend.public_key(), "the wrap is signed by a throw-away key");
        let (url, got) = mock_relay(vec![old_wrap, wrap]).await;

        let sk = bot.secret_key().to_bech32().unwrap();
        let cfg = VectorConfig::from_agent_json(&conf(&friend.public_key().to_bech32().unwrap(), &url), |_| Some(sk.clone()), d.path()).unwrap().unwrap();
        let mut t = VectorTransport::start(&cfg);
        let m = tokio::time::timeout(Duration::from_secs(5), t.next()).await.unwrap().unwrap();
        assert_eq!(m.text, "hello bot", "the stale DM and the duplicate were skipped");
        assert_eq!(m.user, friend.public_key().to_hex());
        assert!(m.is_dm);
        assert_eq!(cfg.policy.route(&m), crate::Route::OwnerAgent);

        // reply: the relay receives an EVENT the friend can decrypt
        t.send(&m.channel, "hi friend").await.unwrap();
        let mut sent = None;
        for _ in 0..50 {
            sent = got.lock().unwrap().iter().find(|v| v[0] == "EVENT").cloned();
            if sent.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let ev = Event::from_json(sent.expect("relay got the reply")[1].to_string()).unwrap();
        assert_eq!(ev.kind, Kind::GiftWrap);
        let gift = UnwrappedGift::from_gift_wrap(&friend, &ev).await.unwrap();
        assert_eq!((gift.sender, gift.rumor.content.as_str()), (bot.public_key(), "hi friend"));
        assert!(!ev.content.contains("hi friend"), "ciphertext only on the wire");
        let req = got.lock().unwrap().iter().find(|v| v[0] == "REQ").cloned().unwrap();
        assert_eq!(req[2]["kinds"], json!([1059]));
        assert_eq!(req[2]["#p"][0], bot.public_key().to_hex());
    }
}
