//! Schedules that run playbooks, and flows (schedule / webhook / note_changed / sms_received
//! triggers): approval gates every unattended firing, a changed file needs approval again.

use axum::{routing::post, Json, Router};
use chrono::{DateTime, Duration, FixedOffset};
use nautik9_agent::flow::{self, Event, FlowError, FlowRunner};
use nautik9_agent::playbook;
use nautik9_agent::schedule::{Runner, Schedules, CRONTAB_PATH, LOG_PATH};
use nautik9_agent::store::Store;
use nautik9_agent::tools::{note_by_path, write_note, ToolCtx};
use nautik9_agent::{Agent, AgentConfig};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule, Provider};
use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

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
        memory: Arc::new(nautik9_agent::memory::MemoryIndex::in_memory(None).unwrap()),
    };
    Arc::new(Agent::new(Arc::new(llm), ctx, AgentConfig::default()))
}

fn dead() -> Arc<Agent> {
    agent("http://127.0.0.1:9/v1")
}

async fn mock(seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(req): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(req);
                Json(json!({"model": "mock-1", "choices": [{"message": {"role":"assistant","content":"Noted."}, "finish_reason": "stop"}]}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn now() -> DateTime<FixedOffset> {
    chrono::Local::now().fixed_offset()
}

async fn put(a: &Agent, path: &str, v: &Value) {
    write_note(&a.ctx.vault, path, &serde_json::to_string_pretty(v).unwrap()).await.unwrap();
}

async fn note(a: &Agent, path: &str) -> Option<String> {
    note_by_path(&a.ctx.vault, path).await.unwrap().map(|n| n.content)
}

async fn log(a: &Agent) -> String {
    note(a, LOG_PATH).await.unwrap_or_default()
}

/// A playbook that appends a line to `Out/marker.md`.
async fn mark_playbook(a: &Agent) {
    put(a, "Jade/Playbooks/mark.json", &json!({"name":"mark","vars":{"who":"nobody"},"tasks":[
        {"do":"save","args":{"path":"Out/marker.md","content":"hello {{ who }}","append":true}}]})).await;
}

async fn marker_lines(a: &Agent) -> usize {
    note(a, "Out/marker.md").await.map(|t| t.lines().count()).unwrap_or(0)
}

async fn approve_pb(a: &Agent, name: &str) {
    playbook::approve(&a.ctx, &a.tools, name).await.unwrap();
}

async fn approve_flow(a: &Agent, name: &str) {
    flow::approve(&a.ctx, &a.tools, name).await.unwrap();
}

async fn fire(a: &Agent, name: &str, e: Event) -> Result<flow::FireOutcome, FlowError> {
    flow::fire(&a.ctx, &a.tools, Some(a as &dyn playbook::Asker), name, &e).await
}

// ── schedules that run playbooks ────────────────────────────────────────────

#[tokio::test]
async fn a_playbook_schedule_runs_only_an_approved_playbook() {
    let a = dead();
    mark_playbook(&a).await;
    let runner = Runner::new(a.clone(), 0);
    let sc = Schedules::new(&a.ctx.store);
    let t0 = now();
    assert!(sc.add_playbook("hourly", "bad name!", t0).await.is_err());
    assert!(sc.add_playbook("whenever", "mark", t0).await.is_err());
    let s = sc.add_playbook("every 10 minutes", "mark", t0).await.unwrap();
    assert_eq!((s.kind.as_str(), s.payload.as_str()), ("playbook", "mark"));

    // not approved: a recorded error, nothing runs
    assert_eq!(runner.tick(t0 + Duration::minutes(11)).await, 1);
    let r = sc.get(&s.id).await.unwrap().unwrap();
    assert!(r.last_result.as_deref().unwrap().starts_with("ERROR:") && r.last_result.as_deref().unwrap().contains("not approved"), "{r:?}");
    assert_eq!(marker_lines(&a).await, 0);
    assert!(log(&a).await.contains(" ERROR ⏰ every 10 minutes playbook mark:"), "{}", log(&a).await);

    // approved: runs with trigger `schedule`
    approve_pb(&a, "mark").await;
    assert_eq!(runner.tick(t0 + Duration::minutes(22)).await, 1);
    let r = sc.get(&s.id).await.unwrap().unwrap();
    assert!(r.last_result.as_deref().unwrap().contains("playbook `mark` (schedule): OK"), "{r:?}");
    assert_eq!(marker_lines(&a).await, 1);
    assert_eq!(a.ctx.store.last_playbook_run("mark").await.unwrap().unwrap().1, "schedule");

    // edited after approval: refused again
    put(&a, "Jade/Playbooks/mark.json", &json!({"name":"mark","tasks":[{"do":"save","args":{"path":"Out/marker.md","content":"changed","append":true}}]})).await;
    assert_eq!(runner.tick(t0 + Duration::minutes(33)).await, 1);
    let r = sc.get(&s.id).await.unwrap().unwrap();
    assert!(r.last_result.as_deref().unwrap().contains("changed since it was approved"), "{r:?}");
    assert_eq!(marker_lines(&a).await, 1);

    // "run now" goes the same way
    approve_pb(&a, "mark").await;
    let r = runner.run_now(&s.id).await.unwrap();
    assert!(r.last_result.as_deref().unwrap().contains("(schedule): OK"), "{r:?}");
    assert_eq!(marker_lines(&a).await, 2);
}

#[tokio::test]
async fn a_crontab_line_can_run_a_playbook() {
    let a = dead();
    mark_playbook(&a).await;
    let runner = Runner::new(a.clone(), 0);
    write_note(&a.ctx.vault, CRONTAB_PATH, "* * * * * playbook mark\n* * * * * playbook bad name here\n").await.unwrap();
    let t0 = {
        use chrono::Timelike;
        now().with_second(5).unwrap().with_nanosecond(0).unwrap()
    };
    assert_eq!(runner.tick(t0).await, 0);
    assert_eq!(runner.tick(t0 + Duration::minutes(2)).await, 1, "due, but not approved");
    assert_eq!(marker_lines(&a).await, 0);
    let l = log(&a).await;
    assert!(l.contains("ERROR ⏰ * * * * * (crontab playbook mark):") && l.contains("not approved"), "{l}");
    assert_eq!(l.matches("Jade/crontab: line 2:").count(), 1, "the bad line is reported: {l}");
    approve_pb(&a, "mark").await;
    assert_eq!(runner.tick(t0 + Duration::minutes(4)).await, 1);
    assert_eq!(marker_lines(&a).await, 1);
    assert!(log(&a).await.contains(" ok ⏰ * * * * * (crontab playbook mark):"));
}

// ── flows ───────────────────────────────────────────────────────────────────

fn flow_doc(name: &str, trigger: Value, action: Value) -> Value {
    json!({"name": name, "trigger": trigger, "action": action})
}

#[tokio::test]
async fn an_unapproved_flow_does_not_fire_and_a_changed_file_needs_approval_again() {
    let a = dead();
    mark_playbook(&a).await;
    approve_pb(&a, "mark").await;
    let doc = flow_doc("hook1", json!({"type":"webhook"}), json!({"playbook":"mark","vars":{"who":"{{ trigger.payload.name }}"}}));
    put(&a, "Jade/Flows/hook1.json", &doc).await;

    // unapproved: nothing runs, the refusal is recorded
    let r = fire(&a, "hook1", Event::Webhook(json!({"name":"ann"}))).await;
    assert!(matches!(r, Err(FlowError::NotApproved(ref m)) if m.contains("not approved")), "{r:?}");
    assert_eq!(marker_lines(&a).await, 0);

    // approved: fires, the playbook sees the templated var, trigger is `flow`
    approve_flow(&a, "hook1").await;
    let r = fire(&a, "hook1", Event::Webhook(json!({"name":"ann"}))).await.unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(note(&a, "Out/marker.md").await.unwrap(), "hello ann");
    assert_eq!(r.report.unwrap().trigger, playbook::Trigger::Flow);
    let last = a.ctx.store.last_playbook_run("flow:hook1").await.unwrap().unwrap();
    assert_eq!((last.1.as_str(), last.2), ("webhook", true));

    // reformatting alone keeps the approval
    write_note(&a.ctx.vault, "Jade/Flows/hook1.json", &serde_json::to_string(&doc).unwrap()).await.unwrap();
    assert!(fire(&a, "hook1", Event::Webhook(json!({"name":"bob"}))).await.unwrap().ok);
    assert_eq!(marker_lines(&a).await, 2);

    // a content change voids it
    let mut changed = doc.clone();
    changed["action"]["vars"]["who"] = json!("mallory-{{ trigger.payload.name }}");
    put(&a, "Jade/Flows/hook1.json", &changed).await;
    let r = fire(&a, "hook1", Event::Webhook(json!({"name":"eve"}))).await;
    assert!(matches!(r, Err(FlowError::NotApproved(ref m)) if m.contains("changed since")), "{r:?}");
    assert_eq!(marker_lines(&a).await, 2);
    // the owner can still run it by hand (manual), and approve it again
    let m = fire(&a, "hook1", Event::Manual(json!({"name":"hand"}))).await.unwrap();
    assert!(m.ok && m.report.as_ref().unwrap().trigger == playbook::Trigger::Manual, "{m:?}");
    approve_flow(&a, "hook1").await;
    assert!(fire(&a, "hook1", Event::Webhook(json!({"name":"eve"}))).await.unwrap().ok);
    assert!(note(&a, "Out/marker.md").await.unwrap().contains("mallory-eve"));

    // runs are in cron.log
    let l = log(&a).await;
    assert!(l.contains(" ok flow:hook1: playbook `mark`: ok"), "{l}");
}

#[tokio::test]
async fn a_flow_needs_its_playbook_approved_too_and_other_gates() {
    let a = dead();
    mark_playbook(&a).await;
    put(&a, "Jade/Flows/f.json", &flow_doc("f", json!({"type":"webhook"}), json!({"playbook":"mark"}))).await;
    approve_flow(&a, "f").await;
    let r = fire(&a, "f", Event::Webhook(Value::Null)).await.unwrap();
    assert!(!r.ok && r.detail.contains("did not run") && r.detail.contains("not approved"), "{r:?}");
    assert_eq!(marker_lines(&a).await, 0);
    // wrong event type
    let r = fire(&a, "f", Event::sms("+1 555 0100", "hi")).await;
    assert!(matches!(r, Err(FlowError::Refused(ref m)) if m.contains("does not take")), "{r:?}");
    // disabled
    let mut d = flow_doc("g", json!({"type":"webhook"}), json!({"playbook":"mark"}));
    d["enabled"] = json!(false);
    put(&a, "Jade/Flows/g.json", &d).await;
    approve_flow(&a, "g").await;
    assert!(matches!(fire(&a, "g", Event::Webhook(Value::Null)).await, Err(FlowError::Refused(ref m)) if m.contains("disabled")));
    // missing / invalid
    assert!(matches!(fire(&a, "nope", Event::Manual(Value::Null)).await, Err(FlowError::NotFound(_))));
    write_note(&a.ctx.vault, "Jade/Flows/h.json", "{not json").await.unwrap();
    assert!(matches!(fire(&a, "h", Event::Manual(Value::Null)).await, Err(FlowError::Invalid(_))));
    // a flow that cannot pass the check cannot be approved
    put(&a, "Jade/Flows/bad.json", &flow_doc("bad", json!({"type":"webhook"}), json!({"playbook":"missing"}))).await;
    assert!(matches!(flow::approve(&a.ctx, &a.tools, "bad").await, Err(FlowError::Invalid(_))));
}

#[tokio::test]
async fn flow_checks_find_problems() {
    let a = dead();
    mark_playbook(&a).await;
    let cases: Vec<(Value, &str)> = vec![
        (flow_doc("c1", json!({"type":"schedule","when":"whenever"}), json!({"playbook":"mark"})), "trigger.when"),
        (flow_doc("c2", json!({"type":"note_changed","path":""}), json!({"playbook":"mark"})), "trigger.path"),
        (flow_doc("c3", json!({"type":"webhook"}), json!({"playbook":"mark","prompt":"x"})), "not both"),
        (flow_doc("c4", json!({"type":"webhook"}), json!({})), "needs a playbook or a prompt"),
        (flow_doc("c5", json!({"type":"sms_received"}), json!({"prompt":"reply to {{ trigger.body }}"})), "can only run a playbook"),
        (flow_doc("c6", json!({"type":"webhook"}), json!({"playbook":"mark","vars":{"who":"{{ other }}"}})), "undeclared variable `other`"),
        (flow_doc("c7", json!({"type":"webhook"}), json!({"playbook":"mark","vars":{"nope":"x"}})), "not declared in the vars"),
        (flow_doc("c8", json!({"type":"webhook"}), json!({"playbook":"ghost"})), "no playbook `ghost`"),
        (flow_doc("c9", json!({"type":"webhook"}), json!({"prompt":"x","vars":{"a":1}})), "only applies to a playbook"),
        (flow_doc("wrongname", json!({"type":"webhook"}), json!({"playbook":"mark"})), "does not match the note"),
        (json!({"name":"c11","trigger":{"type":"webhook","extra":1},"action":{"playbook":"mark"}}), "unknown field"),
        (json!({"name":"c12","trigger":{"type":"carrier_pigeon"},"action":{"playbook":"mark"}}), "unknown variant"),
    ];
    for (doc, want) in cases {
        let file = doc["name"].as_str().unwrap().to_string();
        let file = if file == "wrongname" { "c10".to_string() } else { file };
        put(&a, &format!("Jade/Flows/{file}.json"), &doc).await;
        let r = flow::check_named(&a.ctx, &a.tools, &file).await.unwrap();
        assert!(!r.ok && r.problems.iter().any(|p| p.contains(want)), "{file}: wanted `{want}` in {:?}", r.problems);
    }
    put(&a, "Jade/Flows/ok.json", &flow_doc("ok", json!({"type":"note_changed","path":"Inbox/**/*.md"}), json!({"playbook":"mark","vars":{"who":"{{ trigger.path }}"}}))).await;
    let r = flow::check_named(&a.ctx, &a.tools, "ok").await.unwrap();
    assert!(r.ok, "{:?}", r.problems);
    let l = flow::list(&a.ctx, &a.tools).await.unwrap();
    assert_eq!(l.len(), 13);
    let ok = l.iter().find(|f| f["name"] == "ok").unwrap();
    assert_eq!((ok["valid"].clone(), ok["approved"].clone(), ok["playbook_approved"].clone()), (json!(true), json!(false), json!(false)));
}

#[test]
fn globs() {
    use flow::glob_match as g;
    assert!(g("Inbox/*.md", "Inbox/a.md"));
    assert!(!g("Inbox/*.md", "Inbox/sub/a.md"), "* stays in a folder");
    assert!(g("Inbox/**/*.md", "Inbox/sub/deep/a.md"));
    assert!(g("Inbox/**/*.md", "Inbox/a.md"), "** may match no folder");
    assert!(g("**", "any/thing.md"));
    assert!(g("/Inbox/?.md", "Inbox/a.md"));
    assert!(!g("Inbox/?.md", "Inbox/ab.md"));
    assert!(!g("Inbox/*.md", "Inbox/a.mdx"));
    assert!(g("a.b+c", "a.b+c"), "regex characters are literal");
    assert!(!g("a.b", "axb"));
}

#[tokio::test]
async fn sms_received_fires_matching_approved_flows_only() {
    let a = dead();
    mark_playbook(&a).await;
    approve_pb(&a, "mark").await;
    put(&a, "Jade/Flows/sms_all.json", &flow_doc("sms_all", json!({"type":"sms_received"}), json!({"playbook":"mark","vars":{"who":"{{ trigger.sender }}:{{ trigger.body }}"}}))).await;
    put(&a, "Jade/Flows/sms_pick.json", &flow_doc("sms_pick", json!({"type":"sms_received","from":"555-0100","contains":"TIDE"}), json!({"playbook":"mark","vars":{"who":"pick"}}))).await;
    let ev = Event::sms("+1 (555) 0100", "what is the tide today");
    let run = |e: Event| {
        let a = a.clone();
        async move { flow::fire_matching(&a.ctx, &a.tools, Some(a.as_ref() as &dyn playbook::Asker), &e).await }
    };
    // nothing approved: both are refused and recorded, nothing runs
    let res = run(ev.clone()).await;
    assert_eq!(res.len(), 2);
    assert!(res.iter().all(|(_, r)| matches!(r, Err(FlowError::NotApproved(_)))));
    assert_eq!(marker_lines(&a).await, 0);
    assert!(log(&a).await.contains("ERROR flow:sms_all: not fired:"), "{}", log(&a).await);
    // approve only sms_pick
    approve_flow(&a, "sms_pick").await;
    run(ev.clone()).await;
    assert_eq!(note(&a, "Out/marker.md").await.unwrap(), "hello pick");
    // a text from another number / without the word only reaches the unfiltered flow (still unapproved)
    let res = run(Event::sms("+1 555 9999", "what is the tide")).await;
    assert_eq!(res.len(), 1, "sms_pick does not match this sender");
    approve_flow(&a, "sms_all").await;
    run(Event::sms("+1 555 9999", "hello there")).await;
    assert!(note(&a, "Out/marker.md").await.unwrap().ends_with("hello +1 555 9999:hello there"));
    assert_eq!(marker_lines(&a).await, 2);
}

#[tokio::test]
async fn note_changed_fires_for_new_and_edited_notes_without_looping() {
    let a = dead();
    // Appends the changed path to a note inside the watched folder: it must not retrigger itself.
    put(&a, "Jade/Playbooks/seen.json", &json!({"name":"seen","vars":{"p":"","c":""},"tasks":[
        {"do":"save","args":{"path":"Inbox/seen-log.md","content":"{{ c }} {{ p }}","append":true}}]})).await;
    approve_pb(&a, "seen").await;
    put(&a, "Jade/Flows/watch.json", &flow_doc("watch", json!({"type":"note_changed","path":"Inbox/*.md"}), json!({"playbook":"seen","vars":{"p":"{{ trigger.path }}","c":"{{ trigger.change }}"}}))).await;
    let runner = FlowRunner::new(a.clone());
    let t0 = now();
    write_note(&a.ctx.vault, "Inbox/old.md", "before").await.unwrap();

    // not approved: baseline only, then a change is refused and recorded
    assert_eq!(runner.tick(t0).await, 0, "first look records a baseline");
    write_note(&a.ctx.vault, "Inbox/a.md", "one").await.unwrap();
    assert_eq!(runner.tick(t0).await, 1);
    assert!(note(&a, "Inbox/seen-log.md").await.is_none(), "unapproved flow did not run");
    assert!(log(&a).await.contains("ERROR flow:watch: not fired:"));

    approve_flow(&a, "watch").await;
    write_note(&a.ctx.vault, "Inbox/b.md", "two").await.unwrap();
    write_note(&a.ctx.vault, "Inbox/sub/c.md", "outside the glob").await.unwrap();
    write_note(&a.ctx.vault, "Elsewhere/d.md", "outside the glob").await.unwrap();
    assert_eq!(runner.tick(t0).await, 1);
    assert_eq!(note(&a, "Inbox/seen-log.md").await.unwrap(), "created Inbox/b.md");
    // the flow's own write to Inbox/seen-log.md is absorbed: no refire
    assert_eq!(runner.tick(t0).await, 0);
    assert_eq!(runner.tick(t0).await, 0);
    // an edit fires with change=modified
    write_note(&a.ctx.vault, "Inbox/b.md", "two, edited").await.unwrap();
    assert_eq!(runner.tick(t0).await, 1);
    assert_eq!(note(&a, "Inbox/seen-log.md").await.unwrap(), "created Inbox/b.md\nmodified Inbox/b.md");
    // system notes never trigger, even under a catch-all glob
    put(&a, "Jade/Flows/all.json", &flow_doc("all", json!({"type":"note_changed","path":"**"}), json!({"playbook":"seen"}))).await;
    runner.tick(t0).await;
    write_note(&a.ctx.vault, "Jade/Logs/cron.log", "line").await.unwrap();
    write_note(&a.ctx.vault, "Jade/Playbooks/zzz.json", "{}").await.unwrap();
    assert_eq!(runner.tick(t0).await, 0);
}

#[tokio::test]
async fn a_schedule_flow_fires_when_due_and_only_when_approved() {
    let a = dead();
    mark_playbook(&a).await;
    approve_pb(&a, "mark").await;
    put(&a, "Jade/Flows/tick.json", &flow_doc("tick", json!({"type":"schedule","when":"every 10 minutes"}), json!({"playbook":"mark","vars":{"who":"{{ trigger.when }}"}}))).await;
    let runner = FlowRunner::new(a.clone());
    let t0 = now();
    assert_eq!(runner.tick(t0).await, 0, "first sight only schedules");
    assert_eq!(runner.tick(t0 + Duration::minutes(5)).await, 0);
    assert_eq!(runner.tick(t0 + Duration::minutes(11)).await, 1, "due but not approved: refused");
    assert_eq!(marker_lines(&a).await, 0);
    assert!(log(&a).await.contains("ERROR flow:tick: not fired:"));
    approve_flow(&a, "tick").await;
    assert_eq!(runner.tick(t0 + Duration::minutes(12)).await, 0, "next run is 10 minutes after the last");
    assert_eq!(runner.tick(t0 + Duration::minutes(22)).await, 1);
    assert_eq!(note(&a, "Out/marker.md").await.unwrap(), "hello every 10 minutes");
}

#[tokio::test]
async fn a_prompt_flow_asks_the_agent_and_manual_run_needs_no_approval() {
    let seen = Arc::new(Mutex::new(vec![]));
    let a = agent(&mock(seen.clone()).await);
    put(&a, "Jade/Flows/brief.json", &flow_doc("brief", json!({"type":"note_changed","path":"Inbox/*.md"}), json!({"prompt":"Summarise {{ trigger.path }}"}))).await;
    let r = fire(&a, "brief", Event::NoteChanged { path: "Inbox/x.md".into(), change: "created".into() }).await;
    assert!(matches!(r, Err(FlowError::NotApproved(_))));
    assert!(seen.lock().unwrap().is_empty(), "no model call without approval");
    let r = fire(&a, "brief", Event::Manual(Value::Null)).await.unwrap();
    assert!(r.ok && r.reply.as_deref() == Some("Noted."), "{r:?}");
    assert!(seen.lock().unwrap()[0]["messages"].to_string().contains("Summarise "));
    approve_flow(&a, "brief").await;
    let r = fire(&a, "brief", Event::NoteChanged { path: "Inbox/x.md".into(), change: "created".into() }).await.unwrap();
    assert!(r.ok);
    assert!(seen.lock().unwrap().last().unwrap()["messages"].to_string().contains("Summarise Inbox/x.md"));
    // a dead model is a failed flow run, recorded, not an invented answer
    let b = dead();
    put(&b, "Jade/Flows/brief.json", &flow_doc("brief", json!({"type":"webhook"}), json!({"prompt":"hi"}))).await;
    let r = fire(&b, "brief", Event::Manual(Value::Null)).await.unwrap();
    assert!(!r.ok && r.detail.contains("prompt failed") && r.reply.is_none(), "{r:?}");
    assert!(!b.ctx.store.last_playbook_run("flow:brief").await.unwrap().unwrap().2);
}
