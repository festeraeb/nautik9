//! What a device needs to join, read from what the user pastes, loads or is sent.
//!
//! Accepted forms:
//! - an invite bundle: `{"kind":"lemonade-nexus-invite","v":1,"network","server","genesis_key",
//!   "ca_pem"?, "link_token" | "code", "expires_at"?, "permissions"?, "address_map"?}`
//!   (made by `nexus-invite` on a joined fleet machine);
//! - the sidecar's invite file (`link_token`, `code`, `expires_at`, `issued_by`, ...);
//! - a bare 8-digit code or `lnk_` token, when the network is already known on this device
//!   (`network.json`, written at the last join).
//! Missing server or Genesis key is an error, never a guess.

use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub network: String,
    pub host: String,
    pub port: u16,
    pub genesis_key: String,
    pub ca_pem: Option<String>,
    /// The single-use secret (8-digit code or lnk_ token). Never shown, never logged.
    pub token: String,
    pub expires_at: Option<u64>,
    pub permissions: Vec<String>,
    pub address_map: Map<String, Value>,
}

/// Network facts kept after a join (no secrets): lets a bare code join the same network again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KnownNetwork {
    pub network: String,
    pub server: String,
    pub genesis_key: String,
    pub ca_pem: Option<String>,
    pub address_map: Map<String, Value>,
}

impl KnownNetwork {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(KnownNetwork {
            network: v["network"].as_str()?.to_string(),
            server: v["server"].as_str()?.to_string(),
            genesis_key: v["genesis_key"].as_str()?.to_string(),
            ca_pem: v["ca_pem"].as_str().map(str::to_string),
            address_map: v["address_map"].as_object().cloned().unwrap_or_default(),
        })
    }
    pub fn to_json(&self) -> Value {
        json!({"network": self.network, "server": self.server, "genesis_key": self.genesis_key,
               "ca_pem": self.ca_pem, "address_map": self.address_map})
    }
}

pub fn split_host_port(s: &str) -> Result<(String, u16), String> {
    let s = s.trim().trim_start_matches("https://").trim_end_matches('/');
    let (h, p) = s.rsplit_once(':').ok_or_else(|| format!("server '{s}' has no port"))?;
    let port: u16 = p.parse().map_err(|_| format!("server '{s}' has a bad port"))?;
    if h.is_empty() || h.contains('/') || h.contains(' ') {
        return Err(format!("server '{s}' is not host:port"));
    }
    Ok((h.to_string(), port))
}

fn token_ok(t: &str) -> bool {
    let code = t.len() == 8 && t.bytes().all(|b| b.is_ascii_digit());
    let lnk = t.len() == 68 && t.starts_with("lnk_") && t[4..].bytes().all(|b| b.is_ascii_hexdigit());
    code || lnk
}

/// Read an offer from text. `known` fills server/Genesis/CA for the short forms.
pub fn parse(text: &str, known: Option<&KnownNetwork>) -> Result<Offer, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty invite".into());
    }
    if !t.starts_with('{') {
        let tok: String = t.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
        if !token_ok(&tok) {
            return Err("not an invite: expected the invite file, an 8-digit code or an lnk_ token".into());
        }
        let k = known.ok_or("a bare code needs the network's server and key: load the full invite file once")?;
        let (host, port) = split_host_port(&k.server)?;
        return Ok(Offer {
            network: k.network.clone(),
            host,
            port,
            genesis_key: k.genesis_key.clone(),
            ca_pem: k.ca_pem.clone(),
            token: tok,
            expires_at: None,
            permissions: vec![],
            address_map: k.address_map.clone(),
        });
    }
    let v: Value = serde_json::from_str(t).map_err(|e| format!("invite is not valid JSON: {e}"))?;
    let token = v["link_token"].as_str().or(v["code"].as_str()).unwrap_or("").to_string();
    if !token_ok(&token) {
        return Err("invite has no usable link_token or code".into());
    }
    let server = v["server"].as_str().or(v["issued_by"].as_str()).map(str::to_string).or_else(|| known.map(|k| k.server.clone()));
    let server = server.ok_or("invite does not name its server (server or issued_by)")?;
    let (host, port) = split_host_port(&server)?;
    let genesis = v["genesis_key"].as_str().map(str::to_string).or_else(|| known.map(|k| k.genesis_key.clone()));
    let genesis_key = genesis.ok_or("invite has no genesis_key and this network is not known here yet")?;
    if genesis_key.len() < 40 || genesis_key.len() > 64 {
        return Err("genesis_key does not look like base64 of 32 bytes".into());
    }
    let ca_pem = v["ca_pem"].as_str().map(str::to_string).or_else(|| known.and_then(|k| k.ca_pem.clone()));
    if let Some(ca) = &ca_pem {
        if !ca.contains("-----BEGIN CERTIFICATE-----") {
            return Err("ca_pem is not a PEM certificate".into());
        }
    }
    let network = v["network"].as_str().map(str::to_string).or_else(|| known.map(|k| k.network.clone())).unwrap_or_else(|| host.clone());
    Ok(Offer {
        network,
        host,
        port,
        genesis_key,
        ca_pem,
        token,
        expires_at: v["expires_at"].as_u64(),
        permissions: v["permissions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        address_map: v["address_map"].as_object().cloned().or_else(|| known.map(|k| k.address_map.clone())).unwrap_or_default(),
    })
}

impl Offer {
    /// What the user is shown before confirming: no secret in it.
    pub fn summary(&self, now: u64) -> Value {
        json!({
            "network": self.network,
            "server": format!("{}:{}", self.host, self.port),
            "expires_at": self.expires_at,
            "expired": self.expires_at.map(|e| e <= now).unwrap_or(false),
            "permissions": self.permissions,
            "own_ca": self.ca_pem.is_some(),
            "address_map": self.address_map.keys().cloned().collect::<Vec<_>>(),
        })
    }
    pub fn to_json(&self) -> Value {
        json!({"kind": "lemonade-nexus-invite", "v": 1, "network": self.network,
               "server": format!("{}:{}", self.host, self.port), "genesis_key": self.genesis_key,
               "ca_pem": self.ca_pem, "link_token": self.token, "expires_at": self.expires_at,
               "permissions": self.permissions, "address_map": self.address_map})
    }
    pub fn known(&self) -> KnownNetwork {
        KnownNetwork {
            network: self.network.clone(),
            server: format!("{}:{}", self.host, self.port),
            genesis_key: self.genesis_key.clone(),
            ca_pem: self.ca_pem.clone(),
            address_map: self.address_map.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const G: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const TOK: &str = "lnk_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn bundle_parses_and_summary_has_no_secret() {
        let b = json!({"kind":"lemonade-nexus-invite","v":1,"network":"work","server":"w1.example.lan:9821",
                       "genesis_key":G,"link_token":TOK,"expires_at":100,"permissions":["read"]});
        let o = parse(&b.to_string(), None).unwrap();
        assert_eq!((o.host.as_str(), o.port), ("w1.example.lan", 9821));
        let s = o.summary(200).to_string();
        assert!(!s.contains("lnk_"));
        assert!(s.contains("\"expired\":true"));
    }

    #[test]
    fn sidecar_file_needs_a_known_network_for_the_key() {
        let raw = json!({"link_token":TOK,"code":"12345678","issued_by":"w1.example.lan:9821","expires_at":5});
        assert!(parse(&raw.to_string(), None).unwrap_err().contains("genesis_key"));
        let k = KnownNetwork { network: "work".into(), server: "w1.example.lan:9821".into(), genesis_key: G.into(), ..Default::default() };
        let o = parse(&raw.to_string(), Some(&k)).unwrap();
        assert_eq!(o.token, TOK);
    }

    #[test]
    fn bare_code_and_garbage() {
        let k = KnownNetwork { network: "work".into(), server: "h:1".into(), genesis_key: G.into(), ..Default::default() };
        assert_eq!(parse(" 1234-5678 ", Some(&k)).unwrap().token, "12345678");
        assert!(parse("12345678", None).is_err());
        assert!(parse("hello", Some(&k)).is_err());
        assert!(parse("{\"link_token\":\"nope\"}", Some(&k)).is_err());
    }
}
