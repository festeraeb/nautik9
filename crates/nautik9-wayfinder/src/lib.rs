use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use nautik9_core::{
    avatar::MackState,
    error::NautikError,
    event::{EventPayload, NautikEvent, SearchResultItem},
    module::{ModuleContext, NautikModule},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultItem {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    /// `[[link]]` targets found in the content (names or ids), in order, unique.
    #[serde(default)]
    pub links: Vec<String>,
}

/// Blank out fenced code blocks and inline code so links/tags inside them are ignored.
fn strip_code(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut in_fence = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push('\n');
            continue;
        }
        let mut in_tick = false;
        for ch in line.chars() {
            if ch == '`' {
                in_tick = !in_tick;
                out.push(' ');
            } else if in_tick {
                out.push(' ');
            } else {
                out.push(ch);
            }
        }
        out.push('\n');
    }
    out
}

/// `![[photo.png]]`-style embeds name an attachment, not a note.
fn is_media_name(name: &str) -> bool {
    const EXT: [&str; 14] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "pdf", "mp3", "wav", "ogg", "m4a", "mp4", "webm"];
    name.rsplit_once('.').is_some_and(|(_, e)| EXT.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// `[[Note]]`, `[[Note|Label]]`, `[[Note#Heading]]` -> "Note". Unique, in order.
pub fn parse_links(content: &str) -> Vec<String> {
    let text = strip_code(content);
    let mut links: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        if !inner.contains('\n') {
            let target = inner.split('|').next().unwrap_or("").split('#').next().unwrap_or("").trim();
            if !target.is_empty() && !is_media_name(target) && !links.iter().any(|l| l.eq_ignore_ascii_case(target)) {
                links.push(target.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    links
}

/// `#tag` / `#project/client`: starts with a letter, then word chars, `/`, `-`.
/// Must follow whitespace or an opening bracket (so URLs and headings are not tags).
pub fn parse_tags(content: &str) -> Vec<String> {
    let text = strip_code(content);
    let chars: Vec<char> = text.chars().collect();
    let mut tags: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace() || "([{".contains(chars[i - 1])) {
            if i + 1 < chars.len() && chars[i + 1].is_alphabetic() {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '/' || chars[j] == '-') {
                    j += 1;
                }
                let tag: String = chars[i + 1..j].iter().collect::<String>().trim_end_matches(['/', '-']).to_string();
                let tag = tag.to_lowercase();
                if !tag.is_empty() && !tags.contains(&tag) {
                    tags.push(tag);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    tags
}

/// Explicit tags plus `#tags` found in the content, lowercased, unique.
pub fn merge_tags(explicit: &[String], content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in explicit.iter().cloned().chain(parse_tags(content)) {
        let t = t.trim().trim_start_matches('#').to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDecomposition {
    pub parent_task: String,
    pub steps: Vec<String>,
}

/// SQLite-backed Wayfinder local personal knowledge vault & semantic index
pub struct WayfinderVault {
    conn: Arc<Mutex<Connection>>,
    pub db_path: PathBuf,
}

impl WayfinderVault {
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
    pub fn new(path: impl AsRef<Path>) -> Result<Self, NautikError> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| NautikError::Storage(e.to_string()))?;
        }
        let conn = Connection::open(&db_path).map_err(|e| NautikError::Storage(e.to_string()))?;

        // Initialize SQLite schema
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vault_items (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                tags_json TEXT NOT NULL,
                embedding_blob BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS task_decompositions (
                id TEXT PRIMARY KEY,
                parent_task TEXT NOT NULL,
                steps_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vault_title ON vault_items(title);",
        )
        .map_err(|e| NautikError::Storage(e.to_string()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    pub fn in_memory() -> Result<Self, NautikError> {
        let conn = Connection::open_in_memory().map_err(|e| NautikError::Storage(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vault_items (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                tags_json TEXT NOT NULL,
                embedding_blob BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS task_decompositions (
                id TEXT PRIMARY KEY,
                parent_task TEXT NOT NULL,
                steps_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )
        .map_err(|e| NautikError::Storage(e.to_string()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Insert or update an item into the vault with embedding
    pub async fn put_item(
        &self,
        id: &str,
        title: &str,
        content: &str,
        tags: &[String],
        embedding: Option<&[f32]>,
    ) -> Result<(), NautikError> {
        let conn = self.conn.lock().await;
        let tags = merge_tags(tags, content);
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now().to_rfc3339();

        let embedding_bytes = embedding.map(|vec| {
            let mut bytes = Vec::with_capacity(vec.len() * 4);
            for &val in vec {
                bytes.extend_from_slice(&val.to_le_bytes());
            }
            bytes
        });

        conn.execute(
            "INSERT OR REPLACE INTO vault_items (id, title, content, tags_json, embedding_blob, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, title, content, tags_json, embedding_bytes, now, now],
        )
        .map_err(|e| NautikError::Storage(e.to_string()))?;

        Ok(())
    }

    /// Keyword search over title, content and tags. Not semantic: meaning-based search is the
    /// agent's embeddings index (`nautik9-agent` `memory`), used by `GET /api/notes?q=` when an
    /// embedding endpoint is configured.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResultItem>, NautikError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, title, content, tags_json FROM vault_items")
            .map_err(|e| NautikError::Storage(e.to_string()))?;

        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored_results: Vec<(f32, SearchResultItem)> = Vec::new();

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let title: String = row.get(1)?;
                let content: String = row.get(2)?;
                let tags_json: String = row.get(3)?;
                Ok((id, title, content, tags_json))
            })
            .map_err(|e| NautikError::Storage(e.to_string()))?;

        for row in rows {
            let (id, title, content, tags_json) = row.map_err(|e| NautikError::Storage(e.to_string()))?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            let mut score = 0.0f32;

            // Keyword match (title, content, tags)
            let title_lower = title.to_lowercase();
            let content_lower = content.to_lowercase();
            for word in &query_words {
                if title_lower.contains(word) {
                    score += 0.4;
                }
                if content_lower.contains(word) {
                    score += 0.2;
                }
                for tag in &tags {
                    if tag.to_lowercase().contains(word) {
                        score += 0.5;
                    }
                }
            }

            if score > 0.05 {
                let snippet = if content.len() > 140 {
                    format!("{}...", &content[..140])
                } else {
                    content.clone()
                };

                scored_results.push((
                    score,
                    SearchResultItem {
                        id,
                        title,
                        snippet,
                        score,
                        tags,
                    },
                ));
            }
        }

        // Sort descending by score
        scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let results = scored_results.into_iter().take(top_k).map(|(_, item)| item).collect();
        Ok(results)
    }

    /// Store a task decomposition
    pub async fn save_task_decomposition(&self, id: &str, parent: &str, steps: &[String]) -> Result<(), NautikError> {
        let conn = self.conn.lock().await;
        let steps_json = serde_json::to_string(steps).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO task_decompositions (id, parent_task, steps_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, parent, steps_json, now],
        )
        .map_err(|e| NautikError::Storage(e.to_string()))?;

        Ok(())
    }

    /// List all notes/items in the vault
    pub async fn list_items(&self) -> Result<Vec<VaultItem>, NautikError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, title, content, tags_json, created_at FROM vault_items ORDER BY updated_at DESC")
            .map_err(|e| NautikError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let title: String = row.get(1)?;
                let content: String = row.get(2)?;
                let tags_json: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                Ok((id, title, content, tags_json, created_at))
            })
            .map_err(|e| NautikError::Storage(e.to_string()))?;

        let mut items = Vec::new();
        for r in rows {
            let (id, title, content, tags_json, created_at) = r.map_err(|e| NautikError::Storage(e.to_string()))?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
            // Notes saved before tag parsing existed still get their #tags.
            let tags = merge_tags(&tags, &content);
            let links = parse_links(&content);
            items.push(VaultItem {
                id,
                title,
                content,
                tags,
                created_at,
                links,
            });
        }
        Ok(items)
    }

    /// Notes whose `[[links]]` point at `id` (matched by id or, case-insensitively, by title).
    pub async fn backlinks(&self, id: &str) -> Result<Vec<VaultItem>, NautikError> {
        let items = self.list_items().await?;
        let Some(target) = items.iter().find(|i| i.id == id) else {
            return Ok(Vec::new());
        };
        let (tid, ttitle) = (target.id.clone(), target.title.clone());
        Ok(items
            .into_iter()
            .filter(|n| n.id != tid)
            .filter(|n| n.links.iter().any(|l| l == &tid || l.eq_ignore_ascii_case(&ttitle)))
            .collect())
    }

    /// Link targets in `id`'s content that match no note (id or title).
    pub async fn unresolved_links(&self, id: &str) -> Result<Vec<String>, NautikError> {
        let items = self.list_items().await?;
        let Some(note) = items.iter().find(|i| i.id == id) else {
            return Ok(Vec::new());
        };
        Ok(note
            .links
            .iter()
            .filter(|l| !items.iter().any(|i| &i.id == *l || i.title.eq_ignore_ascii_case(l)))
            .cloned()
            .collect())
    }

    /// Every tag with the number of notes using it, most used first.
    pub async fn list_tags(&self) -> Result<Vec<(String, usize)>, NautikError> {
        let items = self.list_items().await?;
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for n in &items {
            for t in &n.tags {
                *counts.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(v)
    }

    /// Notes carrying `tag` (a parent tag also matches nested tags: `project` finds `project/client`).
    pub async fn notes_with_tag(&self, tag: &str) -> Result<Vec<VaultItem>, NautikError> {
        let tag = tag.trim_start_matches('#').to_lowercase();
        let nested = format!("{}/", tag);
        Ok(self
            .list_items()
            .await?
            .into_iter()
            .filter(|n| n.tags.iter().any(|t| *t == tag || t.starts_with(&nested)))
            .collect())
    }

    /// Delete an item by ID
    pub async fn delete_item(&self, id: &str) -> Result<(), NautikError> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM vault_items WHERE id = ?1", params![id])
            .map_err(|e| NautikError::Storage(e.to_string()))?;
        Ok(())
    }
}

/// A FIXED 8-step checklist with the task name filled in. It does not read the task or call a model.
pub fn decompose_task(task_description: &str) -> Vec<String> {
    vec![
        format!("1. Define objective & constraints for '{}'", task_description),
        "2. Inspect available sensor rasters, logs, or local vault state".to_string(),
        "3. Check hardware tier & allocate local NPU / Lemonade cloud router".to_string(),
        "4. Run initial Wayfinder semantic index search to check existing records".to_string(),
        "5. Execute primary survey or code generation pass".to_string(),
        "6. Validate outputs against strict ground truth (no synthetic fallbacks)".to_string(),
        "7. Update Mack companion avatar status & post mission log to vault".to_string(),
        "8. Sync telemetry to the fleet endpoint, if one is configured".to_string(),
    ]
}

/// The Wayfinder NautiK9 Module
pub struct WayfinderModule {
    pub vault: WayfinderVault,
    pub embedding_dim: usize,
}

impl WayfinderModule {
    pub fn new(vault: WayfinderVault, embedding_dim: usize) -> Self {
        Self {
            vault,
            embedding_dim,
        }
    }

    pub fn in_memory(embedding_dim: usize) -> Result<Self, NautikError> {
        let vault = WayfinderVault::in_memory()?;
        Ok(Self {
            vault,
            embedding_dim,
        })
    }
}

#[async_trait]
impl NautikModule for WayfinderModule {
    fn id(&self) -> &str {
        "wayfinder"
    }

    fn name(&self) -> &str {
        "Wayfinder Semantic Organizer & Vault"
    }

    async fn init(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError> {
        self.embedding_dim = ctx.hardware.recommended_embedding_dim;
        tracing::info!(
            "Wayfinder module initialized with embedding dimension {} for {}",
            self.embedding_dim,
            ctx.hardware.device_name
        );
        Ok(())
    }

    async fn handle_event(
        &mut self,
        event: &NautikEvent,
        ctx: &mut ModuleContext,
    ) -> Result<Option<NautikEvent>, NautikError> {
        match &event.payload {
            EventPayload::WayfinderSearchRequest { query, top_k } => {
                // 1. Transition avatar to Sniffing
                {
                    let mut avatar = ctx.avatar.write().await;
                    avatar.transition_to(
                        MackState::Sniffing,
                        format!("Mack is sniffing the Wayfinder index for '{}'", query),
                    );
                }

                // 2. Keyword search
                let results = self.vault.search(query, *top_k).await?;

                // 3. If hit found, alert Mack
                {
                    let mut avatar = ctx.avatar.write().await;
                    if !results.is_empty() {
                        avatar.transition_to(
                            MackState::TargetLocked,
                            format!("Target match found for '{}' ({} results)", query, results.len()),
                        );
                    } else {
                        avatar.transition_to(
                            MackState::Idle,
                            format!("Search completed for '{}', no targets found", query),
                        );
                    }
                }

                Ok(Some(NautikEvent::new(
                    "wayfinder",
                    EventPayload::WayfinderSearchResult {
                        query: query.clone(),
                        results,
                    },
                )))
            }
            EventPayload::WayfinderItemIndexed { item_id, title, tags } => {
                tracing::info!("Item indexed in Wayfinder: {} ({}) with tags {:?}", title, item_id, tags);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn tick(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
        // Periodic index maintenance / compaction
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NautikError> {
        tracing::info!("Wayfinder module shut down cleanly");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nautik9_core::HardwareProfile;

    #[tokio::test]
    async fn test_wayfinder_vault_keyword_search() {
        let vault = WayfinderVault::in_memory().unwrap();

        vault
            .put_item(
                "item-1",
                "Lake Huron Survey Note",
                "Sonar return shows magnetic anomaly near flag buoy #4. Mack spotted bubbles.",
                &["sonar".to_string(), "survey".to_string(), "shipwreck".to_string()],
                None,
            )
            .await
            .unwrap();

        vault
            .put_item(
                "item-2",
                "Kitchen Supplies",
                "Buy 2% milk and fresh bread from grocery.",
                &["shopping".to_string()],
                None,
            )
            .await
            .unwrap();

        let results = vault.search("sonar anomaly", 5).await.unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].id, "item-1");
        assert!(results[0].score > 0.5);
    }

    #[tokio::test]
    async fn test_task_decomposition() {
        let steps = decompose_task("Survey Thunder Bay Shipwreck Corridor");
        assert_eq!(steps.len(), 8);
        assert!(steps[0].contains("Survey Thunder Bay"));
    }

    #[tokio::test]
    async fn test_wayfinder_module_lifecycle_and_avatar_reaction() {
        let mut module = WayfinderModule::in_memory(768).unwrap();
        let hardware = HardwareProfile::samsung_s25_ultra();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let mut ctx = ModuleContext::new(hardware, tx);

        module.init(&mut ctx).await.unwrap();

        // Index an item
        module
            .vault
            .put_item(
                "target-01",
                "First NautiK9 Boat",
                "Photo of Mack from front of phone and survey vessel with flag.",
                &["mack".to_string(), "nautik9".to_string()],
                None,
            )
            .await
            .unwrap();

        // Send search request event
        let search_event = NautikEvent::new(
            "user",
            EventPayload::WayfinderSearchRequest {
                query: "Mack flag survey boat".to_string(),
                top_k: 3,
            },
        );

        let response = module.handle_event(&search_event, &mut ctx).await.unwrap();
        assert!(response.is_some());

        // Check avatar transitioned to TargetLocked
        let avatar = ctx.avatar.read().await;
        assert_eq!(avatar.current_state, MackState::TargetLocked);
        assert!(avatar.status_message.contains("Target match found"));
    }

    #[test]
    fn parses_links_aliases_and_ignores_code() {
        let c = "See [[Alpha]] and [[Beta|the beta]] and [[Alpha#Intro]].\n`[[InCode]]`\n```\n[[Fenced]]\n```\n[[ ]] [[broken";
        assert_eq!(parse_links(c), vec!["Alpha", "Beta"]);
    }

    #[test]
    fn image_embeds_are_not_note_links_but_note_embeds_are() {
        let c = "![[photo.PNG|300]] ![[Other note]] [[report.pdf]] [[v1.2 plan]]";
        assert_eq!(parse_links(c), vec!["Other note", "v1.2 plan"]);
    }

    #[test]
    fn parses_tags_not_headings_urls_or_code() {
        let c = "# Heading\nwork #Project/client and (#sonar) but http://x.org/#anchor and `#code` #1 #-x\n#end-";
        assert_eq!(parse_tags(c), vec!["project/client", "sonar", "end"]);
    }

    #[tokio::test]
    async fn tags_are_added_on_save_and_listed() {
        let v = WayfinderVault::in_memory().unwrap();
        v.put_item("a", "Alpha", "text #sonar #project/x", &["Manual".to_string()], None).await.unwrap();
        v.put_item("b", "Beta", "more #sonar", &[], None).await.unwrap();
        let tags = v.list_tags().await.unwrap();
        assert_eq!(tags[0], ("sonar".to_string(), 2));
        assert!(tags.contains(&("manual".to_string(), 1)));
        let by = v.notes_with_tag("project").await.unwrap();
        assert_eq!(by.len(), 1);
        assert_eq!(by[0].id, "a");
        assert!(by[0].tags.contains(&"sonar".to_string()));
    }

    #[tokio::test]
    async fn backlinks_and_unresolved_links() {
        let v = WayfinderVault::in_memory().unwrap();
        v.put_item("a", "Alpha", "links to [[beta]] and [[Missing]]", &[], None).await.unwrap();
        v.put_item("b", "Beta", "links back [[Alpha|a]]", &[], None).await.unwrap();
        v.put_item("c", "Gamma", "by id [[b]]", &[], None).await.unwrap();
        let back_b: Vec<String> = v.backlinks("b").await.unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(back_b.len(), 2);
        assert!(back_b.contains(&"a".to_string()) && back_b.contains(&"c".to_string()));
        let back_a: Vec<String> = v.backlinks("a").await.unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(back_a, vec!["b".to_string()]);
        assert_eq!(v.unresolved_links("a").await.unwrap(), vec!["Missing".to_string()]);
        assert!(v.backlinks("nope").await.unwrap().is_empty());
    }
}
