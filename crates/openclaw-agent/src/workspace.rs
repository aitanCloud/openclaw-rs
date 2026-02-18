use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::debug;

/// Bootstrap files that form the agent's system prompt context
const BOOTSTRAP_FILES: &[&str] = &[
    "SOUL.md",
    "TOOLS.md",
    "AGENTS.md",
    "IDENTITY.md",
    "USER.md",
    "MEMORY.md",
];

/// Minimal set for cron/subagent sessions
const MINIMAL_BOOTSTRAP_FILES: &[&str] = &["AGENTS.md", "TOOLS.md"];

#[derive(Debug, Clone)]
pub struct BootstrapFile {
    pub name: String,
    pub content: String,
}

#[derive(Debug)]
pub struct WorkspaceContext {
    pub dir: PathBuf,
    pub bootstrap_files: Vec<BootstrapFile>,
    pub system_prompt: String,
}

/// Cached workspace context entry
struct CachedWorkspace {
    dir: PathBuf,
    minimal: bool,
    bootstrap_files: Vec<BootstrapFile>,
    system_prompt_base: String, // without runtime timestamp
    file_mtimes: Vec<(String, Option<SystemTime>)>,
}

static WORKSPACE_CACHE: Mutex<Option<CachedWorkspace>> = Mutex::new(None);

/// Check if any bootstrap files have been modified since last cache
fn files_changed(dir: &Path, cached: &CachedWorkspace) -> bool {
    for (filename, cached_mtime) in &cached.file_mtimes {
        let path = dir.join(filename);
        let current_mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        if &current_mtime != cached_mtime {
            return true;
        }
    }
    false
}

/// Load workspace bootstrap files and assemble the system prompt.
/// Uses an in-memory cache — only re-reads files if modification times have changed.
pub async fn load_workspace(dir: &Path, minimal: bool) -> Result<WorkspaceContext> {
    let files_to_load = if minimal {
        MINIMAL_BOOTSTRAP_FILES
    } else {
        BOOTSTRAP_FILES
    };

    // Check cache
    {
        let cache = WORKSPACE_CACHE.lock().unwrap();
        if let Some(ref cached) = *cache {
            if cached.dir == dir && cached.minimal == minimal && !files_changed(dir, cached) {
                // Cache hit — just update the runtime timestamp
                let now = chrono::Local::now();
                let runtime = format!(
                    "<!-- runtime -->\nCurrent date/time: {}",
                    now.format("%A, %B %e, %Y — %l:%M %p %Z")
                );
                let system_prompt = format!("{}\n\n{}", cached.system_prompt_base, runtime);
                debug!("Workspace cache hit ({} files)", cached.bootstrap_files.len());
                return Ok(WorkspaceContext {
                    dir: dir.to_path_buf(),
                    bootstrap_files: cached.bootstrap_files.clone(),
                    system_prompt,
                });
            }
        }
    }

    debug!("Workspace cache miss — reading files from {}", dir.display());

    let mut bootstrap_files = Vec::new();
    let mut file_mtimes = Vec::new();

    for &filename in files_to_load {
        let file_path = dir.join(filename);
        let mtime = std::fs::metadata(&file_path).ok().and_then(|m| m.modified().ok());
        file_mtimes.push((filename.to_string(), mtime));

        match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => {
                let content = strip_frontmatter(&content);
                if !content.trim().is_empty() {
                    bootstrap_files.push(BootstrapFile {
                        name: filename.to_string(),
                        content,
                    });
                }
            }
            Err(_) => {
                // Optional files — skip if missing
            }
        }
    }

    let system_prompt_base = assemble_system_prompt_base(&bootstrap_files);
    let system_prompt = assemble_system_prompt(&bootstrap_files);

    // Update cache
    {
        let mut cache = WORKSPACE_CACHE.lock().unwrap();
        *cache = Some(CachedWorkspace {
            dir: dir.to_path_buf(),
            minimal,
            bootstrap_files: bootstrap_files.clone(),
            system_prompt_base,
            file_mtimes,
        });
    }

    Ok(WorkspaceContext {
        dir: dir.to_path_buf(),
        bootstrap_files,
        system_prompt,
    })
}

/// Strip YAML frontmatter (--- ... ---) from markdown content
fn strip_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return content.to_string();
    }
    let rest = &content[3..];
    match rest.find("\n---") {
        Some(end) => {
            let start = end + "\n---".len();
            rest[start..].trim_start().to_string()
        }
        None => content.to_string(),
    }
}

/// Assemble bootstrap files into a base prompt (without runtime timestamp)
fn assemble_system_prompt_base(files: &[BootstrapFile]) -> String {
    if files.is_empty() {
        return "You are a helpful AI assistant.".to_string();
    }

    let mut parts = Vec::new();
    for file in files {
        parts.push(format!(
            "<!-- {} -->\n{}",
            file.name,
            file.content.trim()
        ));
    }
    parts.join("\n\n")
}

/// Assemble bootstrap files into a single system prompt (with runtime timestamp)
fn assemble_system_prompt(files: &[BootstrapFile]) -> String {
    let base = assemble_system_prompt_base(files);
    if files.is_empty() {
        return base;
    }

    let now = chrono::Local::now();
    format!(
        "{}\n\n<!-- runtime -->\nCurrent date/time: {}",
        base,
        now.format("%A, %B %e, %Y — %l:%M %p %Z")
    )
}

/// Resolve the workspace directory for a given agent
pub fn resolve_workspace_dir(agent_name: &str) -> PathBuf {
    let home = dirs::home_dir().expect("Could not determine home directory");
    if agent_name == "main" {
        home.join(".openclaw").join("workspace")
    } else {
        home.join(".openclaw").join(format!("workspace-{}", agent_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_frontmatter() {
        let content = "---\ndescription: test\n---\nActual content here";
        assert_eq!(strip_frontmatter(content), "Actual content here");
    }

    #[test]
    fn test_strip_frontmatter_none() {
        let content = "# Just markdown\nNo frontmatter";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_assemble_empty() {
        let prompt = assemble_system_prompt(&[]);
        assert_eq!(prompt, "You are a helpful AI assistant.");
    }

    #[test]
    fn test_assemble_with_files() {
        let files = vec![
            BootstrapFile {
                name: "SOUL.md".to_string(),
                content: "You are Aitan.".to_string(),
            },
            BootstrapFile {
                name: "TOOLS.md".to_string(),
                content: "You have exec, read, write tools.".to_string(),
            },
        ];
        let prompt = assemble_system_prompt(&files);
        assert!(prompt.contains("<!-- SOUL.md -->"));
        assert!(prompt.contains("You are Aitan."));
        assert!(prompt.contains("<!-- TOOLS.md -->"));
        assert!(prompt.contains("exec, read, write"));
        assert!(prompt.contains("<!-- runtime -->"));
    }
}
