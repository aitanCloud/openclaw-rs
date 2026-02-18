use anyhow::Result;
use std::path::{Path, PathBuf};

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

#[derive(Debug)]
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

/// Load workspace bootstrap files and assemble the system prompt
pub async fn load_workspace(dir: &Path, minimal: bool) -> Result<WorkspaceContext> {
    let files_to_load = if minimal {
        MINIMAL_BOOTSTRAP_FILES
    } else {
        BOOTSTRAP_FILES
    };

    let mut bootstrap_files = Vec::new();

    for &filename in files_to_load {
        let file_path = dir.join(filename);
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

    let system_prompt = assemble_system_prompt(&bootstrap_files);

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

/// Assemble bootstrap files into a single system prompt
fn assemble_system_prompt(files: &[BootstrapFile]) -> String {
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

    // Add runtime context
    let now = chrono::Local::now();
    parts.push(format!(
        "<!-- runtime -->\nCurrent date/time: {}",
        now.format("%A, %B %e, %Y — %l:%M %p %Z")
    ));

    parts.join("\n\n")
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
