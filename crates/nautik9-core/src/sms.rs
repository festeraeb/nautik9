use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmsStatus {
    Pending,
    Drafted,
    Approved,
    /// The reply was put into the phone's SMS composer. Nothing was sent:
    /// the user still has to press send on the phone.
    OpenedInComposer,
    /// No model endpoint answered, so there is no draft.
    NoModel,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsMessage {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub timestamp: String,
    pub draft_reply: Option<String>,
    /// Why there is no draft (set with status NoModel).
    #[serde(default)]
    pub draft_error: Option<String>,
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
    /// Name used in prompts and replies.
    #[serde(default = "default_owner_name")]
    pub owner_name: String,
    /// Phone numbers that count as the owner (digits compared, last 10).
    /// Texts from anyone else never get vault notes.
    #[serde(default)]
    pub owner_numbers: Vec<String>,
}

fn default_owner_name() -> String {
    "Thom".to_string()
}

fn digits_tail(s: &str) -> String {
    let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let skip = d.len().saturating_sub(10);
    d[skip..].to_string()
}

impl WorkModeConfig {
    /// True when `sender` is one of the owner's own numbers.
    pub fn is_owner(&self, sender: &str) -> bool {
        let s = digits_tail(sender);
        s.len() >= 7 && self.owner_numbers.iter().any(|n| digits_tail(n) == s)
    }

    /// Load `workmode.json` over the defaults. Missing file = defaults.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{}: {}", path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {}", path.display(), e)),
        }
    }
}

/// Message shown after the composer was opened. It was not sent.
pub fn composer_opened_message(recipient: &str) -> String {
    format!("Opened in the phone's SMS composer for {} (not sent: press send on the phone)", recipient)
}

impl Default for WorkModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_reply: false, // Default to Safe Review Mode (review each draft before it is used)
            preset: "Survey Helm".to_string(),
            custom_instructions: "Be concise, polite and professional, and acknowledge the inquiry.".to_string(),
            model_target: "strongest_fleet".to_string(),
            owner_name: default_owner_name(),
            owner_numbers: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_status_is_named_honestly() {
        assert_eq!(serde_json::to_string(&SmsStatus::OpenedInComposer).unwrap(), "\"OpenedInComposer\"");
        assert_eq!(serde_json::to_string(&SmsStatus::NoModel).unwrap(), "\"NoModel\"");
        // the old name that claimed a send is gone
        assert!(serde_json::from_str::<SmsStatus>("\"AutoSent\"").is_err());
        let m = composer_opened_message("+15550001");
        assert!(m.contains("not sent") && !m.contains("Dispatched"));
    }

    #[test]
    fn defaults_are_safe_and_neutral() {
        let c = WorkModeConfig::default();
        assert!(!c.auto_reply);
        assert_eq!(c.owner_name, "Thom");
        let l = format!(" {} ", c.custom_instructions.to_lowercase());
        assert!(!l.contains(" he ") && !l.contains(" his "));
    }

    #[test]
    fn owner_matching_uses_digits() {
        let mut c = WorkModeConfig::default();
        assert!(!c.is_owner("+1 (555) 000-1234"));
        c.owner_numbers = vec!["555-000-1234".into()];
        assert!(c.is_owner("+15550001234"));
        assert!(!c.is_owner("+15550009999"));
        assert!(!c.is_owner("123"));
    }

    #[test]
    fn workmode_config_loads_with_defaults_for_missing_fields() {
        let d = std::env::temp_dir().join(format!("n9wm-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("workmode.json");
        std::fs::write(&p, r#"{"enabled":true,"auto_reply":false,"preset":"p","custom_instructions":"x","model_target":"t","owner_name":"Sam"}"#).unwrap();
        let c = WorkModeConfig::load(&p).unwrap();
        assert_eq!(c.owner_name, "Sam");
        assert!(c.owner_numbers.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }
}
