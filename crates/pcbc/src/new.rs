use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use colored::Colorize;
use inquire::{Select, Text};
use minijinja::{Environment, context};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{PcbToml, find_workspace_root, pcb_version_from_cargo};
use std::path::{Path, PathBuf};

use crate::codegen;

const GITIGNORE_TEMPLATE: &str = include_str!("templates/gitignore");
const BOARD_PCB_TOML: &str = include_str!("templates/board_pcb_toml.jinja");
const BOARD_ZEN: &str = include_str!("templates/board_zen.jinja");
const BOARD_README: &str = include_str!("templates/board_readme.jinja");
const PACKAGE_ZEN: &str = include_str!("templates/package_zen.jinja");
const PACKAGE_README: &str = include_str!("templates/package_readme.jinja");

fn create_template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("board_pcb_toml", BOARD_PCB_TOML).unwrap();
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

    /// Import a component from a local directory
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

#[derive(Args, Debug)]
pub struct NewComponentArgs {
    /// Local component directory to import
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
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

/// Validate a board name for use as a directory/git repo name.
fn validate_board_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Board name cannot be empty");
    }

    if name.len() > 40 {
        bail!("Board name cannot exceed 40 characters");
    }

    if name.starts_with('.') || name.starts_with('-') {
        bail!("Board name cannot start with '.' or '-'");
    }

    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.' {
            bail!(
                "Board name contains invalid character '{}'. Only alphanumeric, hyphens, underscores, and dots are allowed",
                c
            );
        }
    }

    Ok(())
}

/// Validate a board name that was derived from an input file name rather than typed by the user.
pub(crate) fn validate_derived_board_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("board name is empty");
    }

    let zen_file_name = format!("{name}.zen");
    if zen_file_name.len() > 255 {
        bail!(
            "board name is too long: {} would be a {}-byte file name, over the 255-byte limit",
            zen_file_name,
            zen_file_name.len()
        );
    }

    if name.chars().all(|c| c == '.') {
        bail!("board name {name:?} consists only of dots");
    }

    for c in name.chars() {
        let reason = match c {
            '"' => "a double quote",
            '\\' => "a backslash",
            '/' => "a path separator",
            '\n' => "a newline",
            '\r' => "a carriage return",
            c if c.is_control() => "a control character",
            _ => continue,
        };
        bail!("board name {name:?} contains {reason}");
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

fn execute_new_component(args: NewComponentArgs) -> Result<()> {
    pcb_diode_api::execute_component_from_local_dir(&args.dir)
}

fn execute_interactive() -> Result<()> {
    if get_workspace().is_some() {
        let options = vec!["package", "component"];

        let selection = Select::new("What would you like to create?", options)
            .prompt()
            .context("Failed to get selection")?;

        match selection {
            "package" => prompt_new_package(),
            "component" => prompt_new_component(),
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

fn prompt_new_component() -> Result<()> {
    let dir = Text::new("Local component directory:")
        .prompt()
        .context("Failed to get component directory")?;

    execute_new_component(NewComponentArgs {
        dir: PathBuf::from(dir),
    })
}

fn execute_new_board(board: &str, repo: &str) -> Result<()> {
    if get_workspace().is_some() {
        bail!("Cannot create a board inside an existing workspace");
    }

    validate_board_name(board)?;

    let repository = clean_repo_url(repo)?;

    let board_path = Path::new(board);

    if board_path.exists() {
        bail!("Directory '{}' already exists", board);
    }

    std::fs::create_dir_all(board_path)
        .with_context(|| format!("Failed to create directory '{}'", board))?;

    init_board_repo(board_path, board, &repository)?;

    eprintln!(
        "{} board {} ({})",
        "Created".green(),
        board.bold(),
        repository.cyan()
    );

    Ok(())
}

pub(crate) fn init_board_repo(dir: &Path, board: &str, repository: &str) -> Result<()> {
    pcb_zen::git::init(dir)?;

    let env = create_template_env();
    let ctx = context! {
        board => board,
        repository => repository,
        pcb_version => pcb_version_from_cargo(),
    };

    let pcb_toml_content = env
        .get_template("board_pcb_toml")
        .unwrap()
        .render(&ctx)
        .context("Failed to render pcb.toml template")?;
    std::fs::write(dir.join("pcb.toml"), pcb_toml_content).context("Failed to write pcb.toml")?;

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

    let (workspace_root, _config) = require_workspace()?;

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
    fn test_validate_board_name() {
        // Valid names
        assert!(validate_board_name("my-project").is_ok());
        assert!(validate_board_name("my_project").is_ok());
        assert!(validate_board_name("myProject123").is_ok());
        assert!(validate_board_name("project.v2").is_ok());
        assert!(validate_board_name(&"a".repeat(40)).is_ok());

        // Invalid names
        assert!(validate_board_name("").is_err());
        assert!(validate_board_name(".hidden").is_err());
        assert!(validate_board_name("-invalid").is_err());
        assert!(validate_board_name("has spaces").is_err());
        assert!(validate_board_name("has/slash").is_err());
        assert!(validate_board_name(&"a".repeat(41)).is_err());
    }

    #[test]
    fn test_validate_name() {
        // Package names still allow dots and up to 100 characters.
        assert!(validate_name("project.v2", "Package").is_ok());
        assert!(validate_name(&"a".repeat(100), "Package").is_ok());
        assert!(validate_name(&"a".repeat(101), "Package").is_err());
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
    fn test_component_requires_directory() {
        let parsed = TestCli::try_parse_from(["pcb", "component", "components/foo"]).unwrap();
        assert!(matches!(
            parsed.args.command,
            Some(NewCommand::Component(NewComponentArgs {
                ref dir,
            })) if dir == &PathBuf::from("components/foo")
        ));

        assert!(TestCli::try_parse_from(["pcb", "component"]).is_err());
        let parsed = TestCli::try_parse_from(["pcb"]).unwrap();
        assert!(parsed.args.command.is_none());
    }

    #[test]
    fn test_legacy_component_flags_are_rejected() {
        for flag in ["--component-id", "--part-number", "--manufacturer"] {
            assert!(TestCli::try_parse_from(["pcb", "component", flag, "legacy"]).is_err());
        }
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
