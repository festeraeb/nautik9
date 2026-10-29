//! Hybrid memory search, the embeddings tool, memory ranking and the journal, against a
//! test-only embedding server that returns fixed vectors and a scripted chat endpoint.

use axum::{routing::post, Json, Router};
use nautik9_agent::memory::{EmbedConfig, MemoryIndex};
use nautik9_agent::store::Store;
use nautik9_agent::tools::{note_by_path, write_note, Registry, ToolCtx};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Deterministic fake embedder. Axes: 0 = hot drinks, 1 = boats, 2 = music, 3 = everything else.
/// Words are matched by prefix so "lattes" and "latte" agree. Counts the texts it was asked for.
fn fake_vec(t: &str) -> Vec<f64> {
    let t = t.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    let mut v = vec![0.0, 0.0, 0.0, 0.05];
    if has(&["caffeinated", "coffee", "latte", "espresso"]) {
        v[0] = 1.0;
    }
    if has(&["boat", "yanmar", "hull", "anchor"]) {
        v[1] = 1.0;
    }
    if has(&["guitar", "piano", "melody"]) {
        v[2] = 1.0;
    }
    v
}

async fn embedding_server(texts_seen: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/embeddings",
        post(move |Json(req): Json<Value>| {
            let n = texts_seen.clone();
            async move {
                let input: Vec<String> = req["input"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
                n.fetch_add(input.len(), Ordering::SeqCst);
                // Reverse the order of `data` to prove the client sorts by `index`.
                let mut data: Vec<Value> = input.iter().enumerate().map(|(i, t)| json!({"index": i, "embedding": fake_vec(t)})).collect();
                data.reverse();
                Json(json!({"model": req["model"], "data": data}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

async fn chat_server(script: Arc<dyn Fn(&Value) -> Value + Send + Sync>, seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(req): Json<Value>| {
            let (script, seen) = (script.clone(), seen.clone());
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

fn embed_cfg(url: &str) -> Option<EmbedConfig> {
    Some(EmbedConfig { url: url.to_string(), model: "fake-4d".into(), api_key_env: None })
}

fn make_agent(chat_base: &str, memory: MemoryIndex, vault: Arc<WayfinderVault>, store: Store, cfg: AgentConfig) -> Agent {
    let llm = LemonadeModule::new(LemonadeConfig {
        providers: vec![Provider { name: "mock".into(), base_url: chat_base.into(), model: Some("mock-1".into()), api_key: None, offline: false }],
        timeout_seconds: 10,
        max_tokens: 256,
    });
    let ctx = ToolCtx { vault, store, http: reqwest::Client::new(), secrets: Arc::new(Default::default()), search: Arc::new(vec![]), owner: true, mcp_local: Arc::new(Default::default()), memory: Arc::new(memory) };
    Agent::new(Arc::new(llm), ctx, cfg)
}

#[tokio::test]
async fn cosine_ranking_finds_meaning_and_hybrid_fuses_both() {
    let n = Arc::new(AtomicUsize::new(0));
    let url = embedding_server(n.clone()).await;
    let m = MemoryIndex::in_memory(embed_cfg(&url)).unwrap();
    let texts: Vec<String> = ["Likes oat milk lattes", "Boat engine is a Yanmar diesel", "Plays the piano on Sundays", "Caffeinated drink budget is tight"].iter().map(|s| s.to_string()).collect();
    let r = m.add("facts", "t", "", &texts).await.unwrap();
    assert_eq!((r["added"].as_u64(), r["embedded"].as_u64()), (Some(4), Some(4)));
    // Dedup: the same text again is neither stored nor embedded again.
    let before = n.load(Ordering::SeqCst);
    assert_eq!(m.add("facts", "t", "", &texts[..1]).await.unwrap()["added"], 0);
    assert_eq!(n.load(Ordering::SeqCst), before);

    // Meaning: "morning espresso" shares no word with the latte note, only the drink axis.
    let o = m.search("morning espresso", "semantic", &[], 4).await.unwrap();
    assert_eq!(o.mode_used, "semantic");
    assert!(o.hits[0].text == "Likes oat milk lattes" || o.hits[0].text.starts_with("Caffeinated"), "{:?}", o.hits[0]);
    assert!(o.hits[0].cosine.unwrap() > 0.99 && o.hits[3].cosine.unwrap() < 0.3, "{:?}", o.hits.iter().map(|h| h.cosine).collect::<Vec<_>>());
    assert!(m.search("morning espresso", "keyword", &[], 4).await.unwrap().hits.is_empty(), "no shared word, so keyword finds nothing");

    // Hybrid: the note that matches the word "caffeinated" AND the meaning comes first.
    let o = m.search("caffeinated", "hybrid", &[], 4).await.unwrap();
    assert_eq!(o.mode_used, "hybrid");
    assert!(o.hits[0].text.starts_with("Caffeinated drink"));
    assert!(o.hits[0].keyword_rank == Some(1) && o.hits[0].semantic_rank.is_some());
    let expect = 1.0 / 61.0 + 1.0 / (60.0 + o.hits[0].semantic_rank.unwrap() as f64);
    assert!((o.hits[0].score - expect).abs() < 1e-12);
    assert_eq!(o.hits[1].text, "Likes oat milk lattes", "semantic-only neighbour follows");
    assert_eq!(m.status().unwrap()["dims"], 4);
    assert_eq!(m.status().unwrap()["collections"]["facts"]["with_vectors"], 4);
    assert!((m.compare("coffee", "latte").await.unwrap() - 1.0).abs() < 1e-5);
    assert!(m.compare("coffee", "boat").await.unwrap() < 0.1);
}

#[tokio::test]
async fn a_dead_embedding_endpoint_degrades_to_keyword_and_says_so() {
    let m = MemoryIndex::in_memory(embed_cfg("http://127.0.0.1:9/v1")).unwrap();
    let r = m.add("facts", "t", "", &["Likes oat milk lattes".into()]).await.unwrap();
    assert_eq!(r["added"], 1);
    assert!(r["embed_error"].as_str().unwrap().contains("embedding endpoint"));
    let o = m.search("lattes", "hybrid", &[], 3).await.unwrap();
    assert!(o.mode_used.starts_with("keyword only") && o.note.is_some());
    assert_eq!(o.hits.len(), 1);
    assert!(m.search("lattes", "semantic", &[], 3).await.is_err());
    let st = m.status().unwrap();
    assert!(st["last_error"].as_str().is_some());
    assert_eq!(st["collections"]["facts"]["with_vectors"], 0);
}

#[tokio::test]
async fn changing_the_model_drops_vectors_but_keeps_chunks() {
    let n = Arc::new(AtomicUsize::new(0));
    let url = embedding_server(n).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vectors.db");
    {
        let m = MemoryIndex::open(&path, embed_cfg(&url)).unwrap();
        m.add("facts", "t", "", &["Likes oat milk lattes".into()]).await.unwrap();
        assert_eq!(m.status().unwrap()["collections"]["facts"]["with_vectors"], 1);
    }
    let m = MemoryIndex::open(&path, Some(EmbedConfig { url: url.clone(), model: "other".into(), api_key_env: None })).unwrap();
    assert_eq!(m.status().unwrap()["collections"]["facts"], json!({"chunks": 1, "with_vectors": 0}));
    assert_eq!(m.embed_pending().await.unwrap(), 1);
}

#[tokio::test]
async fn embeddings_tool_actions_are_owner_only_and_report_keyword_only() {
    let reg = Registry::standard();
    assert!(reg.schema(false, false, &[]).to_string().find("\"embeddings\"").is_none());
    assert!(reg.schema(true, false, &[]).to_string().contains("\"embeddings\""));
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    write_note(&vault, "Boat/Engine.md", "# Engine\nThe Yanmar needs fresh impellers.\n\n## Fuel\nDiesel only, filtered.\n").await.unwrap();
    write_note(&vault, "Misc.md", "Buy oat milk.").await.unwrap();
    let a = make_agent("http://127.0.0.1:9/v1", MemoryIndex::in_memory(None).unwrap(), vault, Store::in_memory().unwrap(), AgentConfig::default());
    let t = a.tools.get("embeddings").unwrap();
    let out = t.call(&json!({"action":"index"}), &a.ctx).await.unwrap();
    assert!(out.contains("\"notes\":2") && out.contains("off: keyword only"), "{out}");
    let out = t.call(&json!({"action":"index","path":"Boat/"}), &a.ctx).await.unwrap();
    assert!(out.contains("\"notes\":1") && out.contains("\"chunks_kept\":2"), "{out}");
    let out = t.call(&json!({"action":"search","query":"diesel","collection":"notes"}), &a.ctx).await.unwrap();
    assert!(out.starts_with("mode: keyword only"), "{out}");
    assert!(out.contains("Boat/Engine.md > Fuel"), "{out}");
    assert!(t.call(&json!({"action":"search","query":"diesel","mode":"semantic"}), &a.ctx).await.unwrap_err().contains("keyword only"));
    assert!(t.call(&json!({"action":"add","text":"x"}), &a.ctx).await.is_err(), "collection is required");
    assert!(t.call(&json!({"action":"nope"}), &a.ctx).await.is_err());
    let st = t.call(&json!({"action":"status"}), &a.ctx).await.unwrap();
    assert!(st.contains("\"notes\":{\"chunks\":3") || st.contains("\"chunks\":3"), "{st}");
    // Removing a source and a stale chunk after a note shrinks.
    write_note(&a.ctx.vault, "Boat/Engine.md", "# Engine\nThe Yanmar needs fresh impellers.\n").await.unwrap();
    let out = t.call(&json!({"action":"index","path":"Boat/"}), &a.ctx).await.unwrap();
    assert!(out.contains("\"chunks_removed\":1"), "{out}");
    assert!(t.call(&json!({"action":"remove","source":"Boat/Engine.md"}), &a.ctx).await.unwrap().contains("\"removed\":1"));
}

#[tokio::test]
async fn memory_block_ranks_by_meaning_with_an_endpoint_and_by_words_without() {
    let n = Arc::new(AtomicUsize::new(0));
    let url = embedding_server(n).await;
    let note = "# Memory\n- Boat is moored at the north dock\n- Collects drink coasters from pubs\n- Likes oat milk lattes\n";
    let cfg = AgentConfig { memory_k: 2, ..Default::default() };
    let query = "caffeinated drink";

    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    write_note(&vault, "Jade/Memory.md", note).await.unwrap();
    let hybrid = make_agent("http://127.0.0.1:9/v1", MemoryIndex::in_memory(embed_cfg(&url)).unwrap(), vault.clone(), Store::in_memory().unwrap(), cfg.clone());
    let p = hybrid.system_prompt(true, query).await;
    assert!(p.contains("- Likes oat milk lattes") && p.contains("- Collects drink coasters"), "hybrid finds the latte by meaning and the coasters by word:\n{p}");
    assert!(!p.contains("moored"), "{p}");
    // The note changes: the index follows (bullet removed and added).
    write_note(&vault, "Jade/Memory.md", &format!("{note}- Drinks espresso after lunch\n- Anchor is 20 kg\n")).await.unwrap();
    let p = hybrid.system_prompt(true, "coffee").await;
    assert!(p.contains("espresso after lunch"), "{p}");

    let kw = make_agent("http://127.0.0.1:9/v1", MemoryIndex::in_memory(None).unwrap(), vault, Store::in_memory().unwrap(), cfg);
    write_note(&kw.ctx.vault, "Jade/Memory.md", note).await.unwrap();
    let p = kw.system_prompt(true, query).await;
    assert!(!p.contains("lattes"), "word overlap alone cannot find it:\n{p}");
    assert!(p.contains("drink coasters"), "{p}");
}

#[tokio::test]
async fn journal_writes_one_entry_per_chat_from_stored_messages_only() {
    let seen = Arc::new(Mutex::new(vec![]));
    let base = chat_server(
        Arc::new(|req: &Value| {
            let user = req["messages"][1]["content"].as_str().unwrap();
            let which = if user.contains("Lisbon") { "Planned a Lisbon trip." } else { "Asked about impellers." };
            json!({"role":"assistant","content": format!("<think>x</think>{which}")})
        }),
        seen.clone(),
    )
    .await;
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    let store = Store::in_memory().unwrap();
    let s1 = store.create_session("Lisbon trip").await.unwrap();
    store.append(&s1, &[json!({"role":"user","content":"Plan Lisbon in May"}), json!({"role":"assistant","content":"Trams and pasteis."}), json!({"role":"tool","tool_call_id":"1","content":"SECRET-TOOL-OUTPUT"})]).await.unwrap();
    let s2 = store.create_session("Engine").await.unwrap();
    store.append(&s2, &[json!({"role":"user","content":"How often change impellers?"})]).await.unwrap();
    let a = make_agent(&base, MemoryIndex::in_memory(None).unwrap(), vault.clone(), store, AgentConfig::default());
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Yesterday has no chats: nothing is written.
    let r = a.journal(None).await.unwrap();
    assert_eq!((r["written"].clone(), r["entries"].clone()), (json!(false), json!(0)));
    assert!(a.journal(Some("last tuesday")).await.is_err());
    assert!(note_by_path(&vault, "Jade/Journal.md").await.unwrap().is_none());

    let r = a.journal(Some(&today)).await.unwrap();
    assert_eq!(r["entries"], 2);
    let day = note_by_path(&vault, &format!("Jade/Journal/{today}.md")).await.unwrap().unwrap().content;
    assert!(day.contains(&format!("## Lisbon trip (session {s1})")) && day.contains("Planned a Lisbon trip."), "{day}");
    assert!(day.contains(&format!("(session {s2})")) && day.contains("Asked about impellers."), "{day}");
    assert!(!day.contains("<think>"));
    // Summaries were built only from the stored user/assistant text: the tool output never went out.
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0]["messages"][1]["content"].as_str().unwrap().contains("Trams and pasteis."));
    assert!(!reqs.iter().any(|r| r.to_string().contains("SECRET-TOOL-OUTPUT")));
    let idx = note_by_path(&vault, "Jade/Journal.md").await.unwrap().unwrap().content;
    assert!(idx.contains(&format!("[[Jade/Journal/{today}.md]]")));
    // Re-running replaces the day and does not double the link or the search rows.
    a.journal(Some(&today)).await.unwrap();
    let idx2 = note_by_path(&vault, "Jade/Journal.md").await.unwrap().unwrap().content;
    assert_eq!(idx2.matches(&format!("[[Jade/Journal/{today}.md]]")).count(), 1);
    let o = a.ctx.memory.search("Lisbon", "keyword", &["journal".into()], 5).await.unwrap();
    assert_eq!(o.hits.len(), 1);
    assert!(o.hits[0].head.contains(&s1));
}

#[tokio::test]
async fn journal_without_a_model_writes_nothing_and_returns_the_error() {
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    let store = Store::in_memory().unwrap();
    let s = store.create_session("Chat").await.unwrap();
    store.append(&s, &[json!({"role":"user","content":"hello"})]).await.unwrap();
    let a = make_agent("http://127.0.0.1:9/v1", MemoryIndex::in_memory(None).unwrap(), vault.clone(), store, AgentConfig::default());
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let e = a.journal(Some(&today)).await.unwrap_err();
    assert!(e.contains("journal not written") && e.contains("no model endpoint answered"), "{e}");
    assert!(note_by_path(&vault, &format!("Jade/Journal/{today}.md")).await.unwrap().is_none());
    assert!(note_by_path(&vault, "Jade/Journal.md").await.unwrap().is_none());
}
