use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENTS_SKILL_MD: &str = include_str!("../../../.agents/skills/pcb/SKILL.md");
const AGENTS_MCP_JSON: &str = include_str!("../../../.agents/skills/pcb/mcp.json");

#[derive(Args, Debug)]
#[command(about = "Run internal commands")]
pub struct RunArgs {
    #[command(subcommand)]
    pub command: RunCommand,
}

#[derive(Subcommand, Debug)]
pub enum RunCommand {
    /// Add the pcb skill to the current git repository
    AddSkill(AddSkillArgs),
}

#[derive(Args, Debug)]
pub struct AddSkillArgs {}

pub fn execute(args: RunArgs) -> Result<()> {
    match args.command {
        RunCommand::AddSkill(_) => {
            let repo_root = find_git_root()?;
            add_skill_to_path(&repo_root)?;
            eprintln!(
                "{} pcb skill to {}",
                "Added".green(),
                repo_root.display().to_string().cyan()
            );
            Ok(())
        }
    }
}

/// Find the root of the git repository containing the current directory
fn find_git_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        anyhow::bail!("Not inside a git repository");
    }

    let path = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in git output")?
        .trim()
        .to_string();

    Ok(PathBuf::from(path))
}

/// Add the pcb skill to the given directory path.
/// Creates .agents/skills/pcb with SKILL.md and mcp.json,
/// and symlinks .claude/skills -> ../.agents/skills if not already present.
pub fn add_skill_to_path(path: &Path) -> Result<()> {
    // Create .agents/skills/pcb directory and write files (overwrites existing)
    let agents_skill_dir = path.join(".agents/skills/pcb");
    std::fs::create_dir_all(&agents_skill_dir)
        .context("Failed to create .agents/skills/pcb directory")?;
    std::fs::write(agents_skill_dir.join("SKILL.md"), AGENTS_SKILL_MD)
        .context("Failed to write SKILL.md")?;
    std::fs::write(agents_skill_dir.join("mcp.json"), AGENTS_MCP_JSON)
        .context("Failed to write mcp.json")?;

    // Create .claude directory if it doesn't exist
    let claude_dir = path.join(".claude");
    std::fs::create_dir_all(&claude_dir).context("Failed to create .claude directory")?;

    // Create skills symlink if it doesn't exist
    let skills_symlink = claude_dir.join("skills");
    if !skills_symlink.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink("../.agents/skills", &skills_symlink)
            .context("Failed to create .claude/skills symlink")?;
    }

    Ok(())
}
