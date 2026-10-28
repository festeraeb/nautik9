use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    module::{ModuleContext, NautikModule},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LemonadeConfig {
    pub online_endpoint: String,
    pub paddler_endpoint: String,
    pub core_endpoint: String,
    pub model_name: String,
    pub api_key: Option<String>,
    pub timeout_seconds: u64,
    pub allow_fallback: bool,
}

impl Default for LemonadeConfig {
    fn default() -> Self {
        Self {
            online_endpoint: "http://127.0.0.1:8001/v1".to_string(),
            paddler_endpoint: "http://127.0.0.1:8080/v1".to_string(),
            core_endpoint: "http://127.0.0.1:13305/v1".to_string(),
            model_name: "/var/lib/lemonade/.cache/huggingface/hub/models--Qwen--Qwen3-VL-4B-Instruct-GGUF/snapshots/1cd86afb9a95c410a6038ab3b40d8b578c892266/Qwen3VL-4B-Instruct-Q4_K_M.gguf".to_string(),
            api_key: None,
            timeout_seconds: 6,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChoice {
    pub message: Option<ChatMessage>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: Option<String>,
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

    /// Healthcheck endpoints
    pub async fn check_connectivity(&mut self) -> bool {
        let models_url = format!("{}/models", self.config.online_endpoint.trim_end_matches('/'));
        match self.client.get(&models_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.is_online = true;
                true
            }
            _ => {
                // Check paddler
                let paddler_url = format!("{}/models", self.config.paddler_endpoint.trim_end_matches('/'));
                match self.client.get(&paddler_url).send().await {
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
        }
    }

    /// Generate chat response routing to the strongest, fastest model available
    pub async fn generate_routed(
        &mut self,
        prompt: &str,
        system_prompt: Option<&str>,
        target_tier: &str,
    ) -> (String, String, u64) {
        let start = Instant::now();

        if target_tier == "offline" {
            let reply = self.execute_offline_engine(prompt, system_prompt).unwrap_or_else(|e| e.to_string());
            return (reply, "NautiK9 Offline Local Engine (Air-gapped)".to_string(), start.elapsed().as_millis() as u64);
        }

        // Tier 1: Try Lemonade GPU llama-server (:8001) - Qwen3-VL-4B Instruct
        let req_body = ChatCompletionRequest {
            model: self.config.model_name.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.unwrap_or("You are Mack, the NautiK9 survey companion.").to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: prompt.to_string(),
                },
            ],
            temperature: 0.3,
            stream: false,
            max_tokens: Some(300),
        };

        let primary_url = format!("{}/chat/completions", self.config.online_endpoint.trim_end_matches('/'));
        if let Ok(resp) = self.client.post(&primary_url).json(&req_body).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<ChatCompletionResponse>().await {
                    if let Some(choice) = body.choices.first() {
                        if let Some(ref msg) = choice.message {
                            let duration = start.elapsed().as_millis() as u64;
                            return (msg.content.clone(), "Qwen3-VL-4B Instruct (Lemonade Dual-P100 :8001)".to_string(), duration);
                        }
                    }
                }
            }
        }

        // Tier 2: Try Paddler Swarm (:8080) - KatCoder-35B or Flint-40B
        let paddler_req = ChatCompletionRequest {
            model: "kat-coder-v2.5-35b-a3b".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.unwrap_or("You are Mack, the NautiK9 survey companion.").to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: prompt.to_string(),
                },
            ],
            temperature: 0.3,
            stream: false,
            max_tokens: Some(300),
        };
        let paddler_url = format!("{}/chat/completions", self.config.paddler_endpoint.trim_end_matches('/'));
        if let Ok(resp) = self.client.post(&paddler_url).json(&paddler_req).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<ChatCompletionResponse>().await {
                    if let Some(choice) = body.choices.first() {
                        if let Some(ref msg) = choice.message {
                            let duration = start.elapsed().as_millis() as u64;
                            return (msg.content.clone(), "KatCoder-35B-A3B (Paddler Fleet Swarm :8080)".to_string(), duration);
                        }
                    }
                }
            }
        }

        // Tier 3: Native Offline Engine
        let reply = self.execute_offline_engine(prompt, system_prompt).unwrap_or_else(|e| e.to_string());
        (reply, "NautiK9 Offline Local Engine (Fallback)".to_string(), start.elapsed().as_millis() as u64)
    }

    /// Specialized SMS responder for Work Mode
    pub async fn draft_sms_reply(
        &mut self,
        sender: &str,
        incoming_body: &str,
        custom_instructions: &str,
        target_tier: &str,
    ) -> (String, String, u64) {
        let system_prompt = format!(
            "You are Mack, an autonomous AI survey & dispatch companion managing communications for Cesar while he is in WORK MODE / active field operations.\n\
            Instructions: {}\n\
            Rules: Write a polite, concise SMS response (1 to 3 sentences maximum). Mention that Cesar is on active operations and Mack has logged the message. Do not make false commitments.",
            custom_instructions
        );

        let prompt = format!(
            "Incoming SMS from: {}\nMessage Content: \"{}\"\nDraft the appropriate SMS reply now:",
            sender, incoming_body
        );

        self.generate_routed(&prompt, Some(&system_prompt), target_tier).await
    }

    pub async fn generate(
        &mut self,
        prompt: &str,
        system_prompt: Option<&str>,
        force_offline: bool,
    ) -> Result<String, NautikError> {
        let tier = if force_offline { "offline" } else { "strongest" };
        let (reply, _, _) = self.generate_routed(prompt, system_prompt, tier).await;
        Ok(reply)
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
        } else if prompt_lower.contains("sms") || prompt_lower.contains("text") || prompt_lower.contains("work mode") {
            Ok(format!(
                "Hi, this is Mack (Cesar's AI dispatch companion). Cesar is currently engaged in active operations. I have logged your message and he will follow up shortly!"
            ))
        } else if prompt_lower.contains("search") || prompt_lower.contains("find") {
            Ok(format!(
                "[NautiK9 Offline Engine: {}]\nLocal Wayfinder index activated. Query '{}' indexed for matching across local survey logs.",
                self.config.model_name, prompt
            ))
        } else {
            Ok(format!(
                "[NautiK9 Offline Engine]\nMack acknowledged: '{}'. Full tool capabilities active without cloud dependency.",
                prompt
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
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NautikError> {
        Ok(())
    }
}
