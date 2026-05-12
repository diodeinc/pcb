use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use colored::Colorize;
use globset::Glob;
use inquire::{Select, Text};
use minijinja::{Environment, context};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{PcbToml, find_workspace_root, pcb_version_from_cargo};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::codegen;

const GITIGNORE_TEMPLATE: &str = include_str!("templates/gitignore");
const BOARD_WORKSPACE_PCB_TOML: &str = include_str!("templates/board_workspace_pcb_toml.jinja");
const BOARD_ZEN: &str = include_str!("templates/board_zen.jinja");
const BOARD_README: &str = include_str!("templates/board_readme.jinja");
const PACKAGE_ZEN: &str = include_str!("templates/package_zen.jinja");
const PACKAGE_README: &str = include_str!("templates/package_readme.jinja");

fn create_template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("board_workspace_pcb_toml", BOARD_WORKSPACE_PCB_TOML)
        .unwrap();
    env.add_template("board_zen", BOARD_ZEN).unwrap();
    env.add_template("board_readme", BOARD_README).unwrap();
    env.add_template("package_zen", PACKAGE_ZEN).unwrap();
    env.add_template("package_readme", PACKAGE_README).unwrap();
    env
}

#[derive(Args, Debug)]
#[command(
    about = "Create a new PCB board, package, or component",
    long_about = "Create a new PCB board, package, or component.\n\n\
        Examples:\n  \
        pcb new board MainBoard https://github.com/user/MainBoard\n  \
        pcb new package modules/power_supply\n  \
        pcb new component\n  \
        pcb new component path/to/component-dir"
)]
pub struct NewArgs {
    #[command(subcommand)]
    pub command: Option<NewCommand>,
}

#[derive(Subcommand, Debug)]
pub enum NewCommand {
    /// Create a new board repository with git init
    Board(NewBoardArgs),

    /// Create a new package at the given path (requires existing workspace)
    Package(NewPackageArgs),

    /// Create a new component by searching online, or import from local directory
    Component(NewComponentArgs),
}

#[derive(Args, Debug)]
pub struct NewBoardArgs {
    /// Board name (also used as the directory name)
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Git repository URL for the board
    #[arg(value_name = "REPO_URL")]
    pub repo: String,
}

#[derive(Args, Debug)]
pub struct NewPackageArgs {
    /// Package path (for example: modules/power_supply)
    #[arg(value_name = "PATH")]
    pub path: String,
}

#[derive(Args, Debug, Default)]
pub struct NewComponentArgs {
    /// Local component directory to import
    #[arg(value_name = "DIR", conflicts_with = "component_id")]
    pub dir: Option<PathBuf>,

    /// Download and add a component returned by web component search
    #[arg(long, value_name = "ID")]
    pub component_id: Option<String>,

    /// Optional fallback MPN if the download response does not include one
    #[arg(long, value_name = "MPN", requires = "component_id")]
    pub part_number: Option<String>,

    /// Optional manufacturer override or fallback
    #[arg(long, value_name = "MFR", requires = "component_id")]
    pub manufacturer: Option<String>,
}

/// Validate a name for use as a directory/git repo name.
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
    match args.command {
        Some(NewCommand::Board(command)) => execute_new_board(&command.name, &command.repo),
        Some(NewCommand::Package(command)) => execute_new_package(&command.path),
        Some(NewCommand::Component(command)) => execute_new_component(command),
        None => execute_interactive(),
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
fn execute_new_component(args: NewComponentArgs) -> Result<()> {
    if let Some(dir) = args.dir.as_deref() {
        return pcb_diode_api::execute_component_from_local_dir(dir);
    }

    if let Some(component_id) = args.component_id.as_deref() {
        return pcb_diode_api::execute_component_from_id(
            component_id,
            args.part_number.as_deref(),
            args.manufacturer.as_deref(),
        );
    }

    let (workspace_root, _) = require_workspace()?;
    pcb_diode_api::execute_web_components_tui(&workspace_root)
}

#[cfg(not(feature = "api"))]
fn execute_new_component(_args: NewComponentArgs) -> Result<()> {
    bail!("Component creation requires the 'api' feature")
}

fn execute_interactive() -> Result<()> {
    if get_workspace().is_some() {
        #[cfg(feature = "api")]
        let options = vec!["package", "component"];
        #[cfg(not(feature = "api"))]
        let options = vec!["package"];

        let selection = Select::new("What would you like to create?", options)
            .prompt()
            .context("Failed to get selection")?;

        match selection {
            "package" => prompt_new_package(),
            #[cfg(feature = "api")]
            "component" => execute_new_component(NewComponentArgs::default()),
            _ => unreachable!(),
        }
    } else {
        prompt_new_board()
    }
}

fn prompt_new_board() -> Result<()> {
    let name = Text::new("Board name:")
        .prompt()
        .context("Failed to get board name")?;

    let repo = Text::new("Repository URL:")
        .prompt()
        .context("Failed to get repository URL")?;

    execute_new_board(&name, &repo)
}

fn prompt_new_package() -> Result<()> {
    let path = Text::new("Package path (e.g., modules/my_module):")
        .prompt()
        .context("Failed to get package path")?;

    execute_new_package(&path)
}

fn init_git(dir: &Path) -> Result<()> {
    if !dir.join(".git").exists() {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("Failed to run 'git init'")?;
        if !status.success() {
            bail!("'git init' failed with exit code: {:?}", status.code());
        }
    }

    Ok(())
}

fn execute_new_board(board: &str, repo: &str) -> Result<()> {
    if get_workspace().is_some() {
        bail!("Cannot create a board inside an existing workspace");
    }

    validate_name(board, "Board")?;

    let repository = clean_repo_url(repo)?;

    let board_path = Path::new(board);

    if board_path.exists() {
        bail!("Directory '{}' already exists", board);
    }

    std::fs::create_dir_all(board_path)
        .with_context(|| format!("Failed to create directory '{}'", board))?;

    init_board_workspace(board_path, board, &repository)?;

    eprintln!(
        "{} board {} ({})",
        "Created".green(),
        board.bold(),
        repository.cyan()
    );

    Ok(())
}

pub(crate) fn init_board_workspace(dir: &Path, board: &str, repository: &str) -> Result<()> {
    init_git(dir)?;

    let env = create_template_env();
    let ctx = context! {
        board => board,
        repository => repository,
        pcb_version => pcb_version_from_cargo(),
    };

    let pcb_toml_content = env
        .get_template("board_workspace_pcb_toml")
        .unwrap()
        .render(&ctx)
        .context("Failed to render pcb.toml template")?;
    std::fs::write(dir.join("pcb.toml"), pcb_toml_content).context("Failed to write pcb.toml")?;
    std::fs::write(dir.join("pcb.sum"), "").context("Failed to write pcb.sum")?;

    let zen_content = env
        .get_template("board_zen")
        .unwrap()
        .render(&ctx)
        .context("Failed to render .zen template")?;
    let board_zen = dir.join(format!("{}.zen", board));
    codegen::zen::write_zen_formatted(&board_zen, &zen_content)
        .context("Failed to write .zen file")?;

    let readme_content = env
        .get_template("board_readme")
        .unwrap()
        .render(&ctx)
        .context("Failed to render README.md template")?;
    std::fs::write(dir.join("README.md"), readme_content).context("Failed to write README.md")?;

    std::fs::write(dir.join(".gitignore"), GITIGNORE_TEMPLATE)
        .context("Failed to write .gitignore")?;

    Ok(())
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
    codegen::zen::write_zen_formatted(&zen_file, &zen_content)
        .context("Failed to write .zen file")?;

    let readme_content = env
        .get_template("package_readme")
        .unwrap()
        .render(&ctx)
        .context("Failed to render README.md template")?;
    std::fs::write(package_dir.join("README.md"), readme_content)
        .context("Failed to write README.md")?;

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
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        args: NewArgs,
    }

    #[test]
    fn test_validate_name() {
        // Valid names
        assert!(validate_name("my-project", "Board").is_ok());
        assert!(validate_name("my_project", "Board").is_ok());
        assert!(validate_name("myProject123", "Board").is_ok());
        assert!(validate_name("project.v2", "Board").is_ok());

        // Invalid names
        assert!(validate_name("", "Board").is_err());
        assert!(validate_name(".hidden", "Board").is_err());
        assert!(validate_name("-invalid", "Board").is_err());
        assert!(validate_name("has spaces", "Board").is_err());
        assert!(validate_name("has/slash", "Board").is_err());
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

    #[test]
    fn test_component_accepts_optional_directory() {
        let parsed = TestCli::try_parse_from(["pcb", "component"]).unwrap();
        assert!(matches!(
            parsed.args.command,
            Some(NewCommand::Component(NewComponentArgs { dir: None, .. }))
        ));

        let parsed = TestCli::try_parse_from(["pcb", "component", "components/foo"]).unwrap();
        assert!(matches!(
            parsed.args.command,
            Some(NewCommand::Component(NewComponentArgs {
                dir: Some(ref dir),
                ..
            })) if dir == &PathBuf::from("components/foo")
        ));

        let parsed = TestCli::try_parse_from(["pcb"]).unwrap();
        assert!(parsed.args.command.is_none());
    }

    #[test]
    fn test_board_requires_repo() {
        let parsed = TestCli::try_parse_from([
            "pcb",
            "board",
            "MainBoard",
            "https://github.com/user/MainBoard",
        ])
        .unwrap();
        assert!(matches!(
            parsed.args.command,
            Some(NewCommand::Board(NewBoardArgs { ref name, ref repo }))
                if name == "MainBoard" && repo == "https://github.com/user/MainBoard"
        ));

        assert!(TestCli::try_parse_from(["pcb", "board", "MainBoard"]).is_err());
        assert!(TestCli::try_parse_from(["pcb", "workspace", "my-project"]).is_err());
    }

    #[test]
    fn test_old_flag_forms_are_rejected() {
        assert!(TestCli::try_parse_from(["pcb", "--workspace", "my-project"]).is_err());
        assert!(TestCli::try_parse_from(["pcb", "--board", "MainBoard"]).is_err());
        assert!(TestCli::try_parse_from(["pcb", "--package", "modules/power_supply"]).is_err());
        assert!(TestCli::try_parse_from(["pcb", "--component"]).is_err());
    }
}
