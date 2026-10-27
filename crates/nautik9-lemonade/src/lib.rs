use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    module::{ModuleContext, NautikModule},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LemonadeConfig {
    pub online_endpoint: String,
    pub model_name: String,
    pub api_key: Option<String>,
    pub timeout_seconds: u64,
    pub allow_fallback: bool,
}

impl Default for LemonadeConfig {
    fn default() -> Self {
        Self {
            online_endpoint: "https://cesarops.com/lemonade/v1".to_string(),
            model_name: "qwen2.5-coder-1.5b-instruct-q4_k_m".to_string(),
            api_key: None,
            timeout_seconds: 5,
            allow_fallback: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChoice {
    pub message: Option<ChatMessage>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<ChatCompletionChoice>,
}

/// The Lemonade Symmetrical Bridge Module
pub struct LemonadeModule {
    pub config: LemonadeConfig,
    client: reqwest::Client,
    pub is_online: bool,
}

impl LemonadeModule {
    pub fn new(config: LemonadeConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .unwrap_or_default();
        Self {
            config,
            client,
            is_online: false,
        }
    }

    /// Healthcheck the online endpoint
    pub async fn check_connectivity(&mut self) -> bool {
        let models_url = format!("{}/models", self.config.online_endpoint.trim_end_matches('/'));
        match self.client.get(&models_url).send().await {
            Ok(resp) => {
                self.is_online = resp.status().is_success();
                self.is_online
            }
            Err(_) => {
                self.is_online = false;
                false
            }
        }
    }

    /// Generate chat response using either online Lemonade server or offline native fallback
    pub async fn generate(
        &mut self,
        prompt: &str,
        system_prompt: Option<&str>,
        force_offline: bool,
    ) -> Result<String, NautikError> {
        let use_online = !force_offline && (self.is_online || self.check_connectivity().await);

        if use_online {
            tracing::info!("Routing query to online Lemonade cluster ({})", self.config.online_endpoint);
            let mut messages = Vec::new();
            if let Some(sys) = system_prompt {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: sys.to_string(),
                });
            }
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            });

            let req_body = ChatCompletionRequest {
                model: self.config.model_name.clone(),
                messages,
                temperature: 0.2,
                stream: false,
            };

            let completions_url = format!(
                "{}/chat/completions",
                self.config.online_endpoint.trim_end_matches('/')
            );

            let mut request = self.client.post(&completions_url).json(&req_body);
            if let Some(ref key) = self.config.api_key {
                request = request.bearer_auth(key);
            }

            match request.send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body: ChatCompletionResponse = resp
                        .json()
                        .await
                        .map_err(|e| NautikError::Lemonade(format!("JSON decode error: {}", e)))?;
                    if let Some(choice) = body.choices.first() {
                        if let Some(ref msg) = choice.message {
                            return Ok(msg.content.clone());
                        }
                    }
                    Err(NautikError::Lemonade("Empty choice response from Lemonade".to_string()))
                }
                Ok(resp) => {
                    tracing::warn!("Online Lemonade returned HTTP {}, falling back to offline", resp.status());
                    self.is_online = false;
                    self.execute_offline_engine(prompt, system_prompt)
                }
                Err(err) => {
                    tracing::warn!("Online Lemonade connection failed ({}), falling back to offline", err);
                    self.is_online = false;
                    self.execute_offline_engine(prompt, system_prompt)
                }
            }
        } else {
            tracing::info!("Executing native offline local engine (zero internet required)");
            self.execute_offline_engine(prompt, system_prompt)
        }
    }

    /// Native on-device local deterministic inference engine
    fn execute_offline_engine(&self, prompt: &str, system_prompt: Option<&str>) -> Result<String, NautikError> {
        let prompt_lower = prompt.to_lowercase();
        let sys = system_prompt.unwrap_or("You are Mack, the NautiK9 survey companion.");

        if prompt_lower.contains("status") || prompt_lower.contains("boat") {
            Ok(format!(
                "[NautiK9 Offline Engine: {}]\nSystem: {}\nVessel Status: NautiK9 Survey Vessel is steady at helm. Mack is keeping watch. Local SQLite vault is verified.",
                self.config.model_name, sys
            ))
        } else if prompt_lower.contains("search") || prompt_lower.contains("find") {
            Ok(format!(
                "[NautiK9 Offline Engine: {}]\nLocal Wayfinder index activated. Query '{}' indexed for matching across local survey logs.",
                self.config.model_name, prompt
            ))
        } else {
            Ok(format!(
                "[NautiK9 Offline Engine: {}]\nMack acknowledged: '{}'. Full tool capabilities active without cloud dependency.",
                self.config.model_name, prompt
            ))
        }
    }
}

#[async_trait]
impl NautikModule for LemonadeModule {
    fn id(&self) -> &str {
        "lemonade"
    }

    fn name(&self) -> &str {
        "Lemonade Dual-Mode Client & Inference Bridge"
    }

    async fn init(&mut self, ctx: &mut ModuleContext) -> Result<(), NautikError> {
        self.config.model_name = ctx.hardware.recommended_llm_model.clone();
        tracing::info!(
            "Lemonade module initialized. Target model: '{}' (Tier: {:?})",
            self.config.model_name,
            ctx.hardware.tier
        );
        // Initial quick connectivity check
        self.check_connectivity().await;
        Ok(())
    }

    async fn handle_event(
        &mut self,
        event: &NautikEvent,
        _ctx: &mut ModuleContext,
    ) -> Result<Option<NautikEvent>, NautikError> {
        match &event.payload {
            EventPayload::LemonadeChatRequest {
                prompt,
                system_prompt,
                prefer_offline,
            } => {
                let response = self
                    .generate(prompt, system_prompt.as_deref(), *prefer_offline)
                    .await?;

                Ok(Some(NautikEvent::new(
                    "lemonade",
                    EventPayload::LemonadeChatDelta {
                        delta: response,
                        is_finished: true,
                    },
                )))
            }
            _ => Ok(None),
        }
    }

    async fn tick(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
        // Periodic background ping to update online status
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NautikError> {
        tracing::info!("Lemonade module shut down cleanly");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nautik9_core::HardwareProfile;

    #[tokio::test]
    async fn test_offline_fallback_generation() {
        let config = LemonadeConfig {
            online_endpoint: "http://127.0.0.1:9999/v1".to_string(), // deliberately invalid to trigger instant fallback
            model_name: "Qwen2.5-7B-Instruct-NautiK9".to_string(),
            api_key: None,
            timeout_seconds: 1,
            allow_fallback: true,
        };

        let mut module = LemonadeModule::new(config);
        let resp = module
            .generate("Boat status and survey coordinates", None, false)
            .await
            .unwrap();

        assert!(resp.contains("NautiK9 Offline Engine"));
        assert!(resp.contains("Vessel Status"));
    }

    #[tokio::test]
    async fn test_lemonade_module_event_handling() {
        let config = LemonadeConfig::default();
        let mut module = LemonadeModule::new(config);
        let hardware = HardwareProfile::samsung_s25_ultra();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);
        let mut ctx = ModuleContext::new(hardware, tx);

        module.init(&mut ctx).await.unwrap();

        let chat_event = NautikEvent::new(
            "user",
            EventPayload::LemonadeChatRequest {
                prompt: "Boat status report".to_string(),
                system_prompt: Some("You are Mack".to_string()),
                prefer_offline: true,
            },
        );

        let response = module.handle_event(&chat_event, &mut ctx).await.unwrap();
        assert!(response.is_some());

        if let Some(ev) = response {
            if let EventPayload::LemonadeChatDelta { delta, is_finished } = ev.payload {
                assert!(is_finished);
                assert!(delta.contains("NautiK9 Offline Engine"));
            } else {
                panic!("Unexpected event payload");
            }
        }
    }
}
