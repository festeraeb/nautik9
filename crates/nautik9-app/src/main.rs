use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use nautik9_core::{
    error::NautikError,
    event::{EventPayload, NautikEvent},
    hardware::HardwareProfile,
    module::{ModuleContext, ModuleRegistry, NautikModule},
};
use nautik9_fleet::{FleetConfig, FleetModule};
use nautik9_lemonade::{LemonadeConfig, LemonadeModule};
use nautik9_wayfinder::{generate_fast_embedding, WayfinderModule, WayfinderVault};

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
    }

    registry.shutdown_all().await?;
    Ok(())
}
