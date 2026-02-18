use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use openclaw_core::paths;
use openclaw_core::sessions;

#[derive(Subcommand)]
pub enum SessionAction {
    /// List all sessions across agents
    List {
        /// Filter by agent name
        #[arg(short, long)]
        agent: Option<String>,
    },
}

pub fn run(action: SessionAction) -> Result<()> {
    match action {
        SessionAction::List { agent } => list_sessions(agent),
    }
}

fn list_sessions(agent_filter: Option<String>) -> Result<()> {
    let agents = sessions::list_agents_with_sessions()?;

    if agents.is_empty() {
        println!("{}", "No agents with sessions found.".dimmed());
        return Ok(());
    }

    let agents_to_show: Vec<&String> = match &agent_filter {
        Some(filter) => agents.iter().filter(|a| a == &filter).collect(),
        None => agents.iter().collect(),
    };

    if agents_to_show.is_empty() {
        if let Some(filter) = &agent_filter {
            println!("{} Agent '{}' not found.", "✗".red(), filter);
        }
        return Ok(());
    }

    for agent_name in agents_to_show {
        let sessions_path = paths::agent_sessions_dir(agent_name).join("sessions.json");
        let sessions_file = sessions::load_sessions(&sessions_path)?;

        println!(
            "{} {} ({} session{})",
            "●".green(),
            agent_name.bold(),
            sessions_file.sessions.len(),
            if sessions_file.sessions.len() == 1 { "" } else { "s" }
        );

        for session in &sessions_file.sessions {
            let label = session
                .label
                .as_deref()
                .unwrap_or(&session.key);
            let model = session
                .model
                .as_deref()
                .unwrap_or("default");
            let msgs = session
                .message_count
                .map(|c| format!("{} msgs", c))
                .unwrap_or_default();

            println!(
                "  {} {} {} {}",
                "→".dimmed(),
                label,
                model.dimmed(),
                msgs.dimmed()
            );
        }
        println!();
    }

    Ok(())
}
