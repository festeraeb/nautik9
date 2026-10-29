//! Bearer-token auth for the HTTP API.
//!
//! The token is generated on first run and kept in a 0600 file. Every
//! `/api/*` route except `/api/health` (and the `/assets` image) needs
//! `Authorization: Bearer <token>` or `?token=<token>`.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::path::{Path, PathBuf};

/// `~/.config/nautik9` (or `$XDG_CONFIG_HOME/nautik9`).
pub fn default_config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("nautik9");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("nautik9")
}

/// Read the token file, or create it (random, mode 0600) if absent.
/// Returns (token, created_now).
pub fn load_or_create_token(path: &Path) -> std::io::Result<(String, bool)> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok((t, false));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = [0u8; 32];
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    }
    let token: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(token.as_bytes())?;
        f.write_all(b"\n")?;
    }
    // Tighten the mode even if the file already existed with a looser one.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((token, true))
}

/// Constant-time comparison (length is not secret here: tokens are fixed size).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn query_token(query: Option<&str>) -> Option<String> {
    query?.split('&').find_map(|kv| kv.strip_prefix("token=")).map(|v| v.to_string())
}

/// Paths that never need a token: the HTML shell (no data in it) and health.
fn is_open(path: &str) -> bool {
    path == "/" || path == "/api/health" || path.starts_with("/xdc/") // /xdc/ files carry their own view key
}

pub async fn require_token(State(token): State<std::sync::Arc<String>>, req: Request, next: Next) -> Response {
    if is_open(req.uri().path()) {
        return next.run(req).await;
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string());
    let supplied = bearer.or_else(|| query_token(req.uri().query()));
    match supplied {
        Some(t) if ct_eq(t.as_bytes(), token.as_bytes()) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({"ok": false, "error": "missing or wrong token"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_created_once_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("token");
        let (t1, created) = load_or_create_token(&p).unwrap();
        assert!(created);
        assert_eq!(t1.len(), 64);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let (t2, created2) = load_or_create_token(&p).unwrap();
        assert!(!created2);
        assert_eq!(t1, t2);
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }

    #[test]
    fn query_token_parsing() {
        assert_eq!(query_token(Some("a=1&token=xyz")), Some("xyz".into()));
        assert_eq!(query_token(Some("sync=true")), None);
        assert_eq!(query_token(None), None);
    }
}
