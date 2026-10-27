pub mod avatar;
pub mod error;
pub mod event;
pub mod hardware;
pub mod module;

pub use avatar::{AvatarStatus, MackState};
pub use error::NautikError;
pub use event::{EventPayload, NautikEvent, SearchResultItem};
pub use hardware::{DeviceTier, HardwareProfile};
pub use module::{ModuleContext, ModuleRegistry, NautikModule};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct DummyModule {
        tick_count: usize,
    }

    #[async_trait]
    impl NautikModule for DummyModule {
        fn id(&self) -> &str {
            "dummy"
        }
        fn name(&self) -> &str {
            "Dummy Test Module"
        }
        async fn init(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
            Ok(())
        }
        async fn handle_event(
            &mut self,
            event: &NautikEvent,
            _ctx: &mut ModuleContext,
        ) -> Result<Option<NautikEvent>, NautikError> {
            if let EventPayload::Custom { topic, .. } = &event.payload {
                if topic == "ping" {
                    return Ok(Some(NautikEvent::new(
                        "dummy",
                        EventPayload::Custom {
                            topic: "pong".to_string(),
                            data: serde_json::json!({"status": "ok"}),
                        },
                    )));
                }
            }
            Ok(None)
        }
        async fn tick(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
            self.tick_count += 1;
            Ok(())
        }
        async fn shutdown(&mut self) -> Result<(), NautikError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_module_registry_and_s25_ultra_profiling() {
        let profile = HardwareProfile::samsung_s25_ultra();
        assert_eq!(profile.tier, DeviceTier::FlagshipS25Ultra);
        assert!(profile.can_run_flagship_llm());
        assert!(profile.has_armv9_sve2);
        assert!(profile.has_hexagon_npu);
        assert_eq!(profile.recommended_embedding_dim, 768);

        let mut registry = ModuleRegistry::new(profile);
        let dummy = Box::new(DummyModule { tick_count: 0 });
        registry.register(dummy).await.unwrap();

        assert!(registry.has_module("dummy"));
        registry.tick_all().await.unwrap();

        // Dispatch ping event
        let ping_event = NautikEvent::new(
            "test",
            EventPayload::Custom {
                topic: "ping".to_string(),
                data: serde_json::json!({}),
            },
        );
        registry.dispatch_event(ping_event).await.unwrap();

        let processed = registry.process_pending_events().await.unwrap();
        assert_eq!(processed, 1); // received the pong follow-up event
    }

    #[tokio::test]
    async fn test_avatar_transitions() {
        let mut avatar = AvatarStatus::default();
        assert_eq!(avatar.current_state, MackState::Idle);

        avatar.transition_to(MackState::Sniffing, "Searching Wayfinder index");
        assert_eq!(avatar.current_state, MackState::Sniffing);
        assert_eq!(avatar.previous_state, MackState::Idle);
        assert_eq!(avatar.animation_frame_hint, "mack_sniffing_wayfinder");

        avatar.transition_to(MackState::TargetLocked, "Target coordinates verified!");
        assert_eq!(avatar.current_state, MackState::TargetLocked);
        assert_eq!(avatar.previous_state, MackState::Sniffing);
    }
}
