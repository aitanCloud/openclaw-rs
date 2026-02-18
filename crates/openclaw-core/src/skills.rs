use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub path: std::path::PathBuf,
    pub has_skill_md: bool,
}

/// Parse SKILL.md frontmatter to extract description
fn parse_skill_description(content: &str) -> Option<String> {
    // SKILL.md uses YAML frontmatter: ---\ndescription: ...\n---
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("---")?;
    let frontmatter = &rest[..end];

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(desc) = line.strip_prefix("description:") {
            return Some(desc.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

/// List all skills in the skills directory
pub fn list_skills(skills_dir: &Path) -> Result<Vec<Skill>> {
    if !skills_dir.exists() {
        return Ok(vec![]);
    }

    let mut skills = Vec::new();
    for entry in std::fs::read_dir(skills_dir)
        .with_context(|| format!("Failed to read skills dir: {}", skills_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let skill_md = entry.path().join("SKILL.md");
        let has_skill_md = skill_md.exists();
        let description = if has_skill_md {
            std::fs::read_to_string(&skill_md)
                .ok()
                .and_then(|c| parse_skill_description(&c))
        } else {
            None
        };

        if let Some(name) = entry.file_name().to_str() {
            skills.push(Skill {
                name: name.to_string(),
                description,
                path: entry.path(),
                has_skill_md,
            });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_description() {
        let content = "---\ndescription: Search the web using Brave\n---\nInstructions here";
        assert_eq!(
            parse_skill_description(content),
            Some("Search the web using Brave".to_string())
        );
    }

    #[test]
    fn test_parse_skill_description_quoted() {
        let content = "---\ndescription: \"Manage GitHub repos\"\n---\n";
        assert_eq!(
            parse_skill_description(content),
            Some("Manage GitHub repos".to_string())
        );
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let content = "# Just a markdown file\nNo frontmatter here";
        assert_eq!(parse_skill_description(content), None);
    }
}
