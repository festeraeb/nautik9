use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NautikEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source_module: String,
    pub payload: EventPayload,
}

impl NautikEvent {
    pub fn new(source: impl Into<String>, payload: EventPayload) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source_module: source.into(),
            payload,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventPayload {
    /// Wayfinder search request
    WayfinderSearchRequest {
        query: String,
        top_k: usize,
    },
    /// Wayfinder search result
    WayfinderSearchResult {
        query: String,
        results: Vec<SearchResultItem>,
    },
    /// Wayfinder new note or item indexed
    WayfinderItemIndexed {
        item_id: String,
        title: String,
        tags: Vec<String>,
    },
    /// Lemonade LLM generation request
    LemonadeChatRequest {
        prompt: String,
        system_prompt: Option<String>,
        prefer_offline: bool,
    },
    /// Lemonade stream token or delta
    LemonadeChatDelta {
        delta: String,
        is_finished: bool,
    },
    /// Mack avatar state change
    AvatarStateChanged {
        previous_state: String,
        new_state: String,
        reason: String,
    },
    /// Hardware environment updated or detected
    HardwareProfileReady {
        tier_name: String,
        recommended_model: String,
    },
    /// Fleet telemetry or task update
    FleetTaskUpdate {
        task_id: String,
        status: String,
        summary: String,
    },
    /// Generic message for custom user modules
    Custom {
        topic: String,
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
    pub tags: Vec<String>,
}
