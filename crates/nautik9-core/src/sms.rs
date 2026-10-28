use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmsStatus {
    Pending,
    Drafted,
    Approved,
    AutoSent,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsMessage {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub timestamp: String,
    pub draft_reply: Option<String>,
    pub status: SmsStatus,
    pub reply_model: Option<String>,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkModeConfig {
    pub enabled: bool,
    pub auto_reply: bool,
    pub preset: String,
    pub custom_instructions: String,
    pub model_target: String,
}

impl Default for WorkModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_reply: false, // Default to Safe Review Mode (Lucky's Ask before risky actions)
            preset: "Survey Helm".to_string(),
            custom_instructions: "You are Mack AI answering texts for Cesar while he is on active field operations. Be concise, polite, professional, and acknowledge the inquiry.".to_string(),
            model_target: "strongest_fleet".to_string(),
        }
    }
}
