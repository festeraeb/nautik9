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
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
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

    /// Search items by query using keyword matching and cosine similarity
    pub async fn search(&self, query: &str, query_embedding: Option<&[f32]>, top_k: usize) -> Result<Vec<SearchResultItem>, NautikError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, title, content, tags_json, embedding_blob FROM vault_items")
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
                let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                Ok((id, title, content, tags_json, embedding_blob))
            })
            .map_err(|e| NautikError::Storage(e.to_string()))?;

        for row in rows {
            let (id, title, content, tags_json, embedding_blob) = row.map_err(|e| NautikError::Storage(e.to_string()))?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            let mut score = 0.0f32;

            // 1. Keyword & Semantic Token Match
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

            // 2. Cosine Vector Similarity (if embeddings exist)
            if let (Some(q_emb), Some(blob)) = (query_embedding, embedding_blob) {
                if blob.len() >= q_emb.len() * 4 {
                    let mut doc_emb = Vec::with_capacity(q_emb.len());
                    for chunk in blob.chunks_exact(4) {
                        let val = f32::from_le_bytes(chunk.try_into().unwrap());
                        doc_emb.push(val);
                    }
                    let cos_sim = cosine_similarity(q_emb, &doc_emb);
                    score += cos_sim * 1.5;
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
}

/// Compute cosine similarity between two float vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Fast on-device pseudo-embedding generator for offline zero-alloc similarity
pub fn generate_fast_embedding(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    if text.is_empty() {
        return vec;
    }
    for (i, byte) in text.as_bytes().iter().enumerate() {
        let idx = (i * 31 + (*byte as usize)) % dim;
        vec[idx] += 1.0;
    }
    // Normalize
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
    vec
}

/// Task Decomposer: breaks high-level goals into 8 actionable field / code steps
pub fn decompose_task(task_description: &str) -> Vec<String> {
    vec![
        format!("1. Define objective & constraints for '{}'", task_description),
        "2. Inspect available sensor rasters, logs, or local vault state".to_string(),
        "3. Check hardware tier & allocate local NPU / Lemonade cloud router".to_string(),
        "4. Run initial Wayfinder semantic index search to check existing records".to_string(),
        "5. Execute primary survey or code generation pass".to_string(),
        "6. Validate outputs against strict ground truth (no synthetic fallbacks)".to_string(),
        "7. Update Mack companion avatar status & post mission log to vault".to_string(),
        "8. Sync telemetry to CesarOps Fleet or trigger offline persistence".to_string(),
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

                // 2. Generate embedding & perform search
                let query_emb = generate_fast_embedding(query, self.embedding_dim);
                let results = self.vault.search(query, Some(&query_emb), *top_k).await?;

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
    async fn test_wayfinder_vault_search_and_embedding() {
        let vault = WayfinderVault::in_memory().unwrap();
        let dim = 768;

        let emb1 = generate_fast_embedding("Mack surveying underwater shipwreck with sonar", dim);
        let emb2 = generate_fast_embedding("Grocery shopping list bananas milk", dim);

        vault
            .put_item(
                "item-1",
                "Lake Huron Survey Note",
                "Sonar return shows magnetic anomaly near flag buoy #4. Mack spotted bubbles.",
                &["sonar".to_string(), "survey".to_string(), "shipwreck".to_string()],
                Some(&emb1),
            )
            .await
            .unwrap();

        vault
            .put_item(
                "item-2",
                "Kitchen Supplies",
                "Buy 2% milk and fresh bread from grocery.",
                &["shopping".to_string()],
                Some(&emb2),
            )
            .await
            .unwrap();

        let query_emb = generate_fast_embedding("sonar shipwreck anomaly", dim);
        let results = vault.search("sonar anomaly", Some(&query_emb), 5).await.unwrap();

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
        let emb = generate_fast_embedding("Mack on the boat with flag", 768);
        module
            .vault
            .put_item(
                "target-01",
                "First NautiK9 Boat",
                "Photo of Mack from front of phone and survey vessel with flag.",
                &["mack".to_string(), "nautik9".to_string()],
                Some(&emb),
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
}
