//! Join, show and leave a Lemonade Nexus network from an app.
//!
//! One shared piece for NautiK9 and Jade: the apps differ only in how they call it (an HTTP route
//! in NautiK9, a JNI call in Jade) and in their labels. The SDK (liblemonade_nexus_sdk.so) is
//! loaded at run time; without it every call answers with a plain "SDK not found".
//!
//! State lives in one directory (mode 0700):
//!   identity.json     this device's Ed25519 key (written by the SDK, 0600)
//!   offer.json        a pending invite the user has not confirmed yet (0600, removed on join/discard)
//!   membership.json   network, server, device name, node id, tunnel IP (no secrets)
//!   network.json      the network's server, Genesis key and CA (no secrets; lets a bare code rejoin)
//!   ca.pem            the network's CA, handed to the SDK
//!   last_status.json  the last status read from the SDK, shown as stale when the server is not reached
//!
//! Status never invents a peer: peers come only from the SDK, and when the SDK cannot be read the
//! last list is kept with `stale: true` and the time it was read.

pub mod ffi;
pub mod invite;

use ffi::{code_name, Sdk};
use invite::{KnownNetwork, Offer};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How an app sets the module up.
#[derive(Clone, Debug)]
pub struct Config {
    /// State directory (created 0700).
    pub dir: PathBuf,
    /// Where to look for the SDK, in order (file paths, or names the loader resolves).
    pub sdk_paths: Vec<PathBuf>,
    /// Default device name for a join (the user may change it before confirming).
    pub device_name: String,
    /// Seconds between background status reads (0 = no background thread).
    pub poll_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    NotJoined,
    Joining,
    Joined,
    Leaving,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::NotJoined => "not_joined",
            Phase::Joining => "joining",
            Phase::Joined => "joined",
            Phase::Leaving => "leaving",
        }
    }
}

struct ClientPtr(ffi::Client);
unsafe impl Send for ClientPtr {}

struct St {
    phase: Phase,
    client: Option<ClientPtr>,
    membership: Option<Value>,
    /// Last status read from the SDK (server_path, tunnel_up, peers, totals) and when.
    last: Option<Value>,
    last_ok: u64,
    last_error: Option<String>,
    down_since: Option<u64>,
    resumed_for_down: bool,
    net_changes: u64,
    last_net_change: Option<Value>,
    ca_set_to: Option<PathBuf>,
    /// When this device last signed in (join or rejoin); sessions expire after 24 h and the SDK does
    /// not sign in again by itself.
    signed_in_at: u64,
    needs_rejoin: bool,
    peers_unconfirmed: bool,
    /// A leave got no answer: no automatic sign-in (it would re-create the node) until Forget or a new join.
    leave_unconfirmed: bool,
}

struct Shared {
    cfg: Config,
    sdk: OnceLock<Result<Arc<Sdk>, String>>,
    st: Mutex<St>,
    /// Serialises join / leave / restore.
    op: Mutex<()>,
}

#[derive(Clone)]
pub struct Nexus(Arc<Shared>);

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok())
}

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn commit_wait(j: &Value) -> bool {
    j["error"].as_str().map(|e| e.contains("waiting for the network to commit")).unwrap_or(false)
}

/// ln_join_network, signing in again (identity only) while the server says the network has not
/// committed the device yet. The first call uses whatever invite is set; retries never resend it.
fn sign_in(sdk: &Sdk, c: ffi::Client, max: Duration) -> (i32, Value) {
    let (mut code, out) = sdk.join(c);
    let mut j = parse_json(&out);
    let started = std::time::Instant::now();
    while (code != 0 || j["success"] == Value::Bool(false)) && commit_wait(&j) && started.elapsed() < max {
        std::thread::sleep(Duration::from_secs(3));
        let _ = sdk.set_link_token(c, "");
        let (c2, o2) = sdk.join(c);
        code = c2;
        j = parse_json(&o2);
    }
    (code, j)
}

/// Rules from ln_set_device_name, checked before the SDK sees the name.
pub fn device_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().any(|c| c.is_ascii_alphanumeric())
        && !name.chars().any(|c| c.is_control() || ";|&$`(){}<>'\"\\".contains(c))
}

impl Nexus {
    pub fn open(cfg: Config) -> Nexus {
        let _ = std::fs::create_dir_all(&cfg.dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cfg.dir, std::fs::Permissions::from_mode(0o700));
        }
        let membership = read_json(&cfg.dir.join("membership.json"));
        let last = read_json(&cfg.dir.join("last_status.json"));
        let last_ok = last.as_ref().and_then(|l| l["read_at"].as_u64()).unwrap_or(0);
        let poll = cfg.poll_secs;
        let n = Nexus(Arc::new(Shared {
            cfg,
            sdk: OnceLock::new(),
            st: Mutex::new(St {
                phase: Phase::NotJoined,
                client: None,
                membership: membership.clone(),
                last,
                last_ok,
                last_error: None,
                down_since: None,
                resumed_for_down: false,
                net_changes: 0,
                last_net_change: None,
                ca_set_to: None,
                signed_in_at: 0,
                needs_rejoin: false,
                peers_unconfirmed: false,
                leave_unconfirmed: false,
            }),
            op: Mutex::new(()),
        }));
        if membership.is_some() {
            let me = n.clone();
            std::thread::spawn(move || {
                if let Err(e) = me.restore() {
                    me.0.st.lock().unwrap().last_error = Some(format!("rejoin after start: {e}"));
                }
            });
        }
        if poll > 0 {
            let weak = Arc::downgrade(&n.0);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(poll));
                let Some(sh) = weak.upgrade() else { return };
                Nexus(sh).tick();
            });
        }
        n
    }

    fn dir(&self) -> &Path {
        &self.0.cfg.dir
    }

    pub fn sdk(&self) -> Result<Arc<Sdk>, String> {
        self.0.sdk.get_or_init(|| Sdk::open(&self.0.cfg.sdk_paths).map(Arc::new)).clone()
    }

    fn known(&self) -> Option<KnownNetwork> {
        read_json(&self.dir().join("network.json")).and_then(|v| KnownNetwork::from_json(&v))
    }

    fn pending(&self) -> Option<Offer> {
        let t = std::fs::read_to_string(self.dir().join("offer.json")).ok()?;
        invite::parse(&t, None).ok()
    }

    /// Store an invite for the user to confirm. Returns what will be joined (no secret).
    pub fn offer(&self, text: &str) -> Result<Value, String> {
        let o = invite::parse(text, self.known().as_ref())?;
        write_private(&self.dir().join("offer.json"), &o.to_json().to_string()).map_err(|e| format!("could not store the invite: {e}"))?;
        Ok(o.summary(now()))
    }

    pub fn discard_offer(&self) {
        let _ = std::fs::remove_file(self.dir().join("offer.json"));
    }

    fn set_ca(&self, sdk: &Sdk, ca_pem: Option<&str>) -> Result<(), String> {
        let Some(pem) = ca_pem else { return Ok(()) };
        let path = self.dir().join("ca.pem");
        write_private(&path, pem).map_err(|e| format!("could not write ca.pem: {e}"))?;
        let mut st = self.0.st.lock().unwrap();
        if st.ca_set_to.as_deref() != Some(path.as_path()) {
            sdk.set_ca_bundle(&path)?;
            st.ca_set_to = Some(path);
        }
        Ok(())
    }

    fn apply_map(&self, sdk: &Sdk, map: &serde_json::Map<String, Value>) -> Result<(), String> {
        if map.is_empty() {
            if sdk.has_address_map() {
                sdk.set_address_map("{}")?;
            }
            return Ok(());
        }
        sdk.set_address_map(&Value::Object(map.clone()).to_string())
    }

    /// Make a client, set identity, Genesis key and name. Shared by join and restore.
    fn client_for(&self, sdk: &Sdk, host: &str, port: u16, genesis: &str, name: Option<&str>) -> Result<ffi::Client, String> {
        let c = sdk.create_tls(host, port)?;
        let fail = |e: String| {
            sdk.destroy(c);
            Err(e)
        };
        let idp = self.dir().join("identity.json");
        let (id, _new) = match sdk.identity_load_or_create(&idp) {
            Ok(x) => x,
            Err(e) => return fail(e),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&idp, std::fs::Permissions::from_mode(0o600));
        }
        let r = sdk.set_identity(c, id);
        sdk.identity_destroy(id);
        if let Err(e) = r {
            return fail(e);
        }
        if let Err(e) = sdk.set_genesis(c, genesis) {
            return fail(e);
        }
        if let Some(n) = name {
            if let Err(e) = sdk.set_device_name(c, n) {
                return fail(e);
            }
        }
        Ok(c)
    }

    /// Join with the pending invite. Blocking (seconds); run it off the UI/async thread.
    pub fn join(&self, device_name: Option<&str>) -> Result<Value, String> {
        let _op = self.0.op.lock().unwrap();
        if self.0.st.lock().unwrap().phase == Phase::Joined {
            return Err("already joined: leave first".into());
        }
        let offer = self.pending().ok_or("no invite to join with: paste or load one first")?;
        if let Some(e) = offer.expires_at {
            if e <= now() {
                self.discard_offer();
                return Err("the invite has expired: get a new one".into());
            }
        }
        let name = device_name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&self.0.cfg.device_name).to_string();
        if !device_name_ok(&name) {
            return Err("device name: letters, digits, '-', '.', '_' only, at most 64".into());
        }
        let sdk = self.sdk()?;
        self.0.st.lock().unwrap().phase = Phase::Joining;
        let res = (|| {
            self.set_ca(&sdk, offer.ca_pem.as_deref())?;
            self.apply_map(&sdk, &offer.address_map)?;
            let c = self.client_for(&sdk, &offer.host, offer.port, &offer.genesis_key, Some(&name))?;
            if let Err(e) = sdk.set_link_token(c, &offer.token) {
                sdk.destroy(c);
                return Err(e);
            }
            // The server may take the invite and then report that its network has not committed the new
            // device yet: the invite is spent, so sign_in retries with the identity alone.
            let (mut code, mut j) = sign_in(&sdk, c, Duration::from_secs(90));
            if code != 0 || j["success"] == Value::Bool(false) {
                // An earlier attempt may have been committed after it reported a wait: this identity then
                // already has its device, and signing in with the identity alone finds it.
                let first_error = j["error"].as_str().map(str::to_string).unwrap_or_else(|| code_name(code).to_string());
                let _ = sdk.set_link_token(c, "");
                let (c2, j2) = sign_in(&sdk, c, Duration::from_secs(30));
                if c2 != 0 || j2["success"] == Value::Bool(false) {
                    sdk.destroy(c);
                    return Err(format!("join refused: {first_error}"));
                }
                code = c2;
                j = j2;
            }
            let _ = code;
            // The invite is single-use and now spent.
            let _ = sdk.set_link_token(c, "");
            self.discard_offer();
            let rc = sdk.mesh_enable(c);
            let membership = json!({
                "network": offer.network,
                "server": format!("{}:{}", offer.host, offer.port),
                "device_name": if j["device_name_refused"].as_bool() == Some(true) { Value::Null } else { json!(name) },
                "device_name_refused": j["device_name_refused"].as_bool().unwrap_or(false),
                "node_id": j["node_id"],
                "tunnel_ip": j["tunnel_ip"],
                "joined_at": now(),
                "membership": j["membership"],
            });
            let _ = write_private(&self.dir().join("membership.json"), &membership.to_string());
            let _ = write_private(&self.dir().join("network.json"), &offer.known().to_json().to_string());
            let mut st = self.0.st.lock().unwrap();
            st.client = Some(ClientPtr(c));
            st.signed_in_at = now();
            st.membership = Some(membership.clone());
            st.last = None;
            st.last_ok = 0;
            st.down_since = None;
            st.last_error = if rc != 0 { Some(format!("mesh did not start: {}", code_name(rc))) } else { None };
            Ok(membership)
        })();
        let mut st = self.0.st.lock().unwrap();
        st.phase = if res.is_ok() { Phase::Joined } else { Phase::NotJoined };
        if let Err(e) = &res {
            st.last_error = Some(e.clone());
        }
        res
    }

    /// After an app restart: authenticate with the stored identity and bring the mesh back.
    fn restore(&self) -> Result<(), String> {
        let _op = self.0.op.lock().unwrap();
        self.restore_locked()
    }

    /// restore() with the op lock already held.
    fn restore_locked(&self) -> Result<(), String> {
        let (m, known) = (self.0.st.lock().unwrap().membership.clone(), self.known());
        let (Some(_m), Some(k)) = (m, known) else { return Err("no stored network".into()) };
        let sdk = self.sdk()?;
        let (host, port) = invite::split_host_port(&k.server)?;
        self.0.st.lock().unwrap().phase = Phase::Joining;
        let r = (|| {
            self.set_ca(&sdk, k.ca_pem.as_deref())?;
            self.apply_map(&sdk, &k.address_map)?;
            let c = self.client_for(&sdk, &host, port, &k.genesis_key, None)?;
            let (code, j) = sign_in(&sdk, c, Duration::from_secs(90));
            if code != 0 || j["success"] == Value::Bool(false) {
                sdk.destroy(c);
                if j["identity_revoked"].as_bool() == Some(true) {
                    return Err("this device was removed from the network: leave, then join with a new invite".into());
                }
                return Err(format!("server: {}", j["error"].as_str().unwrap_or(code_name(code))));
            }
            sdk.mesh_enable(c);
            let mut st = self.0.st.lock().unwrap();
            if let Some(mm) = st.membership.as_mut() {
                mm["node_id"] = j["node_id"].clone();
                mm["tunnel_ip"] = j["tunnel_ip"].clone();
            }
            st.client = Some(ClientPtr(c));
            st.signed_in_at = now();
            st.needs_rejoin = false;
            if st.last_error.as_deref().map(|e| e.starts_with("sign-in") || e.starts_with("server:") || e.starts_with("rejoin")).unwrap_or(false) {
                st.last_error = None;
            }
            Ok(())
        })();
        let mut st = self.0.st.lock().unwrap();
        // Still a member when the server could not be reached: the status says so and keeps trying.
        st.phase = if st.membership.is_some() { Phase::Joined } else { Phase::NotJoined };
        if let Err(e) = &r {
            st.last_error = Some(e.clone());
        }
        r
    }

    /// Leave: delete this device's node on the server, stop the tunnel, remove the identity.
    /// `force` also forgets locally when the server cannot be reached (its node then stays there,
    /// offline, until an admin removes it).
    pub fn leave(&self, force: bool) -> Result<Value, String> {
        let _op = self.0.op.lock().unwrap();
        let sdk = self.sdk()?;
        // Not connected yet (the app just started, or the server was away): sign in first so the
        // server can delete this device's node.
        let needs_client = { let st = self.0.st.lock().unwrap(); st.membership.is_some() && st.client.is_none() && !st.leave_unconfirmed };
        if needs_client {
            let _ = self.restore_locked();
        }
        let client = {
            let mut st = self.0.st.lock().unwrap();
            if st.membership.is_none() {
                return Err("not joined".into());
            }
            st.phase = Phase::Leaving;
            st.client.take()
        };
        let mut server_said = Value::Null;
        let mut ok = false;
        let skip_server = force && self.0.st.lock().unwrap().leave_unconfirmed;
        if let (false, Some(ClientPtr(c))) = (skip_server, &client) {
            // The delete goes over the tunnel: give a just-started tunnel time to come up, and retry
            // a request that did not reach the server.
            let started = std::time::Instant::now();
            while started.elapsed() < Duration::from_secs(20) && !parse_json(&sdk.mesh_status(*c).1)["tunnel_up"].as_bool().unwrap_or(false) {
                std::thread::sleep(Duration::from_millis(500));
            }
            for attempt in 0..3 {
                let (code, out) = sdk.leave(*c);
                server_said = parse_json(&out);
                ok = code == 0 && server_said["success"].as_bool().unwrap_or(false);
                let unreachable = server_said["error"].as_str().map(|e| e.contains("unreachable") || e.contains("did not reach")).unwrap_or(false);
                if ok || !unreachable || attempt == 2 {
                    break;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        }
        let unreachable = |v: &Value| v["error"].as_str().map(|e| e.contains("unreachable") || e.contains("did not reach")).unwrap_or(false);
        // No answer is not "not done": the server deletes the node and drops this device's tunnel before
        // its reply can come back through that tunnel (seen on the work Nexus 2026-11-12). Never sign in
        // again here: a sign-in re-creates a deleted node. Mark it, stop the background rejoin, and let
        // the user finish with Forget.
        let no_answer = !ok && unreachable(&server_said);
        if no_answer {
            self.0.st.lock().unwrap().leave_unconfirmed = true;
        }
        if !ok && !force {
            let mut st = self.0.st.lock().unwrap();
            st.client = client;
            st.phase = Phase::Joined;
            let why = server_said["error"].as_str().unwrap_or(if st.client.is_none() { "not connected to the server" } else { "server refused" }).to_string();
            let e = if no_answer {
                "leave sent, but no answer came back (the server may already have removed this device); press Forget to finish here".to_string()
            } else {
                format!("{} (Forget removes it from this device only)", if why.starts_with("leave failed") { why.clone() } else { format!("leave failed: {why}") })
            };
            st.last_error = Some(e.clone());
            return Err(e);
        }
        if let Some(ClientPtr(c)) = client {
            sdk.mesh_disable(c);
            sdk.destroy(c);
        }
        for f in ["membership.json", "identity.json", "last_status.json", "offer.json"] {
            let _ = std::fs::remove_file(self.dir().join(f));
        }
        let mut st = self.0.st.lock().unwrap();
        st.phase = Phase::NotJoined;
        st.leave_unconfirmed = false;
        st.membership = None;
        st.last = None;
        st.last_ok = 0;
        st.last_error = None;
        Ok(json!({"left": true, "server_removed_node": ok}))
    }

    /// The network under the app changed (Wi-Fi to cellular, a VPN came up): new socket and
    /// handshake, then a path check. No rejoin.
    pub fn network_changed(&self, what: &str) -> Value {
        let sdk = match self.sdk() {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let mut st = self.0.st.lock().unwrap();
        st.net_changes += 1;
        let acted = if let Some(ClientPtr(c)) = &st.client {
            let a = sdk.resume(*c);
            let b = sdk.mesh_refresh(*c);
            format!("resume {} refresh {}", code_name(a), code_name(b))
        } else {
            "not joined".to_string()
        };
        let ev = json!({"at": now(), "what": what, "action": acted});
        st.last_net_change = Some(ev.clone());
        ev
    }

    /// The app is going to the background / coming back (Android onStop / onStart).
    pub fn suspend(&self) {
        if let (Ok(sdk), Some(ClientPtr(c))) = (self.sdk(), &self.0.st.lock().unwrap().client) {
            sdk.suspend(*c);
        }
    }
    pub fn resume(&self) {
        if let (Ok(sdk), Some(ClientPtr(c))) = (self.sdk(), &self.0.st.lock().unwrap().client) {
            sdk.resume(*c);
        }
    }

    /// A loopback port that reaches `ip:port` on the mesh (a peer's tunnel IP and a service port it
    /// exposes). Err when not joined or the route cannot be opened.
    pub fn egress(&self, ip: &str, port: u16) -> Result<u16, String> {
        let sdk = self.sdk()?;
        let first = {
            let st = self.0.st.lock().unwrap();
            let Some(ClientPtr(c)) = &st.client else { return Err("not joined".into()) };
            sdk.open_egress(*c, ip, port)
        };
        match first {
            Err(e) if e.contains("refused (auth)") => {
                // Expired session: sign in again without asking the user, then try once more.
                self.rebuild("session expired");
                let st = self.0.st.lock().unwrap();
                let Some(ClientPtr(c)) = &st.client else { return Err(format!("{e}; sign-in again failed")) };
                sdk.open_egress(*c, ip, port)
            }
            r => r,
        }
    }

    /// The tunnel IP of a peer by hostname, from the last peer list.
    pub fn peer_ip(&self, hostname: &str) -> Option<String> {
        let st = self.0.st.lock().unwrap();
        st.last.as_ref()?["peers"].as_array()?.iter().find(|p| p["hostname"] == hostname).and_then(|p| p["tunnel_ip"].as_str().map(str::to_string))
    }

    /// Read the SDK now and update the cached status.
    fn read(&self) {
        let Ok(sdk) = self.sdk() else { return };
        let mut st = self.0.st.lock().unwrap();
        let Some(ClientPtr(c)) = &st.client else { return };
        let c = *c;
        let (a, s) = sdk.mesh_status(c);
        let (b, p) = sdk.mesh_peers(c);
        let t = now();
        if a != 0 || b != 0 {
            st.last_error = Some(format!("SDK status read failed: {} / {}", code_name(a), code_name(b)));
            return;
        }
        let s = parse_json(&s);
        let peers: Vec<Value> = parse_json(&p).as_array().cloned().unwrap_or_default().into_iter().map(|p| {
            json!({
                "name": p["hostname"], "node_id": p["node_id"], "tunnel_ip": p["tunnel_ip"],
                "path": p["path"], "endpoint": p["endpoint"], "relay_endpoint": p["relay_endpoint"],
                "online": p["is_online"], "server_online": p["server_online"],
                "last_handshake": p["last_handshake"], "last_rx": p["last_rx"],
                "rx_bytes": p["rx_bytes"], "tx_bytes": p["tx_bytes"], "latency_ms": p["latency_ms"],
            })
        }).collect();
        let tunnel_up = s["tunnel_up"].as_bool().unwrap_or(false);
        st.needs_rejoin = s["needs_rejoin"].as_bool().unwrap_or(false);
        // An empty list right after a full one usually means the server stopped answering (the SDK
        // drops its peer list), not that everyone left: keep the last list and mark it unconfirmed.
        let prev_peers = st.last.as_ref().and_then(|l| l["peers"].as_array().map(|a| a.len())).unwrap_or(0);
        let peers_lost = peers.is_empty() && prev_peers > 0;
        st.peers_unconfirmed = tunnel_up && peers_lost;
        if tunnel_up && !peers_lost {
            let snap = json!({
                "read_at": t, "server_path": s["server_path"], "tunnel_up": true,
                "tunnel_ip": s["tunnel_ip"], "needs_rejoin": s["needs_rejoin"],
                "rx_bytes": s["total_rx_bytes"], "tx_bytes": s["total_tx_bytes"],
                "peer_count": s["peer_count"], "online_count": s["online_count"], "peers": peers,
            });
            let _ = write_private(&self.dir().join("last_status.json"), &snap.to_string());
            st.last = Some(snap);
            st.last_ok = t;
            st.down_since = None;
            st.resumed_for_down = false;
            if st.last_error.as_deref().map(|e| e.starts_with("SDK status") || e.starts_with("rejoin")).unwrap_or(false) {
                st.last_error = None;
            }
        } else {
            st.down_since.get_or_insert(t);
            // The server's path name and needs_rejoin are still worth showing while down.
            if let Some(l) = st.last.as_mut() {
                l["needs_rejoin"] = s["needs_rejoin"].clone();
            }
        }
    }

    /// Background: read, and when the tunnel stays down, ask the SDK for a new socket, then rebuild.
    /// Drop the client and sign in again with the identity (the SDK's documented recovery: disable,
    /// join, enable). A failure is shown in the status, and the next tick tries again.
    fn rebuild(&self, why: &str) {
        if let Ok(sdk) = self.sdk() {
            if let Some(ClientPtr(c)) = self.0.st.lock().unwrap().client.take() {
                sdk.mesh_disable(c);
                sdk.destroy(c);
            }
        }
        {
            let mut st = self.0.st.lock().unwrap();
            st.down_since = Some(now());
            st.resumed_for_down = false;
        }
        if let Err(e) = self.restore() {
            self.0.st.lock().unwrap().last_error = Some(format!("sign-in again ({why}) failed: {e}"));
        }
    }

    fn tick(&self) {
        self.read();
        let (down_for, resumed, has_client, joined, signed, rejoin) = {
            let st = self.0.st.lock().unwrap();
            (st.down_since.map(|d| now().saturating_sub(d)), st.resumed_for_down, st.client.is_some(), st.phase == Phase::Joined, st.signed_in_at, st.needs_rejoin)
        };
        if joined && has_client && (rejoin || (signed > 0 && now().saturating_sub(signed) > 12 * 3600)) {
            // The server asks for a new sign-in, or the 24 h session is half gone: renew it now.
            self.rebuild(if rejoin { "the server asked" } else { "session renewal" });
            return;
        }
        if !joined || self.0.st.lock().unwrap().leave_unconfirmed {
            return;
        }
        if !has_client {
            // Joined but the client never came up (server down at start): try again.
            let _ = self.restore();
            return;
        }
        match down_for {
            Some(d) if d >= 15 && !resumed => {
                self.resume();
                self.0.st.lock().unwrap().resumed_for_down = true;
            }
            Some(d) if d >= 90 => {
                // Rebuild: the documented recovery when resume did not bring the tunnel back.
                self.rebuild("tunnel down");
            }
            _ => {}
        }
    }

    /// Everything the Nexus screen shows. Same shape in every app.
    pub fn status(&self) -> Value {
        self.read();
        let t = now();
        let sdk = match self.sdk() {
            Ok(s) => json!({"loaded": true, "path": s.path.to_string_lossy(), "address_map": s.has_address_map()}),
            Err(e) => json!({"loaded": false, "error": e}),
        };
        let offer = self.pending().map(|o| o.summary(t));
        let st = self.0.st.lock().unwrap();
        let m = st.membership.clone().unwrap_or(Value::Null);
        let joined = st.membership.is_some();
        let fresh = joined && st.client.is_some() && st.down_since.is_none() && !st.peers_unconfirmed && st.last_ok > 0 && t.saturating_sub(st.last_ok) < 30;
        let last = st.last.clone().unwrap_or(json!({}));
        let stale_reason = if !joined || fresh {
            Value::Null
        } else if st.client.is_none() {
            json!("not connected to the server")
        } else if st.peers_unconfirmed {
            json!("the server sent no peer list; showing the last one")
        } else if st.last_ok == 0 {
            json!("waiting for the first answer from the server")
        } else {
            json!(format!("server not reached for {} s", t.saturating_sub(st.down_since.unwrap_or(st.last_ok))))
        };
        json!({
            "sdk": sdk,
            "state": st.phase.name(),
            "offer": offer,
            "joined": joined,
            "device_default": self.0.cfg.device_name,
            "network": m["network"], "server": m["server"], "device_name": m["device_name"],
            "node_id": m["node_id"], "tunnel_ip": m["tunnel_ip"], "joined_at": m["joined_at"],
            "server_path": if fresh {
                match last["server_path"].as_str() { Some("") | None => json!("udp"), _ => last["server_path"].clone() }
            } else if joined { json!("none") } else { Value::Null },
            "server_path_reported": last["server_path"],
            "tunnel_up": fresh,
            "needs_rejoin": last["needs_rejoin"],
            "rx_bytes": last["rx_bytes"], "tx_bytes": last["tx_bytes"],
            "peers": last["peers"].as_array().cloned().unwrap_or_default(),
            "read_at": if st.last_ok > 0 { json!(st.last_ok) } else { Value::Null },
            "stale": joined && !fresh,
            "stale_reason": stale_reason,
            "last_error": st.last_error,
            "leave_unconfirmed": st.leave_unconfirmed,
            "signed_in_at": if st.signed_in_at > 0 { json!(st.signed_in_at) } else { Value::Null },
            "network_changes": st.net_changes,
            "last_network_change": st.last_net_change,
            "now": t,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nexus-join-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn without_sdk_status_is_honest_and_join_says_why() {
        let n = Nexus::open(Config { dir: tmp("nosdk"), sdk_paths: vec![PathBuf::from("/nonexistent/liblemonade_nexus_sdk.so")], device_name: "dev".into(), poll_secs: 0 });
        let s = n.status();
        assert_eq!(s["sdk"]["loaded"], false);
        assert_eq!(s["joined"], false);
        assert_eq!(s["peers"].as_array().unwrap().len(), 0);
        assert!(n.join(None).unwrap_err().contains("no invite"));
    }

    #[test]
    fn offer_is_stored_private_and_discarded() {
        let d = tmp("offer");
        let n = Nexus::open(Config { dir: d.clone(), sdk_paths: vec![], device_name: "dev".into(), poll_secs: 0 });
        let b = json!({"kind":"lemonade-nexus-invite","network":"work","server":"h.lan:9821",
            "genesis_key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","link_token":"12345678","expires_at": now()+600});
        let s = n.offer(&b.to_string()).unwrap();
        assert_eq!(s["server"], "h.lan:9821");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("offer.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(n.status()["offer"]["network"], "work");
        assert!(!n.status().to_string().contains("12345678"));
        n.discard_offer();
        assert!(n.status()["offer"].is_null());
    }

    #[test]
    fn device_names() {
        assert!(device_name_ok("s25-nautik9"));
        assert!(!device_name_ok("a;b"));
        assert!(!device_name_ok("---"));
        assert!(!device_name_ok(""));
    }
}
