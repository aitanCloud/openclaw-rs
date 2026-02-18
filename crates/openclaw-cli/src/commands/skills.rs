use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use openclaw_core::paths;
use openclaw_core::skills;

#[derive(Subcommand)]
pub enum SkillAction {
    /// List all installed skills
    List,
}

pub fn run(action: SkillAction) -> Result<()> {
    match action {
        SkillAction::List => list_skills(),
    }
}

fn list_skills() -> Result<()> {
    let skills_dir = paths::skills_dir();
    let skills = skills::list_skills(&skills_dir)?;

    if skills.is_empty() {
        println!("{}", "No skills found.".dimmed());
        return Ok(());
    }

    println!("{} ({} skill{})\n", "Skills".bold(), skills.len(), if skills.len() == 1 { "" } else { "s" });

    for skill in &skills {
        let status = if skill.has_skill_md {
            "ready".green()
        } else {
            "no SKILL.md".yellow()
        };

        let desc = skill
            .description
            .as_deref()
            .unwrap_or("No description");

        println!(
            "  {} {} [{}]",
            "●".green(),
            skill.name.bold(),
            status
        );
        println!("    {}", desc.dimmed());
    }

    Ok(())
}
