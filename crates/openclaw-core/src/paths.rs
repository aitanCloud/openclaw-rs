use std::path::PathBuf;

/// Returns the OpenClaw home directory (~/.openclaw)
pub fn openclaw_home() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".openclaw")
}

/// Returns the path to openclaw-manual.json
pub fn manual_config_path() -> PathBuf {
    openclaw_home().join("openclaw-manual.json")
}

/// Returns the path to cron/jobs.json
pub fn cron_jobs_path() -> PathBuf {
    openclaw_home().join("cron").join("jobs.json")
}

/// Returns the agents directory (~/.openclaw/agents/)
pub fn agents_dir() -> PathBuf {
    openclaw_home().join("agents")
}

/// Returns the workspace directory (~/.openclaw/workspace/)
pub fn workspace_dir() -> PathBuf {
    openclaw_home().join("workspace")
}

/// Returns the skills directory (~/.openclaw/workspace/skills/)
pub fn skills_dir() -> PathBuf {
    workspace_dir().join("skills")
}

/// Returns sessions directory for a given agent
pub fn agent_sessions_dir(agent_name: &str) -> PathBuf {
    agents_dir().join(agent_name).join("sessions")
}

/// Returns the agent config path
pub fn agent_config_path(agent_name: &str) -> PathBuf {
    agents_dir().join(agent_name).join("agent").join("config.json")
}
