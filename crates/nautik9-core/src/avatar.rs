use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MackState {
    /// Relaxing on the deck of the NautiK9 survey boat by the helm
    Idle,
    /// Alert, ears up, listening to user or preparing mission
    Attentive,
    /// Actively computing Wayfinder embeddings, searching vault or sonar records
    Sniffing,
    /// Target hit or anomaly identified! Flag waves fast, Mack alerts
    TargetLocked,
    /// Survey boat underway, GPS waypoint tracking active, flag fluttering
    SurveyNavigating,
    /// Low-power background standby mode
    Resting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarStatus {
    pub current_state: MackState,
    pub state_since: DateTime<Utc>,
    pub previous_state: MackState,
    pub status_message: String,
    pub flag_waving: bool,
    pub vessel_name: String,
    pub animation_frame_hint: String,
}

impl Default for AvatarStatus {
    fn default() -> Self {
        Self {
            current_state: MackState::Idle,
            state_since: Utc::now(),
            previous_state: MackState::Idle,
            status_message: "Mack is keeping watch aboard NautiK9".to_string(),
            flag_waving: true,
            vessel_name: "NautiK9 Survey Vessel #1".to_string(),
            animation_frame_hint: "mack_helm_idle".to_string(),
        }
    }
}

impl AvatarStatus {
    pub fn transition_to(&mut self, new_state: MackState, message: impl Into<String>) {
        self.previous_state = self.current_state;
        self.current_state = new_state;
        self.state_since = Utc::now();
        self.status_message = message.into();
        self.animation_frame_hint = match new_state {
            MackState::Idle => "mack_helm_idle".to_string(),
            MackState::Attentive => "mack_ears_perked".to_string(),
            MackState::Sniffing => "mack_sniffing_wayfinder".to_string(),
            MackState::TargetLocked => "mack_alert_target_hit".to_string(),
            MackState::SurveyNavigating => "nautik9_boat_underway".to_string(),
            MackState::Resting => "mack_resting_deck".to_string(),
        };
    }
}
