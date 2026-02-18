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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Sessions { action }) => commands::sessions::run(action),
        Some(Commands::Skills { action }) => commands::skills::run(action),
        Some(Commands::Config { action }) => commands::config::run(action),
        Some(Commands::Cron { action }) => commands::cron::run(action),
        None => {
            // No subcommand — print version like the original
            println!("openclaw-rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
