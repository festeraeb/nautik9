//! Text to speech: `POST /api/agent/tts {text, voice?}` forwards to an OpenAI-compatible
//! `/v1/audio/speech` endpoint set in `agent.json` and returns the audio bytes.
//!
//! ```json
//! "tts": { "url": "http://<host>:<port>/v1", "model": "kokoro", "voice": "af_sky",
//!          "format": "mp3", "api_key_env": "NAUTIK9_TTS_KEY" }
//! ```
//! `url` may be the base (`.../v1`), the host root, or the full `.../audio/speech` URL.
//! With no `tts` entry, or when the endpoint fails, the answer is 502 with the reason and no
//! audio; the web page then offers the browser's own `speechSynthesis`, labelled as such.
//! Nothing is spoken by a made-up fallback on the server.

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use crate::webxdc_api::Runtime;
use crate::AppState;

const MAX_TEXT_CHARS: usize = 4000;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/agent/tts", post(tts))
}

fn fail(code: StatusCode, e: impl std::fmt::Display) -> Response {
    (code, Json(json!({"ok": false, "error": e.to_string()}))).into_response()
}

/// The speech URL for a configured `url`.
pub fn speech_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.ends_with("/audio/speech") {
        u.to_string()
    } else if u.ends_with("/v1") {
        format!("{u}/audio/speech")
    } else {
        format!("{u}/v1/audio/speech")
    }
}

fn content_type_for(format: &str) -> &'static str {
    match format {
        "wav" => "audio/wav",
        "opus" | "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "pcm" => "audio/L16",
        _ => "audio/mpeg",
    }
}

async fn tts(State(_st): State<AppState>, rt: Option<Extension<Runtime>>, Json(p): Json<Value>) -> Response {
    let Some(text) = p["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
        return fail(StatusCode::BAD_REQUEST, "missing `text`");
    };
    if text.chars().count() > MAX_TEXT_CHARS {
        return fail(StatusCode::BAD_REQUEST, format!("text is over {MAX_TEXT_CHARS} characters"));
    }
    let Some(Extension(rt)) = rt else { return fail(StatusCode::BAD_GATEWAY, "no TTS endpoint configured (no config directory)") };
    let conf: Value = match std::fs::read_to_string(rt.config_dir.join("agent.json")) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(v) => v,
            Err(e) => return fail(StatusCode::BAD_GATEWAY, format!("agent.json: {e}")),
        },
        Err(_) => Value::Null,
    };
    let t = &conf["tts"];
    let Some(url) = t["url"].as_str().filter(|u| !u.is_empty()) else {
        return fail(StatusCode::BAD_GATEWAY, "no TTS endpoint configured (agent.json `tts.url`); the browser can read the text aloud itself");
    };
    let format = t["format"].as_str().unwrap_or("mp3");
    let mut body = json!({
        "model": t["model"].as_str().unwrap_or("tts-1"),
        "input": text,
        "voice": p["voice"].as_str().or(t["voice"].as_str()).unwrap_or("alloy"),
        "response_format": format,
    });
    if let Some(sp) = t["speed"].as_f64() {
        body["speed"] = json!(sp);
    }
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(90)).build() {
        Ok(c) => c,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let target = speech_url(url);
    let mut req = client.post(&target).json(&body);
    if let Some(key) = t["api_key_env"].as_str().and_then(|v| std::env::var(v).ok()).filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return fail(StatusCode::BAD_GATEWAY, format!("TTS endpoint {target} did not answer: {e}")),
    };
    let status = resp.status();
    let upstream_type = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return fail(StatusCode::BAD_GATEWAY, format!("TTS endpoint {target}: reading the audio failed: {e}")),
    };
    if !status.is_success() {
        let snippet: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        return fail(StatusCode::BAD_GATEWAY, format!("TTS endpoint {target} answered HTTP {status}: {snippet}"));
    }
    if bytes.is_empty() {
        return fail(StatusCode::BAD_GATEWAY, format!("TTS endpoint {target} returned no audio"));
    }
    let ctype = upstream_type.filter(|t| t.starts_with("audio/")).unwrap_or_else(|| content_type_for(format).to_string());
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-store".to_string())], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webxdc_api::tests::{test_app, TOKEN};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[test]
    fn url_forms() {
        assert_eq!(speech_url("http://127.0.0.1:8880/v1"), "http://127.0.0.1:8880/v1/audio/speech");
        assert_eq!(speech_url("http://127.0.0.1:8880/"), "http://127.0.0.1:8880/v1/audio/speech");
        assert_eq!(speech_url("http://h/v1/audio/speech"), "http://h/v1/audio/speech");
    }

    async fn post_tts(app: &Router, body: Value) -> (StatusCode, String, Vec<u8>) {
        let r = app.clone().oneshot(Request::builder().method("POST").uri("/api/agent/tts").header("authorization", format!("Bearer {TOKEN}")).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        (r.status(), ct, r.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    #[tokio::test]
    async fn honest_502_without_config_and_proxy_with_one() {
        let d = tempfile::tempdir().unwrap();
        let app = test_app(d.path());
        let (s, _, b) = post_tts(&app, json!({"text": "hello"})).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&b).contains("no TTS endpoint configured"));
        assert_eq!(post_tts(&app, json!({"text": "  "})).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(post_tts(&app, json!({"text": "x".repeat(4001)})).await.0, StatusCode::BAD_REQUEST);

        // a mock OpenAI-compatible speech server
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
        let s2 = seen.clone();
        let mock = Router::new().route(
            "/v1/audio/speech",
            post(move |h: axum::http::HeaderMap, Json(b): Json<Value>| {
                let s = s2.clone();
                async move {
                    s.lock().unwrap().push((h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string(), b.clone()));
                    if b["input"] == "boom" {
                        return (StatusCode::INTERNAL_SERVER_ERROR, "model not loaded").into_response();
                    }
                    ([(header::CONTENT_TYPE, "audio/mpeg")], vec![0x49u8, 0x44, 0x33, 1, 2, 3]).into_response()
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, mock).await.unwrap() });
        std::env::set_var("N9_TTS_TEST_KEY", "sekret-key");
        std::fs::write(d.path().join("agent.json"), json!({"tts": {"url": base, "model": "kokoro", "voice": "af_sky", "api_key_env": "N9_TTS_TEST_KEY"}}).to_string()).unwrap();

        let (s, ct, b) = post_tts(&app, json!({"text": "hello there"})).await;
        assert_eq!((s, ct.as_str(), b.as_slice()), (StatusCode::OK, "audio/mpeg", [0x49u8, 0x44, 0x33, 1, 2, 3].as_slice()));
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent[0].0, "Bearer sekret-key");
        assert_eq!((sent[0].1["model"].as_str(), sent[0].1["voice"].as_str(), sent[0].1["input"].as_str()), (Some("kokoro"), Some("af_sky"), Some("hello there")));
        // per-call voice wins
        post_tts(&app, json!({"text": "hi", "voice": "bm_george"})).await;
        assert_eq!(seen.lock().unwrap()[1].1["voice"], "bm_george");
        // upstream error is passed on as 502 with the reason, never as audio
        let (s, _, b) = post_tts(&app, json!({"text": "boom"})).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&b).contains("model not loaded"));
        // unreachable endpoint
        std::fs::write(d.path().join("agent.json"), json!({"tts": {"url": "http://127.0.0.1:1/v1"}}).to_string()).unwrap();
        let (s, _, b) = post_tts(&app, json!({"text": "hello"})).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&b).contains("did not answer"));
    }
}
