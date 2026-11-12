//! The Apps surface and the UI settings that are kept on the server.
//!
//!   GET  /api/apps                      built-in apps (Wayfinder) with what works in them
//!   POST /api/apps/wayfinder/invoke     {cmd, args}: Wayfinder's UI talks Tauri commands; here the
//!                                       ones NautiK9 can really answer are answered, every other
//!                                       command is a plain error (never a stub result)
//!   GET  /api/ui, POST /api/ui          {theme: dark|lakes|dead, plain: bool} in `ui.json`
//!
//! Wayfinder's web build is served from `/static/apps/wayfinder/` (compiled into the binary).
//! Its desktop build scans, embeds, clusters and searches local files through Rust commands;
//! a phone or browser host has no such files, so those commands are reported as unavailable.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::time::Duration;

use crate::webxdc_api::Runtime;
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apps", get(list))
        .route("/api/apps/wayfinder/invoke", post(wayfinder_invoke))
        .route("/api/ui", get(ui_get).post(ui_set))
}

/// Commands this host implements for Wayfinder.
pub const WF_SUPPORTED: &[&str] = &["check_lemonade_setup", "chat_llama", "get_system_info"];

const WF_UNSUPPORTED: &str = "scanning, embedding, clustering and searching local files, snapshots, file moves, reminders, the git assistant and the file watcher (they read the computer's own files).";

async fn list() -> Json<Value> {
    Json(json!([{
        "id": "wayfinder",
        "name": "Wayfinder",
        "kind": "builtin",
        "url": "/static/apps/wayfinder/index.html",
        "trusted": true,
        "works": WF_SUPPORTED,
        "not_available": WF_UNSUPPORTED,
        "summary": "Wayfinder's web build, served by this app. Chat and the Lemonade setup check run through NautiK9's provider chain."
    }]))
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

/// `/v1` base for an OpenAI-style endpoint given as `http://host:port`, `.../api` or `.../v1`.
pub fn v1_base(raw: &str) -> String {
    let b = raw.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        b.to_string()
    } else if let Some(x) = b.strip_suffix("/api") {
        format!("{x}/api/v1")
    } else {
        format!("{b}/v1")
    }
}

fn s_arg(args: &Value, camel: &str, snake: &str) -> Option<String> {
    args.get(camel).or_else(|| args.get(snake)).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

async fn wayfinder_invoke(State(st): State<AppState>, Json(p): Json<Value>) -> Response {
    let cmd = p["cmd"].as_str().unwrap_or("");
    let args = p.get("args").cloned().unwrap_or(json!({}));
    match cmd {
        "__capabilities" => Json(json!({"result": {"supported": WF_SUPPORTED, "unsupported_summary": WF_UNSUPPORTED}})).into_response(),
        "get_system_info" => Json(json!({"result": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "host": "nautik9"}})).into_response(),
        "chat_llama" => {
            let prompt = args["prompt"].as_str().unwrap_or("").to_string();
            if prompt.trim().is_empty() {
                return err(StatusCode::BAD_REQUEST, "empty prompt");
            }
            let sys = "You are Wayfinder's assistant, running inside NautiK9. Answer briefly and honestly.";
            match st.lemonade.chat(&prompt, Some(sys), false).await {
                Ok(a) => Json(json!({"result": {"text": a.reply, "model_used": a.label(), "provider": a.provider, "model": a.model, "host": "nautik9"}})).into_response(),
                // `attempts` lets the page see that only the on-device model was asleep and wake it.
                Err(f) => (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": format!("{} ({})", f.error, f.attempts.join("; ")), "attempts": f.attempts}))).into_response(),
            }
        }
        "check_lemonade_setup" => {
            let r = check_setup(&st, s_arg(&args, "base", "base"), s_arg(&args, "chatModel", "chat_model"), s_arg(&args, "embedModel", "embed_model")).await;
            Json(json!({"result": r})).into_response()
        }
        other => err(
            StatusCode::NOT_IMPLEMENTED,
            format!("'{other}' is not available in NautiK9: it needs the Wayfinder desktop app (it works on the computer's own files)."),
        ),
    }
}

/// The endpoint a setup check with no `base` looks at: the first remote provider, else (the phone's
/// generated providers.json holds only an `offline: true` on-device entry) the first provider.
fn setup_provider(providers: &[nautik9_lemonade::Provider]) -> Option<&nautik9_lemonade::Provider> {
    providers.iter().find(|p| !p.offline).or_else(|| providers.first())
}

/// Same report shape as Wayfinder's desktop `check_lemonade_setup`, from real requests only.
async fn check_setup(st: &AppState, base: Option<String>, chat_model: Option<String>, embed_model: Option<String>) -> Value {
    let providers = &st.lemonade.config.providers;
    let prov = setup_provider(providers);
    let base_url = match (&base, prov) {
        (Some(b), _) => v1_base(b),
        (None, Some(p)) => p.base_url.trim_end_matches('/').to_string(),
        (None, None) => String::new(),
    };
    let mut problems: Vec<String> = Vec::new();
    if base_url.is_empty() {
        problems.push("no model endpoint is configured in providers.json".into());
        return json!({"all_ok": false, "base": "", "models_ok": false, "problems": problems});
    }
    // The configured key belongs to the configured server: send it only to a matching base.
    let key = providers.iter().find(|p| p.base_url.trim_end_matches('/') == base_url).and_then(|p| p.api_key.clone()).filter(|k| !k.is_empty());
    let client = reqwest::Client::builder().timeout(Duration::from_secs(8)).connect_timeout(Duration::from_secs(3)).build().unwrap_or_default();
    let auth = |rb: reqwest::RequestBuilder| match &key {
        Some(k) => rb.bearer_auth(k),
        None => rb,
    };
    let chat_model = chat_model.or_else(|| prov.and_then(|p| p.model.clone()));
    let embed_model = embed_model.unwrap_or_else(|| "nomic-embed-text-v1-GGUF".to_string());

    let mut models_ok = false;
    let mut ids: Vec<String> = Vec::new();
    let mut downloaded: std::collections::HashMap<String, bool> = Default::default();
    match auth(client.get(format!("{base_url}/models"))).send().await {
        Ok(r) if r.status().is_success() => {
            models_ok = true;
            if let Ok(v) = r.json::<Value>().await {
                for m in v["data"].as_array().cloned().unwrap_or_default() {
                    if let Some(id) = m["id"].as_str() {
                        ids.push(id.to_string());
                        if let Some(d) = m["downloaded"].as_bool() {
                            downloaded.insert(id.to_string(), d);
                        }
                    }
                }
            }
        }
        Ok(r) => problems.push(format!("{base_url}/models answered HTTP {}", r.status().as_u16())),
        Err(e) => problems.push(format!("{base_url} is not reachable: {}", e.without_url())),
    }
    let chat_model = chat_model.or_else(|| ids.first().cloned()).unwrap_or_default();
    let chat_present = !chat_model.is_empty() && ids.iter().any(|i| *i == chat_model);
    let embed_present = ids.iter().any(|i| *i == embed_model);
    if models_ok && !chat_present {
        problems.push(format!("chat model '{chat_model}' is not in the server's model list"));
    }
    if models_ok && !embed_present {
        problems.push(format!("embedding model '{embed_model}' is not in the server's model list"));
    }
    let mut chat_reply: Option<String> = None;
    if models_ok && chat_present {
        let body = json!({"model": chat_model, "messages": [{"role": "user", "content": "Reply with the single word OK."}], "max_tokens": 16});
        match auth(client.post(format!("{base_url}/chat/completions")).json(&body)).send().await {
            Ok(r) if r.status().is_success() => {
                chat_reply = r.json::<Value>().await.ok().and_then(|v| v["choices"][0]["message"]["content"].as_str().map(|s| s.trim().to_string()));
                if chat_reply.as_deref().unwrap_or("").is_empty() {
                    problems.push("the chat model answered with no text (a reasoning model may have spent its token budget)".into());
                }
            }
            Ok(r) => problems.push(format!("chat request answered HTTP {}", r.status().as_u16())),
            Err(e) => problems.push(format!("chat request failed: {}", e.without_url())),
        }
    }
    let mut embed_dim: Option<usize> = None;
    if models_ok && embed_present {
        let body = json!({"model": embed_model, "input": "wayfinder"});
        match auth(client.post(format!("{base_url}/embeddings")).json(&body)).send().await {
            Ok(r) if r.status().is_success() => {
                embed_dim = r.json::<Value>().await.ok().and_then(|v| v["data"][0]["embedding"].as_array().map(|a| a.len()));
                if embed_dim.is_none() {
                    problems.push("the embeddings endpoint answered without a vector".into());
                }
            }
            Ok(r) => problems.push(format!("embeddings request answered HTTP {}", r.status().as_u16())),
            Err(e) => problems.push(format!("embeddings request failed: {}", e.without_url())),
        }
    }
    let all_ok = models_ok && chat_present && embed_present && chat_reply.as_deref().map_or(false, |s| !s.is_empty()) && embed_dim.is_some() && problems.is_empty();
    json!({
        "all_ok": all_ok, "base": base_url, "chat_model": chat_model, "embed_model": embed_model,
        "models_ok": models_ok, "chat_model_present": chat_present,
        "chat_model_downloaded": downloaded.get(&chat_model).copied().unwrap_or(chat_present),
        "embed_model_present": embed_present,
        "embed_model_downloaded": downloaded.get(&embed_model).copied().unwrap_or(embed_present),
        "embed_checkpoint": null, "embed_dim": embed_dim, "chat_reply": chat_reply, "problems": problems,
    })
}

// ---- UI settings kept in the server config ----

fn ui_path(rt: &Runtime) -> std::path::PathBuf {
    rt.config_dir.join("ui.json")
}

pub fn valid_theme(t: &str) -> bool {
    matches!(t, "dark" | "lakes" | "dead")
}

async fn ui_get(Extension(rt): Extension<Runtime>) -> Json<Value> {
    let v: Value = std::fs::read_to_string(ui_path(&rt)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let theme = v["theme"].as_str().filter(|t| valid_theme(t)).unwrap_or("dark");
    Json(json!({"theme": theme, "plain": v["plain"].as_bool().unwrap_or(false), "themes": ["dark", "lakes", "dead"]}))
}

async fn ui_set(Extension(rt): Extension<Runtime>, Json(p): Json<Value>) -> Response {
    let theme = match p["theme"].as_str() {
        Some(t) if valid_theme(t) => t.to_string(),
        _ => return err(StatusCode::BAD_REQUEST, "theme must be dark, lakes or dead"),
    };
    let plain = p["plain"].as_bool().unwrap_or(false);
    let body = serde_json::to_string_pretty(&json!({"theme": theme, "plain": plain})).unwrap_or_default();
    if let Err(e) = std::fs::create_dir_all(&rt.config_dir).and_then(|_| std::fs::write(ui_path(&rt), body)) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("could not save ui.json: {e}"));
    }
    Json(json!({"ok": true, "theme": theme, "plain": plain})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v1_base_forms() {
        assert_eq!(v1_base("http://127.0.0.1:13305/api"), "http://127.0.0.1:13305/api/v1");
        assert_eq!(v1_base("http://h:1/v1/"), "http://h:1/v1");
        assert_eq!(v1_base("http://h:1"), "http://h:1/v1");
    }
    #[test]
    fn setup_check_falls_back_to_the_on_device_endpoint() {
        let p = |name: &str, offline: bool| nautik9_lemonade::Provider { name: name.into(), base_url: format!("http://127.0.0.1:1/{name}/v1"), model: None, api_key: None, offline, headers: None };
        assert_eq!(setup_provider(&[p("local", true)]).unwrap().name, "local", "phone default: only an on-device entry");
        assert_eq!(setup_provider(&[p("local", true), p("home", false)]).unwrap().name, "home", "a remote provider still wins");
        assert!(setup_provider(&[]).is_none());
    }
    #[test]
    fn themes_are_a_closed_set() {
        assert!(valid_theme("lakes") && valid_theme("dead") && valid_theme("dark"));
        assert!(!valid_theme("sail") && !valid_theme(""));
    }
}
