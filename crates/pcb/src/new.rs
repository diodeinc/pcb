use anyhow::{bail, Context, Result};
use clap::Args;
use colored::Colorize;
use globset::Glob;
use inquire::{Select, Text};
use minijinja::{context, Environment};
use pcb_zen_core::config::{find_workspace_root, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::migrate::codemods::manifest_v2::pcb_version_from_cargo;
use crate::run::add_skill_to_path;

const GITIGNORE_TEMPLATE: &str = include_str!("templates/gitignore");
const WORKSPACE_PCB_TOML: &str = include_str!("templates/workspace_pcb_toml.jinja");
const WORKSPACE_README: &str = include_str!("templates/workspace_readme.jinja");
const BOARD_PCB_TOML: &str = include_str!("templates/board_pcb_toml.jinja");
const BOARD_ZEN: &str = include_str!("templates/board_zen.jinja");
const BOARD_README: &str = include_str!("templates/board_readme.jinja");
const BOARD_CHANGELOG: &str = include_str!("templates/board_changelog.jinja");
const PACKAGE_ZEN: &str = include_str!("templates/package_zen.jinja");
const PACKAGE_README: &str = include_str!("templates/package_readme.jinja");
const PACKAGE_CHANGELOG: &str = include_str!("templates/package_changelog.jinja");

fn create_template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("workspace_pcb_toml", WORKSPACE_PCB_TOML)
        .unwrap();
    env.add_template("workspace_readme", WORKSPACE_README)
        .unwrap();
    env.add_template("board_pcb_toml", BOARD_PCB_TOML).unwrap();
    env.add_template("board_zen", BOARD_ZEN).unwrap();
    env.add_template("board_readme", BOARD_README).unwrap();
    env.add_template("board_changelog", BOARD_CHANGELOG)
        .unwrap();
    env.add_template("package_zen", PACKAGE_ZEN).unwrap();
    env.add_template("package_readme", PACKAGE_README).unwrap();
    env.add_template("package_changelog", PACKAGE_CHANGELOG)
        .unwrap();
    env
}

#[derive(Args, Debug)]
#[command(
    about = "Create a new PCB workspace, board, package, or component",
    long_about = "Create a new PCB workspace, board, package, or component.\n\n\
        Examples:\n  \
        pcb new --workspace my-project --repo https://github.com/user/my-project\n  \
        pcb new --board MainBoard\n  \
        pcb new --package modules/power_supply\n  \
        pcb new --component"
)]
pub struct NewArgs {
    /// Create a new workspace directory with git init
    #[arg(long, value_name = "NAME", conflicts_with_all = ["board", "package", "component"])]
    pub workspace: Option<String>,

    /// Git repository URL for the workspace
    #[arg(long, value_name = "URL", requires = "workspace")]
    pub repo: Option<String>,

    /// Create a new board in boards/<NAME>/ (requires existing workspace)
    #[arg(long, value_name = "NAME", conflicts_with_all = ["workspace", "package", "component"])]
    pub board: Option<String>,

    /// Create a new package at the given path (requires existing workspace)
    #[arg(long, value_name = "PATH", conflicts_with_all = ["workspace", "board", "component"])]
    pub package: Option<String>,

    /// Create a new component by searching online (requires existing workspace)
    #[arg(long, conflicts_with_all = ["workspace", "board", "package"])]
    pub component: bool,
}

/// Validate a name for use as directory/git repo name (used for workspaces and boards)
fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty() {
        bail!("{} name cannot be empty", kind);
    }

    if name.len() > 100 {
        bail!("{} name cannot exceed 100 characters", kind);
    }

    if name.starts_with('.') || name.starts_with('-') {
        bail!("{} name cannot start with '.' or '-'", kind);
    }

    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.' {
            bail!(
                "{} name contains invalid character '{}'. Only alphanumeric, hyphens, underscores, and dots are allowed",
                kind,
                c
            );
        }
    }

    Ok(())
}

/// Clean a git repository URL to the canonical format (e.g., "github.com/user/repo")
fn clean_repo_url(url: &str) -> Result<String> {
    let url = url.trim();

    // Handle SSH format: git@github.com:user/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        // Replace first ':' with '/'
        if let Some(idx) = rest.find(':') {
            let (host, path) = rest.split_at(idx);
            let path = &path[1..]; // skip the ':'
            return Ok(format!("{}/{}", host, path));
        }
        bail!("Invalid SSH git URL format: {}", url);
    }

    // Handle HTTPS format: https://github.com/user/repo.git
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let url = url.strip_suffix(".git").unwrap_or(url);
    let url = url.strip_suffix('/').unwrap_or(url);

    // Validate it looks like a valid repo path
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 3 {
        bail!(
            "Repository URL must include host and path (e.g., github.com/user/repo): {}",
            url
        );
    }

    Ok(url.to_string())
}

pub fn execute(args: NewArgs) -> Result<()> {
    match (&args.workspace, &args.board, &args.package, args.component) {
        (Some(workspace), _, _, _) => execute_new_workspace(workspace, args.repo.as_deref()),
        (_, Some(board), _, _) => execute_new_board(board),
        (_, _, Some(package), _) => execute_new_package(package),
        (_, _, _, true) => execute_new_component(),
        _ => execute_interactive(),
    }
}

/// Returns (workspace_root, config) if inside a valid workspace, None otherwise
fn get_workspace() -> Option<(std::path::PathBuf, PcbToml)> {
    let file_provider = DefaultFileProvider::new();
    let cwd = std::env::current_dir().ok()?;
    let workspace_root = find_workspace_root(&file_provider, &cwd).ok()?;
    let pcb_toml = workspace_root.join("pcb.toml");
    if !pcb_toml.exists() {
        return None;
    }
    let config = PcbToml::from_file(&file_provider, &pcb_toml).ok()?;
    if !config.is_workspace() {
        return None;
    }
    Some((workspace_root, config))
}

/// Returns workspace root, or error if not in a workspace
fn require_workspace() -> Result<(std::path::PathBuf, PcbToml)> {
    get_workspace().ok_or_else(|| anyhow::anyhow!("Not inside a pcb workspace"))
}

#[cfg(feature = "api")]
fn execute_new_component() -> Result<()> {
    let (workspace_root, _) = require_workspace()?;
    pcb_diode_api::execute_web_components_tui(&workspace_root, None)
}

#[cfg(not(feature = "api"))]
fn execute_new_component() -> Result<()> {
    bail!("Component creation requires the 'api' feature")
}

fn execute_interactive() -> Result<()> {
    if get_workspace().is_some() {
        #[cfg(feature = "api")]
        let options = vec!["board", "package", "component"];
        #[cfg(not(feature = "api"))]
        let options = vec!["board", "package"];

        let selection = Select::new("What would you like to create?", options)
            .prompt()
            .context("Failed to get selection")?;

        match selection {
            "board" => prompt_new_board(),
            "package" => prompt_new_package(),
            #[cfg(feature = "api")]
            "component" => execute_new_component(),
            _ => unreachable!(),
        }
    } else {
        prompt_new_workspace()
    }
}

fn prompt_new_workspace() -> Result<()> {
    let name = Text::new("Workspace name:")
        .prompt()
        .context("Failed to get workspace name")?;

    let repo = Text::new("Repository URL:")
        .prompt()
        .context("Failed to get repository URL")?;

    execute_new_workspace(&name, Some(&repo))
}

fn prompt_new_board() -> Result<()> {
    let name = Text::new("Board name:")
        .prompt()
        .context("Failed to get board name")?;

    execute_new_board(&name)
}

fn prompt_new_package() -> Result<()> {
    let path = Text::new("Package path (e.g., modules/my_module):")
        .prompt()
        .context("Failed to get package path")?;

    execute_new_package(&path)
}

fn execute_new_workspace(workspace: &str, repo: Option<&str>) -> Result<()> {
    if get_workspace().is_some() {
        bail!("Cannot create a workspace inside an existing workspace");
    }

    validate_name(workspace, "Workspace")?;

    let repo =
        repo.ok_or_else(|| anyhow::anyhow!("--repo is required when creating a workspace"))?;
    let repository = clean_repo_url(repo)?;

    let workspace_path = Path::new(workspace);

    if workspace_path.exists() {
        bail!("Directory '{}' already exists", workspace);
    }

    std::fs::create_dir_all(workspace_path)
        .with_context(|| format!("Failed to create directory '{}'", workspace))?;

    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(workspace_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run 'git init'")?;

    if !status.success() {
        bail!("'git init' failed with exit code: {:?}", status.code());
    }

    let env = create_template_env();
    let ctx = context! {
        repository => repository,
        pcb_version => pcb_version_from_cargo(),
    };

    let pcb_toml_content = env
        .get_template("workspace_pcb_toml")
        .unwrap()
        .render(&ctx)
        .context("Failed to render pcb.toml template")?;
    std::fs::write(workspace_path.join("pcb.toml"), pcb_toml_content)
        .context("Failed to write pcb.toml")?;

    let readme_content = env
        .get_template("workspace_readme")
        .unwrap()
        .render(&ctx)
        .context("Failed to render README.md template")?;
    std::fs::write(workspace_path.join("README.md"), readme_content)
        .context("Failed to write README.md")?;

    std::fs::write(workspace_path.join(".gitignore"), GITIGNORE_TEMPLATE)
        .context("Failed to write .gitignore")?;

    add_skill_to_path(workspace_path)?;

    eprintln!(
        "{} {} ({})",
        "Created".green(),
        workspace.bold(),
        repository.cyan()
    );

    Ok(())
}

fn execute_new_board(board: &str) -> Result<()> {
    validate_name(board, "Board")?;

    let (workspace_root, _) = require_workspace()?;
    let scaffold = scaffold_board(&workspace_root, board)?;

    eprintln!(
        "{} board {} at {}",
        "Created".green(),
        board.bold(),
        scaffold
            .board_dir
            .strip_prefix(&workspace_root)
            .unwrap_or(&scaffold.board_dir)
            .display()
            .to_string()
            .cyan()
    );

    Ok(())
}

pub(crate) struct BoardScaffold {
    pub board_dir: std::path::PathBuf,
    pub zen_file: std::path::PathBuf,
}

pub(crate) fn scaffold_board(workspace_root: &Path, board: &str) -> Result<BoardScaffold> {
    validate_name(board, "Board")?;

    let board_dir = workspace_root.join("boards").join(board);
    if board_dir.exists() {
        bail!("Board directory '{}' already exists", board_dir.display());
    }

    std::fs::create_dir_all(&board_dir)
        .with_context(|| format!("Failed to create directory '{}'", board_dir.display()))?;

    let env = create_template_env();
    let ctx = context! {
        board => board,
        pcb_version => pcb_version_from_cargo(),
    };

    let pcb_toml_content = env
        .get_template("board_pcb_toml")
        .unwrap()
        .render(&ctx)
        .context("Failed to render pcb.toml template")?;
    std::fs::write(board_dir.join("pcb.toml"), pcb_toml_content)
        .context("Failed to write pcb.toml")?;

    let zen_content = env
        .get_template("board_zen")
        .unwrap()
        .render(&ctx)
        .context("Failed to render .zen template")?;
    let zen_file = board_dir.join(format!("{}.zen", board));
    std::fs::write(&zen_file, zen_content).context("Failed to write .zen file")?;

    let readme_content = env
        .get_template("board_readme")
        .unwrap()
        .render(&ctx)
        .context("Failed to render README.md template")?;
    std::fs::write(board_dir.join("README.md"), readme_content)
        .context("Failed to write README.md")?;

    let changelog_content = env
        .get_template("board_changelog")
        .unwrap()
        .render(&ctx)
        .context("Failed to render CHANGELOG.md template")?;
    std::fs::write(board_dir.join("CHANGELOG.md"), changelog_content)
        .context("Failed to write CHANGELOG.md")?;

    Ok(BoardScaffold {
        board_dir,
        zen_file,
    })
}

fn execute_new_package(package_path: &str) -> Result<()> {
    let package_path = package_path.trim_matches('/');
    if package_path.is_empty() {
        bail!("Package path cannot be empty");
    }

    let name = package_path
        .split('/')
        .next_back()
        .ok_or_else(|| anyhow::anyhow!("Invalid package path"))?;
    validate_name(name, "Package")?;

    let (workspace_root, config) = require_workspace()?;
    let members = &config.workspace.as_ref().unwrap().members;
    if members.is_empty() {
        bail!("Workspace has no member patterns defined");
    }

    let matches_pattern = members.iter().any(|pattern| {
        Glob::new(pattern)
            .ok()
            .and_then(|g| g.compile_matcher().is_match(package_path).then_some(()))
            .is_some()
    });

    if !matches_pattern {
        bail!(
            "Package path '{}' does not match any workspace member pattern.\nValid patterns: {:?}",
            package_path,
            members
        );
    }

    let package_dir = workspace_root.join(package_path);
    if package_dir.exists() {
        bail!(
            "Package directory '{}' already exists",
            package_dir.display()
        );
    }

    std::fs::create_dir_all(&package_dir)
        .with_context(|| format!("Failed to create directory '{}'", package_dir.display()))?;

    let env = create_template_env();
    let ctx = context! {
        name => name,
        pcb_version => pcb_version_from_cargo(),
    };

    std::fs::write(package_dir.join("pcb.toml"), "").context("Failed to write pcb.toml")?;

    let zen_content = env
        .get_template("package_zen")
        .unwrap()
        .render(&ctx)
        .context("Failed to render .zen template")?;
    let zen_file = package_dir.join(format!("{}.zen", name));
    std::fs::write(&zen_file, zen_content).context("Failed to write .zen file")?;

    let readme_content = env
        .get_template("package_readme")
        .unwrap()
        .render(&ctx)
        .context("Failed to render README.md template")?;
    std::fs::write(package_dir.join("README.md"), readme_content)
        .context("Failed to write README.md")?;

    let changelog_content = env
        .get_template("package_changelog")
        .unwrap()
        .render(&ctx)
        .context("Failed to render CHANGELOG.md template")?;
    std::fs::write(package_dir.join("CHANGELOG.md"), changelog_content)
        .context("Failed to write CHANGELOG.md")?;

    eprintln!(
        "{} package {} at {}",
        "Created".green(),
        name.bold(),
        package_path.cyan()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_name() {
        // Valid names
        assert!(validate_name("my-project", "Workspace").is_ok());
        assert!(validate_name("my_project", "Board").is_ok());
        assert!(validate_name("myProject123", "Workspace").is_ok());
        assert!(validate_name("project.v2", "Board").is_ok());

        // Invalid names
        assert!(validate_name("", "Workspace").is_err());
        assert!(validate_name(".hidden", "Board").is_err());
        assert!(validate_name("-invalid", "Workspace").is_err());
        assert!(validate_name("has spaces", "Board").is_err());
        assert!(validate_name("has/slash", "Workspace").is_err());
        assert!(validate_name(&"a".repeat(101), "Board").is_err());
    }

    #[test]
    fn test_clean_repo_url() {
        // HTTPS URLs
        assert_eq!(
            clean_repo_url("https://github.com/user/repo").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("https://github.com/user/repo.git").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("https://github.com/user/repo/").unwrap(),
            "github.com/user/repo"
        );

        // SSH URLs
        assert_eq!(
            clean_repo_url("git@github.com:user/repo.git").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("git@gitlab.com:user/repo").unwrap(),
            "gitlab.com/user/repo"
        );

        // Already clean
        assert_eq!(
            clean_repo_url("github.com/user/repo").unwrap(),
            "github.com/user/repo"
        );

        // Invalid
        assert!(clean_repo_url("invalid").is_err());
        assert!(clean_repo_url("github.com/user").is_err());
    }
}
