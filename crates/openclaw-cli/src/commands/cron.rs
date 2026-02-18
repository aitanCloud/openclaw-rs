use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use openclaw_core::cron;
use openclaw_core::paths;

#[derive(Subcommand)]
pub enum CronAction {
    /// List all cron jobs
    List,
}

pub fn run(action: CronAction) -> Result<()> {
    match action {
        CronAction::List => list_cron_jobs(),
    }
}

fn list_cron_jobs() -> Result<()> {
    let cron_path = paths::cron_jobs_path();

    if !cron_path.exists() {
        println!("{}", "No cron jobs configured.".dimmed());
        return Ok(());
    }

    let cron_file = cron::load_cron_jobs(&cron_path)?;

    if cron_file.jobs.is_empty() {
        println!("{}", "No cron jobs configured.".dimmed());
        return Ok(());
    }

    println!(
        "{} ({} job{})\n",
        "Cron Jobs".bold(),
        cron_file.jobs.len(),
        if cron_file.jobs.len() == 1 { "" } else { "s" }
    );

    for job in &cron_file.jobs {
        let status_icon = if job.enabled {
            "●".green()
        } else {
            "○".dimmed()
        };

        let status_text = if job.enabled {
            "enabled".green()
        } else {
            "disabled".dimmed()
        };

        println!("  {} {} [{}]", status_icon, job.name.bold(), status_text);
        println!("    {} {}", "Schedule:".dimmed(), job.schedule);

        if let Some(model) = &job.payload.model {
            println!("    {} {}", "Model:".dimmed(), model);
        }

        if let Some(state) = &job.state {
            if let Some(status) = &state.last_status {
                let status_colored = match status.as_str() {
                    "ok" => status.green(),
                    "error" => status.red(),
                    _ => status.normal(),
                };
                println!("    {} {}", "Last run:".dimmed(), status_colored);
            }
            if let Some(duration) = state.last_duration_ms {
                let secs = duration as f64 / 1000.0;
                println!("    {} {:.1}s", "Duration:".dimmed(), secs);
            }
            if let Some(errors) = state.consecutive_errors {
                if errors > 0 {
                    println!(
                        "    {} {}",
                        "Errors:".dimmed(),
                        format!("{} consecutive", errors).red()
                    );
                }
            }
        }
        println!();
    }

    Ok(())
}
