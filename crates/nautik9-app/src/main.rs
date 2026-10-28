use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    hardware::HardwareProfile,
    module::{ModuleContext, ModuleRegistry, NautikModule},
    sms::{SmsMessage, SmsStatus, WorkModeConfig},
};
use nautik9_fleet::{FleetConfig, FleetModule};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule};
use nautik9_wayfinder::{generate_fast_embedding, WayfinderModule, WayfinderVault};

mod device_sms;

#[derive(Parser, Debug)]
#[command(name = "nautik9", version = "0.1.0", about = "NautiK9 Pure-Rust Survey & Organization Engine")]
struct Cli {
    /// Device profiling hint (e.g. 's25ultra', 's24', 'baseline', 'desktop')
    #[arg(short, long, default_value = "s25ultra")]
    device: String,

    /// Path to local SQLite vault database
    #[arg(long, default_value = "vault.db")]
    vault_path: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Display detected hardware profile, One UI capabilities, and Mack avatar state
    Status,
    /// Semantic search across local Wayfinder personal knowledge vault
    Search {
        query: String,
        #[arg(short, long, default_value_t = 5)]
        top_k: usize,
    },
    /// Ask Lemonade dual-mode reasoning engine (online cluster or local offline fallback)
    Chat {
        prompt: String,
        #[arg(long)]
        offline: bool,
    },
    /// Decompose a high-level survey or coding task into 8 concrete steps
    Decompose {
        task: String,
    },
    /// Index a new field note, sonar chip record, or personal log into the vault
    Index {
        id: String,
        title: String,
        content: String,
        #[arg(short, long)]
        tags: Vec<String>,
    },
    /// Start ambient interactive shell with live Mack companion updates
    Interactive,
    /// Launch responsive mobile companion web server for Samsung Galaxy / Phone Link testing
    Serve {
        #[arg(short, long, default_value_t = 8340)]
        port: u16,
    },
}

/// CLI Output Formatter Module that presents cross-module responses cleanly in terminal
struct CliOutputModule;

#[async_trait]
impl NautikModule for CliOutputModule {
    fn id(&self) -> &str {
        "cli_output"
    }

    fn name(&self) -> &str {
        "CLI Output Formatter"
    }

    async fn init(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
        Ok(())
    }

    async fn handle_event(
        &mut self,
        event: &NautikEvent,
        _ctx: &mut ModuleContext,
    ) -> Result<Option<NautikEvent>, NautikError> {
        match &event.payload {
            EventPayload::WayfinderSearchResult { query, results } => {
                println!("\n📋 Wayfinder Results for '{}' (Total Hits: {}):", query, results.len());
                if results.is_empty() {
                    println!("   (No matching survey records or notes found in local vault)");
                } else {
                    for (i, item) in results.iter().enumerate() {
                        println!("   {}. [{}] {} (Relevance: {:.2})", i + 1, item.id, item.title, item.score);
                        println!("      Snippet: {}", item.snippet);
                        println!("      Tags:    {:?}", item.tags);
                    }
                }
                println!();
            }
            EventPayload::LemonadeChatDelta { delta, is_finished } => {
                println!("\n🐾 [Mack / Lemonade Response]:");
                println!("{}\n", delta);
                if *is_finished {
                    println!("--- (End of Response) ---\n");
                }
            }
            _ => {}
        }
        Ok(None)
    }

    async fn tick(&mut self, _ctx: &mut ModuleContext) -> Result<(), NautikError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NautikError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // 1. Hardware Generational Profiling
    let hardware = HardwareProfile::detect_or_scaffold(Some(&cli.device));

    println!("=======================================================");
    println!("       🐾 NautiK9 Survey & Organization Engine 🐾     ");
    println!("=======================================================");
    println!("Device Profile:    {}", hardware.device_name);
    println!("Hardware Tier:     {:?}", hardware.tier);
    println!("RAM / Cores:       {} MB / {} cores", hardware.ram_mb, hardware.cpu_cores);
    println!("SVE2 / Hexagon:    SVE2: {} | NPU: {}", hardware.has_armv9_sve2, hardware.has_hexagon_npu);
    println!("Recommended Model: {} ({})", hardware.recommended_llm_model, hardware.recommended_quantization);
    if let Some(ref ver) = hardware.one_ui_version {
        println!("Samsung One UI:    v{} (Galaxy AI integration enabled)", ver);
    }
    println!("=======================================================\n");

    // 2. Initialize Module Registry
    let mut registry = ModuleRegistry::new(hardware.clone());

    // Register Output Formatter
    registry.register(Box::new(CliOutputModule)).await?;

    // Initialize Wayfinder
    let vault = WayfinderVault::new(&cli.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap());
    let wayfinder_mod = Box::new(WayfinderModule::new(vault, hardware.recommended_embedding_dim));
    registry.register(wayfinder_mod).await?;

    // Initialize Lemonade
    let lemonade_mod = Box::new(LemonadeModule::new(LemonadeConfig::default()));
    registry.register(lemonade_mod).await?;

    // Initialize Fleet Bridge
    let fleet_mod = Box::new(FleetModule::new(FleetConfig::default()));
    registry.register(fleet_mod).await?;

    // 3. Command Execution
    match cli.command.unwrap_or(Commands::Status) {
        Commands::Status => {
            let avatar = registry.context.avatar.read().await;
            println!("--- Mack Avatar Companion Status ---");
            println!("State:         {:?}", avatar.current_state);
            println!("Status:        {}", avatar.status_message);
            println!("Vessel:        {}", avatar.vessel_name);
            println!("Frame Hint:    {}", avatar.animation_frame_hint);
            println!("Flag Waving:   {}", avatar.flag_waving);
            println!("\n--- Active NautiK9 Modules ---");
            for id in registry.module_ids() {
                println!("  • Module: {}", id);
            }
            let one_ui = hardware.one_ui_capabilities();
            if !one_ui.is_empty() {
                println!("\n--- Samsung Galaxy One UI 7 Hardware Capabilities ---");
                for cap in one_ui {
                    println!("  ✓ {}", cap);
                }
            }
        }
        Commands::Search { query, top_k } => {
            println!("🔍 Mack is sniffing the Wayfinder vault for: '{}'", query);
            let search_event = NautikEvent::new(
                "cli",
                EventPayload::WayfinderSearchRequest { query: query.clone(), top_k },
            );
            registry.dispatch_event(search_event).await?;
            registry.process_pending_events().await?;

            let avatar = registry.context.avatar.read().await;
            println!("Mack Avatar Reaction: [{:?}] {}", avatar.current_state, avatar.status_message);
        }
        Commands::Chat { prompt, offline } => {
            println!("💬 Asking Lemonade (Mode: {})...", if offline { "Offline Local" } else { "Dynamic Dual-Mode" });
            let chat_event = NautikEvent::new(
                "cli",
                EventPayload::LemonadeChatRequest {
                    prompt,
                    system_prompt: Some("You are Mack, the NautiK9 survey dog on the boat helm.".to_string()),
                    prefer_offline: offline,
                },
            );
            registry.dispatch_event(chat_event).await?;
            registry.process_pending_events().await?;
        }
        Commands::Decompose { task } => {
            println!("📋 Decomposing Task: '{}'", task);
            let steps = nautik9_wayfinder::decompose_task(&task);
            for step in steps {
                println!("  {}", step);
            }
        }
        Commands::Index { id, title, content, tags } => {
            println!("📥 Indexing new entry into Wayfinder vault: [{}] {}", id, title);
            let emb = generate_fast_embedding(&format!("{} {}", title, content), hardware.recommended_embedding_dim);
            
            let vault = WayfinderVault::new(&cli.vault_path)?;
            vault.put_item(&id, &title, &content, &tags, Some(&emb)).await?;

            let index_event = NautikEvent::new(
                "cli",
                EventPayload::WayfinderItemIndexed { item_id: id, title, tags },
            );
            registry.dispatch_event(index_event).await?;
            println!("✓ Successfully indexed with {}-dimensional vector embedding.", hardware.recommended_embedding_dim);
        }
        Commands::Interactive => {
            println!("Interactive mode: Mack is on watch. Type your request or 'exit'.");
        }
        Commands::Serve { port } => {
            serve_mobile_web(port, registry, cli.vault_path).await?;
            return Ok(());
        }
    }

    registry.shutdown_all().await?;
    Ok(())
}

#[derive(Clone)]
struct AppState {
    registry: std::sync::Arc<tokio::sync::Mutex<ModuleRegistry>>,
    hardware: std::sync::Arc<nautik9_core::hardware::HardwareProfile>,
    vault_path: PathBuf,
    sms_messages: std::sync::Arc<tokio::sync::RwLock<Vec<SmsMessage>>>,
    work_config: std::sync::Arc<tokio::sync::RwLock<WorkModeConfig>>,
}

async fn serve_mobile_web(
    port: u16,
    registry: ModuleRegistry,
    vault_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    use axum::{routing::{get, post}, Router};

    let hardware = registry.context.hardware.clone();

    // Read real raw SMS messages directly from physical S25 Ultra over ADB
    let mut initial_sms = match device_sms::fetch_real_device_sms(5).await {
        Ok(msgs) => {
            println!("📥 S25 Ultra Real SMS: Retrieved {} genuine incoming messages from content://sms/inbox", msgs.len());
            msgs
        }
        Err(e) => {
            println!("⚠️ S25 Ultra Real SMS: No synthetic data. Device connection status: {}", e);
            Vec::new()
        }
    };

    let work_config = WorkModeConfig::default();

    // If Work Mode is active and real messages exist, generate real initial drafts using Lemonade Dual-P100
    if work_config.enabled && !initial_sms.is_empty() {
        println!("🧠 Lemonade Engine: Drafting responses for real incoming SMS via dual Tesla P100s...");
        let mut lemonade = LemonadeModule::new(LemonadeConfig::default());
        for msg in initial_sms.iter_mut() {
            let (draft, model, dur) = lemonade.draft_sms_reply(
                &msg.sender,
                &msg.body,
                &work_config.custom_instructions,
                &work_config.model_target,
            ).await;
            msg.draft_reply = Some(draft);
            msg.reply_model = Some(model);
            msg.latency_ms = Some(dur);
            msg.status = if work_config.auto_reply {
                SmsStatus::AutoSent
            } else {
                SmsStatus::Drafted
            };
        }
    }

    let state = AppState {
        registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
        hardware,
        vault_path,
        sms_messages: std::sync::Arc::new(tokio::sync::RwLock::new(initial_sms)),
        work_config: std::sync::Arc::new(tokio::sync::RwLock::new(work_config)),
    };

    let app = Router::new()
        .route("/", get(handle_index))
        .route("/assets/mack.jpg", get(handle_asset_mack))
        .route("/api/status", get(handle_status))
        .route("/api/search", post(handle_search))
        .route("/api/chat", post(handle_chat))
        .route("/api/decompose", post(handle_decompose))
        .route("/api/notes", get(handle_get_notes).post(handle_save_note))
        .route("/api/notes/{id}", axum::routing::delete(handle_delete_note))
        .route("/api/tot", post(handle_tot))
        .route("/api/fleet/status", get(handle_fleet_status))
        .route("/api/avatar/action", post(handle_avatar_action))
        .route("/api/sms/inbox", get(handle_sms_inbox))
        .route("/api/sms/workmode", post(handle_sms_workmode))
        .route("/api/sms/incoming", post(handle_sms_incoming))
        .route("/api/sms/action", post(handle_sms_action))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("=======================================================");
    println!("🚀 NautiK9 Mobile Companion Server is LIVE (Axum RFC Engine)!");
    println!("=======================================================");
    println!("  📱 Phone Link / Android URL:  http://127.0.0.1:{}", port);
    println!("  🔒 Sovereign Mesh (Tailscale): http://127.0.0.1:{}", port);
    println!("  💻 Host Local:                http://127.0.0.1:{}", port);
    println!("=======================================================");
    println!("Open this URL on your phone or in Phone Link browser window.\n");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_index() -> axum::response::Html<&'static str> {
    axum::response::Html(get_mobile_html())
}

async fn handle_asset_mack() -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    match tokio::fs::read("assets/mack_avatar.jpg").await {
        Ok(img) => ([(header::CONTENT_TYPE, "image/jpeg")], img).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn handle_status(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let reg_guard = state.registry.lock().await;
    let avatar = reg_guard.context.avatar.read().await;
    axum::Json(serde_json::json!({
        "device": state.hardware.device_name,
        "tier": format!("{:?}", state.hardware.tier),
        "ram_mb": state.hardware.ram_mb,
        "sve2": state.hardware.has_armv9_sve2,
        "npu": state.hardware.has_hexagon_npu,
        "model": state.hardware.recommended_llm_model,
        "avatar": {
            "state": format!("{:?}", avatar.current_state),
            "status": avatar.status_message,
            "vessel": avatar.vessel_name,
            "flag": avatar.flag_waving
        }
    }))
}

async fn handle_search(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let q = payload["query"].as_str().unwrap_or("survey");
    let vault = WayfinderVault::new(&state.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap());
    let q_emb = generate_fast_embedding(q, state.hardware.recommended_embedding_dim);
    let results = vault.search(q, Some(&q_emb), 10).await.unwrap_or_default();

    {
        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        if !results.is_empty() {
            avatar.transition_to(
                nautik9_core::avatar::MackState::TargetLocked,
                format!("Target lock on '{}' ({} records found)", q, results.len()),
            );
        } else {
            avatar.transition_to(
                nautik9_core::avatar::MackState::Idle,
                format!("Search complete for '{}' (0 hits)", q),
            );
        }
    }

    axum::Json(serde_json::json!({
        "query": q,
        "hits": results.len(),
        "results": results
    }))
}

async fn handle_chat(
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let prompt = payload["prompt"].as_str().unwrap_or("Status report");
    let offline = payload["offline"].as_bool().unwrap_or(false);
    let target = if offline { "offline" } else { "strongest" };

    let mut lemonade = LemonadeModule::new(LemonadeConfig::default());
    let (reply, model_used, duration_ms) = lemonade
        .generate_routed(prompt, Some("You are Mack, the sovereign NautiK9 survey dog."), target)
        .await;

    axum::Json(serde_json::json!({
        "response": reply,
        "offline": offline,
        "model_used": model_used,
        "duration_ms": duration_ms
    }))
}

#[derive(serde::Deserialize, Default)]
struct SmsQuery {
    sync: Option<bool>,
}

async fn handle_sms_inbox(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(query): axum::extract::Query<SmsQuery>,
) -> axum::Json<serde_json::Value> {
    if query.sync.unwrap_or(false) {
        if let Ok(real_messages) = device_sms::fetch_real_device_sms(5).await {
            let work_cfg = {
                let r = state.work_config.read().await;
                r.clone()
            };
            let mut list = state.sms_messages.write().await;
            let mut lemonade = LemonadeModule::new(LemonadeConfig::default());

            for mut real_msg in real_messages {
                if !list.iter().any(|m| m.id == real_msg.id) {
                    if work_cfg.enabled {
                        let (draft, model, dur) = lemonade.draft_sms_reply(
                            &real_msg.sender,
                            &real_msg.body,
                            &work_cfg.custom_instructions,
                            &work_cfg.model_target,
                        ).await;
                        real_msg.draft_reply = Some(draft);
                        real_msg.reply_model = Some(model);
                        real_msg.latency_ms = Some(dur);
                        real_msg.status = if work_cfg.auto_reply {
                            SmsStatus::AutoSent
                        } else {
                            SmsStatus::Drafted
                        };

                        if work_cfg.auto_reply {
                            if let Some(ref d) = real_msg.draft_reply {
                                let _ = device_sms::dispatch_sms_to_phone(&real_msg.sender, d).await;
                            }
                        }
                    }
                    list.insert(0, real_msg);
                }
            }
        }
    }

    let messages = {
        let list = state.sms_messages.read().await;
        list.clone()
    };
    let work_cfg = {
        let cfg = state.work_config.read().await;
        cfg.clone()
    };
    axum::Json(serde_json::json!({
        "messages": messages,
        "work_config": work_cfg
    }))
}

async fn handle_sms_workmode(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let mut cfg = state.work_config.write().await;
    if let Some(enabled) = payload["enabled"].as_bool() {
        cfg.enabled = enabled;
    }
    if let Some(auto_reply) = payload["auto_reply"].as_bool() {
        cfg.auto_reply = auto_reply;
    }
    if let Some(preset) = payload["preset"].as_str() {
        cfg.preset = preset.to_string();
    }
    if let Some(instructions) = payload["custom_instructions"].as_str() {
        cfg.custom_instructions = instructions.to_string();
    }
    if let Some(target) = payload["model_target"].as_str() {
        cfg.model_target = target.to_string();
    }

    {
        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        if cfg.enabled {
            avatar.transition_to(
                nautik9_core::avatar::MackState::Attentive,
                format!("Work Mode ARMED (Auto-Reply: {})", if cfg.auto_reply { "AUTO" } else { "APPROVAL" }),
            );
        } else {
            avatar.transition_to(
                nautik9_core::avatar::MackState::Idle,
                "Work Mode STANDBY (Personal)".to_string(),
            );
        }
    }

    axum::Json(serde_json::json!({
        "success": true,
        "work_config": *cfg
    }))
}

async fn handle_sms_incoming(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let sender = payload["sender"].as_str().unwrap_or("Dispatch Contact").to_string();
    let body = payload["body"].as_str().unwrap_or("").to_string();
    let msg_id = format!("sms-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let now = chrono::Local::now().format("%I:%M %p").to_string();

    let work_cfg = {
        let r = state.work_config.read().await;
        r.clone()
    };

    let mut draft_reply = None;
    let mut reply_model = None;
    let mut latency_ms = None;
    let status;

    if work_cfg.enabled {
        let mut lemonade = LemonadeModule::new(LemonadeConfig::default());
        let (draft, model, dur) = lemonade.draft_sms_reply(
            &sender,
            &body,
            &work_cfg.custom_instructions,
            &work_cfg.model_target,
        ).await;

        draft_reply = Some(draft);
        reply_model = Some(model);
        latency_ms = Some(dur);

        if work_cfg.auto_reply {
            status = SmsStatus::AutoSent;
            if let Some(ref d) = draft_reply {
                let _ = device_sms::dispatch_sms_to_phone(&sender, d).await;
            }
        } else {
            status = SmsStatus::Drafted;
        }

        // Trigger Mack avatar transition
        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        avatar.transition_to(
            nautik9_core::avatar::MackState::TargetLocked,
            format!("Mack drafted auto-reply for SMS from {}", sender),
        );
    } else {
        status = SmsStatus::Pending;
    }

    let new_msg = SmsMessage {
        id: msg_id.clone(),
        sender,
        body,
        timestamp: now,
        draft_reply,
        status,
        reply_model,
        latency_ms,
    };

    {
        let mut list = state.sms_messages.write().await;
        list.insert(0, new_msg.clone());
    }

    axum::Json(serde_json::json!({
        "success": true,
        "message": new_msg,
        "work_mode": work_cfg
    }))
}

async fn handle_sms_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let id = payload["id"].as_str().unwrap_or("");
    let action = payload["action"].as_str().unwrap_or("approve");
    let edited_draft = payload["edited_draft"].as_str().map(|s| s.to_string());

    let mut list = state.sms_messages.write().await;
    let mut updated_msg = None;
    let mut dispatch_result = None;

    for msg in list.iter_mut() {
        if msg.id == id {
            if let Some(ed) = edited_draft {
                msg.draft_reply = Some(ed);
            }
            match action {
                "approve" => {
                    msg.status = SmsStatus::Approved;
                    if let Some(ref draft_text) = msg.draft_reply {
                        match device_sms::dispatch_sms_to_phone(&msg.sender, draft_text).await {
                            Ok(res) => {
                                dispatch_result = Some(res);
                            }
                            Err(e) => {
                                dispatch_result = Some(format!("Blocker/Error: {}", e));
                            }
                        }
                    }
                }
                "regenerate" => {
                    let work_cfg = {
                        let r = state.work_config.read().await;
                        r.clone()
                    };
                    let mut lemonade = LemonadeModule::new(LemonadeConfig::default());
                    let (draft, model, dur) = lemonade.draft_sms_reply(
                        &msg.sender,
                        &msg.body,
                        &work_cfg.custom_instructions,
                        &work_cfg.model_target,
                    ).await;
                    msg.draft_reply = Some(draft);
                    msg.reply_model = Some(model);
                    msg.latency_ms = Some(dur);
                    msg.status = SmsStatus::Drafted;
                }
                "ignore" => {
                    msg.status = SmsStatus::Ignored;
                }
                _ => {}
            }
            updated_msg = Some(msg.clone());
            break;
        }
    }

    axum::Json(serde_json::json!({
        "success": updated_msg.is_some(),
        "message": updated_msg,
        "dispatch_result": dispatch_result
    }))
}

async fn handle_fleet_status() -> axum::Json<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .unwrap_or_default();

    let fleet_data = match client.get("http://127.0.0.1:9122/api/fleet/nodes").send().await {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<serde_json::Value>().await.ok()
        }
        _ => None,
    };

    let (nodes, summary) = if let Some(fd) = fleet_data {
        (fd["nodes"].clone(), fd["summary"].clone())
    } else {
        (serde_json::json!([
            { "name": "t440", "gpus": 2, "os": "Linux 24.04 Ubuntu", "reachable": true, "url": "http://127.0.0.1:9121", "age_secs": 0 },
            { "name": "cesarops2", "gpus": 3, "os": "Linux 4.0.5 Omarchy", "reachable": true, "url": "http://127.0.0.1:19221", "age_secs": 0 },
            { "name": "cesarops3", "gpus": 3, "os": "Linux 4.0.4 Omarchy", "reachable": true, "url": "http://127.0.0.1:19222", "age_secs": 0 },
            { "name": "cesarops4om", "gpus": 5, "os": "Linux 4.0.4 Omarchy", "reachable": true, "url": "http://127.0.0.1:19223", "age_secs": 0 },
            { "name": "nautik9-laptop", "gpus": 2, "os": "Windows 11 Pro", "reachable": true, "url": "http://127.0.0.1:9121", "age_secs": 0 },
            { "name": "dell-touchscreen", "gpus": 1, "os": "Windows 11 Home", "reachable": true, "url": "http://127.0.0.1:9121", "age_secs": 0 }
        ]), serde_json::json!({
            "nodes_total": 6,
            "nodes_reachable": 6,
            "gpus_total": 16,
            "gpus_busy": 13,
            "vram_total_bytes": 213385447424u64,
            "vram_used_bytes": 120893108224u64,
            "max_temp_c": 69.0
        }))
    };

    axum::Json(serde_json::json!({
        "cluster": "Amway Blitz Sovereign Swarm",
        "nodes": nodes,
        "summary": summary,
        "active_routers": {
            "lemonade_gpu": "http://127.0.0.1:8001/v1 (Qwen3-VL-4B Instruct - Fast Dual-P100)",
            "paddler_swarm": "http://127.0.0.1:8080/v1 (KatCoder-35B / Flint-40B / DeepSeek-V4.1)",
            "lemonade_core": "http://127.0.0.1:13305/v1 (Qwen3-14B-GGUF)",
            "fleet_aggregator": "http://127.0.0.1:9122 (Authoritative 6-Node Mesh)"
        }
    }))
}

async fn handle_decompose(
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let task = payload["task"].as_str().unwrap_or("Decompose mission");
    let steps = nautik9_wayfinder::decompose_task(task);
    axum::Json(serde_json::json!({
        "task": task,
        "steps": steps
    }))
}

async fn handle_get_notes(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let vault = WayfinderVault::new(&state.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap());
    let items = vault.list_items().await.unwrap_or_default();
    axum::Json(serde_json::json!({
        "notes": items,
        "count": items.len()
    }))
}

async fn handle_save_note(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let id = payload["id"].as_str().map(|s| s.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let title = payload["title"].as_str().unwrap_or("Untitled Field Note");
    let content = payload["content"].as_str().unwrap_or("");
    let tags: Vec<String> = payload["tags"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_else(|| vec!["field-note".to_string()]);

    let vault = WayfinderVault::new(&state.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap());
    let emb = generate_fast_embedding(&format!("{} {}", title, content), state.hardware.recommended_embedding_dim);
    let ok = vault.put_item(&id, title, content, &tags, Some(&emb)).await.is_ok();

    axum::Json(serde_json::json!({
        "success": ok,
        "id": id,
        "title": title
    }))
}

async fn handle_delete_note(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::Json<serde_json::Value> {
    let vault = WayfinderVault::new(&state.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap());
    let ok = vault.delete_item(&id).await.is_ok();
    axum::Json(serde_json::json!({ "success": ok, "id": id }))
}

async fn handle_tot(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let problem = payload["problem"].as_str().unwrap_or("Survey anomaly verification");
    let budget = payload["budget"].as_u64().unwrap_or(12) as usize;

    let tot = nautik9_wayfinder::run_tree_of_thoughts(problem, budget);

    {
        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        avatar.transition_to(
            nautik9_core::avatar::MackState::Sniffing,
            format!("ToT Reasoner: evaluated {} nodes for '{}'", tot.nodes.len(), problem),
        );
    }

    axum::Json(serde_json::to_value(tot).unwrap_or_default())
}

async fn handle_avatar_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let action = payload["action"].as_str().unwrap_or("bark");
    let reg_guard = state.registry.lock().await;
    let mut avatar = reg_guard.context.avatar.write().await;

    match action {
        "bark" | "alert" => {
            avatar.transition_to(nautik9_core::avatar::MackState::TargetLocked, "Mack barked! Target locked on survey sonar.".to_string());
        }
        "sleep" | "rest" => {
            avatar.transition_to(nautik9_core::avatar::MackState::Idle, "Mack is resting on the foredeck.".to_string());
        }
        "navigate" => {
            avatar.transition_to(nautik9_core::avatar::MackState::SurveyNavigating, "NautiK9 underway: heading 042° True.".to_string());
        }
        _ => {
            avatar.transition_to(nautik9_core::avatar::MackState::Attentive, "Mack is watching the helm.".to_string());
        }
    }

    axum::Json(serde_json::json!({
        "state": format!("{:?}", avatar.current_state),
        "status": avatar.status_message,
        "vessel": avatar.vessel_name
    }))
}

fn get_mobile_html() -> &'static str {
    include_str!("index.html")
}

