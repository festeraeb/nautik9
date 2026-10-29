//! Helpers for the playbook tasks `save` and the `allow_hosts` guard (kept apart from
//! `playbook.rs`, which holds the language itself).

use crate::tools::{note_by_path, write_note, ToolCtx};
use reqwest::Url;
use serde_json::Value;

/// Notes a playbook `save` may not write: the definitions that approvals are bound to.
const PROTECTED_PREFIXES: &[&str] = &["Jade/Playbooks/", "Jade/Flows/"];
const MAX_SAVE_CHARS: usize = 1_000_000;

/// A usable, non-protected note path.
pub fn check_save_path(path: &str) -> Result<(), String> {
    let p = path.trim().trim_start_matches('/');
    if p.is_empty() || p.len() > 300 {
        return Err("save: the path must be 1-300 characters".into());
    }
    if p.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return Err("save: the path may not contain `..` or empty parts".into());
    }
    if PROTECTED_PREFIXES.iter().any(|x| p.starts_with(x)) {
        return Err(format!("save: {} is where approved definitions live; a playbook may not write there", p.split('/').take(2).collect::<Vec<_>>().join("/")));
    }
    Ok(())
}

/// The text `save` writes: strings as they are, anything else as pretty JSON.
pub fn save_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Write (or append) a result to a note. Returns a one-line summary.
pub async fn save_note(ctx: &ToolCtx, path: &str, content: &Value, append: bool) -> Result<String, String> {
    check_save_path(path)?;
    let text = save_text(content);
    if text.chars().count() > MAX_SAVE_CHARS {
        return Err(format!("save: content longer than {MAX_SAVE_CHARS} characters"));
    }
    let body = if append {
        let old = note_by_path(&ctx.vault, path).await?.map(|n| n.content).unwrap_or_default();
        if old.is_empty() {
            text.clone()
        } else if old.ends_with('\n') {
            format!("{old}{text}")
        } else {
            format!("{old}\n{text}")
        }
    } else {
        text.clone()
    };
    write_note(&ctx.vault, path, &body).await?;
    Ok(format!("{} {} characters to {}", if append { "appended" } else { "saved" }, text.chars().count(), path.trim().trim_start_matches('/')))
}

/// Does `host` match one entry (`example.com` exact, `*.example.com` any subdomain, not the bare domain)?
pub fn host_matches(host: &str, pattern: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pat = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    match pat.strip_prefix("*.") {
        Some(rest) => !rest.is_empty() && host.len() > rest.len() + 1 && host.ends_with(rest) && host[..host.len() - rest.len()].ends_with('.'),
        None => !pat.is_empty() && host == pat,
    }
}

pub fn host_allowed(url: &Url, allow: &[String]) -> bool {
    url.host_str().is_some_and(|h| allow.iter().any(|p| host_matches(h, p)))
}

/// Refuse a URL whose host is not in `allow` (an empty list allows nothing).
pub fn check_url(url: &str, allow: &[String]) -> Result<(), String> {
    let u = Url::parse(url).map_err(|e| format!("allow_hosts: cannot read the URL `{url}`: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("allow_hosts: only http and https URLs".into());
    }
    if host_allowed(&u, allow) {
        Ok(())
    } else {
        Err(format!("allow_hosts: host `{}` is not allowed (allowed: {})", u.host_str().unwrap_or("?"), if allow.is_empty() { "none".to_string() } else { allow.join(", ") }))
    }
}

/// A copy of the context whose HTTP client stops at any redirect that leaves the allowed hosts.
pub fn restricted_ctx(ctx: &ToolCtx, allow: &[String]) -> Result<ToolCtx, String> {
    let allow: Vec<String> = allow.to_vec();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if host_allowed(attempt.url(), &allow) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    Ok(ToolCtx { http: client, ..ctx.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_patterns() {
        assert!(host_matches("api.example.com", "api.example.com"));
        assert!(host_matches("API.Example.com.", "api.example.com"));
        assert!(host_matches("a.b.example.com", "*.example.com"));
        assert!(!host_matches("example.com", "*.example.com"), "wildcard needs a subdomain");
        assert!(!host_matches("badexample.com", "*.example.com"));
        assert!(!host_matches("example.com.evil.io", "example.com"));
        assert!(!host_matches("example.com", ""));
    }

    #[test]
    fn urls_are_parsed_not_matched_as_text() {
        let a = vec!["example.com".to_string()];
        assert!(check_url("https://example.com/x", &a).is_ok());
        assert!(check_url("https://example.com@evil.io/x", &a).is_err(), "userinfo trick");
        assert!(check_url("https://evil.io/?u=example.com", &a).is_err());
        assert!(check_url("ftp://example.com/", &a).is_err());
        assert!(check_url("https://example.com/", &[]).is_err(), "empty list allows nothing");
    }

    #[test]
    fn save_paths() {
        assert!(check_save_path("Reports/today.md").is_ok());
        assert!(check_save_path("/Reports/today.md").is_ok());
        assert!(check_save_path("Jade/Playbooks/x.json").is_err());
        assert!(check_save_path("/Jade/Flows/x.json").is_err());
        assert!(check_save_path("a/../b").is_err());
        assert!(check_save_path("").is_err());
    }
}

/// Tests for the playbook features that live partly here (`save`, `allow_hosts`) and the other
/// language gaps closed with them: retries/until/delay, failed_when, rescue/always, loop_var,
/// debug and wait.
#[cfg(test)]
mod run_tests {
    use crate::playbook::*;
    use crate::store::Store;
    use crate::tools::{Registry, Tool, ToolCtx};
    use async_trait::async_trait;
    use nautik9_wayfinder::WayfinderVault;
    use serde_json::{json, Map, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Fails its first `fail_first` calls, then answers `n=<call number>`.
    struct Flaky {
        calls: AtomicUsize,
        fail_first: usize,
    }

    #[async_trait]
    impl Tool for Flaky {
        fn name(&self) -> &str {
            "flaky"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object","properties":{}})
        }
        async fn call(&self, _a: &Value, _c: &ToolCtx) -> Result<String, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= self.fail_first {
                Err(format!("flaky failure {n}"))
            } else {
                Ok(format!("n={n}"))
            }
        }
    }

    fn ctx() -> ToolCtx {
        ToolCtx { vault: Arc::new(WayfinderVault::in_memory().unwrap()), store: Store::in_memory().unwrap(), http: reqwest::Client::new(), secrets: Arc::new(Default::default()), search: Arc::new(vec![]), owner: true, mcp_local: Arc::new(Default::default()), memory: Arc::new(crate::memory::MemoryIndex::in_memory(None).unwrap()) }
    }

    fn reg(fail_first: usize) -> Registry {
        Registry::with_extra(vec![Arc::new(Flaky { calls: AtomicUsize::new(0), fail_first })]).0
    }

    async fn run_with(r: &Registry, c: &ToolCtx, v: Value) -> RunReport {
        let p: Playbook = serde_json::from_value(v).unwrap();
        let rep = check(&p, None, r);
        assert!(rep.ok, "check failed: {:?}", rep.problems);
        run_playbook(&p, &Map::new(), &[], Trigger::Manual, r, c, None).await
    }

    fn problems(v: Value) -> Vec<String> {
        let p: Playbook = serde_json::from_value(v).unwrap();
        check(&p, None, &reg(0)).problems
    }

    #[tokio::test]
    async fn retries_try_again_and_report_attempts() {
        let c = ctx();
        let r = run_with(&reg(2), &c, json!({"name":"t","tasks":[{"do":"flaky","retries":2}]})).await;
        assert!(r.ok, "{}", r.recap());
        assert!(r.log[0].result.contains("n=3 (attempt 3/3)"), "{}", r.log[0].result);
        let r = run_with(&reg(2), &c, json!({"name":"t","tasks":[{"do":"flaky","retries":1}]})).await;
        assert!(!r.ok && r.error.as_ref().unwrap().contains("after 2 attempts"), "{}", r.recap());
        let r = run_with(&reg(1), &c, json!({"name":"t","tasks":[{"do":"flaky"}]})).await;
        assert!(!r.ok, "no retries unless asked");
    }

    #[tokio::test]
    async fn until_repeats_while_the_condition_is_false() {
        let c = ctx();
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"flaky","register":"f","retries":5,"until":"f.result == 'n=3'"}]})).await;
        assert!(r.ok, "{}", r.recap());
        assert!(r.log[0].result.contains("attempt 3/6"), "{}", r.log[0].result);
        // `result` works too, and an unmet condition is a failure that names it
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"flaky","retries":1,"until":"result.result == 'never'"}]})).await;
        assert!(!r.ok && r.error.as_ref().unwrap().contains("until not met"), "{}", r.recap());
        // until alone means 3 retries
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"flaky","until":"result.result == 'n=4'"}]})).await;
        assert!(r.ok, "{}", r.recap());
    }

    #[tokio::test]
    async fn delay_waits_between_attempts() {
        let c = ctx();
        let t0 = std::time::Instant::now();
        let r = run_with(&reg(99), &c, json!({"name":"t","tasks":[{"do":"flaky","retries":2,"delay":0.1}]})).await;
        assert!(!r.ok);
        assert!(t0.elapsed() >= std::time::Duration::from_millis(200), "two gaps of 0.1 s, got {:?}", t0.elapsed());
        assert!(problems(json!({"name":"t","tasks":[{"do":"flaky","delay":1}]})).iter().any(|p| p.contains("delay needs retries")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"flaky","retries":11}]})).iter().any(|p| p.contains("retries must be")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"flaky","retries":1,"delay":9999}]})).iter().any(|p| p.contains("delay must be")));
    }

    #[tokio::test]
    async fn failed_when_decides_the_verdict() {
        let c = ctx();
        // a successful action marked failed
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"flaky","failed_when":"result.result contains 'n=1'"}]})).await;
        assert!(!r.ok && r.error.as_ref().unwrap().contains("failed_when matched"), "{}", r.recap());
        // a failing action accepted
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"fail","args":{"msg":"warning only"},"register":"w","failed_when":"not (w.error contains 'warning')"},{"do":"assert","args":{"that":"w.ok"}}]})).await;
        assert!(r.ok, "{}", r.recap());
        // failed_when plus retries retries on the failed_when verdict
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"flaky","retries":3,"failed_when":"result.result != 'n=2'"}]})).await;
        assert!(r.ok, "{}", r.recap());
        // unknown names are caught by check
        assert!(problems(json!({"name":"t","tasks":[{"do":"flaky","failed_when":"nope == 1"}]})).iter().any(|p| p.contains("undeclared variable `nope`")));
    }

    #[tokio::test]
    async fn rescue_and_always_blocks() {
        let c = ctx();
        let r = reg(0);
        let base = |rescue: Value| json!({"name":"t","tasks":[
            {"do":"block","tasks":[{"do":"fail","args":{"msg":"boom"}}],
             "rescue": rescue,
             "always":[{"do":"save","args":{"path":"Out/always.md","content":"cleaned"}}]},
            {"do":"assert","args":{"that":"true"}}]});
        let rep = run_with(&r, &c, base(json!([{"do":"assert","args":{"that":"error contains 'boom'"}},{"do":"set","args":{"healed":true}}]))).await;
        assert!(rep.ok, "{}", rep.recap());
        assert!(rep.log.iter().any(|l| l.action == "block" && l.result.contains("(rescued)")), "{}", rep.recap());
        assert!(crate::tools::note_by_path(&c.vault, "Out/always.md").await.unwrap().is_some(), "always ran");
        // a failing rescue fails the run, and always still ran
        crate::tools::write_note(&c.vault, "Out/always.md", "old").await.unwrap();
        let rep = run_with(&r, &c, base(json!([{"do":"fail","args":{"msg":"rescue broke"}}]))).await;
        assert!(!rep.ok && rep.error.as_ref().unwrap().contains("rescue failed"), "{}", rep.recap());
        assert_eq!(crate::tools::note_by_path(&c.vault, "Out/always.md").await.unwrap().unwrap().content, "cleaned");
        // without a rescue the failure stands; always still runs
        let rep = run_with(&r, &c, json!({"name":"t","tasks":[{"do":"block","tasks":[{"do":"fail"}],"always":[{"do":"set","args":{"x":1}}]}]})).await;
        assert!(!rep.ok);
        assert!(rep.log.iter().any(|l| l.action == "set"));
        // a fatal problem (bad expression at run time) is not rescued, not hidden by ignore_errors, but always runs
        let rep = run_with(&r, &c, json!({"name":"t","vars":{"v":"x"},"tasks":[{"do":"block","ignore_errors":true,"tasks":[{"do":"set","args":{"a":"{{ int(v) }}"}}],"rescue":[{"do":"set","args":{"rescued":true}}],"always":[{"do":"set","args":{"cleaned":true}}]}]})).await;
        assert!(!rep.ok, "{}", rep.recap());
        assert!(!rep.log.iter().any(|l| l.path.contains("rescue")), "{}", rep.recap());
        assert!(rep.log.iter().any(|l| l.path.contains("always")), "{}", rep.recap());
        // misplaced fields are check problems
        assert!(problems(json!({"name":"t","tasks":[{"do":"set","args":{"a":1},"rescue":[{"do":"set","args":{"b":1}}]}]})).iter().any(|p| p.contains("only `block` tasks have rescue")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"block","retries":1,"tasks":[{"do":"set","args":{"a":1}}]}]})).iter().any(|p| p.contains("block cannot have retries")));
    }

    #[tokio::test]
    async fn loop_var_renames_item_and_restores_outer_names() {
        let c = ctx();
        let r = run_with(&reg(0), &c, json!({"name":"t","vars":{"f":"outer"},"tasks":[
            {"do":"set","args":{"last":"{{ f }}"},"loop":["a","b"],"loop_var":"f","register":"s"},
            {"do":"debug","args":{"msg":"{{ f }} {{ s.results[1].item }}"},"register":"d"},
            {"do":"assert","args":{"that":["f == 'outer'","d.result == 'outer b'"]}}]})).await;
        assert!(r.ok, "{}", r.recap());
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[
            {"do":"debug","args":{"msg":"{{ file }}#{{ file_index }}"},"loop":["a","b"],"loop_var":"file","register":"d"},
            {"do":"assert","args":{"that":"d.results[1].result == 'b#1'"}}]})).await;
        assert!(r.ok, "{}", r.recap());
        // `item` is not defined when loop_var renames it; loop_var needs a loop
        assert!(problems(json!({"name":"t","tasks":[{"do":"debug","args":{"msg":"{{ item }}"},"loop":["a"],"loop_var":"x"}]})).iter().any(|p| p.contains("undeclared variable `item`")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"debug","args":{"msg":"hi"},"loop_var":"x"}]})).iter().any(|p| p.contains("loop_var needs a loop")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"debug","args":{"msg":"hi"},"loop":[1],"loop_var":"not ok"}]})).iter().any(|p| p.contains("usable loop_var")));
    }

    #[tokio::test]
    async fn debug_and_wait() {
        let c = ctx();
        let r = run_with(&reg(0), &c, json!({"name":"t","vars":{"n":2},"tasks":[
            {"do":"debug","args":{"msg":"n is {{ n }}","var":"n + 1"},"register":"d"},
            {"do":"wait","args":{"seconds":0.05}},
            {"do":"assert","args":{"that":"d.result == 'n is 2 | n + 1 = 3'"}}]})).await;
        assert!(r.ok, "{}", r.recap());
        assert!(r.log[1].result.starts_with("waited 0.05"));
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[{"do":"wait","args":{"seconds":99999}}]})).await;
        assert!(!r.ok && r.error.as_ref().unwrap().contains("between 0 and"), "{}", r.recap());
        assert!(problems(json!({"name":"t","tasks":[{"do":"debug"}]})).iter().any(|p| p.contains("debug needs")));
        assert!(problems(json!({"name":"t","tasks":[{"do":"wait"}]})).iter().any(|p| p.contains("wait needs")));
    }

    #[tokio::test]
    async fn save_writes_and_appends_results_to_notes() {
        let c = ctx();
        let r = run_with(&reg(0), &c, json!({"name":"t","tasks":[
            {"do":"flaky","register":"f"},
            {"do":"save","args":{"path":"Reports/out.md","content":"result: {{ f.result }}"}},
            {"do":"save","args":{"path":"Reports/out.md","content":"{{ f }}","append":true}},
            {"do":"save","args":{"path":"Reports/new.md","content":"{{ f }}","append":true}}]})).await;
        assert!(r.ok, "{}", r.recap());
        let n = crate::tools::note_by_path(&c.vault, "Reports/out.md").await.unwrap().unwrap().content;
        assert!(n.starts_with("result: n=1\n{"), "{n}");
        assert!(n.contains("\"result\": \"n=1\""), "an object is saved as JSON: {n}");
        assert!(crate::tools::note_by_path(&c.vault, "Reports/new.md").await.unwrap().is_some(), "append creates the note");
        // approved definitions are off limits, literal path (check) or computed path (run)
        assert!(problems(json!({"name":"t","tasks":[{"do":"save","args":{"path":"Jade/Playbooks/x.json","content":"{}"}}]})).iter().any(|p| p.contains("approved definitions")));
        let r = run_with(&reg(0), &c, json!({"name":"t","vars":{"d":"Jade/Flows"},"tasks":[{"do":"save","args":{"path":"{{ d }}/x.json","content":"{}"}}]})).await;
        assert!(!r.ok && r.error.as_ref().unwrap().contains("approved definitions"), "{}", r.recap());
    }

    async fn server() -> String {
        use axum::routing::get;
        let app = axum::Router::new()
            .route("/hello", get(|| async { "hello there" }))
            .route("/same", get(|| async { axum::response::Redirect::temporary("/hello") }))
            .route("/away", get(|| async { axum::response::Redirect::temporary("http://localhost:1/hello") }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn allow_hosts_limits_http_tools_and_redirects() {
        let base = server().await;
        let c = ctx();
        let r = reg(0);
        let pb = |path: &str, hosts: Value, tool: &str| json!({"name":"t","tasks":[{"do":tool,"args":{"url":format!("{base}{path}")},"allow_hosts":hosts,"register":"h"}]});
        let ok = run_with(&r, &c, pb("/hello", json!(["127.0.0.1"]), "http_request")).await;
        assert!(ok.ok && ok.log[0].result.contains("hello there"), "{}", ok.recap());
        let ok = run_with(&r, &c, pb("/same", json!(["127.0.0.1"]), "http_request")).await;
        assert!(ok.log[0].result.contains("hello there"), "a redirect inside the list is followed: {}", ok.recap());
        let no = run_with(&r, &c, pb("/hello", json!(["example.com"]), "http_request")).await;
        assert!(!no.ok && no.error.as_ref().unwrap().contains("not allowed"), "{}", no.recap());
        let none = run_with(&r, &c, pb("/hello", json!([]), "web_fetch")).await;
        assert!(!none.ok && none.error.as_ref().unwrap().contains("allowed: none"), "{}", none.recap());
        let away = run_with(&r, &c, pb("/away", json!(["127.0.0.1"]), "http_request")).await;
        assert!(away.log[0].result.contains("HTTP 307") && !away.log[0].result.contains("hello"), "a redirect out of the list is not followed: {}", away.recap());
        // without allow_hosts nothing is restricted, and allow_hosts on other actions is a check problem
        let free = run_with(&r, &c, json!({"name":"t","tasks":[{"do":"http_request","args":{"url":format!("{base}/same")}}]})).await;
        assert!(free.log[0].result.contains("hello there"));
        assert!(problems(json!({"name":"t","tasks":[{"do":"set","args":{"a":1},"allow_hosts":["x.com"]}]})).iter().any(|p| p.contains("allow_hosts only applies")));
    }
}
