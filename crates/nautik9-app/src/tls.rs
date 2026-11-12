//! Optional HTTPS for `serve` (rustls). Plain HTTP stays the default.
//!
//!   --tls                      HTTPS with a certificate kept in the config dir
//!                              (`tls-cert.pem` + `tls-key.pem`, made on first run, self-signed)
//!   --tls-cert F --tls-key F   HTTPS with your own PEM files (implies --tls)
//!
//! A self-signed certificate encrypts the traffic but the browser or phone has to be told to
//! trust it once (it does not prove who you are talking to). Use your own certificate, for
//! example from a private CA, when you want that.

use std::path::{Path, PathBuf};

use axum_server::tls_rustls::RustlsConfig;

/// Where the certificate and key are read from.
#[derive(Clone, Debug, PartialEq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
    /// True when this run created them (self-signed).
    pub generated: bool,
}

/// Use the given files, or the config dir's pair, making a self-signed pair when absent.
/// `names` are extra host names / IPs the generated certificate is valid for.
pub fn resolve_files(config_dir: &Path, cert: Option<&Path>, key: Option<&Path>, names: &[String]) -> Result<TlsFiles, String> {
    match (cert, key) {
        (Some(c), Some(k)) => {
            for p in [c, k] {
                if !p.is_file() {
                    return Err(format!("TLS file {} does not exist", p.display()));
                }
            }
            Ok(TlsFiles { cert: c.to_path_buf(), key: k.to_path_buf(), generated: false })
        }
        (Some(_), None) | (None, Some(_)) => Err("--tls-cert and --tls-key go together".into()),
        (None, None) => {
            let (c, k) = (config_dir.join("tls-cert.pem"), config_dir.join("tls-key.pem"));
            if c.is_file() && k.is_file() {
                return Ok(TlsFiles { cert: c, key: k, generated: false });
            }
            generate_self_signed(&c, &k, names)?;
            Ok(TlsFiles { cert: c, key: k, generated: true })
        }
    }
}

fn generate_self_signed(cert: &Path, key: &Path, names: &[String]) -> Result<(), String> {
    let mut sans: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    for n in names {
        if !n.is_empty() && !sans.contains(n) {
            sans.push(n.clone());
        }
    }
    let ck = rcgen::generate_simple_self_signed(sans).map_err(|e| format!("cannot make a self-signed certificate: {e}"))?;
    if let Some(p) = cert.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    write_private(key, ck.signing_key.serialize_pem().as_bytes())?;
    std::fs::write(cert, ck.cert.pem()).map_err(|e| format!("{}: {e}", cert.display()))
}

fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(data).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Serve `app` over HTTPS on an already-bound listener.
pub async fn serve(listener: std::net::TcpListener, app: axum::Router, files: &TlsFiles) -> Result<(), String> {
    // Ring is the only provider compiled in; installing twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cfg = RustlsConfig::from_pem_file(&files.cert, &files.key).await.map_err(|e| format!("TLS cert/key unusable ({} / {}): {e}", files.cert.display(), files.key.display()))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    axum_server::from_tcp_rustls(listener, cfg).map_err(|e| e.to_string())?.serve(app.into_make_service()).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn self_signed_pair_is_made_once_with_a_private_key() {
        let d = tempfile::tempdir().unwrap();
        let a = resolve_files(d.path(), None, None, &["192.0.2.7".into()]).unwrap();
        assert!(a.generated);
        assert_eq!(std::fs::metadata(&a.key).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(std::fs::read_to_string(&a.cert).unwrap().contains("BEGIN CERTIFICATE"));
        let before = std::fs::read(&a.cert).unwrap();
        let b = resolve_files(d.path(), None, None, &[]).unwrap();
        assert!(!b.generated);
        assert_eq!(std::fs::read(&b.cert).unwrap(), before, "second run reuses the pair");
    }

    #[test]
    fn explicit_paths_must_exist_and_come_as_a_pair() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.pem");
        std::fs::write(&f, "x").unwrap();
        assert!(resolve_files(d.path(), Some(&f), None, &[]).is_err());
        assert!(resolve_files(d.path(), Some(&f), Some(&d.path().join("missing.pem")), &[]).is_err());
        let r = resolve_files(d.path(), Some(&f), Some(&f), &[]).unwrap();
        assert!(!r.generated);
    }

    #[tokio::test]
    async fn the_tls_listener_serves_health_and_keeps_the_token_rule() {
        let d = tempfile::tempdir().unwrap();
        let files = resolve_files(d.path(), None, None, &[]).unwrap();
        let app = axum::Router::new()
            .route("/api/health", axum::routing::get(|| async { axum::Json(serde_json::json!({"ok": true, "service": "nautik9"})) }))
            .route("/api/status", axum::routing::get(|| async { "secret" }))
            .layer(axum::middleware::from_fn_with_state(std::sync::Arc::new("tok".to_string()), crate::auth::require_token));
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let f2 = files.clone();
        tokio::spawn(async move { serve(l, app, &f2).await.unwrap() });

        // Trust exactly the generated certificate.
        let pem = std::fs::read(&files.cert).unwrap();
        let client = reqwest::Client::builder().add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap()).build().unwrap();
        let mut last = None;
        for _ in 0..50 {
            match client.get(format!("https://127.0.0.1:{port}/api/health")).send().await {
                Ok(r) => {
                    last = Some(r);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        let r = last.expect("TLS listener answered");
        assert!(r.status().is_success());
        assert_eq!(r.json::<serde_json::Value>().await.unwrap()["service"], "nautik9");
        let r = client.get(format!("https://127.0.0.1:{port}/api/status")).send().await.unwrap();
        assert_eq!(r.status(), 401);
        // Plain HTTP to the TLS port gets no answer.
        let plain = reqwest::Client::new().get(format!("http://127.0.0.1:{port}/api/health")).send().await;
        assert!(plain.map(|r| !r.status().is_success()).unwrap_or(true));
    }
}
