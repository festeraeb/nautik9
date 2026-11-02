//! Vault extras: ranked note search, the link/tag graph, and attachments.
//!
//!   GET  /api/notes?q=&tag=&k=&mode=   list, or search (`mode`: hybrid (default) | keyword)
//!   GET  /api/notes/graph              {nodes, edges}: note, tag and missing-note nodes
//!   POST /api/attachments?name=x.png   raw body (max 25 MB) -> {name, embed}
//!   GET  /api/attachments/{name}       the stored file
//!
//! Search is keyword ranking over title/content/tags. When the agent has an embedding endpoint
//! (`agent.json` `embeddings`) the notes are also ranked by meaning through the same
//! `MemoryIndex` the `embeddings` tool uses, and the two lists are fused with its reciprocal
//! rank fusion. The reply always says whether the meaning part ran (`semantic`).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use nautik9_agent::memory::{rrf, MemoryIndex};
use nautik9_wayfinder::VaultItem;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

const RRF_K: f64 = 60.0;
const MAX_ATTACHMENT: usize = 25 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/notes", get(get_notes))
        .route("/api/notes/graph", get(graph))
        .route("/api/attachments", axum::routing::post(upload).layer(DefaultBodyLimit::max(MAX_ATTACHMENT + 1024)))
        .route("/api/attachments/{name}", get(download))
}

// ── search ──────────────────────────────────────────────────────────────────

fn query_words(q: &str) -> Vec<String> {
    let mut w: Vec<String> = q.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).map(str::to_string).collect();
    w.dedup();
    w
}

/// Keyword ranking: title hits weigh 3, tag hits 2, each content occurrence 1 (up to 5 per
/// word), plus a bonus when the whole query appears in the title. Best first; no hit = absent.
pub fn keyword_ranking(items: &[VaultItem], q: &str) -> Vec<(usize, f64)> {
    let words = query_words(q);
    if words.is_empty() {
        return vec![];
    }
    let phrase = q.trim().to_lowercase();
    let mut out: Vec<(usize, f64)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let (title, content) = (n.title.to_lowercase(), n.content.to_lowercase());
            let mut s = 0.0;
            for w in &words {
                if title.contains(w.as_str()) {
                    s += 3.0;
                }
                if n.tags.iter().any(|t| t.to_lowercase().contains(w.as_str())) {
                    s += 2.0;
                }
                s += content.matches(w.as_str()).count().min(5) as f64;
            }
            if words.len() > 1 && title.contains(&phrase) {
                s += 3.0;
            }
            (s > 0.0).then_some((i, s))
        })
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    out
}

#[derive(Debug, Default)]
pub struct Ranked {
    /// (index into the items, fused score, keyword rank, semantic rank), best first.
    pub order: Vec<(usize, f64, Option<usize>, Option<usize>)>,
    /// True only when the meaning-based ranking actually ran for this query.
    pub semantic: bool,
    pub note: Option<String>,
}

/// Hybrid when `memory` has an embedding endpoint and it answers; otherwise keyword only.
pub async fn rank(items: &[VaultItem], q: &str, memory: Option<&MemoryIndex>, k: usize) -> Ranked {
    let kw = keyword_ranking(items, q);
    let mut out = Ranked::default();
    let mut sem_idx: Vec<usize> = vec![];
    if let Some(m) = memory.filter(|m| m.semantic_on()) {
        // Keep the `notes` collection equal to the vault (cheap when nothing changed).
        let sync = m.index_vault_notes(items, "").await;
        let res = match &sync {
            Ok(r) if r.embed_error.is_some() => Err(format!("embedding endpoint failed: {}", r.embed_error.clone().unwrap_or_default())),
            Ok(_) => m.search(q, "semantic", &["notes".to_string()], 30).await.map(|o| o.hits),
            Err(e) => Err(e.clone()),
        };
        match res {
            Ok(hits) => {
                out.semantic = true;
                let by_title: HashMap<&str, usize> = items.iter().enumerate().rev().map(|(i, n)| (n.title.as_str(), i)).collect();
                for h in hits {
                    if let Some(&i) = by_title.get(h.source.as_str()) {
                        if !sem_idx.contains(&i) {
                            sem_idx.push(i);
                        }
                    }
                }
            }
            Err(e) => out.note = Some(format!("keyword only: {e}")),
        }
    }
    let kw_ids: Vec<i64> = kw.iter().map(|x| x.0 as i64).collect();
    let sem_ids: Vec<i64> = sem_idx.iter().map(|x| *x as i64).collect();
    let lists = if out.semantic { vec![kw_ids.clone(), sem_ids.clone()] } else { vec![kw_ids.clone()] };
    for (id, score) in rrf(&lists, RRF_K).into_iter().take(k) {
        out.order.push((id as usize, score, kw_ids.iter().position(|x| *x == id).map(|p| p + 1), sem_ids.iter().position(|x| *x == id).map(|p| p + 1)));
    }
    out
}

#[derive(Deserialize, Default)]
pub struct NotesQuery {
    tag: Option<String>,
    q: Option<String>,
    k: Option<usize>,
    mode: Option<String>,
}

pub async fn get_notes(State(st): State<AppState>, Query(q): Query<NotesQuery>) -> Response {
    let vault = crate::open_vault(&st);
    let items = match q.tag.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => vault.notes_with_tag(t).await.unwrap_or_default(),
        None => vault.list_items().await.unwrap_or_default(),
    };
    let Some(query) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Json(json!({"notes": items, "count": items.len()})).into_response();
    };
    let mode = q.mode.as_deref().unwrap_or("hybrid");
    if mode != "hybrid" && mode != "keyword" {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "mode must be hybrid or keyword"}))).into_response();
    }
    let memory = if mode == "keyword" { None } else { st.agent.as_ref().ok().map(|a| a.ctx.memory.clone()) };
    let r = rank(&items, query, memory.as_deref(), q.k.unwrap_or(20).clamp(1, 50)).await;
    let notes: Vec<Value> = r
        .order
        .iter()
        .map(|(i, score, kr, sr)| {
            let mut v = serde_json::to_value(&items[*i]).unwrap_or(Value::Null);
            v["score"] = json!(score);
            v["keyword_rank"] = json!(kr);
            v["semantic_rank"] = json!(sr);
            v
        })
        .collect();
    let used = if r.semantic { "hybrid" } else { "keyword" };
    Json(json!({"query": query, "semantic": r.semantic, "mode": used, "note": r.note, "notes": notes, "count": notes.len()})).into_response()
}

// ── graph ───────────────────────────────────────────────────────────────────

/// Nodes: every note (`kind: note`), every tag in use (`tag`, id `tag:<name>`), and every
/// `[[link]]` that matches no note (`missing`, id `missing:<name>`). Edges: `link` (note to note
/// or to a missing note, matched by id or case-insensitive title) and `tag` (note to tag).
pub fn build_graph(items: &[VaultItem]) -> Value {
    let find = |name: &str| items.iter().find(|i| i.id == name).or_else(|| items.iter().find(|i| i.title.eq_ignore_ascii_case(name)));
    let mut degree: HashMap<String, usize> = HashMap::new();
    let mut edges: Vec<Value> = vec![];
    let mut seen: std::collections::HashSet<(String, String, &str)> = Default::default();
    let mut tags: BTreeMap<String, String> = BTreeMap::new();
    let mut missing: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |a: &str, b: &str, kind: &'static str, edges: &mut Vec<Value>| {
        if a != b && seen.insert((a.to_string(), b.to_string(), kind)) {
            *degree.entry(a.to_string()).or_default() += 1;
            *degree.entry(b.to_string()).or_default() += 1;
            edges.push(json!({"source": a, "target": b, "kind": kind}));
        }
    };
    for n in items {
        for l in &n.links {
            match find(l) {
                Some(t) => add(&n.id, &t.id, "link", &mut edges),
                None => {
                    let id = format!("missing:{}", l.to_lowercase());
                    missing.entry(id.clone()).or_insert_with(|| l.clone());
                    add(&n.id, &id, "link", &mut edges);
                }
            }
        }
        for t in &n.tags {
            let id = format!("tag:{t}");
            tags.entry(id.clone()).or_insert_with(|| t.clone());
            add(&n.id, &id, "tag", &mut edges);
        }
    }
    let mut nodes: Vec<Value> = items.iter().map(|n| json!({"id": n.id, "title": n.title, "kind": "note", "tags": n.tags, "degree": degree.get(&n.id).copied().unwrap_or(0)})).collect();
    nodes.extend(tags.iter().map(|(id, t)| json!({"id": id, "title": format!("#{t}"), "kind": "tag", "degree": degree.get(id).copied().unwrap_or(0)})));
    nodes.extend(missing.iter().map(|(id, t)| json!({"id": id, "title": t, "kind": "missing", "degree": degree.get(id).copied().unwrap_or(0)})));
    json!({"nodes": nodes, "edges": edges, "notes": items.len(), "tags": tags.len(), "missing": missing.len()})
}

pub async fn graph(State(st): State<AppState>) -> Json<Value> {
    let items = crate::open_vault(&st).list_items().await.unwrap_or_default();
    Json(build_graph(&items))
}

// ── attachments ─────────────────────────────────────────────────────────────

/// (extension, content type, safe to show inline). No SVG: it can carry script.
const KINDS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("avif", "image/avif"),
    ("pdf", "application/pdf"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("txt", "text/plain; charset=utf-8"),
];

fn content_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    KINDS.iter().find(|k| k.0 == ext).map(|k| k.1)
}

/// One plain file name: letters, digits, `. _ - space ( )`, no leading dot, no `..`, a known
/// extension. Anything else (separators, NUL, control chars, odd Unicode) is refused.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || " ._-()".contains(c))
        && content_type(name).is_some()
}

fn attach_dir(st: &AppState) -> PathBuf {
    st.vault_path.parent().map(|p| p.join("attachments")).unwrap_or_else(|| PathBuf::from("attachments"))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"ok": false, "error": msg}))).into_response()
}

#[derive(Deserialize)]
pub struct UploadQuery {
    name: Option<String>,
}

pub async fn upload(State(st): State<AppState>, Query(q): Query<UploadQuery>, body: Bytes) -> Response {
    let Some(name) = q.name.as_deref() else { return err(StatusCode::BAD_REQUEST, "missing ?name=") };
    if !valid_name(name) {
        return err(StatusCode::BAD_REQUEST, "bad file name: use letters, digits, space . _ - ( ) and one of png jpg jpeg gif webp bmp avif pdf mp3 wav ogg m4a mp4 webm txt (svg is refused)");
    }
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, "empty file");
    }
    if body.len() > MAX_ATTACHMENT {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "file is over 25 MB");
    }
    let dir = attach_dir(&st);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot create the attachments folder: {e}"));
    }
    // Never overwrite: "a.png" taken -> "a-2.png". Same bytes again is reused as is.
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let mut final_name = name.to_string();
    for n in 2..1000 {
        match tokio::fs::read(dir.join(&final_name)).await {
            Ok(old) if old == body.as_ref() => break,
            Ok(_) => final_name = format!("{stem}-{n}.{ext}"),
            Err(_) => break,
        }
    }
    if let Err(e) = tokio::fs::write(dir.join(&final_name), &body).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot write the file: {e}"));
    }
    Json(json!({"ok": true, "name": final_name, "bytes": body.len(), "embed": format!("![[{final_name}]]")})).into_response()
}

pub async fn download(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    if !valid_name(&name) {
        return err(StatusCode::BAD_REQUEST, "bad file name");
    }
    let Some(ct) = content_type(&name) else { return err(StatusCode::BAD_REQUEST, "bad file name") };
    let path = attach_dir(&st).join(&name);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(ct)),
                (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
                (header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox")),
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600")),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "no such attachment"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

    fn item(id: &str, title: &str, content: &str) -> VaultItem {
        VaultItem {
            id: id.into(),
            title: title.into(),
            content: content.into(),
            tags: nautik9_wayfinder::parse_tags(content),
            created_at: String::new(),
            links: nautik9_wayfinder::parse_links(content),
        }
    }

    /// A tiny embeddings endpoint: texts about water map to one axis, about food to another.
    async fn mock_embeddings() -> String {
        async fn emb(Json(p): Json<Value>) -> Json<Value> {
            let data: Vec<Value> = p["input"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let t = t.as_str().unwrap().to_lowercase();
                    let water = ["ocean", "sea", "lake", "boat", "water"].iter().any(|w| t.contains(w));
                    let food = ["bread", "milk", "recipe", "kitchen"].iter().any(|w| t.contains(w));
                    let v = if water { [1.0, 0.0, 0.1] } else if food { [0.0, 1.0, 0.1] } else { [0.1, 0.1, 1.0] };
                    json!({"index": i, "embedding": v})
                })
                .collect();
            Json(json!({"data": data}))
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, Router::new().route("/v1/embeddings", axum::routing::post(emb))).await.unwrap() });
        url
    }

    fn state(dir: &std::path::Path, embeddings_url: Option<&str>) -> AppState {
        if let Some(u) = embeddings_url {
            std::fs::write(dir.join("agent.json"), json!({"embeddings": {"url": u, "model": "mock"}}).to_string()).unwrap();
        }
        let hw = nautik9_core::hardware::HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let lemonade = std::sync::Arc::new(nautik9_lemonade::LemonadeModule::new(nautik9_lemonade::LemonadeConfig::default()));
        AppState {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(nautik9_core::module::ModuleRegistry::new(hw.clone()))),
            hardware: std::sync::Arc::new(hw),
            vault_path: dir.join("vault.db"),
            sms_messages: Default::default(),
            work_config: Default::default(),
            lemonade: lemonade.clone(),
            fleet_url: "http://127.0.0.1:1".into(),
            sms_bridge: crate::SmsBridge::Adb,
            agent: crate::agent_api::build_agent(dir, &dir.join("vault.db"), lemonade, Default::default()),
            scheduler: None,
        }
    }

    async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>, body: Option<Vec<u8>>) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let resp = app.clone().oneshot(b.body(Body::from(body.unwrap_or_default())).unwrap()).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec(), headers)
    }

    async fn call_json(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {TOKEN}"));
        let req = match body {
            Some(j) => {
                b = b.header("content-type", "application/json");
                b.body(Body::from(j.to_string())).unwrap()
            }
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[test]
    fn keyword_ranking_weights_title_tags_and_content() {
        let items = vec![item("a", "Groceries", "milk and bread"), item("b", "Sonar survey", "scan the wreck #sonar"), item("c", "Misc", "sonar sonar sonar")];
        let r = keyword_ranking(&items, "sonar");
        assert_eq!(r[0].0, 1, "title + tag + content beats content only: {r:?}");
        assert_eq!(r[1].0, 2);
        assert_eq!(r.len(), 2, "the grocery note has no hit");
        assert!(keyword_ranking(&items, "  ").is_empty());
    }

    #[tokio::test]
    async fn without_an_embedding_endpoint_search_is_keyword_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::build_router(state(d.path(), None), TOKEN.into());
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n1", "title": "Lake survey", "content": "boat on the lake"}))).await;
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n2", "title": "Kitchen", "content": "bread and milk"}))).await;
        let (s, v) = call_json(&app, "GET", "/api/notes?q=lake", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["semantic"], json!(false), "{v}");
        assert_eq!(v["mode"], "keyword");
        assert_eq!(v["notes"][0]["id"], "n1");
        // Meaning-only match finds nothing without embeddings, and nothing pretends otherwise.
        let (_, v) = call_json(&app, "GET", "/api/notes?q=ocean", None).await;
        assert_eq!(v["count"], 0);
        assert_eq!(v["semantic"], json!(false));
        // No q: the plain list, unchanged.
        let (_, v) = call_json(&app, "GET", "/api/notes", None).await;
        assert_eq!(v["count"], 2);
        assert!(v.get("semantic").is_none());
        let (s, _) = call_json(&app, "GET", "/api/notes?q=x&mode=magic", None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn with_an_embedding_endpoint_search_is_hybrid() {
        let d = tempfile::tempdir().unwrap();
        let url = mock_embeddings().await;
        let app = crate::build_router(state(d.path(), Some(&url)), TOKEN.into());
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n1", "title": "Lake survey", "content": "boat on the lake"}))).await;
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n2", "title": "Kitchen", "content": "bread and milk"}))).await;
        // "ocean" appears in no note: only meaning can find the lake note.
        let (_, v) = call_json(&app, "GET", "/api/notes?q=ocean", None).await;
        assert_eq!(v["semantic"], json!(true), "{v}");
        assert_eq!(v["mode"], "hybrid");
        assert_eq!(v["notes"][0]["id"], "n1", "{v}");
        assert!(v["notes"][0]["semantic_rank"].is_number());
        assert!(v["notes"][0]["keyword_rank"].is_null());
        // mode=keyword turns the meaning part off for this request.
        let (_, v) = call_json(&app, "GET", "/api/notes?q=ocean&mode=keyword", None).await;
        assert_eq!(v["semantic"], json!(false));
        assert_eq!(v["count"], 0);
        // A deleted note leaves the index on the next search.
        call_json(&app, "DELETE", "/api/notes/n1", None).await;
        let (_, v) = call_json(&app, "GET", "/api/notes?q=ocean", None).await;
        assert!(v["notes"].as_array().unwrap().iter().all(|n| n["id"] != "n1"), "{v}");
    }

    #[tokio::test]
    async fn a_dead_embedding_endpoint_falls_back_to_keyword_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::build_router(state(d.path(), Some("http://127.0.0.1:1/v1")), TOKEN.into());
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n1", "title": "Lake survey", "content": "boat on the lake"}))).await;
        let (_, v) = call_json(&app, "GET", "/api/notes?q=lake", None).await;
        assert_eq!(v["semantic"], json!(false), "{v}");
        assert!(v["note"].as_str().unwrap().contains("keyword only"), "{v}");
        assert_eq!(v["notes"][0]["id"], "n1");
    }

    #[test]
    fn graph_has_note_tag_and_missing_nodes_with_link_and_tag_edges() {
        let items = vec![
            item("a", "Alpha", "to [[Beta]] and [[Nowhere]] #sonar"),
            item("b", "Beta", "back to [[a]] #sonar #wreck"),
            item("c", "Gamma", "self [[Gamma]] and image ![[p.png]]"),
        ];
        let g = build_graph(&items);
        let nodes = g["nodes"].as_array().unwrap();
        let kind = |k: &str| nodes.iter().filter(|n| n["kind"] == k).count();
        assert_eq!((kind("note"), kind("tag"), kind("missing")), (3, 2, 1), "{g}");
        let edges = g["edges"].as_array().unwrap();
        let has = |s: &str, t: &str, k: &str| edges.iter().any(|e| e["source"] == s && e["target"] == t && e["kind"] == k);
        assert!(has("a", "b", "link") && has("b", "a", "link"));
        assert!(has("a", "missing:nowhere", "link"));
        assert!(has("a", "tag:sonar", "tag") && has("b", "tag:sonar", "tag") && has("b", "tag:wreck", "tag"));
        assert!(!edges.iter().any(|e| e["source"] == "c"), "self link and image embed make no edge: {g}");
        let deg = |id: &str| nodes.iter().find(|n| n["id"] == id).unwrap()["degree"].as_u64().unwrap();
        assert_eq!(deg("tag:sonar"), 2);
        assert_eq!(deg("c"), 0);
    }

    #[tokio::test]
    async fn graph_endpoint_needs_the_token_and_returns_the_vault() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::build_router(state(d.path(), None), TOKEN.into());
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n1", "title": "One", "content": "see [[Two]] #x"}))).await;
        call_json(&app, "POST", "/api/notes", Some(json!({"id": "n2", "title": "Two", "content": "hi"}))).await;
        let (s, _, _) = call(&app, "GET", "/api/notes/graph", None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, g) = call_json(&app, "GET", "/api/notes/graph", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(g["notes"], 2);
        assert!(g["edges"].as_array().unwrap().iter().any(|e| e["source"] == "n1" && e["target"] == "n2"));
        // The note-delete route still works next to the static "graph" route.
        let (_, d) = call_json(&app, "DELETE", "/api/notes/n2", None).await;
        assert_eq!(d["success"], true);
    }

    #[test]
    fn name_rules() {
        for ok in ["a.png", "My photo (1).JPG", "x-y_z.pdf"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", ".hidden.png", "../a.png", "a/../b.png", "a/b.png", "a\\b.png", "..", "a..png", "a.svg", "a.html", "noext", "a\0.png", "é.png", "a.png/", &"x".repeat(130)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn attachments_round_trip_with_token_and_never_escape_the_folder() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::build_router(state(d.path(), None), TOKEN.into());
        // Something outside the folder that must stay unreachable.
        std::fs::write(d.path().join("secret.txt"), b"top secret").unwrap();
        std::fs::write(d.path().join("vault.png"), b"not an attachment").unwrap();
        let png = b"\x89PNG\r\n\x1a\nfake".to_vec();

        let (s, _, _) = call(&app, "POST", "/api/attachments?name=a.png", None, Some(png.clone())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, b, _) = call(&app, "POST", "/api/attachments?name=a.png", Some(TOKEN), Some(png.clone())).await;
        assert_eq!(s, StatusCode::OK);
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!((v["name"].as_str(), v["embed"].as_str()), (Some("a.png"), Some("![[a.png]]")));
        // Same bytes: reused. Different bytes: a new name, the old file untouched.
        let (_, b, _) = call(&app, "POST", "/api/attachments?name=a.png", Some(TOKEN), Some(png.clone())).await;
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["name"], "a.png");
        let (_, b, _) = call(&app, "POST", "/api/attachments?name=a.png", Some(TOKEN), Some(b"other".to_vec())).await;
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["name"], "a-2.png");

        let (s, body, h) = call(&app, "GET", "/api/attachments/a.png", None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{body:?}");
        let (s, body, h2) = call(&app, "GET", "/api/attachments/a.png", Some(TOKEN), None).await;
        assert_eq!((s, body), (StatusCode::OK, png.clone()));
        assert_eq!(h2[header::CONTENT_TYPE], "image/png");
        assert_eq!(h2[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        drop(h);
        // The ?token= form works for <img>/<a> too.
        let (s, _, _) = call(&app, "GET", &format!("/api/attachments/a.png?token={TOKEN}"), None, None).await;
        assert_eq!(s, StatusCode::OK);

        // Traversal and bad names, every spelling a client can send.
        for uri in [
            "/api/attachments/..%2Fsecret.txt",
            "/api/attachments/%2e%2e%2fsecret.txt",
            "/api/attachments/..%5Csecret.txt",
            "/api/attachments/%2e%2e",
            "/api/attachments/..",
            "/api/attachments/../secret.txt",
            "/api/attachments/%2Fetc%2Fpasswd",
            "/api/attachments/a.png%00.txt",
            "/api/attachments/secret.txt",
            "/api/attachments/vault.png",
            "/api/attachments/.hidden.png",
        ] {
            let (s, body, _) = call(&app, "GET", uri, Some(TOKEN), None).await;
            assert_ne!(s, StatusCode::OK, "{uri}");
            assert!(!String::from_utf8_lossy(&body).contains("top secret") && !String::from_utf8_lossy(&body).contains("not an attachment"), "{uri} leaked");
        }
        for name in ["..%2Fx.png", "a%2Fb.png", "..", "x.svg", "x.html", "%2e%2e%2f%2e%2e%2fetc%2fcron.png", ""] {
            let (s, _, _) = call(&app, "POST", &format!("/api/attachments?name={name}"), Some(TOKEN), Some(png.clone())).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{name}");
        }
        assert!(!d.path().join("x.png").exists());
        let (s, _, _) = call(&app, "POST", "/api/attachments?name=e.png", Some(TOKEN), Some(vec![])).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        // Only the intended files exist in the folder.
        let mut files: Vec<String> = std::fs::read_dir(d.path().join("attachments")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        files.sort();
        assert_eq!(files, vec!["a-2.png", "a.png"]);
    }

    #[tokio::test]
    async fn static_files_are_served_without_a_token() {
        let d = tempfile::tempdir().unwrap();
        let app = crate::build_router(state(d.path(), None), TOKEN.into());
        let (s, b, h) = call(&app, "GET", "/static/katex/katex.min.js", None, None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b.len() > 100_000);
        assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/javascript"));
        let (s, _, _) = call(&app, "GET", "/static/katex/fonts/KaTeX_Main-Regular.woff2", None, None).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = call(&app, "GET", "/static/nope", None, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = call(&app, "GET", "/static/..%2Ftoken", None, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
}
