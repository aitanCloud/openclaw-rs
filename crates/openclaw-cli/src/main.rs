mod commands;

use clap::{Parser, Subcommand};

/// OpenClaw CLI — fast Rust replacement
#[derive(Parser)]
#[command(name = "openclaw", version, about = "OpenClaw AI agent platform (Rust CLI)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// List and manage sessions
    Sessions {
        #[command(subcommand)]
        action: commands::sessions::SessionAction,
    },
    /// List and manage skills
    Skills {
        #[command(subcommand)]
        action: commands::skills::SkillAction,
    },
    /// Show configuration
    Config {
        #[command(subcommand)]
        action: commands::config::ConfigAction,
    },
    /// List and manage cron jobs
    Cron {
        #[command(subcommand)]
        action: commands::cron::CronAction,
    },
    /// Run one agent turn with tools (exec, read, write)
    Agent {
        /// The message to send
        #[arg(short, long)]
        message: String,
        /// Agent name
        #[arg(long, default_value = "main")]
        agent: String,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// API key override
        #[arg(long, env = "MOONSHOT_API_KEY")]
        api_key: Option<String>,
        /// Base URL override
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Send a raw chat message to an LLM (no tools, no workspace context)
    Chat {
        /// The message to send
        message: String,
        /// API key (or set MOONSHOT_API_KEY env var)
        #[arg(long, env = "MOONSHOT_API_KEY")]
        api_key: Option<String>,
        /// Base URL for the API
        #[arg(long, default_value = "https://api.moonshot.ai/v1")]
        base_url: String,
        /// Model to use
        #[arg(long, default_value = "kimi-k2.5")]
        model: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Sessions { action }) => commands::sessions::run(action),
        Some(Commands::Skills { action }) => commands::skills::run(action),
        Some(Commands::Config { action }) => commands::config::run(action),
        Some(Commands::Cron { action }) => commands::cron::run(action),
        Some(Commands::Agent { message, agent, model, api_key, base_url }) => {
            commands::agent::run(commands::agent::AgentOptions {
                message,
                agent,
                model,
                api_key,
                base_url,
            })
            .await
        }
        Some(Commands::Chat { message, api_key, base_url, model }) => {
            let key = api_key.ok_or_else(|| {
                anyhow::anyhow!("API key required. Set MOONSHOT_API_KEY or pass --api-key")
            })?;
            commands::chat::run(&message, &key, &base_url, &model).await
        }
        None => {
            println!("openclaw-rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
