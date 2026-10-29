//! Agent tools (Jade Vault tool names and semantics, see docs/jade-and-memory.md upstream).
//!
//! Every tool is a name, a description, a JSON-schema for its arguments and an async `call`.
//! A tool returns text the model reads, or `Err(text)` which the model also reads (as
//! `ERROR: ...`), never a made-up success. Owner-only tools are not offered to other callers.
//!
//! Notes: a note's path is its vault title (`Jade/Memory.md`). `write` replaces a whole note
//! (creating it if missing); `edit` replaces exact text that must occur exactly once.

use async_trait::async_trait;
use nautik9_wayfinder::{VaultItem, WayfinderVault};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::html2md::html_to_markdown;
use crate::store::Store;

pub const MEMORY_PATH: &str = "Jade/Memory.md";

/// Everything a tool may touch.
#[derive(Clone)]
pub struct ToolCtx {
    pub vault: Arc<WayfinderVault>,
    pub store: Store,
    pub http: reqwest::Client,
    /// `{{NAME}}` -> value, for http_request. Values never appear in tool output.
    pub secrets: Arc<HashMap<String, String>>,
    /// web_search backends, tried in order.
    pub search: Arc<Vec<SearchBackend>>,
    /// True when the caller is the owner (the token holder). Others get no owner tools.
    pub owner: bool,
    /// Local MCP tool name -> its server (what this server's /api/agent/mcp proxy forwards to).
    pub mcp_local: Arc<HashMap<String, McpServer>>,
    /// Hybrid search index and optional embedding client (see `memory`).
    pub memory: Arc<crate::memory::MemoryIndex>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    /// Only offered to the owner.
    fn owner_only(&self) -> bool {
        true
    }
    /// Safe to run at the same time as other parallel tools (no writes).
    fn parallel(&self) -> bool {
        false
    }
    /// Read-only (offered to `delegate` sub-agents).
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: &Value, ctx: &ToolCtx) -> Result<String, String>;
}

pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    /// The standard set plus `extra` (e.g. MCP tools); an extra whose name is taken is skipped
    /// and returned in the second value.
    pub fn with_extra(extra: Vec<Arc<dyn Tool>>) -> (Self, Vec<String>) {
        let mut r = Self::standard();
        let mut skipped = Vec::new();
        for t in extra {
            if r.get(t.name()).is_some() || t.name() == "delegate" {
                skipped.push(t.name().to_string());
            } else {
                r.tools.push(t);
            }
        }
        (r, skipped)
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// The standard set.
    pub fn standard() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(NotesSearch),
            Arc::new(NotesRead),
            Arc::new(NotesList),
            Arc::new(NotesWrite),
            Arc::new(NotesEdit),
            Arc::new(Memory),
            Arc::new(WebSearch),
            Arc::new(WebFetch),
            Arc::new(HttpRequest),
            Arc::new(ChatHistory),
            Arc::new(SkillTool),
            Arc::new(crate::memory::EmbeddingsTool),
        ];
        Registry { tools }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// OpenAI `tools` array for the caller (owner or not; read-only for sub-agents).
    pub fn schema(&self, owner: bool, read_only: bool, extra: &[Value]) -> Value {
        let mut v: Vec<Value> = self
            .tools
            .iter()
            .filter(|t| (owner || !t.owner_only()) && (!read_only || t.read_only()))
            .map(|t| json!({"type": "function", "function": {"name": t.name(), "description": t.description(), "parameters": t.parameters()}}))
            .collect();
        v.extend(extra.iter().cloned());
        Value::Array(v)
    }

    pub fn allowed(&self, name: &str, owner: bool, read_only: bool) -> Option<Arc<dyn Tool>> {
        self.get(name).filter(|t| (owner || !t.owner_only()) && (!read_only || t.read_only()))
    }
}

fn s<'a>(args: &'a Value, k: &str) -> Result<&'a str, String> {
    args[k].as_str().ok_or_else(|| format!("missing string argument `{k}`"))
}

pub async fn note_by_path(vault: &WayfinderVault, path: &str) -> Result<Option<VaultItem>, String> {
    let p = path.trim().trim_start_matches('/');
    let items = vault.list_items().await.map_err(|e| e.to_string())?;
    Ok(items.into_iter().find(|n| n.title.trim_start_matches('/') == p || n.id == p))
}

pub async fn write_note(vault: &WayfinderVault, path: &str, content: &str) -> Result<bool, String> {
    let p = path.trim().trim_start_matches('/');
    if p.is_empty() {
        return Err("empty path".into());
    }
    let existing = note_by_path(vault, p).await?;
    let id = existing.as_ref().map(|n| n.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    vault.put_item(&id, p, content, &[], None).await.map_err(|e| e.to_string())?;
    Ok(existing.is_none())
}

// ── notes ───────────────────────────────────────────────────────────────────

struct NotesSearch;
#[async_trait]
impl Tool for NotesSearch {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search the user's notes by keywords. Returns note paths with a short snippet; read a note with `read` before quoting it."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string"},"k":{"type":"integer","description":"max results (default 8)"}},"required":["query"]})
    }
    fn parallel(&self) -> bool {
        true
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let q = s(a, "query")?;
        let k = a["k"].as_u64().unwrap_or(8).clamp(1, 30) as usize;
        let hits = c.vault.search(q, None, k).await.map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(format!("No notes match `{q}`."));
        }
        Ok(hits
            .iter()
            .map(|h| format!("- {} (score {:.2}): {}", h.title, h.score, h.snippet.chars().take(160).collect::<String>().replace('\n', " ")))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct NotesRead;
#[async_trait]
impl Tool for NotesRead {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a note by path (its title, e.g. `Projects/Boat.md`). Returns the full markdown."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    }
    fn parallel(&self) -> bool {
        true
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let p = s(a, "path")?;
        match note_by_path(&c.vault, p).await? {
            Some(n) => Ok(n.content),
            None => Err(format!("no note at `{p}` (use `list` or `search` to find paths)")),
        }
    }
}

struct NotesList;
#[async_trait]
impl Tool for NotesList {
    fn name(&self) -> &str {
        "list"
    }
    fn description(&self) -> &str {
        "List note paths, optionally only those under a folder prefix (e.g. `Jade/`)."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"prefix":{"type":"string"}}})
    }
    fn parallel(&self) -> bool {
        true
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let prefix = a["prefix"].as_str().unwrap_or("").trim_start_matches('/');
        let mut paths: Vec<String> = c.vault.list_items().await.map_err(|e| e.to_string())?.into_iter().map(|n| n.title).filter(|t| t.trim_start_matches('/').starts_with(prefix)).collect();
        paths.sort();
        Ok(if paths.is_empty() { "(no notes)".into() } else { paths.join("\n") })
    }
}

struct NotesWrite;
#[async_trait]
impl Tool for NotesWrite {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Create a note, or REPLACE the whole content of an existing one. Prefer `edit` for small changes."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let p = s(a, "path")?;
        let created = write_note(&c.vault, p, s(a, "content")?).await?;
        Ok(format!("{} `{}`", if created { "created" } else { "replaced" }, p.trim_start_matches('/')))
    }
}

struct NotesEdit;
#[async_trait]
impl Tool for NotesEdit {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Edit a note by exact text replacement: `old` must occur exactly once in the note."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let p = s(a, "path")?;
        let (old, new) = (s(a, "old")?, s(a, "new")?);
        let n = note_by_path(&c.vault, p).await?.ok_or_else(|| format!("no note at `{p}`"))?;
        match n.content.matches(old).count() {
            1 => {
                let updated = n.content.replacen(old, new, 1);
                c.vault.put_item(&n.id, &n.title, &updated, &[], None).await.map_err(|e| e.to_string())?;
                Ok(format!("edited `{}`", n.title))
            }
            0 => Err("`old` text not found in the note; read it first".into()),
            k => Err(format!("`old` text occurs {k} times; include more context so it matches once")),
        }
    }
}

// ── memory (Jade/Memory.md bullets) ─────────────────────────────────────────

struct Memory;

pub fn memory_bullets(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.trim().strip_prefix("- ").map(|b| b.trim().to_string())).filter(|b| !b.is_empty()).collect()
}

#[async_trait]
impl Tool for Memory {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "Long-term memory about the user (durable facts and preferences, kept in Jade/Memory.md). Actions: save (fact), list, search (query), forget (fact text or a unique part of it)."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["save","list","search","forget"]},"fact":{"type":"string"},"query":{"type":"string"}},"required":["action"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let note = note_by_path(&c.vault, MEMORY_PATH).await?;
        let text = note.as_ref().map(|n| n.content.clone()).unwrap_or_else(|| "# Memory\n".to_string());
        let bullets = memory_bullets(&text);
        match s(a, "action")? {
            "save" => {
                let fact = s(a, "fact")?.trim().replace('\n', " ");
                if bullets.iter().any(|b| b.eq_ignore_ascii_case(&fact)) {
                    return Ok("already remembered".into());
                }
                let mut t = text.trim_end().to_string();
                t.push_str(&format!("\n- {fact}\n"));
                write_note(&c.vault, MEMORY_PATH, &t).await?;
                Ok(format!("remembered: {fact}"))
            }
            "list" => Ok(if bullets.is_empty() { "(memory is empty)".into() } else { bullets.iter().map(|b| format!("- {b}")).collect::<Vec<_>>().join("\n") }),
            "search" => {
                let words: Vec<String> = s(a, "query")?.split_whitespace().map(|w| w.to_lowercase()).collect();
                let hits: Vec<&String> = bullets.iter().filter(|b| words.iter().any(|w| b.to_lowercase().contains(w.as_str()))).collect();
                Ok(if hits.is_empty() { "no matching memories".into() } else { hits.iter().map(|b| format!("- {b}")).collect::<Vec<_>>().join("\n") })
            }
            "forget" => {
                let f = s(a, "fact")?.to_lowercase();
                let matching: Vec<&String> = bullets.iter().filter(|b| b.to_lowercase().contains(&f)).collect();
                if matching.len() != 1 {
                    return Err(format!("{} memories match; give text that matches exactly one", matching.len()));
                }
                let target = format!("- {}", matching[0]);
                let kept: Vec<&str> = text.lines().filter(|l| l.trim() != target).collect();
                write_note(&c.vault, MEMORY_PATH, &(kept.join("\n") + "\n")).await?;
                Ok(format!("forgot: {}", matching[0]))
            }
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

// ── web ─────────────────────────────────────────────────────────────────────

struct WebSearch;

/// Where `web_search` looks, tried in order until one answers.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchBackend {
    /// A SearXNG instance (`/search?format=json`).
    Searx(String),
    /// Another NautiK9 server's search (`POST {url}/api/agent/web_search` with its bearer token):
    /// the phone searches through the home server.
    Remote { url: String, token: String },
    /// DuckDuckGo's plain HTML results page, fetched directly (no key, no server).
    DuckDuckGo,
}

impl SearchBackend {
    pub fn label(&self) -> String {
        match self {
            SearchBackend::Searx(u) => format!("searxng {u}"),
            SearchBackend::Remote { url, .. } => format!("remote {url}"),
            SearchBackend::DuckDuckGo => "duckduckgo".into(),
        }
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (title, url, snippet) from DuckDuckGo's HTML results page.
pub fn parse_ddg_html(html: &str, k: usize) -> Vec<(String, String, String)> {
    let text_of = |frag: &str| crate::html2md::html_to_markdown(frag, "").1.replace("**", "");
    let mut out = Vec::new();
    for chunk in html.split("class=\"result__a\"").skip(1) {
        let Some(h) = chunk.find("href=\"") else { continue };
        let href = &chunk[h + 6..];
        let href = &href[..href.find('"').unwrap_or(0)];
        let href = href.replace("&amp;", "&");
        let url = match href.find("uddg=") {
            Some(i) => pct_decode(href[i + 5..].split('&').next().unwrap_or("")),
            None if href.starts_with("http") => href.clone(),
            None => continue,
        };
        let title = chunk.find('>').map(|i| &chunk[i + 1..]).and_then(|t| t.find("</a>").map(|e| text_of(&t[..e]))).unwrap_or_default();
        let snippet = chunk
            .find("class=\"result__snippet\"")
            .and_then(|i| chunk[i..].find('>').map(|j| &chunk[i + j + 1..]))
            .and_then(|t| t.find("</a>").map(|e| text_of(&t[..e])))
            .unwrap_or_default();
        if url.starts_with("http") && !url.contains("duckduckgo.com/y.js") {
            out.push((title.trim().to_string(), url, snippet.trim().to_string()));
        }
        if out.len() >= k {
            break;
        }
    }
    out
}

pub async fn search_once(b: &SearchBackend, http: &reqwest::Client, q: &str, k: usize) -> Result<String, String> {
    let rows = |v: Vec<(String, String, String)>| v.iter().map(|(t, u, s)| format!("- {t}\n  {u}\n  {}", s.chars().take(240).collect::<String>())).collect::<Vec<_>>().join("\n");
    match b {
        SearchBackend::Searx(base) => {
            let url = format!("{}/search", base.trim_end_matches('/'));
            let resp = http.get(&url).query(&[("q", q), ("format", "json")]).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status().as_u16()));
            }
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            let r: Vec<(String, String, String)> = v["results"].as_array().into_iter().flatten().take(k).map(|r| (r["title"].as_str().unwrap_or("").to_string(), r["url"].as_str().unwrap_or("").to_string(), r["content"].as_str().unwrap_or("").to_string())).collect();
            if r.is_empty() {
                let down: Vec<String> = v["unresponsive_engines"].as_array().into_iter().flatten().filter_map(|e| Some(format!("{} ({})", e[0].as_str()?, e[1].as_str().unwrap_or("?")))).collect();
                Err(if down.is_empty() { "no results".into() } else { format!("no results; engines down: {}", down.join(", ")) })
            } else {
                Ok(rows(r))
            }
        }
        SearchBackend::Remote { url, token } => {
            let resp = http
                .post(format!("{}/api/agent/web_search", url.trim_end_matches('/')))
                .bearer_auth(token)
                .json(&json!({"query": q, "k": k}))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("HTTP {} {}", status.as_u16(), v["error"].as_str().unwrap_or("")));
            }
            v["result"].as_str().map(str::to_string).ok_or_else(|| "remote answered without a result".into())
        }
        SearchBackend::DuckDuckGo => {
            // POST first; if DuckDuckGo gives no results (it refuses bursts now and then), wait and
            // try once more with GET. A bot-check page is reported as such.
            let mut last = String::new();
            for attempt in 0..2 {
                if attempt == 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                }
                let rb = if attempt == 0 {
                    http.post("https://html.duckduckgo.com/html/").form(&[("q", q)])
                } else {
                    http.get("https://html.duckduckgo.com/html/").query(&[("q", q)])
                };
                let resp = rb.header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) NautiK9/0.1").send().await.map_err(|e| e.to_string())?;
                let status = resp.status();
                let body = resp.text().await.map_err(|e| e.to_string())?;
                let r = parse_ddg_html(&body, k);
                if status.is_success() && !r.is_empty() {
                    return Ok(rows(r));
                }
                let low = body.to_lowercase();
                last = if low.contains("anomaly") || low.contains("captcha") || low.contains("bots use duckduckgo") {
                    "DuckDuckGo answered with a bot check".into()
                } else if !status.is_success() {
                    format!("HTTP {}", status.as_u16())
                } else {
                    "no results".into()
                };
            }
            Err(last)
        }
    }
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "Search the web. Returns titles, URLs and snippets; use web_fetch to read a page."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string"},"k":{"type":"integer"}},"required":["query"]})
    }
    fn parallel(&self) -> bool {
        true
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let q = s(a, "query")?;
        let k = a["k"].as_u64().unwrap_or(6).clamp(1, 20) as usize;
        if c.search.is_empty() {
            return Err("no web search backend is configured".into());
        }
        let mut tried = Vec::new();
        for b in c.search.iter() {
            match search_once(b, &c.http, q, k).await {
                Ok(text) => return Ok(format!("(results from {})\n{text}", b.label())),
                Err(e) => tried.push(format!("{}: {e}", b.label())),
            }
        }
        Err(format!("web search failed on every backend: {}", tried.join("; ")))
    }
}

struct WebFetch;
#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch a web page and return it as Markdown (title, headings, text, lists, links, code, tables). `max_chars` limits the length (default 12000)."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"url":{"type":"string"},"max_chars":{"type":"integer"}},"required":["url"]})
    }
    fn parallel(&self) -> bool {
        true
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let url = s(a, "url")?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("url must start with http:// or https://".into());
        }
        let max = a["max_chars"].as_u64().unwrap_or(12000).clamp(500, 100_000) as usize;
        let resp = c.http.get(url).header("User-Agent", "NautiK9/0.1 (+web_fetch)").send().await.map_err(|e| format!("fetch failed: {e}"))?;
        let status = resp.status();
        let final_url = resp.url().to_string();
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = resp.text().await.map_err(|e| format!("read failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("HTTP {} from {}", status.as_u16(), final_url));
        }
        let (title, md) = if ctype.contains("html") || body.trim_start().starts_with('<') { html_to_markdown(&body, &final_url) } else { (None, body) };
        let mut out = format!("URL: {final_url}\n");
        if let Some(t) = title {
            out.push_str(&format!("Title: {t}\n"));
        }
        out.push('\n');
        let truncated = md.chars().count() > max;
        out.extend(md.chars().take(max));
        if truncated {
            out.push_str(&format!("\n\n[truncated at {max} characters]"));
        }
        Ok(out)
    }
}

// ── http_request with {{NAME}} secrets ──────────────────────────────────────

struct HttpRequest;

/// Replace `{{NAME}}` with secrets; an unknown name is an error (never sent as-is).
pub fn fill_secrets(text: &str, secrets: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        let Some(j) = rest[i + 2..].find("}}") else { break };
        let name = rest[i + 2..i + 2 + j].trim();
        let v = secrets.get(name).ok_or_else(|| format!("unknown secret `{{{{{name}}}}}` (add it to secrets.json)"))?;
        out.push_str(&rest[..i]);
        out.push_str(v);
        rest = &rest[i + 2 + j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Mask every secret value that appears in `text`.
pub fn redact(text: &str, secrets: &HashMap<String, String>) -> String {
    let mut t = text.to_string();
    for (k, v) in secrets {
        if v.len() >= 4 {
            t = t.replace(v.as_str(), &format!("{{{{{k}}}}}"));
        }
    }
    t
}

#[async_trait]
impl Tool for HttpRequest {
    fn name(&self) -> &str {
        "http_request"
    }
    fn description(&self) -> &str {
        "Call any HTTP API. Put secrets as {{NAME}} in url, headers or body (values come from the owner's secrets.json and are never shown). Returns status and body."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"method":{"type":"string","enum":["GET","POST","PUT","PATCH","DELETE"]},"url":{"type":"string"},"headers":{"type":"object","additionalProperties":{"type":"string"}},"body":{"type":"string"}},"required":["url"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let method = a["method"].as_str().unwrap_or("GET").to_uppercase();
        let url = fill_secrets(s(a, "url")?, &c.secrets)?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("url must start with http:// or https://".into());
        }
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| format!("bad method `{method}`"))?;
        let mut rb = c.http.request(m, &url);
        if let Some(h) = a["headers"].as_object() {
            for (k, v) in h {
                rb = rb.header(k.as_str(), fill_secrets(v.as_str().unwrap_or(""), &c.secrets)?);
            }
        }
        if let Some(b) = a["body"].as_str() {
            rb = rb.body(fill_secrets(b, &c.secrets)?);
        }
        let resp = rb.send().await.map_err(|e| redact(&format!("request failed: {e}"), &c.secrets))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| format!("read failed: {e}"))?;
        let body: String = text.chars().take(20_000).collect();
        Ok(redact(&format!("HTTP {status}\n\n{body}"), &c.secrets))
    }
}

// ── chat_history ────────────────────────────────────────────────────────────

struct ChatHistory;
#[async_trait]
impl Tool for ChatHistory {
    fn name(&self) -> &str {
        "chat_history"
    }
    fn description(&self) -> &str {
        "The raw record of past chats. Actions: list; read (session id, optional from/limit); search (query: all words must appear)."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["list","read","search"]},"session":{"type":"string"},"from":{"type":"integer"},"limit":{"type":"integer"},"query":{"type":"string"}},"required":["action"]})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        match s(a, "action")? {
            "list" => {
                let l = c.store.list_sessions().await?;
                Ok(l.iter().take(50).map(|x| format!("- {} | {} | {} messages | {}", x.id, x.title, x.n_messages, x.updated_at)).collect::<Vec<_>>().join("\n"))
            }
            "read" => {
                let id = s(a, "session")?;
                let from = a["from"].as_u64().unwrap_or(0) as usize;
                let limit = a["limit"].as_u64().unwrap_or(40).min(200) as usize;
                let m = c.store.messages(id).await?;
                if m.is_empty() {
                    return Err(format!("no messages in session `{id}`"));
                }
                let rows: Vec<Value> = m.iter().skip(from).take(limit).map(|x| json!({"index": x.idx, "timestamp": x.created_at, "role": x.message["role"], "text": x.message["content"], "tool_calls": x.message["tool_calls"]})).collect();
                Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            "search" => Ok(serde_json::to_string_pretty(&c.store.search(s(a, "query")?, 20).await?).unwrap_or_default()),
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

// ── skills (Jade/skills/<name>/SKILL.md) ────────────────────────────────────

struct SkillTool;

/// (name, description) from SKILL.md front matter.
pub fn skill_header(text: &str) -> Option<(String, String)> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let (mut name, mut desc) = (None, None);
    for l in body[..end].lines() {
        if let Some(v) = l.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        }
        if let Some(v) = l.strip_prefix("description:") {
            desc = Some(v.trim().to_string());
        }
    }
    Some((name?, desc.unwrap_or_default()))
}

pub async fn list_skills(vault: &WayfinderVault) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    for n in vault.list_items().await.map_err(|e| e.to_string())? {
        let t = n.title.trim_start_matches('/');
        if t.starts_with("Jade/skills/") && t.ends_with("/SKILL.md") {
            if let Some((name, desc)) = skill_header(&n.content) {
                out.push((name, desc, t.to_string()));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }
    fn description(&self) -> &str {
        "Load an instruction pack by name (listed in your instructions) and follow it. Actions: list, load (name)."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["list","load"]},"name":{"type":"string"}},"required":["action"]})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let skills = list_skills(&c.vault).await?;
        match s(a, "action")? {
            "list" => Ok(if skills.is_empty() { "(no skills; create Jade/skills/<name>/SKILL.md)".into() } else { skills.iter().map(|(n, d, _)| format!("- {n}: {d}")).collect::<Vec<_>>().join("\n") }),
            "load" => {
                let name = s(a, "name")?;
                let (_, _, path) = skills.iter().find(|(n, _, _)| n == name).ok_or_else(|| format!("no skill `{name}`"))?;
                Ok(note_by_path(&c.vault, path).await?.map(|n| n.content).unwrap_or_default())
            }
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

// ── MCP servers (Jade's mcp.json intent; the CESARops fleet MCP is POST /mcp JSON) ─────────

/// One MCP server: `POST {url}` with `{"method":"tools/list"}` / `{"method":"tools/call",
/// "params":{"name","arguments"}}`, optional bearer token (a NautiK9 home server's
/// /api/agent/mcp proxy needs its token; the fleet MCP on loopback needs none).
#[derive(Clone, Debug)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub token: Option<String>,
}

/// Fleet MCP tools that only read (offered to `delegate` helpers and run in parallel).
const MCP_READ_ONLY: &[&str] = &[
    "file_read", "list_dir", "codebase_grep", "git_status", "git_diff", "qdrant_search", "qdrant_status", "gpu_status", "service_status",
    "service_logs", "fleet_nodes", "jobs_board", "buzz_tasks", "paddler_status", "warp_routing", "fabric_inventory", "fabric_find", "memory_get",
];

pub struct McpTool {
    server: McpServer,
    name: String,
    description: String,
    schema: Value,
}

impl McpServer {
    async fn post(&self, http: &reqwest::Client, body: &Value) -> Result<Value, String> {
        let mut rb = http.post(&self.url).json(body).timeout(std::time::Duration::from_secs(120));
        if let Some(t) = &self.token {
            rb = rb.bearer_auth(t);
        }
        let resp = rb.send().await.map_err(|e| format!("mcp {}: {e}", self.name))?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| format!("mcp {}: bad JSON: {e}", self.name))?;
        if !status.is_success() {
            return Err(format!("mcp {}: HTTP {} {}", self.name, status.as_u16(), v["error"].as_str().unwrap_or("")));
        }
        Ok(v)
    }

    /// The server's tools as agent tools.
    pub async fn load(&self, http: &reqwest::Client) -> Result<Vec<Arc<dyn Tool>>, String> {
        let v = self.post(http, &json!({"method": "tools/list"})).await?;
        let list = v["tools"].as_array().ok_or_else(|| format!("mcp {}: tools/list has no `tools` array", self.name))?;
        Ok(list
            .iter()
            .filter_map(|t| {
                let name = t["name"].as_str()?.to_string();
                let schema = t.get("inputSchema").or(t.get("input_schema")).cloned().unwrap_or(json!({"type":"object","properties":{}}));
                Some(Arc::new(McpTool { server: self.clone(), description: format!("[{} MCP] {}", self.name, t["description"].as_str().unwrap_or("")), name, schema }) as Arc<dyn Tool>)
            })
            .collect())
    }

    /// Raw call (also used by the home server's proxy).
    pub async fn call_raw(&self, http: &reqwest::Client, body: &Value) -> Result<Value, String> {
        self.post(http, body).await
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn parallel(&self) -> bool {
        MCP_READ_ONLY.contains(&self.name.as_str())
    }
    fn read_only(&self) -> bool {
        MCP_READ_ONLY.contains(&self.name.as_str())
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let v = self.server.post(&c.http, &json!({"method": "tools/call", "params": {"name": self.name, "arguments": a}})).await?;
        let text: Vec<String> = v["content"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str().map(str::to_string)).collect();
        let text = if text.is_empty() { v["content"].to_string() } else { text.join("\n") };
        if v["isError"].as_bool().unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(vault: WayfinderVault, secrets: &[(&str, &str)]) -> ToolCtx {
        ToolCtx {
            vault: Arc::new(vault),
            store: Store::in_memory().unwrap(),
            http: reqwest::Client::new(),
            secrets: Arc::new(secrets.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
            search: Arc::new(vec![]),
            owner: true,
            mcp_local: Arc::new(HashMap::new()),
            memory: Arc::new(crate::memory::MemoryIndex::in_memory(None).unwrap()),
        }
    }

    #[tokio::test]
    async fn notes_write_read_edit_list() {
        let c = ctx(WayfinderVault::in_memory().unwrap(), &[]);
        let r = Registry::standard();
        let call = |n: &str, a: Value| {
            let t = r.get(n).unwrap();
            let c = c.clone();
            async move { t.call(&a, &c).await }
        };
        assert_eq!(call("write", json!({"path":"/Projects/Boat.md","content":"Hull: oak\nHull paint: red"})).await.unwrap(), "created `Projects/Boat.md`");
        assert_eq!(call("read", json!({"path":"Projects/Boat.md"})).await.unwrap(), "Hull: oak\nHull paint: red");
        assert!(call("edit", json!({"path":"Projects/Boat.md","old":"Hull","new":"Keel"})).await.unwrap_err().contains("2 times"));
        assert!(call("edit", json!({"path":"Projects/Boat.md","old":"paint: red","new":"paint: blue"})).await.is_ok());
        assert!(call("read", json!({"path":"Projects/Boat.md"})).await.unwrap().ends_with("blue"));
        assert_eq!(call("write", json!({"path":"Projects/Boat.md","content":"x"})).await.unwrap(), "replaced `Projects/Boat.md`");
        assert_eq!(call("list", json!({"prefix":"Projects/"})).await.unwrap(), "Projects/Boat.md");
        assert!(call("read", json!({"path":"nope"})).await.is_err());
    }

    #[tokio::test]
    async fn memory_save_search_forget() {
        let c = ctx(WayfinderVault::in_memory().unwrap(), &[]);
        let m = Registry::standard().get("memory").unwrap();
        m.call(&json!({"action":"save","fact":"Prefers short weekly reviews"}), &c).await.unwrap();
        m.call(&json!({"action":"save","fact":"Boat is called Mack"}), &c).await.unwrap();
        assert_eq!(m.call(&json!({"action":"save","fact":"boat is called mack"}), &c).await.unwrap(), "already remembered");
        assert!(m.call(&json!({"action":"search","query":"boat"}), &c).await.unwrap().contains("Mack"));
        m.call(&json!({"action":"forget","fact":"weekly"}), &c).await.unwrap();
        assert_eq!(m.call(&json!({"action":"list"}), &c).await.unwrap(), "- Boat is called Mack");
    }

    #[test]
    fn secrets_fill_and_redact() {
        let s: HashMap<String, String> = [("TOKEN".to_string(), "abc123secret".to_string())].into();
        assert_eq!(fill_secrets("Bearer {{TOKEN}}", &s).unwrap(), "Bearer abc123secret");
        assert!(fill_secrets("{{NOPE}}", &s).unwrap_err().contains("NOPE"));
        assert_eq!(redact("echo abc123secret back", &s), "echo {{TOKEN}} back");
    }

    #[test]
    fn ddg_results_parse() {
        let html = r#"<div><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FSS_Cedarville&amp;rut=0f">SS Cedarville - Wikipedia</a>
<a class="result__snippet" href="x">The SS <b>Cedarville</b> left Port Calcite at 5:01 a.m.</a></div>
<div><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.straitspreserve.com%2Fshipwrecks%2Fcedarville%2F&amp;rut=8">Cedarville | Straits Preserve</a></div>"#;
        let r = parse_ddg_html(html, 5);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], ("SS Cedarville - Wikipedia".to_string(), "https://en.wikipedia.org/wiki/SS_Cedarville".to_string(), "The SS Cedarville left Port Calcite at 5:01 a.m.".to_string()));
        assert_eq!(r[1].1, "https://www.straitspreserve.com/shipwrecks/cedarville/");
    }

    #[tokio::test]
    async fn search_falls_through_backends_and_says_which_answered() {
        let mut c = ctx(WayfinderVault::in_memory().unwrap(), &[]);
        c.search = Arc::new(vec![SearchBackend::Searx("http://127.0.0.1:9".into())]);
        let t = Registry::standard().get("web_search").unwrap();
        let e = t.call(&json!({"query":"x"}), &c).await.unwrap_err();
        assert!(e.starts_with("web search failed on every backend: searxng http://127.0.0.1:9:"), "{e}");
        c.search = Arc::new(vec![]);
        assert!(t.call(&json!({"query":"x"}), &c).await.unwrap_err().contains("no web search backend"));
    }

    #[test]
    fn skill_front_matter() {
        assert_eq!(skill_header("---\nname: weekly-review\ndescription: Review projects.\n---\n# x"), Some(("weekly-review".into(), "Review projects.".into())));
        assert_eq!(skill_header("# no front matter"), None);
    }

    #[test]
    fn non_owner_gets_no_owner_tools() {
        let r = Registry::standard();
        assert_eq!(r.schema(false, false, &[]).as_array().unwrap().len(), 0);
        let ro: Vec<String> = r.schema(true, true, &[]).as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
        assert!(ro.contains(&"read".to_string()) && !ro.contains(&"write".to_string()) && !ro.contains(&"http_request".to_string()));
    }
}
