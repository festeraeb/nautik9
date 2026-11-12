//! Who may talk to the server at all, judged by the peer address, before the token is looked at.
//!
//!   loopback  127.0.0.0/8, ::1                         (the app's own WebView, adb forward)
//!   tailnet   100.64.0.0/10, fd7a:115c:a1e0::/48       (Tailscale)
//!   lan       10/8, 172.16/12, 192.168/16, fe80::/10, fc00::/7 (minus the tailnet range)
//!
//! `serve --allow-from loopback,tailnet` lists the classes that may connect; anything else gets
//! 403 with an empty body. Tailnet sources are also refused while remote care is off (the switch
//! in Settings, `remote_care.json`), whatever `--allow-from` says. `/api/health` needs no token and
//! no remote care, but it still has to come from a listed class: from Wi-Fi it is refused too, so
//! a café network learns nothing; over Tailscale it tells the fleet the app is up even when remote
//! care is off.
//!
//! A carrier's CGNAT address is also in 100.64.0.0/10 (see Bridge.java `tailscaleUp`). For this
//! gate that is acceptable: the phone binds its own listener, and the packets that arrive with a
//! 100.64/10 source are, in practice, delivered by Tailscale's tun interface. A carrier that lets
//! subscribers in the same CGNAT pool reach each other could also deliver one; such a peer still
//! needs the 64-hex token, so this gate is the first wall, not the only one. Checking that the
//! connection arrived on the Tailscale interface's own address would close that gap (see the
//! open item in android/README.md, "Remote care").

use axum::extract::connect_info::{ConnectInfo, MockConnectInfo};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Class {
    Loopback,
    Tailnet,
    Lan,
}

/// The class of a peer address, or None for anything else (the internet).
pub fn classify(ip: IpAddr) -> Option<Class> {
    match ip.to_canonical() {
        IpAddr::V4(v) => {
            let o = v.octets();
            if o[0] == 127 {
                Some(Class::Loopback)
            } else if o[0] == 100 && (o[1] & 0xC0) == 64 {
                Some(Class::Tailnet)
            } else if o[0] == 10 || (o[0] == 172 && (o[1] & 0xF0) == 16) || (o[0] == 192 && o[1] == 168) {
                Some(Class::Lan)
            } else {
                None
            }
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            if v.is_loopback() {
                Some(Class::Loopback)
            } else if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                Some(Class::Tailnet)
            } else if (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xfe00) == 0xfc00 {
                Some(Class::Lan)
            } else {
                None
            }
        }
    }
}

/// The run-time half of the gate: which classes `--allow-from` listed, and the remote-care switch.
#[derive(Clone, Debug)]
pub struct Gate {
    pub allow: Vec<Class>,
    pub remote_care: std::sync::Arc<std::sync::RwLock<crate::admin_api::RemoteCare>>,
}

impl Gate {
    pub fn allows(&self, ip: IpAddr, path: &str) -> bool {
        let Some(class) = classify(ip) else { return false };
        if !self.allow.contains(&class) {
            return false;
        }
        match class {
            Class::Tailnet => path == "/api/health" || self.remote_care.read().map(|r| r.enabled).unwrap_or(false),
            _ => true,
        }
    }
}

/// The peer of this request: real (`into_make_service_with_connect_info`) or a test's mock.
pub(crate) fn peer(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| req.extensions().get::<MockConnectInfo<SocketAddr>>().map(|m| m.0.ip()))
}

/// Middleware: runs before `auth::require_token`. No peer address known = refused.
pub async fn gate(State(g): State<Gate>, req: Request, next: Next) -> Response {
    match peer(&req) {
        Some(ip) if g.allows(ip, req.uri().path()) => next.run(req).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// Tests: a request that came from this machine.
#[cfg(test)]
pub fn loopback_peer() -> MockConnectInfo<SocketAddr> {
    MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classes() {
        for (a, c) in [
            ("127.0.0.1", Some(Class::Loopback)),
            ("127.9.9.9", Some(Class::Loopback)),
            ("::1", Some(Class::Loopback)),
            ("::ffff:127.0.0.1", Some(Class::Loopback)),
            ("100.64.0.1", Some(Class::Tailnet)),
            ("100.72.182.77", Some(Class::Tailnet)),
            ("100.127.255.254", Some(Class::Tailnet)),
            ("::ffff:100.72.127.73", Some(Class::Tailnet)),
            ("fd7a:115c:a1e0::1", Some(Class::Tailnet)),
            ("fd7a:115c:a1e0:ab12::99", Some(Class::Tailnet)),
            ("100.63.255.255", None),
            ("100.128.0.1", None),
            ("10.1.2.3", Some(Class::Lan)),
            ("172.16.0.1", Some(Class::Lan)),
            ("172.31.255.255", Some(Class::Lan)),
            ("172.32.0.1", None),
            ("192.168.4.20", Some(Class::Lan)),
            ("fe80::1", Some(Class::Lan)),
            ("fd00::5", Some(Class::Lan)),
            ("fd7a:115c:a1e1::1", Some(Class::Lan)),
            ("8.8.8.8", None),
            ("2001:4860::8888", None),
        ] {
            assert_eq!(classify(ip(a)), c, "{a}");
        }
    }

    fn gate(allow: &[Class], on: bool) -> Gate {
        Gate { allow: allow.to_vec(), remote_care: std::sync::Arc::new(std::sync::RwLock::new(crate::admin_api::RemoteCare { enabled: on })) }
    }

    #[test]
    fn allow_list_and_remote_care_switch() {
        let g = gate(&[Class::Loopback], true);
        assert!(g.allows(ip("127.0.0.1"), "/api/status"));
        assert!(!g.allows(ip("100.72.182.77"), "/api/status"), "tailnet not listed");
        assert!(!g.allows(ip("192.168.4.20"), "/api/health"), "lan not listed, not even health");

        let g = gate(&[Class::Loopback, Class::Tailnet], false);
        assert!(!g.allows(ip("100.72.182.77"), "/api/status"), "remote care off refuses tailnet");
        assert!(g.allows(ip("100.72.182.77"), "/api/health"), "health answers the fleet with remote care off");
        assert!(!g.allows(ip("192.168.4.20"), "/api/status"));
        assert!(!g.allows(ip("8.8.8.8"), "/api/health"));

        let g = gate(&[Class::Loopback, Class::Tailnet], true);
        assert!(g.allows(ip("100.72.182.77"), "/api/status"));
        assert!(g.allows(ip("fd7a:115c:a1e0::5"), "/"));
        assert!(!g.allows(ip("10.0.0.2"), "/api/status"), "Wi-Fi stays out with remote care on");

        let g = gate(&[Class::Lan], true);
        assert!(g.allows(ip("10.0.0.2"), "/api/status"));
        assert!(!g.allows(ip("127.0.0.1"), "/api/status"), "only what is listed");
    }

    /// The full router, with the request arriving from `from`.
    async fn through_router(dir: &std::path::Path, from: &str, path: &str, token: Option<&str>) -> (StatusCode, Vec<u8>) {
        use axum::body::Body;
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let app = crate::build_router(crate::AppState::test_state(dir, hw, "http://127.0.0.1:1"), "tok-0123456789".into())
            .layer(MockConnectInfo(SocketAddr::new(ip(from), 51000)));
        let mut b = axum::http::Request::builder().uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let r = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        let st = r.status();
        (st, r.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    #[tokio::test]
    async fn the_router_refuses_by_source_before_the_token_and_follows_the_switch() {
        let d = tempfile::tempdir().unwrap();
        let tok = Some("tok-0123456789");
        // test_state allows loopback and tailnet; remote care starts off.
        assert_eq!(through_router(d.path(), "127.0.0.1", "/api/net", tok).await.0, StatusCode::OK);
        assert_eq!(through_router(d.path(), "127.0.0.1", "/api/net", None).await.0, StatusCode::UNAUTHORIZED, "loopback still needs the token");
        let (s, body) = through_router(d.path(), "192.168.4.20", "/api/net", tok).await;
        assert_eq!((s, body.len()), (StatusCode::FORBIDDEN, 0), "Wi-Fi: 403, empty body, even with the right token");
        assert_eq!(through_router(d.path(), "192.168.4.20", "/api/health", None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(through_router(d.path(), "192.168.4.20", "/", None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(through_router(d.path(), "8.8.8.8", "/api/health", None).await.0, StatusCode::FORBIDDEN);
        let (s, body) = through_router(d.path(), "100.72.182.77", "/api/net", tok).await;
        assert_eq!((s, body.len()), (StatusCode::FORBIDDEN, 0), "remote care off: tailnet refused");
        assert_eq!(through_router(d.path(), "100.72.182.77", "/api/health", None).await.0, StatusCode::OK, "health still answers over the tailnet");

        crate::admin_api::RemoteCare { enabled: true }.save(d.path()).unwrap();
        assert_eq!(through_router(d.path(), "100.72.182.77", "/api/net", tok).await.0, StatusCode::OK, "remote care on");
        assert_eq!(through_router(d.path(), "100.72.182.77", "/api/net", None).await.0, StatusCode::UNAUTHORIZED, "and the token is still needed");
        assert_eq!(through_router(d.path(), "10.0.0.7", "/api/net", tok).await.0, StatusCode::FORBIDDEN, "Wi-Fi stays out");
    }
}
