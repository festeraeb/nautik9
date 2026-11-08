//! Remote care: the switch that lets the home fleet reach this server over Tailscale, and the
//! state behind it.
//!
//! `<config_dir>/remote_care.json` {"enabled": bool}, default off. The Android app reads the same
//! file at boot (BootReceiver) to decide whether to start itself.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::source_gate::{Class, Gate};

pub const FILE: &str = "remote_care.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCare {
    #[serde(default)]
    pub enabled: bool,
}

impl RemoteCare {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Write via a temp file and rename, so a reader (the app at boot) never sees half a file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_string(self).unwrap_or_default() + "\n")?;
        std::fs::rename(tmp, dir.join(FILE))
    }
}

/// Everything the admin routes and the source gate share. Cheap to clone.
#[derive(Clone)]
pub struct Admin {
    pub config_dir: PathBuf,
    pub update_dir: PathBuf,
    pub port: u16,
    pub gate: Gate,
    /// Device actions waiting for the Android app (it polls /api/admin/pending).
    pub pending: Arc<Mutex<Vec<Value>>>,
    /// Largest update the server will download (a run-time cap, not a constant in the handler).
    pub max_update_bytes: u64,
    /// One download at a time.
    pub update_lock: Arc<tokio::sync::Mutex<()>>,
}

pub const DEFAULT_MAX_UPDATE_BYTES: u64 = 200 * 1024 * 1024;

impl Admin {
    pub fn new(config_dir: &Path, update_dir: Option<PathBuf>, port: u16, allow: Vec<Class>) -> Self {
        Admin {
            config_dir: config_dir.to_path_buf(),
            update_dir: update_dir.unwrap_or_else(|| default_update_dir(config_dir)),
            port,
            gate: Gate { allow, remote_care: Arc::new(RwLock::new(RemoteCare::load(config_dir))) },
            pending: Default::default(),
            max_update_bytes: DEFAULT_MAX_UPDATE_BYTES,
            update_lock: Default::default(),
        }
    }

    pub fn remote_care(&self) -> RemoteCare {
        self.gate.remote_care.read().map(|r| *r).unwrap_or_default()
    }

    #[cfg(test)]
    pub fn for_test(dir: &Path) -> Self {
        Admin::new(dir, Some(dir.join("update")), 8340, vec![Class::Loopback, Class::Tailnet])
    }
}

/// `<config_dir>/../update` (on the phone the app passes its own files dir explicitly).
pub fn default_update_dir(config_dir: &Path) -> PathBuf {
    config_dir.parent().map(|p| p.join("update")).unwrap_or_else(|| config_dir.join("update"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_care_defaults_off_and_survives_a_restart() {
        let d = tempfile::tempdir().unwrap();
        assert!(!RemoteCare::load(d.path()).enabled);
        std::fs::write(d.path().join(FILE), "not json").unwrap();
        assert!(!RemoteCare::load(d.path()).enabled, "a broken file means off");
        RemoteCare { enabled: true }.save(d.path()).unwrap();
        assert!(RemoteCare::load(d.path()).enabled);
        assert!(Admin::new(d.path(), None, 8340, vec![Class::Loopback]).remote_care().enabled);
        assert_eq!(default_update_dir(Path::new("/data/files/config")), PathBuf::from("/data/files/update"));
    }
}
