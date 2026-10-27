use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::avatar::AvatarStatus;
use crate::error::NautikError;
use crate::event::NautikEvent;
use crate::hardware::HardwareProfile;

/// Shared runtime context passed to modules during initialization, event dispatch, and ticks.
#[derive(Clone)]
pub struct ModuleContext {
    pub hardware: Arc<HardwareProfile>,
    pub avatar: Arc<RwLock<AvatarStatus>>,
    pub event_tx: mpsc::Sender<NautikEvent>,
    pub shared_state: Arc<RwLock<HashMap<String, serde_json::Value>>>,
}

impl ModuleContext {
    pub fn new(hardware: HardwareProfile, event_tx: mpsc::Sender<NautikEvent>) -> Self {
        Self {
            hardware: Arc::new(hardware),
            avatar: Arc::new(RwLock::new(AvatarStatus::default())),
            event_tx,
            shared_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Read shared state key
    pub async fn get_state(&self, key: &str) -> Option<serde_json::Value> {
        let read = self.shared_state.read().await;
        read.get(key).cloned()
    }

    /// Write shared state key
    pub async fn set_state(&self, key: impl Into<String>, value: serde_json::Value) {
        let mut write = self.shared_state.write().await;
        write.insert(key.into(), value);
    }

    /// Send an event into the bus
    pub async fn emit_event(&self, event: NautikEvent) -> Result<(), NautikError> {
        self.event_tx
            .send(event)
            .await
            .map_err(|e| NautikError::Module {
                module: "EventBus".to_string(),
                message: format!("Failed to emit event: {}", e),
            })
    }
}

/// The universal trait that all NautiK9 modules implement (Wayfinder, Lemonade, Fleet, Custom user modules).
#[async_trait]
pub trait NautikModule: Send + Sync {
    /// Unique identifier for the module (e.g. "wayfinder", "lemonade", "fleet", "survey")
    fn id(&self) -> &str;

    /// Human-friendly display name
    fn name(&self) -> &str;

    /// Lifecycle hook called when the module is registered and started
    async fn init(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError>;

    /// Event handler for reactive cross-module message passing
    async fn handle_event(
        &mut self,
        event: &NautikEvent,
        ctx: &mut ModuleContext,
    ) -> Result<Option<NautikEvent>, NautikError>;

    /// Periodic background work tick
    async fn tick(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError>;

    /// Clean shutdown hook
    async fn shutdown(&mut self) -> Result<(), NautikError>;
}

/// Universal Module Registry managing active modules, event bus loops, and lifecycle
pub struct ModuleRegistry {
    modules: HashMap<String, Box<dyn NautikModule>>,
    pub context: ModuleContext,
    event_rx: mpsc::Receiver<NautikEvent>,
}

impl ModuleRegistry {
    pub fn new(hardware: HardwareProfile) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let context = ModuleContext::new(hardware, tx);
        Self {
            modules: HashMap::new(),
            context,
            event_rx: rx,
        }
    }

    /// Register a module
    pub async fn register(&mut self, mut module: Box<dyn NautikModule>) -> Result<(), NautikError> {
        let id = module.id().to_string();
        module.init(&mut self.context).await?;
        self.modules.insert(id, module);
        Ok(())
    }

    /// Check if a module is registered
    pub fn has_module(&self, id: &str) -> bool {
        self.modules.contains_key(id)
    }

    /// List registered module IDs
    pub fn module_ids(&self) -> Vec<String> {
        self.modules.keys().cloned().collect()
    }

    /// Dispatch an event to all registered modules and route any response events
    pub async fn dispatch_event(&mut self, event: NautikEvent) -> Result<(), NautikError> {
        let mut queued_responses = Vec::new();

        for (id, module) in self.modules.iter_mut() {
            // Avoid looping back to the sender if needed, or allow all to listen
            match module.handle_event(&event, &mut self.context).await {
                Ok(Some(response_event)) => queued_responses.push(response_event),
                Ok(None) => {}
                Err(err) => {
                    tracing::error!("Error in module [{}] handling event: {:?}", id, err);
                }
            }
        }

        // Emit any follow-up events produced by modules
        for follow_up in queued_responses {
            self.context.emit_event(follow_up).await?;
        }

        Ok(())
    }

    /// Process all pending events in the channel
    pub async fn process_pending_events(&mut self) -> Result<usize, NautikError> {
        let mut processed = 0;
        while let Ok(event) = self.event_rx.try_recv() {
            self.dispatch_event(event).await?;
            processed += 1;
        }
        Ok(processed)
    }

    /// Tick all registered modules
    pub async fn tick_all(&mut self) -> Result<(), NautikError> {
        for (id, module) in self.modules.iter_mut() {
            if let Err(err) = module.tick(&mut self.context).await {
                tracing::warn!("Module [{}] tick warning: {:?}", id, err);
            }
        }
        Ok(())
    }

    /// Shutdown all modules gracefully
    pub async fn shutdown_all(&mut self) -> Result<(), NautikError> {
        for (id, module) in self.modules.iter_mut() {
            if let Err(err) = module.shutdown().await {
                tracing::error!("Module [{}] shutdown error: {:?}", id, err);
            }
        }
        self.modules.clear();
        Ok(())
    }
}
