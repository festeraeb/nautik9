//! The agent loop against a scripted OpenAI-compatible mock: tool calls really change the vault,
//! results go back to the model, the session is stored, and a model that never stops is cut off
//! honestly.

use axum::{routing::post, Json, Router};
use nautik9_agent::store::Store;
use nautik9_agent::tools::{note_by_path, ToolCtx};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Mock: replies are picked by how many tool messages the request already carries.
async fn mock(script: Arc<dyn Fn(&Value) -> Value + Send + Sync>, seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(req): Json<Value>| {
            let script = script.clone();
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(req.clone());
                Json(json!({"model": "mock-1", "choices": [{"message": script(&req), "finish_reason": "stop"}]}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn agent(base: &str, vault: Arc<WayfinderVault>, store: Store, cfg: AgentConfig) -> Agent {
    let llm = LemonadeModule::new(LemonadeConfig {
        providers: vec![Provider { name: "mock".into(), base_url: base.into(), model: Some("mock-1".into()), api_key: None, offline: false }],
        timeout_seconds: 10,
        max_tokens: 256,
    });
    let ctx = ToolCtx { vault, store, http: reqwest::Client::new(), secrets: Arc::new(Default::default()), search: Arc::new(vec![]), owner: true, mcp_local: Arc::new(Default::default()), memory: Arc::new(nautik9_agent::memory::MemoryIndex::in_memory(None).unwrap()) };
    Agent::new(Arc::new(llm), ctx, cfg)
}

fn tool_msgs(req: &Value) -> Vec<Value> {
    req["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "tool").cloned().collect()
}

#[tokio::test]
async fn tool_calls_write_the_vault_and_the_session_is_stored() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = mock(
        Arc::new(|req: &Value| match tool_msgs(req).len() {
            0 => json!({"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"write","arguments":"{\"path\":\"Boat/Log.md\",\"content\":\"Launched 2026-10-29\"}"}}]}),
            1 => json!({"role":"assistant","content":"","tool_calls":[{"id":"c2","type":"function","function":{"name":"read","arguments":"{\"path\":\"Boat/Log.md\"}"}},{"id":"c3","type":"function","function":{"name":"search","arguments":"{\"query\":\"Launched\"}"}}]}),
            _ => json!({"role":"assistant","content":"<think>done</think>Logged it in Boat/Log.md."}),
        }),
        seen.clone(),
    )
    .await;
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    let store = Store::in_memory().unwrap();
    let a = agent(&base, vault.clone(), store.clone(), AgentConfig::default());
    let r = a.turn(None, "Log that the boat launched today", true).await.unwrap();

    assert_eq!(r.reply.as_deref(), Some("Logged it in Boat/Log.md."));
    assert_eq!((r.steps, r.tool_calls.len()), (3, 3));
    assert!(r.tool_calls.iter().all(|t| t.ok), "{:?}", r.tool_calls);
    assert_eq!(note_by_path(&vault, "Boat/Log.md").await.unwrap().unwrap().content, "Launched 2026-10-29");
    // The model saw the real read result and the tool list.
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    assert!(tool_msgs(&reqs[2]).iter().any(|m| m["content"] == "Launched 2026-10-29"));
    assert!(reqs[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "delegate"));
    assert!(reqs[0]["messages"][0]["content"].as_str().unwrap().contains("Now: "));
    // Stored: user, assistant(tool call), tool, assistant(2 calls), tool, tool, assistant(final).
    let stored = store.messages(&r.session_id).await.unwrap();
    assert_eq!(stored.len(), 7);
    assert_eq!(stored[0].message["role"], "user");
    // A second turn in the same session sends the earlier turn back.
    let r2 = a.turn(Some(&r.session_id), "thanks", true).await.unwrap();
    assert_eq!(r2.session_id, r.session_id);
    assert!(seen.lock().unwrap().last().unwrap()["messages"].as_array().unwrap().len() >= 9);
}

#[tokio::test]
async fn a_model_that_never_stops_is_cut_off_honestly() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = mock(
        Arc::new(|_req: &Value| json!({"role":"assistant","content":"","tool_calls":[{"id":"x","type":"function","function":{"name":"list","arguments":"{}"}}]})),
        seen,
    )
    .await;
    let a = agent(&base, Arc::new(WayfinderVault::in_memory().unwrap()), Store::in_memory().unwrap(), AgentConfig { max_steps: 3, ..Default::default() });
    let r = a.turn(None, "loop forever", true).await.unwrap();
    assert_eq!(r.reply, None);
    assert!(r.stopped.unwrap().contains("stopped after 3 model calls"));
    assert_eq!(r.tool_calls.len(), 3);
}

#[tokio::test]
async fn bad_tool_arguments_and_unknown_tools_go_back_as_errors() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = mock(
        Arc::new(|req: &Value| match tool_msgs(req).len() {
            0 => json!({"role":"assistant","content":"","tool_calls":[{"id":"a","type":"function","function":{"name":"read","arguments":"{not json"}},{"id":"b","type":"function","function":{"name":"rm_rf","arguments":"{}"}}]}),
            _ => json!({"role":"assistant","content":"Both tools failed."}),
        }),
        seen.clone(),
    )
    .await;
    let a = agent(&base, Arc::new(WayfinderVault::in_memory().unwrap()), Store::in_memory().unwrap(), AgentConfig::default());
    let r = a.turn(None, "try", true).await.unwrap();
    assert!(r.tool_calls.iter().all(|t| !t.ok));
    let last = seen.lock().unwrap().last().unwrap().clone();
    let tm = tool_msgs(&last);
    assert!(tm[0]["content"].as_str().unwrap().starts_with("ERROR: arguments are not valid JSON"));
    assert!(tm[1]["content"].as_str().unwrap().starts_with("ERROR: no tool `rm_rf`"));
}

#[tokio::test]
async fn non_owner_gets_no_tools_and_no_memory() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = mock(Arc::new(|_req: &Value| json!({"role":"assistant","content":"Hi!"})), seen.clone()).await;
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    nautik9_agent::tools::write_note(&vault, "Jade/Memory.md", "# Memory\n- Owner's boat code is 4417\n").await.unwrap();
    let a = agent(&base, vault, Store::in_memory().unwrap(), AgentConfig::default());
    a.turn(None, "hello", false).await.unwrap();
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.get("tools").is_none(), "no tools for others: {}", req["tools"]);
    assert!(!req["messages"][0]["content"].as_str().unwrap().contains("4417"));
}

#[tokio::test]
async fn no_endpoint_is_an_error_not_an_answer() {
    let a = agent("http://127.0.0.1:9/v1", Arc::new(WayfinderVault::in_memory().unwrap()), Store::in_memory().unwrap(), AgentConfig::default());
    let e = a.turn(None, "hello", true).await.unwrap_err();
    assert_eq!(e.error, "no model endpoint answered");
}
