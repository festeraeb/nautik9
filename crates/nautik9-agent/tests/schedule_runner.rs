//! The schedule runner against a scripted OpenAI-compatible mock: a due schedule runs once and
//! is logged, a disabled one does not run, a bad crontab line is logged (once), crontab lines and
//! the heartbeat run as agent turns.

use axum::{routing::post, Json, Router};
use chrono::{DateTime, Duration, FixedOffset};
use nautik9_agent::schedule::{Runner, Schedules, ScheduleTool, CRONTAB_PATH, HEARTBEAT_PATH, LOG_PATH};
use nautik9_agent::store::Store;
use nautik9_agent::tools::{note_by_path, write_note, Tool, ToolCtx};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

async fn mock(seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(req): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(req);
                Json(json!({"model": "mock-1", "choices": [{"message": {"role":"assistant","content":"The tide is high."}, "finish_reason": "stop"}]}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn agent(base: &str) -> Arc<Agent> {
    let llm = LemonadeModule::new(LemonadeConfig {
        providers: vec![Provider { name: "mock".into(), base_url: base.into(), model: Some("mock-1".into()), api_key: None, offline: false }],
        timeout_seconds: 10,
        max_tokens: 256,
    });
    let ctx = ToolCtx {
        vault: Arc::new(WayfinderVault::in_memory().unwrap()),
        store: Store::in_memory().unwrap(),
        http: reqwest::Client::new(),
        secrets: Arc::new(Default::default()),
        search: Arc::new(vec![]),
        owner: true,
        mcp_local: Arc::new(Default::default()),
    };
    Arc::new(Agent::new(Arc::new(llm), ctx, AgentConfig::default()))
}

fn now() -> DateTime<FixedOffset> {
    chrono::Local::now().fixed_offset()
}

async fn log(a: &Agent) -> String {
    note_by_path(&a.ctx.vault, LOG_PATH).await.unwrap().map(|n| n.content).unwrap_or_default()
}

#[tokio::test]
async fn a_due_schedule_runs_once_and_a_disabled_one_does_not() {
    let seen = Arc::new(Mutex::new(vec![]));
    let a = agent(&mock(seen.clone()).await);
    let runner = Runner::new(a.clone(), 0);
    let sc = Schedules::new(&a.ctx.store);
    let t0 = now();
    let on = sc.add("every 10 minutes", "Check the tide", t0).await.unwrap();
    let off = sc.add("every 10 minutes", "Never run me", t0).await.unwrap();
    sc.set_enabled(&off.id, false, t0).await.unwrap();

    assert_eq!(runner.tick(t0 + Duration::minutes(5)).await, 0, "not due yet");
    assert_eq!(runner.tick(t0 + Duration::minutes(11)).await, 1);
    assert_eq!(runner.tick(t0 + Duration::minutes(12)).await, 0, "ran once; next run is 10 minutes after");
    assert_eq!(seen.lock().unwrap().len(), 1);
    let last = seen.lock().unwrap()[0].clone();
    assert!(last["messages"].to_string().contains("Check the tide"));

    let s = sc.get(&on.id).await.unwrap().unwrap();
    assert_eq!(s.last_result.as_deref(), Some("The tide is high."));
    assert!(s.last_run.is_some());
    assert!(DateTime::parse_from_rfc3339(s.next_run.as_deref().unwrap()).unwrap() > t0 + Duration::minutes(11));
    assert!(sc.get(&off.id).await.unwrap().unwrap().last_run.is_none());

    let l = log(&a).await;
    assert_eq!(l.lines().count(), 1, "{l}");
    assert!(l.contains(" ok ⏰ every 10 minutes: The tide is high."), "{l}");
    // The run got a session of its own with the schedule's title.
    let sessions = a.ctx.store.list_sessions().await.unwrap();
    assert_eq!(sessions.iter().filter(|s| s.title == "⏰ every 10 minutes").count(), 1);
}

#[tokio::test]
async fn a_failed_run_is_recorded_as_an_error() {
    let a = agent("http://127.0.0.1:9/v1");
    let runner = Runner::new(a.clone(), 0);
    let sc = Schedules::new(&a.ctx.store);
    let t0 = now();
    let s = sc.add("hourly", "x", t0).await.unwrap();
    sc.queue_now(&s.id, t0).await.unwrap();
    assert_eq!(runner.tick(t0).await, 1);
    let s = sc.get(&s.id).await.unwrap().unwrap();
    assert!(s.last_result.unwrap().starts_with("ERROR: no model endpoint answered"));
    assert!(log(&a).await.contains(" ERROR ⏰ hourly:"));
}

#[tokio::test]
async fn crontab_runs_and_bad_lines_are_logged_once() {
    let seen = Arc::new(Mutex::new(vec![]));
    let a = agent(&mock(seen.clone()).await);
    let runner = Runner::new(a.clone(), 0);
    write_note(&a.ctx.vault, CRONTAB_PATH, "# jobs\n* * * * * prompt Say hello\nnot a cron line\n").await.unwrap();
    let t0 = now();
    assert_eq!(runner.tick(t0).await, 0, "first sight only schedules the next run");
    assert_eq!(runner.tick(t0 + Duration::seconds(30)).await, 0);
    assert_eq!(runner.tick(t0 + Duration::minutes(2)).await, 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let l = log(&a).await;
    assert_eq!(l.matches("Jade/crontab: line 3:").count(), 1, "bad line logged once:\n{l}");
    assert!(l.contains(" ok ⏰ * * * * * (crontab): The tide is high."), "{l}");
    // Fixing the line stops the error; breaking it again later reports again.
    write_note(&a.ctx.vault, CRONTAB_PATH, "* * * * * prompt Say hello\n99 * * * * prompt bad\n").await.unwrap();
    runner.tick(t0 + Duration::minutes(2) + Duration::seconds(30)).await;
    assert_eq!(log(&a).await.matches("line 2:").count(), 1);
}

#[tokio::test]
async fn heartbeat_runs_only_when_enabled_and_the_note_has_content() {
    let seen = Arc::new(Mutex::new(vec![]));
    let a = agent(&mock(seen.clone()).await);
    let off = Runner::new(a.clone(), 0);
    write_note(&a.ctx.vault, HEARTBEAT_PATH, "Check the weather and tell me if anything needs doing.").await.unwrap();
    let t0 = now();
    off.tick(t0).await;
    assert_eq!(off.tick(t0 + Duration::hours(3)).await, 0, "heartbeat_minutes = 0 is off");

    let hb = Runner::new(a.clone(), 30);
    assert_eq!(hb.tick(t0).await, 0);
    assert_eq!(hb.tick(t0 + Duration::minutes(29)).await, 0);
    assert_eq!(hb.tick(t0 + Duration::minutes(31)).await, 1);
    assert!(seen.lock().unwrap()[0]["messages"].to_string().contains("Check the weather"));
    assert!(a.ctx.store.list_sessions().await.unwrap().iter().any(|s| s.title == "♥ Heartbeat"));
    assert!(log(&a).await.contains(" ok ♥ Heartbeat:"));
    // An empty note (comments only) skips the beat.
    write_note(&a.ctx.vault, HEARTBEAT_PATH, "<!-- nothing to do -->\n").await.unwrap();
    assert_eq!(hb.tick(t0 + Duration::minutes(70)).await, 0);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_schedule_tool_adds_lists_toggles_queues_and_removes() {
    let a = agent("http://127.0.0.1:9/v1");
    let call = |v: Value| {
        let a = a.clone();
        async move { ScheduleTool.call(&v, &a.ctx).await }
    };
    assert!(call(json!({"action":"add","spec":"sometime","prompt":"x"})).await.unwrap_err().contains("unknown schedule"));
    let added = call(json!({"action":"add","spec":"weekdays at 07:30","prompt":"Morning brief"})).await.unwrap();
    let id = added.trim_start_matches("added: ").split(' ').next().unwrap().to_string();
    assert!(call(json!({"action":"list"})).await.unwrap().contains("Morning brief"));
    assert!(call(json!({"action":"disable","id":id})).await.unwrap().starts_with("disabled"));
    assert!(call(json!({"action":"enable","id":id})).await.unwrap().starts_with("enabled"));
    assert!(call(json!({"action":"run_now","id":id})).await.unwrap().starts_with("queued"));
    assert_eq!(Schedules::new(&a.ctx.store).due(now()).await.unwrap().len(), 1);
    assert!(call(json!({"action":"remove","id":id})).await.unwrap().starts_with("removed"));
    assert!(call(json!({"action":"remove","id":id})).await.unwrap_err().contains("no schedule"));
    assert_eq!(call(json!({"action":"list"})).await.unwrap(), "no schedules");
    assert!(ScheduleTool.owner_only());
}
