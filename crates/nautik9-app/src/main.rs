use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    hardware::HardwareProfile,
    module::{ModuleContext, ModuleRegistry, NautikModule},
    sms::{composer_opened_message, SmsMessage, SmsStatus, WorkModeConfig},
};
use nautik9_fleet::{FleetConfig, FleetModule};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule};
use nautik9_wayfinder::{generate_fast_embedding, WayfinderModule, WayfinderVault};

mod agent_api;
mod auth;
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

    /// Config directory (token, providers.json, workmode.json). Default ~/.config/nautik9
    #[arg(long)]
    config_dir: Option<PathBuf>,

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
    /// Ask the configured model endpoints (first one that answers wins; errors if none do)
    Chat {
        prompt: String,
        /// Only use endpoints marked "offline": true in providers.json
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
        /// Address to bind. 0.0.0.0 (default) lets the phone on the LAN reach it; use 127.0.0.1 for this machine only.
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// Fleet aggregator whose /api/fleet/nodes the Fleet tab reports
        #[arg(long, default_value = "http://127.0.0.1:9122")]
        fleet_url: String,
        /// Print the access URL including the token (otherwise only done on a terminal)
        #[arg(long)]
        print_token: bool,
        /// Where texts come from: `adb` (a phone on this machine's adb) or `app` (the NautiK9
        /// Android app pushes its inbox to /api/sms/ingest and opens the composer itself)
        #[arg(long, value_enum, default_value = "adb")]
        sms_bridge: SmsBridge,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SmsBridge {
    Adb,
    App,
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
    println!("SVE2 / Hexagon:    SVE2: {} | NPU: {} (from the --device hint, not probed)", hardware.has_armv9_sve2, hardware.has_hexagon_npu);
    println!("Model hint:        {} ({}) - a suggestion for this device class, not what runs", hardware.recommended_llm_model, hardware.recommended_quantization);
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

    // Initialize Lemonade (provider chain from <config dir>/providers.json, else defaults)
    let config_dir = cli.config_dir.clone().unwrap_or_else(auth::default_config_dir);
    let lemonade_config = LemonadeConfig::load(&config_dir.join("providers.json"))?;
    let lemonade_mod = Box::new(LemonadeModule::new(lemonade_config.clone()));
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
            println!("Asking the model chain ({})...", if offline { "offline endpoints only" } else { "all endpoints in order" });
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
        Commands::Serve { port, bind, fleet_url, print_token, sms_bridge } => {
            let opts = ServeOptions { port, bind, fleet_url, print_token, config_dir, lemonade_config, sms_bridge };
            serve_mobile_web(opts, registry, cli.vault_path).await?;
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
    lemonade: std::sync::Arc<LemonadeModule>,
    fleet_url: String,
    sms_bridge: SmsBridge,
    /// The agent, or why it could not start (reported by every /api/agent call).
    agent: Result<std::sync::Arc<nautik9_agent::Agent>, String>,
    /// Runs schedules, `Jade/crontab` and the heartbeat (None when the agent is unavailable).
    scheduler: Option<std::sync::Arc<nautik9_agent::schedule::Runner>>,
}

struct ServeOptions {
    port: u16,
    bind: String,
    fleet_url: String,
    print_token: bool,
    config_dir: PathBuf,
    lemonade_config: LemonadeConfig,
    sms_bridge: SmsBridge,
}

fn build_router(state: AppState, token: String) -> axum::Router {
    use axum::{routing::{get, post}, Router};
    Router::new()
        .route("/", get(handle_index))
        .route("/assets/mack.jpg", get(handle_asset_mack))
        .route("/api/health", get(handle_health))
        .route("/api/status", get(handle_status))
        .route("/api/providers", get(handle_providers))
        .route("/api/search", post(handle_search))
        .route("/api/chat", post(handle_chat))
        .route("/api/decompose", post(handle_decompose))
        .route("/api/notes", get(handle_get_notes).post(handle_save_note))
        .route("/api/notes/{id}", axum::routing::delete(handle_delete_note))
        .route("/api/notes/{id}/backlinks", get(handle_backlinks))
        .route("/api/tags", get(handle_tags))
        .route("/api/tot", post(handle_tot))
        .route("/api/fleet/status", get(handle_fleet_status))
        .route("/api/avatar/action", post(handle_avatar_action))
        .route("/api/sms/inbox", get(handle_sms_inbox))
        .route("/api/sms/workmode", post(handle_sms_workmode))
        .route("/api/sms/incoming", post(handle_sms_incoming))
        .route("/api/sms/action", post(handle_sms_action))
        .route("/api/sms/ingest", post(handle_sms_ingest))
        .route("/api/agent/chat", post(agent_api::chat))
        .route("/api/agent/sessions", get(agent_api::sessions))
        .route("/api/agent/sessions/{id}", get(agent_api::session).delete(agent_api::delete_session))
        .route("/api/agent/sessions/{id}/rate", post(agent_api::rate))
        .route("/api/agent/sessions/{id}/retry", post(agent_api::retry))
        .route("/api/agent/schedules", get(agent_api::schedules).post(agent_api::add_schedule))
        .route("/api/agent/schedules/{id}", axum::routing::delete(agent_api::delete_schedule))
        .route("/api/agent/schedules/{id}/run", post(agent_api::run_schedule))
        .route("/api/agent/export", get(agent_api::export))
        .route("/api/agent/web_search", post(agent_api::web_search))
        .route("/api/agent/mcp", post(agent_api::mcp_proxy))
        .layer(axum::middleware::from_fn_with_state(
            std::sync::Arc::new(token),
            auth::require_token,
        ))
        .with_state(state)
}

/// Best-effort LAN address of this machine (no packet is sent).
fn lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

/// Fill in the draft on `msg`, or mark it NoModel. Never invents a reply.
/// Texts from other people get no vault notes; only the owner's own numbers do.
async fn apply_draft(
    msg: &mut SmsMessage,
    lemonade: &LemonadeModule,
    cfg: &WorkModeConfig,
    vault_path: &std::path::Path,
    embedding_dim: usize,
) {
    let is_owner = cfg.is_owner(&msg.sender);
    let vault_context = if is_owner {
        match WayfinderVault::new(vault_path) {
            Ok(v) => {
                let q_emb = generate_fast_embedding(&msg.body, embedding_dim);
                let hits = v.search(&msg.body, Some(&q_emb), 3).await.unwrap_or_default();
                if hits.is_empty() {
                    None
                } else {
                    Some(hits.iter().map(|h| format!("- {}: {}", h.title, h.snippet)).collect::<Vec<_>>().join("\n"))
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let req = nautik9_lemonade::SmsDraftRequest {
        owner_name: &cfg.owner_name,
        instructions: &cfg.custom_instructions,
        sender: &msg.sender,
        body: &msg.body,
        is_owner,
        vault_context: vault_context.as_deref(),
    };
    let offline_only = cfg.model_target == "offline";
    let started = std::time::Instant::now();
    match lemonade.draft_sms_reply(&req, offline_only).await {
        Ok(a) => {
            msg.draft_reply = Some(a.reply.clone());
            msg.reply_model = Some(a.label());
            msg.latency_ms = Some(a.duration_ms);
            msg.draft_error = None;
            msg.status = SmsStatus::Drafted;
        }
        Err(f) => {
            msg.draft_reply = None;
            msg.reply_model = None;
            msg.latency_ms = Some(started.elapsed().as_millis() as u64);
            msg.draft_error = Some(format!("{}: {}", f.error, f.attempts.join("; ")));
            msg.status = SmsStatus::NoModel;
        }
    }
}

/// With auto_reply on, open the composer for a fresh draft. Nothing is sent.
/// With the app bridge the server cannot open it; the page does when the user approves.
async fn maybe_open_composer(msg: &mut SmsMessage, cfg: &WorkModeConfig, bridge: SmsBridge) {
    if bridge == SmsBridge::Adb && cfg.auto_reply && msg.status == SmsStatus::Drafted {
        if let Some(d) = msg.draft_reply.clone() {
            if device_sms::dispatch_sms_to_phone(&msg.sender, &d).await.is_ok() {
                msg.status = SmsStatus::OpenedInComposer;
            }
        }
    }
}

async fn serve_mobile_web(
    opts: ServeOptions,
    registry: ModuleRegistry,
    vault_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let hardware = registry.context.hardware.clone();

    let (token, token_created) = auth::load_or_create_token(&opts.config_dir.join("token"))?;
    let work_config = WorkModeConfig::load(&opts.config_dir.join("workmode.json"))?;
    let lemonade = std::sync::Arc::new(LemonadeModule::new(opts.lemonade_config.clone()));

    // Read the phone inbox over ADB. Counts only are printed, never contents.
    // With the app bridge the app pushes its inbox to /api/sms/ingest instead.
    let mut initial_sms = match opts.sms_bridge {
        SmsBridge::App => {
            println!("Phone inbox: pushed by the NautiK9 app (no adb)");
            Vec::new()
        }
        SmsBridge::Adb => match device_sms::fetch_real_device_sms(5).await {
            Ok(msgs) => {
                println!("Phone inbox: read {} message(s) over adb", msgs.len());
                msgs
            }
            Err(e) => {
                println!("Phone inbox: unavailable ({})", e);
                Vec::new()
            }
        },
    };

    if work_config.enabled && !initial_sms.is_empty() {
        println!("Drafting replies through the configured model chain...");
        for msg in initial_sms.iter_mut() {
            apply_draft(msg, &lemonade, &work_config, &vault_path, hardware.recommended_embedding_dim).await;
            maybe_open_composer(msg, &work_config, opts.sms_bridge).await;
        }
        let none = initial_sms.iter().filter(|m| m.status == SmsStatus::NoModel).count();
        if none > 0 {
            println!("  {} message(s) have no draft: no model endpoint answered", none);
        }
    }

    let agent = match agent_api::load_mcp(&opts.config_dir).await {
        Ok((tools, local, notes)) => {
            for n in notes {
                println!("Agent: {n}");
            }
            agent_api::build_agent(&opts.config_dir, &vault_path, lemonade.clone(), (tools, local)).map(std::sync::Arc::new)
        }
        Err(e) => Err(e),
    };
    let state = AppState {
        registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
        hardware,
        vault_path,
        sms_messages: std::sync::Arc::new(tokio::sync::RwLock::new(initial_sms)),
        work_config: std::sync::Arc::new(tokio::sync::RwLock::new(work_config)),
        scheduler: match &agent {
            Ok(a) => match agent_api::heartbeat_minutes(&opts.config_dir) {
                Ok(hb) => Some(nautik9_agent::schedule::Runner::new(a.clone(), hb)),
                Err(e) => {
                    println!("Agent: scheduler NOT started: {e}");
                    None
                }
            },
            Err(_) => None,
        },
        agent,
        lemonade,
        fleet_url: opts.fleet_url.clone(),
        sms_bridge: opts.sms_bridge,
    };
    match &state.agent {
        Ok(_) => println!("Agent: ready (sessions in {})", opts.config_dir.join("sessions.db").display()),
        Err(e) => println!("Agent: NOT available: {e}"),
    }

    if let Some(r) = &state.scheduler {
        r.spawn();
        println!("Agent: scheduler running (ticks every 30 s; Jade/crontab, Jade/HEARTBEAT.md, log Jade/Logs/cron.log)");
    }
    let app = build_router(state, token.clone());

    let addr = format!("{}:{}", opts.bind, opts.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("=======================================================");
    println!("NautiK9 companion server listening on {}", addr);
    println!("  Local:  http://127.0.0.1:{}", opts.port);
    let lan = if opts.bind == "0.0.0.0" { lan_ip() } else { None };
    if let Some(ip) = &lan {
        println!("  LAN:    http://{}:{}", ip, opts.port);
    }
    println!("  Token:  {} ({})", opts.config_dir.join("token").display(), if token_created { "created now" } else { "existing" });
    if opts.print_token || std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        let host = lan.unwrap_or_else(|| "127.0.0.1".to_string());
        println!("  Open once: http://{}:{}/?token={}", host, opts.port, token);
    } else {
        println!("  (token URL not printed to a log; read the token file or start with --print-token)");
    }
    if opts.bind != "127.0.0.1" && opts.bind != "localhost" {
        println!("  WARNING: plain HTTP. The token and your data cross the network unencrypted.");
        println!("  Keep this on a trusted LAN or VPN; never port-forward it to the internet.");
    }
    println!("=======================================================");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_index() -> axum::response::Html<&'static str> {
    axum::response::Html(get_mobile_html())
}

async fn handle_health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true, "service": "nautik9", "auth": "bearer token required for /api/*" }))
}

async fn handle_providers(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    // API keys are never returned.
    let list: Vec<_> = state
        .lemonade
        .config
        .providers
        .iter()
        .map(|p| serde_json::json!({
            "name": p.name, "base_url": p.base_url, "model": p.model, "offline": p.offline,
            "has_api_key": p.api_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false),
        }))
        .collect();
    axum::Json(serde_json::json!({ "providers": list }))
}

async fn handle_asset_mack() -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    // The avatar is compiled in, so it works from any directory.
    static AVATAR: &[u8] = include_bytes!("../../../assets/mack_avatar.jpg");
    ([(header::CONTENT_TYPE, "image/jpeg")], AVATAR).into_response()
}

async fn handle_status(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let reg_guard = state.registry.lock().await;
    let avatar = reg_guard.context.avatar.read().await;
    axum::Json(serde_json::json!({
        // These come from the --device hint, not from probing hardware.
        "profile_source": "device-hint",
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
        "mode": "keyword",
        "hits": results.len(),
        "results": results
    }))
}

async fn handle_chat(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let prompt = payload["prompt"].as_str().unwrap_or("Status report");
    let offline = payload["offline"].as_bool().unwrap_or(false);

    match state
        .lemonade
        .chat(prompt, Some("You are Mighty Mack, a NautiDog and helpful companion (sometimes)."), offline)
        .await
    {
        Ok(a) => axum::Json(serde_json::json!({
            "response": a.reply,
            "offline": offline,
            "model_used": a.label(),
            "model": a.model,
            "provider": a.provider,
            "endpoint": a.endpoint,
            "duration_ms": a.duration_ms
        }))
        .into_response(),
        Err(f) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({
                "ok": false,
                "error": f.error,
                "attempts": f.attempts
            })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize, Default)]
struct SmsQuery {
    sync: Option<bool>,
}

async fn handle_sms_inbox(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(query): axum::extract::Query<SmsQuery>,
) -> axum::Json<serde_json::Value> {
    if query.sync.unwrap_or(false) && state.sms_bridge == SmsBridge::Adb {
        if let Ok(real_messages) = device_sms::fetch_real_device_sms(5).await {
            let work_cfg = state.work_config.read().await.clone();
            let mut list = state.sms_messages.write().await;
            for mut real_msg in real_messages {
                if !list.iter().any(|m| m.id == real_msg.id) {
                    if work_cfg.enabled {
                        apply_draft(&mut real_msg, &state.lemonade, &work_cfg, &state.vault_path, state.hardware.recommended_embedding_dim).await;
                        maybe_open_composer(&mut real_msg, &work_cfg, state.sms_bridge).await;
                    }
                    list.insert(0, real_msg);
                }
            }
        }
    }

    let messages = state.sms_messages.read().await.clone();
    let work_cfg = state.work_config.read().await.clone();
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
    if let Some(name) = payload["owner_name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        cfg.owner_name = name.to_string();
    }
    if let Some(nums) = payload["owner_numbers"].as_array() {
        cfg.owner_numbers = nums.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    }

    {
        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        if cfg.enabled {
            avatar.transition_to(
                nautik9_core::avatar::MackState::Attentive,
                format!("Work Mode ARMED (auto-open composer: {})", if cfg.auto_reply { "ON" } else { "OFF, approval needed" }),
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
    let sender = payload["sender"].as_str().unwrap_or("Unknown sender").to_string();
    let body = payload["body"].as_str().unwrap_or("").to_string();
    let msg_id = format!("sms-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let now = chrono::Local::now().format("%I:%M %p").to_string();

    let work_cfg = state.work_config.read().await.clone();

    let mut new_msg = SmsMessage {
        id: msg_id,
        sender,
        body,
        timestamp: now,
        draft_reply: None,
        draft_error: None,
        status: SmsStatus::Pending,
        reply_model: None,
        latency_ms: None,
    };

    if work_cfg.enabled {
        apply_draft(&mut new_msg, &state.lemonade, &work_cfg, &state.vault_path, state.hardware.recommended_embedding_dim).await;
        maybe_open_composer(&mut new_msg, &work_cfg, state.sms_bridge).await;

        let reg_guard = state.registry.lock().await;
        let mut avatar = reg_guard.context.avatar.write().await;
        avatar.transition_to(
            nautik9_core::avatar::MackState::TargetLocked,
            if new_msg.status == SmsStatus::NoModel {
                "Incoming text logged; no model answered, no draft".to_string()
            } else {
                "Mack drafted a reply for an incoming text".to_string()
            },
        );
    }

    state.sms_messages.write().await.insert(0, new_msg.clone());

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
    let work_cfg = state.work_config.read().await.clone();

    let mut list = state.sms_messages.write().await;
    let mut updated_msg = None;
    let mut dispatch_result = None;
    let mut composer = None;

    for msg in list.iter_mut() {
        if msg.id == id {
            if let Some(ed) = edited_draft {
                msg.draft_reply = Some(ed);
            }
            match action {
                "approve" => {
                    msg.status = SmsStatus::Approved;
                    match msg.draft_reply.clone().filter(|d| !d.trim().is_empty()) {
                        Some(draft_text) if state.sms_bridge == SmsBridge::App => {
                            match device_sms::clean_recipient(&msg.sender) {
                                Ok(to) => composer = Some(serde_json::json!({ "to": to, "body": draft_text })),
                                Err(e) => dispatch_result = Some(format!("Error: {}", e)),
                            }
                        }
                        Some(draft_text) => match device_sms::dispatch_sms_to_phone(&msg.sender, &draft_text).await {
                            Ok(res) => {
                                msg.status = SmsStatus::OpenedInComposer;
                                dispatch_result = Some(res);
                            }
                            Err(e) => dispatch_result = Some(format!("Error: {}", e)),
                        },
                        None => dispatch_result = Some("Error: there is no draft to open".to_string()),
                    }
                }
                "regenerate" => {
                    apply_draft(msg, &state.lemonade, &work_cfg, &state.vault_path, state.hardware.recommended_embedding_dim).await;
                }
                "ignore" => {
                    msg.status = SmsStatus::Ignored;
                }
                // The app reports that the phone's composer opened with the draft.
                "opened" if msg.status == SmsStatus::Approved => {
                    msg.status = SmsStatus::OpenedInComposer;
                    dispatch_result = Some(composer_opened_message(&msg.sender));
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
        "dispatch_result": dispatch_result,
        "composer": composer
    }))
}

#[derive(serde::Deserialize)]
struct IngestedSms {
    id: String,
    sender: String,
    body: String,
    date_ms: Option<i64>,
}

#[derive(serde::Deserialize)]
struct SmsIngest {
    messages: Vec<IngestedSms>,
}

/// The Android app pushes the newest inbox rows here (it holds READ_SMS; the server does not).
/// Rows already seen are skipped; new ones are drafted when work mode is on.
async fn handle_sms_ingest(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::Json(payload): axum::Json<SmsIngest>,
) -> axum::Json<serde_json::Value> {
    let work_cfg = state.work_config.read().await.clone();
    let mut added = 0;
    for row in payload.messages {
        let id = format!("sms-{}", row.id);
        if state.sms_messages.read().await.iter().any(|m| m.id == id) {
            continue;
        }
        let mut msg = SmsMessage {
            id,
            sender: row.sender,
            body: row.body,
            timestamp: device_sms::format_sms_time(row.date_ms),
            draft_reply: None,
            draft_error: None,
            status: SmsStatus::Pending,
            reply_model: None,
            latency_ms: None,
        };
        if work_cfg.enabled {
            apply_draft(&mut msg, &state.lemonade, &work_cfg, &state.vault_path, state.hardware.recommended_embedding_dim).await;
        }
        state.sms_messages.write().await.insert(0, msg);
        added += 1;
    }
    axum::Json(serde_json::json!({ "success": true, "added": added }))
}

/// Report the aggregator's real answer, or the real error. Nothing is invented.
async fn fleet_status_value(fleet_url: &str) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .unwrap_or_default();
    let url = format!("{}/api/fleet/nodes", fleet_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fleet aggregator {} unreachable: {}", fleet_url, if e.is_timeout() { "timed out".to_string() } else if e.is_connect() { "connection refused".to_string() } else { e.to_string() }))?;
    if !resp.status().is_success() {
        return Err(format!("fleet aggregator {} answered HTTP {}", fleet_url, resp.status().as_u16()));
    }
    let mut v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("fleet aggregator {} sent unreadable JSON: {}", fleet_url, e))?;
    match v.as_object_mut() {
        Some(o) => {
            o.insert("ok".into(), serde_json::json!(true));
            o.insert("source".into(), serde_json::json!(url));
            Ok(v)
        }
        None => Ok(serde_json::json!({ "ok": true, "source": url, "data": v })),
    }
}

async fn handle_fleet_status(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match fleet_status_value(&state.fleet_url).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({ "ok": false, "error": e }))).into_response(),
    }
}

async fn handle_decompose(
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let task = payload["task"].as_str().unwrap_or("Decompose mission");
    let steps = nautik9_wayfinder::decompose_task(task);
    axum::Json(serde_json::json!({
        "task": task,
        "kind": "fixed checklist, not model-generated",
        "steps": steps
    }))
}

#[derive(serde::Deserialize, Default)]
struct NotesQuery {
    tag: Option<String>,
}

fn open_vault(state: &AppState) -> WayfinderVault {
    WayfinderVault::new(&state.vault_path).unwrap_or_else(|_| WayfinderVault::in_memory().unwrap())
}

async fn handle_get_notes(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(q): axum::extract::Query<NotesQuery>,
) -> axum::Json<serde_json::Value> {
    let vault = open_vault(&state);
    let items = match q.tag.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => vault.notes_with_tag(t).await.unwrap_or_default(),
        None => vault.list_items().await.unwrap_or_default(),
    };
    axum::Json(serde_json::json!({
        "notes": items,
        "count": items.len()
    }))
}

async fn handle_backlinks(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::Json<serde_json::Value> {
    let vault = open_vault(&state);
    let back = vault.backlinks(&id).await.unwrap_or_default();
    let unresolved = vault.unresolved_links(&id).await.unwrap_or_default();
    let brief: Vec<_> = back.iter().map(|n| serde_json::json!({ "id": n.id, "title": n.title })).collect();
    axum::Json(serde_json::json!({
        "id": id,
        "backlinks": brief,
        "unresolved_links": unresolved
    }))
}

async fn handle_tags(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let tags = open_vault(&state).list_tags().await.unwrap_or_default();
    let list: Vec<_> = tags.iter().map(|(t, n)| serde_json::json!({ "tag": t, "count": n })).collect();
    axum::Json(serde_json::json!({ "tags": list }))
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
        .unwrap_or_default();
    // Explicit tags plus #tags found in the text; [[links]] are read from the text on every list.
    let all_tags = nautik9_wayfinder::merge_tags(&tags, content);
    let links = nautik9_wayfinder::parse_links(content);

    let vault = open_vault(&state);
    let emb = generate_fast_embedding(&format!("{} {}", title, content), state.hardware.recommended_embedding_dim);
    let ok = vault.put_item(&id, title, content, &tags, Some(&emb)).await.is_ok();

    axum::Json(serde_json::json!({
        "success": ok,
        "id": id,
        "title": title,
        "tags": all_tags,
        "links": links
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

/// Tree-of-thought reasoning is not implemented. The old endpoint returned a fixed
/// template with invented verifier results, so it now says so instead.
async fn handle_tot() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        axum::Json(serde_json::json!({
            "ok": false,
            "error": "Tree-of-thought reasoning is not implemented yet (planned). Use Chat for a model answer."
        })),
    )
        .into_response()
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


#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const TOKEN: &str = "t0ken-for-tests-t0ken-for-tests-0123456789";

    fn dead_lemonade() -> LemonadeModule {
        let mut cfg = LemonadeConfig::default();
        cfg.providers = vec![nautik9_lemonade::Provider {
            name: "dead".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: Some("m".into()),
            api_key: None,
            offline: false,
        }];
        cfg.timeout_seconds = 2;
        LemonadeModule::new(cfg)
    }

    fn app(dir: &std::path::Path) -> axum::Router {
        app_with_bridge(dir, SmsBridge::Adb)
    }

    fn app_with_bridge(dir: &std::path::Path, sms_bridge: SmsBridge) -> axum::Router {
        let hw = HardwareProfile::detect_or_scaffold(Some("s25ultra"));
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(ModuleRegistry::new(hw.clone()))),
            hardware: std::sync::Arc::new(hw),
            vault_path: dir.join("vault.db"),
            sms_messages: Default::default(),
            work_config: Default::default(),
            lemonade: std::sync::Arc::new(dead_lemonade()),
            fleet_url: "http://127.0.0.1:1".into(),
            sms_bridge,
            agent: agent_api::build_agent(dir, &dir.join("vault.db"), std::sync::Arc::new(dead_lemonade()), Default::default()).map(std::sync::Arc::new),
            scheduler: None,
        };
        let state = AppState { scheduler: state.agent.as_ref().ok().map(|a| nautik9_agent::schedule::Runner::new(a.clone(), 0)), ..state };
        build_router(state, TOKEN.to_string())
    }

    async fn call(app: &axum::Router, method: &str, uri: &str, bearer: Option<&str>, body: Option<serde_json::Value>) -> (StatusCode, serde_json::Value) {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {}", t));
        }
        let req = match body {
            Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn schedules_api_add_list_run_delete() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, _) = call(&a, "GET", "/api/agent/schedules", None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, v) = call(&a, "POST", "/api/agent/schedules", Some(TOKEN), Some(serde_json::json!({"spec":"whenever","prompt":"x"}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        let (s, v) = call(&a, "POST", "/api/agent/schedules", Some(TOKEN), Some(serde_json::json!({"spec":"daily at 08:00","prompt":"Morning brief"}))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let id = v["id"].as_str().unwrap().to_string();
        let (_, l) = call(&a, "GET", "/api/agent/schedules", Some(TOKEN), None).await;
        assert_eq!(l.as_array().unwrap().len(), 1);
        // No model endpoint in this test: the run is recorded as an error, not an answer.
        let (s, v) = call(&a, "POST", &format!("/api/agent/schedules/{id}/run"), Some(TOKEN), Some(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert!(v["last_result"].as_str().unwrap().starts_with("ERROR:"), "{v}");
        let (s, _) = call(&a, "POST", "/api/agent/schedules/nope/run", Some(TOKEN), Some(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(&a, "DELETE", &format!("/api/agent/schedules/{id}"), Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&a, "DELETE", &format!("/api/agent/schedules/{id}"), Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_requires_token_except_health() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, _) = call(&a, "GET", "/api/health", None, None).await;
        assert_eq!(s, StatusCode::OK);
        for uri in ["/api/sms/inbox", "/api/notes", "/api/status", "/api/fleet/status", "/api/tags", "/api/providers", "/assets/mack.jpg"] {
            let (s, _) = call(&a, "GET", uri, None, None).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri} must need a token");
        }
        let (s, _) = call(&a, "POST", "/api/chat", None, Some(serde_json::json!({"prompt":"hi"}))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = call(&a, "GET", "/api/sms/inbox", Some("wrong"), None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = call(&a, "GET", "/api/sms/inbox", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&a, "GET", &format!("/api/sms/inbox?token={}", TOKEN), None, None).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&a, "GET", "/api/sms/inbox?token=nope", None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn chat_is_502_with_no_invented_reply_when_all_endpoints_down() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, v) = call(&a, "POST", "/api/chat", Some(TOKEN), Some(serde_json::json!({"prompt":"Status of the boat"}))).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert_eq!(v["ok"], false);
        assert!(v.get("response").is_none());
        assert_eq!(v["attempts"].as_array().unwrap().len(), 1);
        // offline flag with no offline endpoint is also an honest error
        let (s, v) = call(&a, "POST", "/api/chat", Some(TOKEN), Some(serde_json::json!({"prompt":"hi","offline":true}))).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert!(v["error"].as_str().unwrap().contains("offline"));
    }

    #[tokio::test]
    async fn fleet_status_reports_aggregator_error_not_invented_nodes() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, v) = call(&a, "GET", "/api/fleet/status", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("unreachable"));
        assert!(v.get("nodes").is_none() && v.get("summary").is_none());
    }

    #[tokio::test]
    async fn fleet_status_passes_the_real_answer_through() {
        use axum::routing::get;
        let mock = axum::Router::new().route(
            "/api/fleet/nodes",
            get(|| async { axum::Json(serde_json::json!({"nodes":[{"name":"only-one","reachable":false}],"summary":{"nodes_total":1}})) }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, mock).await.unwrap() });
        let v = fleet_status_value(&format!("http://{}", addr)).await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["nodes"][0]["name"], "only-one");
        assert_eq!(v["nodes"][0]["reachable"], false);
    }

    #[tokio::test]
    async fn app_bridge_ingests_once_and_hands_the_composer_to_the_page() {
        let d = tempfile::tempdir().unwrap();
        let a = app_with_bridge(d.path(), SmsBridge::App);
        let rows = serde_json::json!({"messages":[
            {"id":"41","sender":"+1 (555) 000-2222","body":"on my way","date_ms":1761638400000i64},
            {"id":"42","sender":"+15550003333","body":"call me"}
        ]});
        let (s, v) = call(&a, "POST", "/api/sms/ingest", Some(TOKEN), Some(rows.clone())).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["added"], 2);
        let (_, v) = call(&a, "POST", "/api/sms/ingest", Some(TOKEN), Some(rows)).await;
        assert_eq!(v["added"], 0, "rows already seen are skipped");
        let (s, _) = call(&a, "POST", "/api/sms/ingest", None, Some(serde_json::json!({"messages":[]}))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);

        // Approve: the server does not open anything; it returns the composer for the app.
        let (_, v) = call(&a, "POST", "/api/sms/action", Some(TOKEN),
            Some(serde_json::json!({"id":"sms-41","action":"approve","edited_draft":"See you soon"}))).await;
        assert_eq!(v["message"]["status"], "Approved");
        assert_eq!(v["composer"]["to"], "+15550002222");
        assert_eq!(v["composer"]["body"], "See you soon");
        // Only the app's report moves it to OpenedInComposer.
        let (_, v) = call(&a, "POST", "/api/sms/action", Some(TOKEN), Some(serde_json::json!({"id":"sms-41","action":"opened"}))).await;
        assert_eq!(v["message"]["status"], "OpenedInComposer");
        // Work mode is on by default: the other text was drafted, and with no model it is NoModel.
        // "opened" does nothing for a message that was not approved.
        let (_, v) = call(&a, "POST", "/api/sms/action", Some(TOKEN), Some(serde_json::json!({"id":"sms-42","action":"opened"}))).await;
        assert_eq!(v["message"]["status"], "NoModel");
    }

    #[tokio::test]
    async fn incoming_text_with_no_model_has_no_draft_and_nomodel_status() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, v) = call(&a, "POST", "/api/sms/incoming", Some(TOKEN), Some(serde_json::json!({"sender":"+15550001111","body":"test text"}))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["message"]["status"], "NoModel");
        assert!(v["message"]["draft_reply"].is_null());
        assert!(v["message"]["reply_model"].is_null());
        assert!(v["message"]["draft_error"].as_str().unwrap().contains("no model endpoint answered"));
        // auto_reply is off by default, so no composer was opened
        let (_, inbox) = call(&a, "GET", "/api/sms/inbox", Some(TOKEN), None).await;
        assert_eq!(inbox["work_config"]["auto_reply"], false);
        assert_eq!(inbox["work_config"]["owner_name"], "Thom");
    }

    #[tokio::test]
    async fn workmode_owner_name_is_configurable() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (_, v) = call(&a, "POST", "/api/sms/workmode", Some(TOKEN), Some(serde_json::json!({"owner_name":"Sam","owner_numbers":["555-000-1234"]}))).await;
        assert_eq!(v["work_config"]["owner_name"], "Sam");
        assert_eq!(v["work_config"]["owner_numbers"][0], "555-000-1234");
    }

    #[tokio::test]
    async fn notes_links_tags_and_backlinks_over_the_api() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (_, v) = call(&a, "POST", "/api/notes", Some(TOKEN), Some(serde_json::json!({"id":"n1","title":"Alpha","content":"see [[Beta]] #sonar"}))).await;
        assert_eq!(v["links"][0], "Beta");
        assert_eq!(v["tags"][0], "sonar");
        call(&a, "POST", "/api/notes", Some(TOKEN), Some(serde_json::json!({"id":"n2","title":"Beta","content":"plain"}))).await;
        let (_, b) = call(&a, "GET", "/api/notes/n2/backlinks", Some(TOKEN), None).await;
        assert_eq!(b["backlinks"][0]["id"], "n1");
        let (_, t) = call(&a, "GET", "/api/tags", Some(TOKEN), None).await;
        assert_eq!(t["tags"][0]["tag"], "sonar");
        let (_, n) = call(&a, "GET", "/api/notes?tag=sonar", Some(TOKEN), None).await;
        assert_eq!(n["count"], 1);
    }

    #[tokio::test]
    async fn tot_is_not_faked() {
        let d = tempfile::tempdir().unwrap();
        let a = app(d.path());
        let (s, v) = call(&a, "POST", "/api/tot", Some(TOKEN), Some(serde_json::json!({"problem":"x"}))).await;
        assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(v["ok"], false);
        assert!(v.get("nodes").is_none());
    }

    #[test]
    fn ui_does_not_claim_a_send_or_ship_canned_engine_text() {
        let html = get_mobile_html();
        assert!(!html.contains("AutoSent"));
        assert!(!html.contains("AUTO-DISPATCHED"));
        assert!(!html.contains("100% Offline"));
    }
}
