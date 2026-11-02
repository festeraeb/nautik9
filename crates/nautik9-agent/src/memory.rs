//! Hybrid memory search and the `embeddings` tool (Jade Vault's "Hybrid memory search" and
//! "The embeddings tool", rewritten in Rust; no Jade code copied).
//!
//! * [`Embedder`]: a client for any OpenAI-compatible `/v1/embeddings` endpoint, configured in
//!   `agent.json` as `embeddings: {"url", "model", "api_key_env"?}`. With no endpoint every
//!   semantic part is OFF and every report says "keyword only". Vectors are never invented.
//! * [`MemoryIndex`]: one SQLite file (`index_path`, default `<config dir>/vectors.db`):
//!   `chunks(id, collection, source, head, text, hash)`, an FTS5 table (bm25, porter stemming)
//!   and the vectors as little-endian f32 BLOBs, searched by brute-force cosine in Rust. It is a
//!   cache: deleting the file loses nothing that the notes and chats do not hold; `index` and the
//!   memory sync rebuild it. The same text is never stored twice in a collection (hash).
//! * Search is reciprocal rank fusion (k = 60) of the bm25 ranking and the cosine ranking.
//! * Collections: `memory` (bullets of `Jade/Memory.md`, resynced when the note changes),
//!   `journal` (written by the journal, one entry per chat), `notes` (the `index` action splits
//!   notes by headings and paragraphs) and any name the agent chooses.
//!
//! Chunks added while no embedding endpoint answers are stored without a vector and stay
//! searchable by keyword; the next `index` or memory sync embeds them.

use async_trait::async_trait;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use crate::tools::{Tool, ToolCtx};

pub const RRF_K: f64 = 60.0;
/// Candidates taken from each ranking before fusion.
const CANDIDATES: usize = 50;
const BATCH: usize = 16;
const MAX_CHUNK_CHARS: usize = 900;
const MAX_EMBED_CHARS: usize = 2000;

// ── pure helpers ────────────────────────────────────────────────────────────

/// Reciprocal rank fusion: each list is ids best-first; an id scores the sum of
/// `1 / (k + rank)` over the lists that contain it (rank from 1). Best first; ties keep the
/// order of first appearance.
pub fn rrf(lists: &[Vec<i64>], k: f64) -> Vec<(i64, f64)> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    for l in lists {
        let mut seen = HashSet::new();
        for (i, id) in l.iter().enumerate() {
            if !seen.insert(*id) {
                continue;
            }
            if !score.contains_key(id) {
                order.push(*id);
            }
            *score.entry(*id).or_insert(0.0) += 1.0 / (k + (i + 1) as f64);
        }
    }
    let mut out: Vec<(i64, f64)> = order.iter().map(|id| (*id, score[id])).collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Cosine similarity; None for empty, mismatched or zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

/// A stable (not cryptographic) 128-bit content hash of the whitespace-trimmed text, as hex:
/// two FNV-1a passes with different seeds. Used only to avoid storing the same text twice.
pub fn text_hash(text: &str) -> String {
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;
    for b in text.trim().bytes() {
        h1 = (h1 ^ b as u64).wrapping_mul(0x100000001b3);
        h2 = (h2 ^ b as u64).wrapping_mul(0x1000193).rotate_left(5);
    }
    format!("{h1:016x}{h2:016x}")
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Split a note into (head, text) chunks: by headings (not inside code fences), then by blank-line
/// paragraphs packed up to `max_chars`; an over-long paragraph is cut at whitespace. `head` is the
/// nearest heading, or the note title above the first heading.
pub fn chunk_note(title: &str, content: &str, max_chars: usize) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<String>)> = vec![(title.to_string(), vec![])];
    let mut para = String::new();
    let mut fence = false;
    let flush = |para: &mut String, sections: &mut Vec<(String, Vec<String>)>| {
        let p = para.trim();
        if !p.is_empty() {
            sections.last_mut().unwrap().1.push(p.to_string());
        }
        para.clear();
    };
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fence = !fence;
        }
        let heading = !fence && t.starts_with('#') && t.trim_start_matches('#').starts_with(' ') && t.len() - t.trim_start_matches('#').len() <= 6;
        if heading {
            flush(&mut para, &mut sections);
            sections.push((t.trim_start_matches('#').trim().to_string(), vec![]));
        } else if line.trim().is_empty() && !fence {
            flush(&mut para, &mut sections);
        } else {
            para.push_str(line);
            para.push('\n');
        }
    }
    flush(&mut para, &mut sections);
    let mut out = Vec::new();
    for (head, paras) in sections {
        let mut cur = String::new();
        let push = |cur: &mut String, out: &mut Vec<(String, String)>| {
            if !cur.trim().is_empty() {
                out.push((head.clone(), cur.trim().to_string()));
            }
            cur.clear();
        };
        for p in paras {
            for piece in split_long(&p, max_chars) {
                if !cur.is_empty() && cur.chars().count() + piece.chars().count() + 2 > max_chars {
                    push(&mut cur, &mut out);
                }
                if !cur.is_empty() {
                    cur.push_str("\n\n");
                }
                cur.push_str(&piece);
            }
        }
        push(&mut cur, &mut out);
    }
    out
}

fn split_long(p: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in p.split_inclusive(char::is_whitespace) {
        if cur.chars().count() + word.chars().count() > max && !cur.is_empty() {
            out.push(cur.trim().to_string());
            cur.clear();
        }
        cur.push_str(word);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// FTS5 MATCH expression: every word quoted, OR-ed (bm25 ranks the ones that match most).
fn fts_query(q: &str) -> Option<String> {
    let mut seen = HashSet::new();
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .take(32)
        .map(|t| format!("\"{t}\""))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

// ── embedding client ────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct EmbedConfig {
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
}

impl EmbedConfig {
    /// From agent.json `embeddings`; None when the key is absent or null (semantic OFF).
    pub fn from_json(v: &Value) -> Result<Option<Self>, String> {
        if v.is_null() {
            return Ok(None);
        }
        let url = v["url"].as_str().filter(|u| !u.is_empty()).ok_or("agent.json embeddings: missing `url`")?;
        let model = v["model"].as_str().filter(|u| !u.is_empty()).ok_or("agent.json embeddings: missing `model`")?;
        Ok(Some(EmbedConfig { url: url.to_string(), model: model.to_string(), api_key_env: v["api_key_env"].as_str().map(str::to_string) }))
    }
}

pub struct Embedder {
    pub cfg: EmbedConfig,
    http: reqwest::Client,
}

impl Embedder {
    pub fn new(cfg: EmbedConfig) -> Self {
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap_or_default();
        Embedder { cfg, http }
    }

    fn endpoint(&self) -> String {
        let u = self.cfg.url.trim_end_matches('/');
        if u.ends_with("/embeddings") {
            u.to_string()
        } else {
            format!("{u}/embeddings")
        }
    }

    /// One vector per text, in order. Any failure is an error; nothing is made up.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut all: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let input: Vec<String> = batch.iter().map(|t| t.chars().take(MAX_EMBED_CHARS).collect()).collect();
            let mut req = self.http.post(self.endpoint()).json(&json!({"model": self.cfg.model, "input": input}));
            if let Some(var) = &self.cfg.api_key_env {
                match std::env::var(var) {
                    Ok(k) if !k.is_empty() => req = req.bearer_auth(k),
                    _ => return Err(format!("embeddings api_key_env `{var}` is not set")),
                }
            }
            let resp = req.send().await.map_err(|e| format!("embedding endpoint: {}", e.without_url()))?;
            let status = resp.status();
            let v: Value = resp.json().await.map_err(|e| format!("embedding endpoint: bad response: {}", e.without_url()))?;
            if !status.is_success() {
                let msg = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("");
                return Err(format!("embedding endpoint: HTTP {} {msg}", status.as_u16()).trim().to_string());
            }
            let data = v["data"].as_array().ok_or("embedding endpoint: reply has no `data`")?;
            if data.len() != batch.len() {
                return Err(format!("embedding endpoint returned {} vectors for {} texts", data.len(), batch.len()));
            }
            let mut rows: Vec<(usize, Vec<f32>)> = Vec::new();
            for (i, d) in data.iter().enumerate() {
                let e = d["embedding"].as_array().ok_or("embedding endpoint: item without `embedding`")?;
                let vec: Vec<f32> = e.iter().map(|x| x.as_f64().unwrap_or(f64::NAN) as f32).collect();
                if vec.is_empty() || vec.iter().any(|x| !x.is_finite()) {
                    return Err("embedding endpoint returned an empty or non-numeric vector".into());
                }
                rows.push((d["index"].as_u64().map(|x| x as usize).unwrap_or(i), vec));
            }
            rows.sort_by_key(|r| r.0);
            all.extend(rows.into_iter().map(|r| r.1));
        }
        if let Some(d) = all.first().map(Vec::len) {
            if all.iter().any(|v| v.len() != d) {
                return Err("embedding endpoint returned vectors of different sizes".into());
            }
        }
        Ok(all)
    }
}

// ── the index ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: i64,
    pub collection: String,
    pub source: String,
    pub head: String,
    pub text: String,
    /// The reciprocal-rank-fusion score (one list only in keyword or semantic mode); `cosine`
    /// is the raw similarity when the semantic ranking found it.
    pub score: f64,
    pub cosine: Option<f32>,
    pub keyword_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
}

impl Hit {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "collection": self.collection, "source": self.source, "head": self.head, "text": self.text,
               "score": self.score, "cosine": self.cosine, "keyword_rank": self.keyword_rank, "semantic_rank": self.semantic_rank})
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// What actually ran: "hybrid", "semantic", "keyword" or "keyword only (...)".
    pub mode_used: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncReport {
    pub added: usize,
    pub kept: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotesIndexReport {
    pub notes: usize,
    pub sync: SyncReport,
    pub embedded: usize,
    pub embed_error: Option<String>,
}

pub struct MemoryIndex {
    conn: Mutex<Connection>,
    embedder: Option<Embedder>,
}

fn db<T>(r: rusqlite::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

impl MemoryIndex {
    /// Open (or create) the index file. A changed embedding model drops the stored vectors (they
    /// are re-embedded on the next index or memory sync); chunks and keyword search are kept.
    pub fn open(path: &Path, embed: Option<EmbedConfig>) -> Result<Self, String> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Self::init(Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?, embed)
    }

    pub fn in_memory(embed: Option<EmbedConfig>) -> Result<Self, String> {
        Self::init(db(Connection::open_in_memory())?, embed)
    }

    fn init(conn: Connection, embed: Option<EmbedConfig>) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks (id INTEGER PRIMARY KEY, collection TEXT NOT NULL, source TEXT NOT NULL, head TEXT NOT NULL,
                 text TEXT NOT NULL, hash TEXT NOT NULL, UNIQUE (collection, hash));
             CREATE INDEX IF NOT EXISTS chunks_source ON chunks (collection, source);
             CREATE TABLE IF NOT EXISTS vectors (id INTEGER PRIMARY KEY, dims INTEGER NOT NULL, vec BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        conn.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, head, tokenize = 'porter unicode61');")
            .map_err(|e| format!("this SQLite has no FTS5 ({e})"))?;
        if let Some(cfg) = &embed {
            let old: Option<String> = db(conn.query_row("SELECT v FROM meta WHERE k = 'model'", [], |r| r.get(0)).optional())?;
            if old.as_deref() != Some(cfg.model.as_str()) {
                db(conn.execute("DELETE FROM vectors", []))?;
                db(conn.execute("INSERT OR REPLACE INTO meta VALUES ('model', ?1)", params![cfg.model]))?;
            }
        }
        Ok(MemoryIndex { conn: Mutex::new(conn), embedder: embed.map(Embedder::new) })
    }

    /// True when an embedding endpoint is configured (it may still be down).
    pub fn semantic_on(&self) -> bool {
        self.embedder.is_some()
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_meta(&self, k: &str, v: &str) {
        let _ = self.conn().execute("INSERT OR REPLACE INTO meta VALUES (?1, ?2)", params![k, v]);
    }

    fn get_meta(&self, k: &str) -> Option<String> {
        self.conn().query_row("SELECT v FROM meta WHERE k = ?1", params![k], |r| r.get(0)).optional().ok().flatten()
    }

    fn record_error(&self, e: &str) {
        self.set_meta("last_error", &format!("{} {e}", chrono::Local::now().format("%Y-%m-%d %H:%M")));
    }

    /// Insert chunks that are not yet in the collection (by hash); returns the new ids.
    fn insert(&self, collection: &str, source: &str, items: &[(String, String)]) -> Result<(Vec<i64>, usize), String> {
        let c = self.conn();
        let (mut ids, mut dup) = (Vec::new(), 0);
        for (head, text) in items {
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let n = db(c.execute(
                "INSERT OR IGNORE INTO chunks (collection, source, head, text, hash) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![collection, source, head, text, text_hash(text)],
            ))?;
            if n == 0 {
                dup += 1;
                continue;
            }
            let id = c.last_insert_rowid();
            db(c.execute("INSERT INTO fts (rowid, text, head) VALUES (?1, ?2, ?3)", params![id, text, head]))?;
            ids.push(id);
        }
        Ok((ids, dup))
    }

    fn delete_ids(&self, ids: &[i64]) -> Result<(), String> {
        let c = self.conn();
        for id in ids {
            db(c.execute("DELETE FROM fts WHERE rowid = ?1", params![id]))?;
            db(c.execute("DELETE FROM vectors WHERE id = ?1", params![id]))?;
            db(c.execute("DELETE FROM chunks WHERE id = ?1", params![id]))?;
        }
        Ok(())
    }

    /// Chunks with no stored vector: (id, text to embed).
    fn pending(&self, limit: usize) -> Result<Vec<(i64, String)>, String> {
        let c = self.conn();
        let mut st = db(c.prepare("SELECT c.id, c.head, c.text FROM chunks c LEFT JOIN vectors v ON v.id = c.id WHERE v.id IS NULL ORDER BY c.id LIMIT ?1"))?;
        let rows = db(st.query_map(params![limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, head, text) = db(r)?;
            out.push((id, if head.is_empty() { text } else { format!("{head}\n{text}") }));
        }
        Ok(out)
    }

    /// Embed every chunk that has no vector. Ok(count embedded); Err keeps the rest keyword-only.
    pub async fn embed_pending(&self) -> Result<usize, String> {
        let Some(emb) = &self.embedder else { return Ok(0) };
        let mut done = 0;
        loop {
            let todo = self.pending(64)?;
            if todo.is_empty() {
                return Ok(done);
            }
            let texts: Vec<String> = todo.iter().map(|t| t.1.clone()).collect();
            let vecs = match emb.embed(&texts).await {
                Ok(v) => v,
                Err(e) => {
                    self.record_error(&e);
                    return Err(e);
                }
            };
            let c = self.conn();
            for ((id, _), v) in todo.iter().zip(&vecs) {
                db(c.execute("INSERT OR REPLACE INTO vectors VALUES (?1, ?2, ?3)", params![id, v.len() as i64, to_blob(v)]))?;
            }
            drop(c);
            self.set_meta("dims", &vecs[0].len().to_string());
            done += todo.len();
        }
    }

    /// Store texts in a collection: new ones are embedded now (when an endpoint is set), repeats
    /// are skipped. `error` is set when embedding failed (the texts are kept keyword-only).
    pub async fn add(&self, collection: &str, source: &str, head: &str, texts: &[String]) -> Result<Value, String> {
        let items: Vec<(String, String)> = texts.iter().map(|t| (head.to_string(), t.clone())).collect();
        let (ids, dup) = self.insert(collection, source, &items)?;
        let (embedded, error) = match self.embed_pending().await {
            Ok(n) => (n, None),
            Err(e) => (0, Some(e)),
        };
        Ok(json!({"added": ids.len(), "duplicates_skipped": dup, "ids": ids, "embedded": embedded,
                  "semantic": if self.semantic_on() { "on" } else { "off: keyword only" }, "embed_error": error}))
    }

    /// Make a source's chunks in a collection equal `items` (hash-matched): stale chunks go,
    /// new ones are added, unchanged ones keep their vectors. Embedding is separate (`embed_pending`).
    pub fn replace_source(&self, collection: &str, source: &str, items: &[(String, String)]) -> Result<SyncReport, String> {
        let want: HashSet<String> = items.iter().map(|(_, t)| text_hash(t)).collect();
        let existing: Vec<(i64, String)> = {
            let c = self.conn();
            let mut st = db(c.prepare("SELECT id, hash FROM chunks WHERE collection = ?1 AND source = ?2"))?;
            let rows = db(st.query_map(params![collection, source], |r| Ok((r.get(0)?, r.get(1)?))))?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let have: HashSet<&String> = existing.iter().map(|e| &e.1).collect();
        let stale: Vec<i64> = existing.iter().filter(|e| !want.contains(&e.1)).map(|e| e.0).collect();
        let kept = existing.len() - stale.len();
        self.delete_ids(&stale)?;
        let fresh: Vec<(String, String)> = items.iter().filter(|(_, t)| !have.contains(&text_hash(t))).cloned().collect();
        let (ids, _) = self.insert(collection, source, &fresh)?;
        Ok(SyncReport { added: ids.len(), kept, removed: stale.len() })
    }

    /// Keep the `memory` collection equal to the bullets of `Jade/Memory.md`. Cheap when the note
    /// did not change; embeds anything still missing a vector.
    pub async fn sync_memory(&self, note_text: &str, bullets: &[String]) -> Result<SyncReport, String> {
        let h = text_hash(note_text);
        let mut rep = SyncReport::default();
        if self.get_meta("memory_hash").as_deref() != Some(h.as_str()) {
            let items: Vec<(String, String)> = bullets.iter().map(|b| (String::new(), b.clone())).collect();
            rep = self.replace_source("memory", crate::tools::MEMORY_PATH, &items)?;
            self.set_meta("memory_hash", &h);
        }
        self.embed_pending().await?;
        Ok(rep)
    }

    /// Chunk the vault notes whose title starts with `prefix` into the `notes` collection and embed
    /// what is missing. Memory.md and the journal are not notes for search. With an empty prefix,
    /// notes that no longer exist are dropped from the collection too.
    pub async fn index_vault_notes(&self, items: &[nautik9_wayfinder::VaultItem], prefix: &str) -> Result<NotesIndexReport, String> {
        let prefix = prefix.trim_start_matches('/');
        let mut rep = NotesIndexReport::default();
        let mut live: HashSet<&str> = HashSet::new();
        for n in items.iter().filter(|n| n.title != crate::tools::MEMORY_PATH && !n.title.starts_with("Jade/Journal/")) {
            live.insert(n.title.as_str());
            if !n.title.trim_start_matches('/').starts_with(prefix) {
                continue;
            }
            let chunks = chunk_note(&n.title, &n.content, MAX_CHUNK_CHARS);
            let r = self.replace_source("notes", &n.title, &chunks)?;
            (rep.sync.added, rep.sync.kept, rep.sync.removed, rep.notes) = (rep.sync.added + r.added, rep.sync.kept + r.kept, rep.sync.removed + r.removed, rep.notes + 1);
        }
        if prefix.is_empty() {
            let sources: Vec<String> = {
                let c = self.conn();
                let mut st = db(c.prepare("SELECT DISTINCT source FROM chunks WHERE collection = 'notes'"))?;
                let rows = db(st.query_map([], |r| r.get::<_, String>(0)))?;
                rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
            };
            for s in sources.iter().filter(|s| !live.contains(s.as_str())) {
                rep.sync.removed += self.remove(None, Some(s), Some("notes"))?;
            }
        }
        match self.embed_pending().await {
            Ok(n) => rep.embedded = n,
            Err(e) => rep.embed_error = Some(e),
        }
        Ok(rep)
    }

    /// Delete by id, by source (optionally within a collection) or a whole collection.
    pub fn remove(&self, id: Option<i64>, source: Option<&str>, collection: Option<&str>) -> Result<usize, String> {
        let ids: Vec<i64> = {
            let c = self.conn();
            let (sql, ps): (&str, Vec<rusqlite::types::Value>) = match (id, source, collection) {
                (Some(i), _, _) => ("SELECT id FROM chunks WHERE id = ?1", vec![i.into()]),
                (None, Some(s), Some(col)) => ("SELECT id FROM chunks WHERE source = ?1 AND collection = ?2", vec![s.to_string().into(), col.to_string().into()]),
                (None, Some(s), None) => ("SELECT id FROM chunks WHERE source = ?1", vec![s.to_string().into()]),
                (None, None, Some(col)) => ("SELECT id FROM chunks WHERE collection = ?1", vec![col.to_string().into()]),
                _ => return Err("remove needs `id`, `source` or `collection`".into()),
            };
            let mut st = db(c.prepare(sql))?;
            let rows = db(st.query_map(params_from_iter(ps), |r| r.get(0)))?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        self.delete_ids(&ids)?;
        Ok(ids.len())
    }

    fn keyword_ranking(&self, query: &str, cols: &[String]) -> Result<Vec<i64>, String> {
        let Some(q) = fts_query(query) else { return Ok(vec![]) };
        let c = self.conn();
        let mut sql = "SELECT c.id FROM fts JOIN chunks c ON c.id = fts.rowid WHERE fts MATCH ?1".to_string();
        let mut ps: Vec<rusqlite::types::Value> = vec![q.into()];
        if !cols.is_empty() {
            sql.push_str(&format!(" AND c.collection IN ({})", (0..cols.len()).map(|i| format!("?{}", i + 2)).collect::<Vec<_>>().join(",")));
            ps.extend(cols.iter().map(|x| x.clone().into()));
        }
        sql.push_str(&format!(" ORDER BY fts.rank LIMIT {CANDIDATES}"));
        let mut st = db(c.prepare(&sql))?;
        let rows = db(st.query_map(params_from_iter(ps), |r| r.get(0)))?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// Brute-force cosine over the stored vectors; best first as (id, cosine).
    fn semantic_ranking(&self, qv: &[f32], cols: &[String]) -> Result<Vec<(i64, f32)>, String> {
        let c = self.conn();
        let mut sql = "SELECT v.id, v.vec FROM vectors v JOIN chunks c ON c.id = v.id".to_string();
        let mut ps: Vec<rusqlite::types::Value> = vec![];
        if !cols.is_empty() {
            sql.push_str(&format!(" WHERE c.collection IN ({})", (0..cols.len()).map(|i| format!("?{}", i + 1)).collect::<Vec<_>>().join(",")));
            ps.extend(cols.iter().map(|x| x.clone().into()));
        }
        let mut st = db(c.prepare(&sql))?;
        let rows = db(st.query_map(params_from_iter(ps), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))))?;
        let mut scored = Vec::new();
        for r in rows {
            let (id, blob) = db(r)?;
            if let Some(s) = cosine(qv, &from_blob(&blob)) {
                scored.push((id, s));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(CANDIDATES);
        Ok(scored)
    }

    fn hydrate(&self, id: i64) -> Option<(String, String, String, String)> {
        self.conn().query_row("SELECT collection, source, head, text FROM chunks WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok()
    }

    /// Search. `mode` is "hybrid" (default), "semantic" or "keyword"; `collections` empty = all.
    /// Without an embedding endpoint hybrid degrades to keyword and says so; semantic is an error.
    pub async fn search(&self, query: &str, mode: &str, collections: &[String], k: usize) -> Result<SearchOutcome, String> {
        let k = k.clamp(1, 30);
        if !["hybrid", "semantic", "keyword"].contains(&mode) {
            return Err(format!("unknown mode `{mode}` (hybrid, semantic or keyword)"));
        }
        let mut note = None;
        let mut sem: Vec<(i64, f32)> = vec![];
        let mut used = mode.to_string();
        if mode != "keyword" {
            match &self.embedder {
                None if mode == "semantic" => return Err("semantic search is off: no embedding endpoint is configured (keyword only)".into()),
                None => {
                    used = "keyword only (no embedding endpoint configured)".into();
                }
                Some(e) => match e.embed(&[query.to_string()]).await {
                    Ok(v) => sem = self.semantic_ranking(&v[0], collections)?,
                    Err(err) => {
                        self.record_error(&err);
                        if mode == "semantic" {
                            return Err(err);
                        }
                        used = "keyword only (embedding endpoint failed)".into();
                        note = Some(err);
                    }
                },
            }
        }
        let kw = if used.starts_with("semantic") { vec![] } else { self.keyword_ranking(query, collections)? };
        let cos: HashMap<i64, f32> = sem.iter().cloned().collect();
        let sem_ids: Vec<i64> = sem.iter().map(|s| s.0).collect();
        let fused = rrf(&[kw.clone(), sem_ids.clone()], RRF_K);
        let mut hits = Vec::new();
        for (id, score) in fused.into_iter().take(k) {
            let Some((collection, source, head, text)) = self.hydrate(id) else { continue };
            hits.push(Hit {
                id,
                collection,
                source,
                head,
                text,
                score,
                cosine: cos.get(&id).copied(),
                keyword_rank: kw.iter().position(|x| *x == id).map(|p| p + 1),
                semantic_rank: sem_ids.iter().position(|x| *x == id).map(|p| p + 1),
            });
        }
        Ok(SearchOutcome { hits, mode_used: used, note })
    }

    pub async fn compare(&self, a: &str, b: &str) -> Result<f32, String> {
        let Some(e) = &self.embedder else { return Err("compare needs an embedding endpoint; none is configured (keyword only)".into()) };
        let v = e.embed(&[a.to_string(), b.to_string()]).await.inspect_err(|err| self.record_error(err))?;
        cosine(&v[0], &v[1]).ok_or_else(|| "cannot compare these vectors".to_string())
    }

    pub fn status(&self) -> Result<Value, String> {
        let c = self.conn();
        let mut st = db(c.prepare("SELECT c.collection, COUNT(*), COUNT(v.id) FROM chunks c LEFT JOIN vectors v ON v.id = c.id GROUP BY c.collection ORDER BY c.collection"))?;
        let rows = db(st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))?;
        let mut cols = serde_json::Map::new();
        for r in rows {
            let (n, chunks, vecs) = db(r)?;
            cols.insert(n, json!({"chunks": chunks, "with_vectors": vecs}));
        }
        let ver: String = db(c.query_row("SELECT sqlite_version()", [], |r| r.get(0)))?;
        drop(st);
        drop(c);
        Ok(json!({
            "semantic": if self.semantic_on() { "on" } else { "off: keyword only" },
            "model": self.embedder.as_ref().map(|e| e.cfg.model.clone()),
            "dims": self.get_meta("dims").and_then(|d| d.parse::<i64>().ok()),
            "collections": cols,
            "fts5": true,
            "sqlite": ver,
            "last_error": self.get_meta("last_error"),
        }))
    }
}

// ── the tool and its HTTP-shared action runner ──────────────────────────────

fn str_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => vec![],
    }
}

/// Run one `embeddings` action; shared by the tool and `POST /api/agent/embeddings`.
pub async fn run_action(ctx: &ToolCtx, a: &Value) -> Result<Value, String> {
    let m = &ctx.memory;
    match a["action"].as_str().ok_or("missing string argument `action`")? {
        "search" => {
            let q = a["query"].as_str().filter(|q| !q.trim().is_empty()).ok_or("missing string argument `query`")?;
            let out = m.search(q, a["mode"].as_str().unwrap_or("hybrid"), &str_list(&a["collection"]), a["k"].as_u64().unwrap_or(8) as usize).await?;
            Ok(json!({"mode_used": out.mode_used, "note": out.note, "hits": out.hits.iter().map(Hit::to_json).collect::<Vec<_>>()}))
        }
        "add" => {
            let mut texts = str_list(&a["texts"]);
            if let Some(t) = a["text"].as_str() {
                texts.insert(0, t.to_string());
            }
            if texts.is_empty() {
                return Err("add needs `text` or `texts`".into());
            }
            let coll = a["collection"].as_str().filter(|c| !c.trim().is_empty()).ok_or("missing string argument `collection`")?;
            m.add(coll.trim(), a["source"].as_str().unwrap_or(""), a["head"].as_str().unwrap_or(""), &texts).await
        }
        "index" => {
            let items = ctx.vault.list_items().await.map_err(|e| e.to_string())?;
            let r = m.index_vault_notes(&items, a["path"].as_str().unwrap_or("")).await?;
            Ok(json!({"notes": r.notes, "chunks_added": r.sync.added, "chunks_kept": r.sync.kept, "chunks_removed": r.sync.removed, "embedded": r.embedded,
                      "semantic": if m.semantic_on() { "on" } else { "off: keyword only" }, "embed_error": r.embed_error}))
        }
        "remove" => {
            let n = m.remove(a["id"].as_i64(), a["source"].as_str(), a["collection"].as_str())?;
            Ok(json!({"removed": n}))
        }
        "compare" => {
            let (x, y) = (a["a"].as_str().ok_or("missing string argument `a`")?, a["b"].as_str().ok_or("missing string argument `b`")?);
            Ok(json!({"cosine": m.compare(x, y).await?}))
        }
        "status" => m.status(),
        other => Err(format!("unknown action `{other}` (search, add, index, remove, compare, status)")),
    }
}

fn render(action: &str, v: &Value) -> String {
    if action != "search" {
        return v.to_string();
    }
    let hits = v["hits"].as_array().cloned().unwrap_or_default();
    let mut s = format!("mode: {}{}\n", v["mode_used"].as_str().unwrap_or(""), v["note"].as_str().map(|n| format!(" ({n})")).unwrap_or_default());
    if hits.is_empty() {
        s.push_str("no matches");
    }
    for h in hits {
        let snippet: String = h["text"].as_str().unwrap_or("").chars().take(240).collect::<String>().replace('\n', " ");
        let cos = h["cosine"].as_f64().map(|c| format!(", cosine {c:.3}")).unwrap_or_default();
        s.push_str(&format!("- #{} [{}] {} {}(score {:.4}{cos}): {snippet}\n", h["id"], h["collection"].as_str().unwrap_or(""), h["source"].as_str().unwrap_or(""), h["head"].as_str().filter(|x| !x.is_empty()).map(|x| format!("> {x} ")).unwrap_or_default(), h["score"].as_f64().unwrap_or(0.0)));
    }
    s.trim_end().to_string()
}

pub struct EmbeddingsTool;

#[async_trait]
impl Tool for EmbeddingsTool {
    fn name(&self) -> &str {
        "embeddings"
    }
    fn description(&self) -> &str {
        "Search by meaning and keywords (hybrid, reciprocal rank fusion) over your own indexed texts. Actions: search (query, mode hybrid|semantic|keyword, collection = comma list, k <= 30), add (text or texts, collection, source?, head?; never stored twice), index (path? prefix: embeds the notes into collection `notes`), remove (id | source | collection), compare (a, b: cosine), status. Collections: memory and journal are filled automatically, notes by index, any other name is yours. With no embedding endpoint the results say `keyword only`."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["search","add","index","remove","compare","status"]},
            "query":{"type":"string"},"mode":{"type":"string","enum":["hybrid","semantic","keyword"]},
            "collection":{"type":"string"},"k":{"type":"integer"},
            "text":{"type":"string"},"texts":{"type":"array","items":{"type":"string"}},
            "source":{"type":"string"},"head":{"type":"string"},"path":{"type":"string"},"id":{"type":"integer"},
            "a":{"type":"string"},"b":{"type":"string"}},"required":["action"]})
    }
    async fn call(&self, a: &Value, c: &ToolCtx) -> Result<String, String> {
        let v = run_action(c, a).await?;
        Ok(render(a["action"].as_str().unwrap_or(""), &v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_math() {
        // a: rank 1 and 2 -> 1/61 + 1/62; b: rank 2 and 1 -> same; c only in one list at rank 3.
        let r = rrf(&[vec![1, 2, 3], vec![2, 1]], 60.0);
        assert_eq!(r.len(), 3);
        let top = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((r[0].1 - top).abs() < 1e-12 && (r[1].1 - top).abs() < 1e-12);
        assert_eq!((r[0].0, r[1].0), (1, 2), "ties keep first appearance");
        assert_eq!(r[2], (3, 1.0 / 63.0));
        // An item high in both beats one first in only one list.
        let r = rrf(&[vec![9, 5], vec![7, 5]], 60.0);
        assert_eq!(r[0].0, 5);
        assert!(rrf(&[], 60.0).is_empty());
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap().abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), None);
        assert_eq!(from_blob(&to_blob(&[1.5, -2.0])), vec![1.5, -2.0]);
    }

    #[test]
    fn chunking_by_headings_and_paragraphs() {
        let note = "Intro line\n\n# Boat\nEngine notes.\n\nSecond para.\n\n## Fuel\n```\n# not a heading\n```\nDiesel only.\n";
        let c = chunk_note("Boat.md", note, 900);
        assert_eq!(c[0], ("Boat.md".to_string(), "Intro line".to_string()));
        assert_eq!(c[1], ("Boat".to_string(), "Engine notes.\n\nSecond para.".to_string()));
        assert_eq!(c[2].0, "Fuel");
        assert!(c[2].1.contains("# not a heading") && c[2].1.contains("Diesel only."));
        assert_eq!(c.len(), 3);
        // Packing: small max forces one chunk per paragraph; long paragraphs are cut at spaces.
        let c = chunk_note("T", "aaa bbb\n\nccc ddd", 8);
        assert_eq!(c.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), vec!["aaa bbb", "ccc ddd"]);
        let long = "word ".repeat(100);
        assert!(chunk_note("T", &long, 50).iter().all(|(_, t)| t.chars().count() <= 50));
        assert!(chunk_note("T", "  \n\n", 50).is_empty());
    }

    #[tokio::test]
    async fn dedup_and_fts_keyword_search() {
        let m = MemoryIndex::in_memory(None).unwrap();
        let r = m.add("snippets", "s", "h", &["Likes oat milk lattes".into(), "Likes oat milk lattes  ".into(), "Boat engine is a Yanmar diesel".into()]).await.unwrap();
        assert_eq!((r["added"].as_u64(), r["duplicates_skipped"].as_u64()), (Some(2), Some(1)));
        assert_eq!(m.add("snippets", "s", "h", &["Likes oat milk lattes".into()]).await.unwrap()["added"], 0);
        // The same text in another collection is a separate row.
        assert_eq!(m.add("other", "s", "", &["Likes oat milk lattes".into()]).await.unwrap()["added"], 1);
        let o = m.search("diesel engines", "hybrid", &[], 5).await.unwrap();
        assert!(o.mode_used.starts_with("keyword only"), "{}", o.mode_used);
        assert_eq!(o.hits.len(), 1);
        assert!(o.hits[0].text.contains("Yanmar") && o.hits[0].keyword_rank == Some(1) && o.hits[0].cosine.is_none());
        // Stemming and collection filter.
        assert_eq!(m.search("latte", "keyword", &["other".into()], 5).await.unwrap().hits[0].collection, "other");
        assert!(m.search("caffeinated drink", "keyword", &[], 5).await.unwrap().hits.is_empty());
        assert!(m.search("!!!", "keyword", &[], 5).await.unwrap().hits.is_empty());
        let e = m.search("x", "semantic", &[], 5).await.unwrap_err();
        assert!(e.contains("keyword only"), "{e}");
        assert!(m.compare("a", "b").await.unwrap_err().contains("keyword only"));
        // remove by source, then by collection.
        assert_eq!(m.remove(None, Some("s"), Some("snippets")).unwrap(), 2);
        assert!(m.search("diesel", "keyword", &[], 5).await.unwrap().hits.is_empty());
        assert_eq!(m.remove(None, None, Some("other")).unwrap(), 1);
        assert!(m.remove(None, None, None).is_err());
        let st = m.status().unwrap();
        assert_eq!(st["semantic"], "off: keyword only");
        assert_eq!(st["fts5"], true);
    }

    #[test]
    fn replace_source_keeps_unchanged_and_drops_stale() {
        let m = MemoryIndex::in_memory(None).unwrap();
        let it = |t: &str| (String::new(), t.to_string());
        assert_eq!(m.replace_source("memory", "M", &[it("one"), it("two")]).unwrap(), SyncReport { added: 2, kept: 0, removed: 0 });
        assert_eq!(m.replace_source("memory", "M", &[it("two"), it("three")]).unwrap(), SyncReport { added: 1, kept: 1, removed: 1 });
    }

    #[test]
    fn embed_config_parses() {
        assert_eq!(EmbedConfig::from_json(&Value::Null).unwrap(), None);
        assert!(EmbedConfig::from_json(&json!({"url":"x"})).is_err());
        let c = EmbedConfig::from_json(&json!({"url":"http://h/v1","model":"m","api_key_env":"K"})).unwrap().unwrap();
        assert_eq!((c.model.as_str(), c.api_key_env.as_deref()), ("m", Some("K")));
        assert_eq!(Embedder::new(c).endpoint(), "http://h/v1/embeddings");
    }
}
