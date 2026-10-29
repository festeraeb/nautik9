//! Note scripts (`jade.*` in JavaScript) and templates through the agent's tools.

use axum::{routing::post, Json, Router};
use nautik9_agent::script::{run_source, ScriptConfig, ScriptTool};
use nautik9_agent::store::Store;
use nautik9_agent::tools::{note_by_path, write_note, Registry, ToolCtx};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::sync::Arc;

async fn mock_llm() -> String {
    let app = Router::new().route("/v1/chat/completions", post(|Json(_): Json<Value>| async { Json(json!({"model":"m","choices":[{"message":{"role":"assistant","content":"model says hi"},"finish_reason":"stop"}]})) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

async fn make(cfg: ScriptConfig) -> (Arc<Agent>, Arc<WayfinderVault>) {
    let base = mock_llm().await;
    let vault = Arc::new(WayfinderVault::in_memory().unwrap());
    let llm = LemonadeModule::new(LemonadeConfig { providers: vec![Provider { name: "mock".into(), base_url: base, model: Some("m".into()), api_key: None, offline: false }], timeout_seconds: 10, max_tokens: 64 });
    let ctx = ToolCtx { vault: vault.clone(), store: Store::in_memory().unwrap(), http: reqwest::Client::new(), secrets: Arc::new(Default::default()), search: Arc::new(vec![]), owner: true, mcp_local: Arc::new(Default::default()), memory: Arc::new(nautik9_agent::memory::MemoryIndex::in_memory(None).unwrap()) };
    let agent = Arc::new_cyclic(|me| {
        let (reg, _) = Registry::with_extra(vec![Arc::new(ScriptTool { agent: me.clone(), cfg })]);
        Agent::with_tools(Arc::new(llm), ctx, AgentConfig::default(), reg)
    });
    (agent, vault)
}

#[tokio::test]
async fn read_search_list_and_return_values() {
    let (a, v) = make(ScriptConfig::default()).await;
    write_note(&v, "Boat/Log.md", "Launched the boat on Friday").await.unwrap();
    write_note(&v, "Boat/Fuel.md", "diesel 40L").await.unwrap();
    let src = "Counts notes.\n```js\nconst names = await jade.list('Boat/');\nconst log = await jade.read('Boat/Log.md');\nconst hits = jade.search('Launched');\nconsole.log('found', names.length);\nreturn {names, words: log.split(' ').length, hit: hits[0].title, missing: jade.read('Nope.md'), args};\n```\n";
    let r = run_source(&a, &ScriptConfig::default(), src, json!({"x": 1})).await;
    assert!(r.ok, "{}", r.text());
    assert_eq!(r.logs, vec!["found 2"]);
    let out: Value = serde_json::from_str(r.result.as_ref().unwrap()).unwrap();
    assert_eq!(out["names"], json!(["Boat/Fuel.md", "Boat/Log.md"]));
    assert_eq!(out["words"], 5);
    assert_eq!(out["hit"], "Boat/Log.md");
    assert!(out["missing"].is_null());
    assert_eq!(out["args"]["x"], 1);
}

#[tokio::test]
async fn writes_and_tools_are_off_by_default() {
    let (a, v) = make(ScriptConfig::default()).await;
    let r = run_source(&a, &ScriptConfig::default(), "jade.write('X.md', 'hi')", Value::Null).await;
    assert!(!r.ok && r.error.as_ref().unwrap().contains("scripts.write"), "{}", r.text());
    assert!(note_by_path(&v, "X.md").await.unwrap().is_none());
    let r = run_source(&a, &ScriptConfig::default(), "return jade.tool('read', {path: 'X.md'})", Value::Null).await;
    assert!(r.error.as_ref().unwrap().contains("scripts.tools"), "{}", r.text());
    let r = run_source(&a, &ScriptConfig::default(), "return jade.ask('hi')", Value::Null).await;
    assert!(r.error.as_ref().unwrap().contains("scripts.tools"));
}

#[tokio::test]
async fn write_tool_and_ask_when_allowed() {
    let cfg = ScriptConfig { write: true, tools: true, ..Default::default() };
    let (a, v) = make(cfg.clone()).await;
    let src = "await jade.write('Out/A.md', 'alpha');\nconst t = await jade.tools.read({path: 'Out/A.md'});\nawait jade.tool('edit', {path: 'Out/A.md', old: 'alpha', new: 'beta'});\nconst said = await jade.ask('say hi');\nreturn [t, said];";
    let r = run_source(&a, &cfg, src, Value::Null).await;
    assert!(r.ok, "{}", r.text());
    assert_eq!(r.result.as_deref(), Some("[\"alpha\",\"model says hi\"]"));
    assert_eq!(note_by_path(&v, "Out/A.md").await.unwrap().unwrap().content, "beta");
    // A script cannot call `script` (no recursion).
    let r = run_source(&a, &cfg, "jade.tool('script', {action:'run', path:'x'})", Value::Null).await;
    assert!(r.error.unwrap().contains("cannot call"));
}

#[tokio::test]
async fn limits_stop_runaway_scripts() {
    let cfg = ScriptConfig { max_loop: 10_000, max_calls: 5, ..Default::default() };
    let (a, _) = make(cfg.clone()).await;
    let t = std::time::Instant::now();
    let r = run_source(&a, &cfg, "while (true) {}", Value::Null).await;
    assert!(!r.ok, "an endless loop must be stopped: {}", r.text());
    assert!(t.elapsed().as_secs() < 10);
    let r = run_source(&a, &cfg, "let n = 0; while (true) { n++; }", Value::Null).await;
    assert!(!r.ok);
    let r = run_source(&a, &cfg, "function f(){ return f(); } f();", Value::Null).await;
    assert!(!r.ok, "unbounded recursion must stop");
    let r = run_source(&a, &cfg, "for (let i = 0; i < 20; i++) jade.list('');", Value::Null).await;
    assert!(r.error.as_ref().unwrap().contains("max_calls"), "{}", r.text());
    assert_eq!(r.host_calls, 5);
    let r = run_source(&a, &cfg, "throw new Error('boom')", Value::Null).await;
    assert!(r.error.unwrap().contains("boom"));
    let r = run_source(&a, &cfg, "this is not javascript", Value::Null).await;
    assert!(!r.ok);
    let r = run_source(&a, &cfg, "await new Promise(() => {})", Value::Null).await;
    assert!(r.error.unwrap().contains("never finished"));
}

#[tokio::test]
async fn timeout_between_host_calls() {
    let cfg = ScriptConfig { timeout_secs: 1, max_loop: u64::MAX, ..Default::default() };
    let (a, _) = make(cfg.clone()).await;
    let r = run_source(&a, &cfg, "const end = Date.now() + 1500; while (Date.now() < end) {} jade.list('');", Value::Null).await;
    assert!(r.error.as_ref().is_some_and(|e| e.contains("wall-clock")), "{}", r.text());
}

#[tokio::test]
async fn script_tool_runs_a_note_and_owner_only() {
    let (a, v) = make(ScriptConfig::default()).await;
    write_note(&v, "Scripts/hello.md", "```javascript\nreturn 'hello ' + args.name;\n```").await.unwrap();
    let t = a.tools.allowed("script", true, false).unwrap();
    assert!(a.tools.allowed("script", false, false).is_none(), "non-owner gets no script tool");
    let out = t.call(&json!({"action":"run","path":"Scripts/hello.md","args":{"name":"Mack"}}), &a.ctx).await.unwrap();
    assert!(out.contains("returned: \"hello Mack\""), "{out}");
    let chk = t.call(&json!({"action":"check","path":"Scripts/hello.md"}), &a.ctx).await.unwrap();
    assert!(chk.contains("return 'hello '") && !chk.contains("```"));
    let err = t.call(&json!({"action":"run","path":"Scripts/none.md"}), &a.ctx).await.unwrap_err();
    assert!(err.starts_with("no note at"));
    let out = t.call(&json!({"action":"run","path":"Scripts/hello.md"}), &a.ctx).await.unwrap();
    assert!(out.contains("hello undefined") || out.contains("returned"), "args without a value: {out}");
}

#[tokio::test]
async fn template_tool_creates_notes_and_never_overwrites() {
    let (a, v) = make(ScriptConfig::default()).await;
    write_note(&v, "Templates/Daily.md", "# <% tp.file.title %>\nDate: <% tp.date.now(\"YYYY-MM-DD\") %>\nFocus: <% tp.system.prompt(\"Focus?\") %>\n- [ ] <% tp.file.cursor() %>").await.unwrap();
    let t = a.tools.allowed("template", true, false).unwrap();
    assert!(a.tools.allowed("template", false, false).is_none());
    assert_eq!(t.call(&json!({"action":"list"}), &a.ctx).await.unwrap(), "Templates/Daily.md");
    let err = t.call(&json!({"action":"create","template":"Daily","title":"Days/Today"}), &a.ctx).await.unwrap_err();
    assert!(err.contains("Focus?"));
    assert!(note_by_path(&v, "Days/Today.md").await.unwrap().is_none(), "nothing written on error");
    let prev = t.call(&json!({"action":"preview","template":"Daily","title":"Days/Today","values":{"Focus?":"paint"}}), &a.ctx).await.unwrap();
    assert!(prev.contains("Focus: paint"));
    assert!(note_by_path(&v, "Days/Today.md").await.unwrap().is_none(), "preview writes nothing");
    let out = t.call(&json!({"action":"create","template":"Daily","title":"Days/Today","values":{"Focus?":"paint"}}), &a.ctx).await.unwrap();
    assert!(out.contains("created `Days/Today.md`"));
    let n = note_by_path(&v, "Days/Today.md").await.unwrap().unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(n.content, format!("# Today\nDate: {today}\nFocus: paint\n- [ ] "));
    let err = t.call(&json!({"action":"create","template":"Daily","title":"Days/Today","values":{"Focus?":"x"}}), &a.ctx).await.unwrap_err();
    assert!(err.contains("already exists"));
    assert!(t.call(&json!({"action":"create","template":"Nope","title":"a"}), &a.ctx).await.unwrap_err().contains("no template"));
}
