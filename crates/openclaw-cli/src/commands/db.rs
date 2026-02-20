use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;

#[derive(Subcommand)]
pub enum DbAction {
    /// Record a deployment
    Deploy {
        /// Host name (e.g. "g731", "nixos")
        #[arg(long)]
        host: String,
        /// Component (e.g. "gateway", "cli")
        #[arg(long)]
        component: String,
        /// Version string
        #[arg(long)]
        version: String,
        /// Path to binary
        #[arg(long)]
        binary: Option<String>,
        /// Systemd service name
        #[arg(long)]
        service: Option<String>,
        /// Git commit hash
        #[arg(long)]
        commit: Option<String>,
        /// Notes
        #[arg(long)]
        notes: Option<String>,
    },
    /// Add a known issue
    Issue {
        /// Issue title
        title: String,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
        /// Severity: low, medium, high, critical
        #[arg(short, long, default_value = "medium")]
        severity: String,
        /// Component: gateway, agent, cli, core, infra
        #[arg(short, long)]
        component: Option<String>,
        /// Workaround text
        #[arg(short, long)]
        workaround: Option<String>,
    },
    /// Resolve a known issue by ID
    Resolve {
        /// Issue ID
        id: i64,
    },
    /// Log a completed task
    Task {
        /// Task description
        description: String,
        /// Category: feature, bugfix, refactor, deploy, config
        #[arg(short, long)]
        category: Option<String>,
        /// Version tag
        #[arg(short, long)]
        version: Option<String>,
        /// Git commit
        #[arg(long)]
        commit: Option<String>,
    },
    /// Set a project context key-value pair
    Set {
        /// Key name
        key: String,
        /// JSON value
        value: String,
    },
    /// Get a project context value
    Get {
        /// Key name
        key: String,
    },
    /// Snapshot the current config to Postgres
    Snapshot {
        /// Source of change: manual, watchdog, windsurf, cli
        #[arg(short, long, default_value = "manual")]
        source: String,
    },
    /// Show connection status
    Status,
}

pub async fn run(action: DbAction) -> Result<()> {
    let pool = match openclaw_db::pool() {
        Some(p) => p,
        None => {
            eprintln!("{}", "Postgres not connected. Set DATABASE_URL env var.".red());
            std::process::exit(1);
        }
    };

    match action {
        DbAction::Deploy { host, component, version, binary, service, commit, notes } => {
            let id = openclaw_db::context::record_deployment(
                pool,
                &host,
                &component,
                &version,
                binary.as_deref(),
                service.as_deref(),
                commit.as_deref(),
                notes.as_deref(),
            ).await?;
            println!("{} Deployment #{} recorded: {} {} v{}", "✓".green(), id, host, component, version);
        }

        DbAction::Issue { title, description, severity, component, workaround } => {
            let id = openclaw_db::context::add_issue(
                pool,
                &title,
                description.as_deref(),
                &severity,
                component.as_deref(),
                workaround.as_deref(),
            ).await?;
            println!("{} Issue #{} created: [{}] {}", "✓".green(), id, severity.to_uppercase(), title);
        }

        DbAction::Resolve { id } => {
            let resolved = openclaw_db::context::resolve_issue(pool, id).await?;
            if resolved {
                println!("{} Issue #{} resolved", "✓".green(), id);
            } else {
                println!("{} Issue #{} not found or already resolved", "✗".red(), id);
            }
        }

        DbAction::Task { description, category, version, commit } => {
            let id = openclaw_db::context::log_task(
                pool,
                &description,
                category.as_deref(),
                &[],
                version.as_deref(),
                commit.as_deref(),
            ).await?;
            println!("{} Task #{} logged: {}", "✓".green(), id, description);
        }

        DbAction::Set { key, value } => {
            let json_value: serde_json::Value = serde_json::from_str(&value)
                .unwrap_or_else(|_| serde_json::Value::String(value.clone()));
            openclaw_db::context::set_context(pool, &key, &json_value).await?;
            println!("{} Set {} = {}", "✓".green(), key.bold(), value);
        }

        DbAction::Get { key } => {
            match openclaw_db::context::get_context(pool, &key).await? {
                Some(val) => println!("{}", serde_json::to_string_pretty(&val)?),
                None => println!("{} Key '{}' not found", "✗".yellow(), key),
            }
        }

        DbAction::Snapshot { source } => {
            let config_path = openclaw_core::paths::manual_config_path();
            if !config_path.exists() {
                anyhow::bail!("Config not found at {}", config_path.display());
            }
            let content = std::fs::read_to_string(&config_path)?;
            let config_json: serde_json::Value = serde_json::from_str(&content)?;

            // Detect changed keys vs last snapshot
            let changed_keys = detect_changed_keys(pool, &config_json).await;

            let id = openclaw_db::context::snapshot_config(
                pool,
                &config_path.display().to_string(),
                &config_json,
                &changed_keys,
                &source,
            ).await?;

            if changed_keys.is_empty() {
                println!("{} Config snapshot #{} saved (no changes detected)", "✓".green(), id);
            } else {
                println!("{} Config snapshot #{} saved — changed keys: {}", "✓".green(), id,
                    changed_keys.join(", ").yellow());
            }
        }

        DbAction::Status => {
            println!("{} Connected to Postgres", "✓".green());
            let row: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM deployments")
                .fetch_one(pool).await?;
            let issues: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM known_issues WHERE status = 'open'")
                .fetch_one(pool).await?;
            let tasks: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM task_log")
                .fetch_one(pool).await?;
            let llm: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM llm_calls")
                .fetch_one(pool).await?;
            let sessions: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM sessions")
                .fetch_one(pool).await?;
            let snapshots: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM config_snapshots")
                .fetch_one(pool).await?;
            let metrics: (i64,) = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM metrics_snapshots")
                .fetch_one(pool).await?;

            println!("  Deployments:      {}", row.0);
            println!("  Open issues:      {}", issues.0);
            println!("  Tasks logged:     {}", tasks.0);
            println!("  LLM calls:        {}", llm.0);
            println!("  Sessions:         {}", sessions.0);
            println!("  Config snapshots: {}", snapshots.0);
            println!("  Metrics samples:  {}", metrics.0);
        }
    }

    Ok(())
}

/// Compare current config JSON against the last snapshot to find changed top-level keys
async fn detect_changed_keys(pool: &sqlx::PgPool, current: &serde_json::Value) -> Vec<String> {
    let prev: Option<(serde_json::Value,)> = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT config_json FROM config_snapshots ORDER BY snapshot_at DESC LIMIT 1"
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let Some((prev_json,)) = prev else {
        // First snapshot — all keys are "changed"
        return current.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
    };

    let mut changed = Vec::new();
    if let (Some(curr_obj), Some(prev_obj)) = (current.as_object(), prev_json.as_object()) {
        for (key, curr_val) in curr_obj {
            match prev_obj.get(key) {
                Some(prev_val) if prev_val == curr_val => {}
                _ => changed.push(key.clone()),
            }
        }
        // Keys removed
        for key in prev_obj.keys() {
            if !curr_obj.contains_key(key) {
                changed.push(key.clone());
            }
        }
    }

    changed
}
