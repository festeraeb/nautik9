//! Chat sessions (Jade's chats and `chat_history`) and the trajectory log.
//!
//! Sessions live in their own SQLite file next to the vault: `sessions(id, title, created_at,
//! updated_at)` and `messages(session_id, idx, json, created_at)`, where `json` is the OpenAI
//! message exactly as it went to or came from the model (role, content, tool_calls,
//! tool_call_id), plus `rating` for replies (+1 / -1). The trajectory log appends one JSON line
//! per model call (`trajectories.jsonl`): the messages and tools the model saw and what it
//! answered, so a run can be exported for fine-tuning (Jade's traj.js intent).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub n_messages: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoredMessage {
    pub idx: usize,
    pub message: serde_json::Value,
    pub created_at: String,
    pub rating: Option<i64>,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    traj_path: Option<PathBuf>,
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

impl Store {
    /// Open (or create) the session database; `traj_path` enables the trajectory log.
    pub fn open(db: &Path, traj_path: Option<PathBuf>) -> Result<Self, String> {
        if let Some(p) = db.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let conn = Connection::open(db).map_err(|e| format!("{}: {e}", db.display()))?;
        Self::init(conn, traj_path)
    }

    pub fn in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?, None)
    }

    fn init(conn: Connection, traj_path: Option<PathBuf>) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (session_id TEXT NOT NULL, idx INTEGER NOT NULL, json TEXT NOT NULL, created_at TEXT NOT NULL,
                 rating INTEGER, PRIMARY KEY (session_id, idx));",
        )
        .map_err(|e| e.to_string())?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)), traj_path })
    }

    pub async fn create_session(&self, title: &str) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.conn
            .lock()
            .await
            .execute("INSERT INTO sessions VALUES (?1, ?2, ?3, ?3)", params![id, title, t])
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub async fn exists(&self, id: &str) -> Result<bool, String> {
        let c = self.conn.lock().await;
        c.query_row("SELECT 1 FROM sessions WHERE id = ?1", params![id], |_| Ok(()))
            .optional()
            .map(|r| r.is_some())
            .map_err(|e| e.to_string())
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, String> {
        let c = self.conn.lock().await;
        let mut st = c
            .prepare(
                "SELECT s.id, s.title, s.created_at, s.updated_at, (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
                 FROM sessions s ORDER BY s.updated_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok(SessionInfo { id: r.get(0)?, title: r.get(1)?, created_at: r.get(2)?, updated_at: r.get(3)?, n_messages: r.get::<_, i64>(4)? as usize })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub async fn messages(&self, session: &str) -> Result<Vec<StoredMessage>, String> {
        let c = self.conn.lock().await;
        let mut st = c
            .prepare("SELECT idx, json, created_at, rating FROM messages WHERE session_id = ?1 ORDER BY idx")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![session], |r| {
                let json: String = r.get(1)?;
                Ok((r.get::<_, i64>(0)? as usize, json, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let (idx, json, created_at, rating) = row.map_err(|e| e.to_string())?;
            let message = serde_json::from_str(&json).map_err(|e| format!("message {idx}: {e}"))?;
            out.push(StoredMessage { idx, message, created_at, rating });
        }
        Ok(out)
    }

    /// Append messages in order; returns the index of the first one.
    pub async fn append(&self, session: &str, msgs: &[serde_json::Value]) -> Result<usize, String> {
        let c = self.conn.lock().await;
        let next: i64 = c
            .query_row("SELECT COALESCE(MAX(idx) + 1, 0) FROM messages WHERE session_id = ?1", params![session], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let t = now();
        for (k, m) in msgs.iter().enumerate() {
            c.execute(
                "INSERT INTO messages (session_id, idx, json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![session, next + k as i64, m.to_string(), t],
            )
            .map_err(|e| e.to_string())?;
        }
        c.execute("UPDATE sessions SET updated_at = ?2 WHERE id = ?1", params![session, t]).map_err(|e| e.to_string())?;
        Ok(next as usize)
    }

    /// Drop messages from `idx` on (Retry of the latest reply).
    pub async fn truncate(&self, session: &str, idx: usize) -> Result<(), String> {
        self.conn
            .lock()
            .await
            .execute("DELETE FROM messages WHERE session_id = ?1 AND idx >= ?2", params![session, idx as i64])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// 👍 (+1) / 👎 (-1) / clear (None) on one stored message.
    pub async fn rate(&self, session: &str, idx: usize, rating: Option<i64>) -> Result<bool, String> {
        self.conn
            .lock()
            .await
            .execute("UPDATE messages SET rating = ?3 WHERE session_id = ?1 AND idx = ?2", params![session, idx as i64, rating])
            .map(|n| n == 1)
            .map_err(|e| e.to_string())
    }

    pub async fn rename(&self, session: &str, title: &str) -> Result<(), String> {
        self.conn
            .lock()
            .await
            .execute("UPDATE sessions SET title = ?2 WHERE id = ?1", params![session, title])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn delete_session(&self, session: &str) -> Result<bool, String> {
        let c = self.conn.lock().await;
        c.execute("DELETE FROM messages WHERE session_id = ?1", params![session]).map_err(|e| e.to_string())?;
        c.execute("DELETE FROM sessions WHERE id = ?1", params![session]).map(|n| n == 1).map_err(|e| e.to_string())
    }

    /// Keyword search over every stored message's text (`chat_history search`): every word must
    /// appear (case-insensitive). Returns (session id, session title, idx, role, snippet).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return Ok(vec![]);
        }
        let c = self.conn.lock().await;
        let mut st = c
            .prepare("SELECT m.session_id, s.title, m.idx, m.json FROM messages m JOIN sessions s ON s.id = m.session_id ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let (sid, title, idx, json) = row.map_err(|e| e.to_string())?;
            let m: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
            let text = m["content"].as_str().unwrap_or("");
            let low = text.to_lowercase();
            if words.iter().all(|w| low.contains(w.as_str())) {
                let pos = low.find(words[0].as_str()).unwrap_or(0);
                let start = text.char_indices().map(|(i, _)| i).filter(|&i| i <= pos.saturating_sub(80)).last().unwrap_or(0);
                let snippet: String = text[start..].chars().take(240).collect();
                out.push(serde_json::json!({"session": sid, "title": title, "idx": idx, "role": m["role"], "snippet": snippet}));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Append one model call to the trajectory log (no-op when disabled).
    pub fn log_trajectory(&self, record: &serde_json::Value) {
        if let Some(p) = &self.traj_path {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{record}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sessions_roundtrip_rate_truncate_search() {
        let s = Store::in_memory().unwrap();
        let id = s.create_session("Lisbon trip").await.unwrap();
        assert!(s.exists(&id).await.unwrap());
        s.append(&id, &[serde_json::json!({"role":"user","content":"Plan Lisbon in May"}), serde_json::json!({"role":"assistant","content":"Sure: trams and pasteis."})])
            .await
            .unwrap();
        let first = s.append(&id, &[serde_json::json!({"role":"user","content":"and Porto?"})]).await.unwrap();
        assert_eq!(first, 2);
        assert!(s.rate(&id, 1, Some(1)).await.unwrap());
        let m = s.messages(&id).await.unwrap();
        assert_eq!((m.len(), m[1].rating), (3, Some(1)));
        assert_eq!(s.search("lisbon may", 5).await.unwrap().len(), 1);
        s.truncate(&id, 2).await.unwrap();
        assert_eq!(s.messages(&id).await.unwrap().len(), 2);
        assert_eq!(s.list_sessions().await.unwrap()[0].n_messages, 2);
        assert!(s.delete_session(&id).await.unwrap());
        assert!(!s.exists(&id).await.unwrap());
    }
}
