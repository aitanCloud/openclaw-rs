use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use openclaw_core::config;
use openclaw_core::paths;

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current configuration summary
    Show,
    /// Print the raw config file path
    Path,
}

pub fn run(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Show => show_config(),
        ConfigAction::Path => {
            println!("{}", paths::manual_config_path().display());
            Ok(())
        }
    }
}

fn show_config() -> Result<()> {
    let config_path = paths::manual_config_path();

    if !config_path.exists() {
        println!("{} Config not found at {}", "✗".red(), config_path.display());
        return Ok(());
    }

    let cfg = config::load_manual_config(&config_path)?;

    println!("{}", "OpenClaw Configuration".bold());
    println!("{}", "─".repeat(40).dimmed());

    if let Some(name) = &cfg.meta.name {
        println!("  {} {}", "Name:".dimmed(), name);
    }

    // Gateway
    println!("\n{}", "Gateway".bold());
    if let Some(mode) = &cfg.gateway.mode {
        println!("  {} {}", "Mode:".dimmed(), mode);
    }
    if let Some(bind) = &cfg.gateway.bind {
        println!("  {} {}", "Bind:".dimmed(), bind);
    }
    if let Some(port) = cfg.gateway.port {
        println!("  {} {}", "Port:".dimmed(), port);
    }
    if let Some(auth) = &cfg.gateway.auth {
        println!("  {} {}", "Auth:".dimmed(), auth);
    }

    // Agent defaults
    println!("\n{}", "Agent Defaults".bold());
    if let Some(timeout) = cfg.agents.defaults.timeout_seconds {
        println!("  {} {}s", "Timeout:".dimmed(), timeout);
    }
    if let Some(max) = cfg.agents.defaults.max_concurrent {
        println!("  {} {}", "Max Concurrent:".dimmed(), max);
    }

    println!("\n  {} {}", "Config file:".dimmed(), config_path.display());

    Ok(())
}
