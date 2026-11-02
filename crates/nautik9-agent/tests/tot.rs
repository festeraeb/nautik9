//! The tree-of-thought reasoner against a scripted OpenAI-compatible mock.

use axum::{routing::post, Json, Router};
use nautik9_agent::tot::{reason, TotError, TotParams};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

async fn mock(seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(req): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(req.clone());
                let sys = req["messages"][0]["content"].as_str().unwrap_or("").to_string();
                let user = req["messages"][1]["content"].as_str().unwrap_or("").to_string();
                let content = if sys.contains("ROLE: PROPOSE") {
                    // Thoughts are named after how many steps came before, so the tree is visible.
                    let n = user.matches("\n1. ").count() + user.matches("\n2. ").count();
                    format!("<think>hm</think>[\"depth{n}-alpha\", \"depth{n}-beta\", \"depth{n}-gamma\"]")
                } else if sys.contains("ROLE: SCORE") {
                    // Only "beta" steps are good.
                    if user.rsplit("\n").next().unwrap_or("").contains("beta") { "{\"score\": 9, \"reason\": \"sound\"}".into() } else { "{\"score\": 3, \"reason\": \"weak\"}".into() }
                } else {
                    format!("FINAL from path: {}", user.replace('\n', " | "))
                };
                Json(json!({"model": "mock-1", "choices": [{"message": {"role":"assistant","content": content}, "finish_reason": "stop"}]}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn llm(base: &str) -> LemonadeModule {
    LemonadeModule::new(LemonadeConfig { providers: vec![Provider { name: "mock".into(), base_url: base.into(), model: Some("mock-1".into()), api_key: None, offline: false }], timeout_seconds: 10, max_tokens: 256 })
}

#[tokio::test]
async fn expands_the_best_and_prunes_the_rest() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = mock(seen.clone()).await;
    let p = TotParams { branch: 3, beam: 1, depth: 2, budget: 40, max_tokens: 200 };
    let r = reason(&llm(&base), "How do I raise the boat?", &p).await.ok().expect("ok");
    // depth 1: 3 thoughts, depth 2: 3 children of the single kept node.
    assert_eq!(r.nodes.len(), 6);
    let promoted: Vec<&str> = r.nodes.iter().filter(|n| n.status != "pruned").map(|n| n.hypothesis.as_str()).collect();
    assert!(promoted.iter().all(|h| h.contains("beta")), "{promoted:?}");
    assert_eq!(r.nodes.iter().filter(|n| n.status == "pruned").count(), 4);
    assert_eq!(r.winning_plan.len(), 2);
    assert!(r.winning_plan[0].contains("beta") && r.winning_plan[1].contains("beta"));
    assert!((r.nodes[1].score - 0.9).abs() < 1e-9);
    assert!(r.nodes[1].verifier_checks[0].contains("sound"));
    // The second level was expanded from the winning first-level node only.
    assert!(r.nodes[3].parent == Some(1));
    // The answer is the model's own text built from the winning path.
    assert!(r.answer.starts_with("FINAL from path:") && r.answer.contains("depth0-beta"), "{}", r.answer);
    // 1 propose + 3 scores, 1 propose + 3 scores, 1 answer.
    assert_eq!(r.model_calls, 9);
    assert_eq!(seen.lock().unwrap().len(), 9);
    assert!(!r.budget_exhausted);
    assert!(r.scored_by.contains("self-evaluation"));
}

#[tokio::test]
async fn budget_stops_the_search_and_says_so() {
    let base = mock(Default::default()).await;
    let p = TotParams { branch: 3, beam: 2, depth: 4, budget: 7, max_tokens: 200 };
    let r = reason(&llm(&base), "q", &p).await.ok().expect("ok");
    assert!(r.budget_exhausted);
    assert!(r.model_calls <= 7, "{}", r.model_calls);
    assert!(!r.answer.is_empty());
}

#[tokio::test]
async fn no_endpoint_is_an_error_with_attempts_and_no_tree() {
    match reason(&llm("http://127.0.0.1:9/v1"), "q", &TotParams::default()).await {
        Err(TotError::NoModel(f)) => assert!(!f.attempts.is_empty()),
        _ => panic!("expected NoModel"),
    }
    let empty = LemonadeModule::new(LemonadeConfig { providers: vec![], timeout_seconds: 2, max_tokens: 64 });
    assert!(matches!(reason(&empty, "q", &TotParams::default()).await, Err(TotError::NoModel(_))));
}

#[tokio::test]
async fn unusable_replies_are_reported_not_replaced() {
    let app = Router::new().route("/v1/chat/completions", post(|| async { Json(json!({"model":"m","choices":[{"message":{"role":"assistant","content":"I would rather not."},"finish_reason":"stop"}]})) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    match reason(&llm(&format!("http://{addr}/v1")), "q", &TotParams::default()).await {
        Err(TotError::Unusable(m)) => assert!(m.contains("no usable thoughts") && m.contains("rather not"), "{m}"),
        _ => panic!("expected Unusable"),
    }
}
