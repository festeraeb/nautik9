use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    module::{ModuleContext, NautikModule},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetConfig {
    pub buzz_board_endpoint: String,
    pub telemetry_endpoint: String,
    pub fleet_node_id: String,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            buzz_board_endpoint: "http://127.0.0.1:5560".to_string(),
            telemetry_endpoint: "http://127.0.0.1:8090/telemetry".to_string(),
            fleet_node_id: "nautik9-mobile-s25u".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileTelemetryPacket {
    pub node_id: String,
    pub timestamp: String,
    pub device_tier: String,
    pub avatar_state: String,
    pub vessel_heading_deg: Option<f32>,
    pub battery_percent: Option<u8>,
}

pub struct FleetModule {
    pub config: FleetConfig,
    client: reqwest::Client,
}

impl FleetModule {
    pub fn new(config: FleetConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Construct a telemetry packet from current module context
    pub async fn create_telemetry_packet(&self, ctx: &ModuleContext) -> MobileTelemetryPacket {
        let avatar_state = {
            let read = ctx.avatar.read().await;
            format!("{:?}", read.current_state)
        };

        MobileTelemetryPacket {
            node_id: self.config.fleet_node_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            device_tier: format!("{:?}", ctx.hardware.tier),
            avatar_state,
            vessel_heading_deg: Some(342.5), // Survey heading
            battery_percent: Some(94),
        }
    }
}

#[async_trait]
impl NautikModule for FleetModule {
    fn id(&self) -> &str {
        "fleet"
    }

    fn name(&self) -> &str {
        "Fleet & Telemetry Bridge"
    }

    async fn init(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError> {
        tracing::info!(
            "Fleet module initialized for node '{}' (Hardware: {})",
            self.config.fleet_node_id,
            ctx.hardware.device_name
        );
        Ok(())
    }

    async fn handle_event(
        &mut self,
        event: &NautikEvent,
        _ctx: &mut ModuleContext,
    ) -> Result<Option<NautikEvent>, NautikError> {
        match &event.payload {
            EventPayload::FleetTaskUpdate { task_id, status, summary } => {
                tracing::info!("Fleet task update received: [{}] {} - {}", task_id, status, summary);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn tick(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError> {
        let packet = self.create_telemetry_packet(ctx).await;
        // Non-blocking telemetry broadcast attempt
        let _ = self.client.post(&self.config.telemetry_endpoint).json(&packet).send().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NautikError> {
        tracing::info!("Fleet module shut down cleanly");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nautik9_core::HardwareProfile;

    #[tokio::test]
    async fn test_fleet_telemetry_packet_generation() {
        let config = FleetConfig::default();
        let module = FleetModule::new(config);
        let hardware = HardwareProfile::samsung_s25_ultra();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let ctx = ModuleContext::new(hardware, tx);

        let packet = module.create_telemetry_packet(&ctx).await;
        assert_eq!(packet.node_id, "nautik9-mobile-s25u");
        assert_eq!(packet.device_tier, "FlagshipS25Ultra");
        assert_eq!(packet.avatar_state, "Idle");
    }
}
