use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use openclaw_core::models;
use openclaw_core::paths;

#[derive(Subcommand)]
pub enum ModelAction {
    /// List all configured providers and models
    List {
        /// Show all models (not just first per provider)
        #[arg(short, long)]
        all: bool,
    },
    /// Show the fallback chain order
    Fallback,
    /// Scan providers for reachability (ping base URLs)
    Scan,
}

pub async fn run(action: ModelAction) -> Result<()> {
    let config_path = paths::manual_config_path();
    if !config_path.exists() {
        anyhow::bail!("Config not found at {}", config_path.display());
    }

    match action {
        ModelAction::List { all } => list_models(&config_path, all),
        ModelAction::Fallback => show_fallback(&config_path),
        ModelAction::Scan => scan_providers(&config_path).await,
    }
}

fn list_models(config_path: &std::path::Path, show_all: bool) -> Result<()> {
    let providers = models::load_providers(config_path)?;

    if providers.is_empty() {
        println!("{}", "No providers configured.".dimmed());
        return Ok(());
    }

    let total_models: usize = providers.iter().map(|p| p.model_count).sum();
    println!(
        "{} ({} provider{}, {} model{})\n",
        "Models".bold(),
        providers.len(),
        if providers.len() == 1 { "" } else { "s" },
        total_models,
        if total_models == 1 { "" } else { "s" },
    );

    for provider in &providers {
        let local_tag = if provider.api == "ollama"
            || provider.base_url.contains("localhost")
            || provider.base_url.contains("127.0.0.1")
        {
            " [local]".green().to_string()
        } else {
            String::new()
        };

        println!(
            "  {} {} ({}){}",
            "●".green(),
            provider.name.bold(),
            provider.base_url.dimmed(),
            local_tag,
        );

        let models_to_show = if show_all {
            &provider.models[..]
        } else {
            &provider.models[..provider.models.len().min(3)]
        };

        for model in models_to_show {
            let mut tags = Vec::new();
            if model.reasoning {
                tags.push("reasoning".yellow().to_string());
            }
            if let Some(ctx) = model.context_window {
                tags.push(format!("{}k ctx", ctx / 1024).dimmed().to_string());
            }
            if model.local {
                tags.push("free".green().to_string());
            }

            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" ({})", tags.join(", "))
            };

            println!(
                "    {} {} {}{}",
                "→".dimmed(),
                model.id.cyan(),
                model.display_name.dimmed(),
                tag_str,
            );
        }

        if !show_all && provider.models.len() > 3 {
            println!(
                "    {} +{} more (use --all to show)",
                "…".dimmed(),
                provider.models.len() - 3,
            );
        }
        println!();
    }

    Ok(())
}

fn show_fallback(config_path: &std::path::Path) -> Result<()> {
    let chain = models::load_fallback_chain(config_path)?;

    if chain.is_empty() {
        println!(
            "{}\n  {}",
            "Fallback Chain".bold(),
            "No explicit fallback order configured. Using default: ollama → moonshot → openai-compatible"
                .dimmed()
        );
        return Ok(());
    }

    println!("{}\n", "Fallback Chain".bold());
    for (i, model_spec) in chain.iter().enumerate() {
        let marker = match i {
            0 => "🥇",
            1 => "🥈",
            2 => "🥉",
            _ => "  ",
        };
        println!("  {} {}", marker, model_spec.yellow());
    }
    println!(
        "\n  {}",
        "Circuit breaker: >3 consecutive failures = provider skipped".dimmed()
    );

    Ok(())
}

async fn scan_providers(config_path: &std::path::Path) -> Result<()> {
    let providers = models::load_providers(config_path)?;

    if providers.is_empty() {
        println!("{}", "No providers configured.".dimmed());
        return Ok(());
    }

    println!("{}\n", "Scanning providers…".bold());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    for provider in &providers {
        let url = if provider.api == "ollama" {
            format!("{}/api/tags", provider.base_url)
        } else {
            format!("{}/models", provider.base_url)
        };

        let t0 = std::time::Instant::now();
        let result = client.get(&url).send().await;
        let ms = t0.elapsed().as_millis();

        match result {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 401 => {
                let status_text = if resp.status().is_success() {
                    "reachable".green()
                } else {
                    "auth required".yellow()
                };
                println!(
                    "  {} {} — {} ({}ms)",
                    "✅".green(),
                    provider.name.bold(),
                    status_text,
                    ms,
                );
            }
            Ok(resp) => {
                println!(
                    "  {} {} — {} ({}ms)",
                    "⚠️",
                    provider.name.bold(),
                    format!("HTTP {}", resp.status()).yellow(),
                    ms,
                );
            }
            Err(e) => {
                let reason = if e.is_timeout() {
                    "timeout".to_string()
                } else if e.is_connect() {
                    "connection refused".to_string()
                } else {
                    format!("{}", e)
                };
                println!(
                    "  {} {} — {}",
                    "❌".red(),
                    provider.name.bold(),
                    reason.red(),
                );
            }
        }
    }

    Ok(())
}
