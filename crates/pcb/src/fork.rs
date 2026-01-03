use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use path_slash::PathExt;
use pcb_ui::{Style, StyledText};
use pcb_zen::cache_index::cache_base;
use pcb_zen::get_workspace_info;
use pcb_zen::tags::get_all_versions_for_repo;
use pcb_zen::{copy_dir_all, ensure_sparse_checkout};
use pcb_zen_core::config::{split_repo_and_subpath, PatchSpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use semver::Version;
use std::fs;

#[derive(Args)]
pub struct ForkArgs {
    /// Fully-qualified module URL (e.g., github.com/diodeinc/registry/modules/UsbPdController)
    #[arg(value_name = "URL")]
    pub url: String,

    /// Specific version to fork (default: latest tagged version)
    #[arg(long)]
    pub version: Option<String>,

    /// Force overwrite if fork directory already exists
    #[arg(long)]
    pub force: bool,
}

pub fn execute(args: ForkArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let file_provider = DefaultFileProvider::new();
    let workspace_info = get_workspace_info(&file_provider, &cwd)
        .context("Failed to detect PCB workspace (no pcb.toml found up the tree?)")?;
    let workspace_root = &workspace_info.root;

    // Validate V2 workspace
    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !pcb_toml_path.exists() {
        anyhow::bail!(
            "pcb fork requires a V2 workspace with pcb.toml at {}",
            pcb_toml_path.display()
        );
    }

    let mut config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
    if !config.is_v2() {
        anyhow::bail!("pcb fork only supports V2 workspaces (pcb-version >= 0.3)");
    }

    // Parse module URL
    let module_url = args.url.trim().to_string();
    let (repo_url, pkg_path) = split_repo_and_subpath(&module_url);

    println!("{} {}", "Forking".cyan().bold(), module_url.bold());

    // Discover versions
    println!("  {} Discovering versions...", "→".dimmed());
    let all_versions = get_all_versions_for_repo(repo_url)
        .with_context(|| format!("Failed to fetch versions from {}", repo_url))?;

    let versions_for_pkg = all_versions.get(pkg_path).ok_or_else(|| {
        let available_packages: Vec<_> = all_versions.keys().take(10).collect();
        if available_packages.is_empty() {
            anyhow::anyhow!("No tagged versions found in repository {}", repo_url)
        } else {
            anyhow::anyhow!(
                "No tagged versions found for path '{}' in {}.\nAvailable packages: {}{}",
                pkg_path,
                repo_url,
                available_packages
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if all_versions.len() > 10 { ", ..." } else { "" }
            )
        }
    })?;

    // Select version - use iter().max() to explicitly pick highest semver
    let version = if let Some(v_str) = &args.version {
        let v = Version::parse(v_str.trim())
            .with_context(|| format!("Invalid version '{}'. Expected semver format.", v_str))?;
        if !versions_for_pkg.contains(&v) {
            anyhow::bail!(
                "Version {} not found for {}.\nAvailable versions: {}",
                v,
                module_url,
                versions_for_pkg
                    .iter()
                    .take(10)
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        v
    } else {
        versions_for_pkg
            .iter()
            .max()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No versions available for {}", module_url))?
    };
    let version_str = version.to_string();

    println!(
        "  {} Selected version {}",
        "→".dimmed(),
        version_str.green()
    );

    // Compute fork directory and relative path
    let fork_dir = workspace_root
        .join("fork")
        .join(&module_url)
        .join(&version_str);

    let relative_fork_path = fork_dir
        .strip_prefix(workspace_root)
        .expect("fork_dir should be under workspace_root")
        .to_slash_lossy()
        .into_owned();

    // Check for conflicting patch BEFORE modifying filesystem
    // This avoids side-effects (creating fork dir) when we'll error anyway
    let patch_updated =
        update_patch_section(&mut config, &module_url, &relative_fork_path, args.force)?;

    // Ensure package is in cache using shared sparse checkout logic
    println!("  {} Fetching package...", "→".dimmed());
    let cache_dir = cache_base().join(&module_url).join(&version_str);
    ensure_sparse_checkout(&cache_dir, &module_url, &version_str, true)
        .with_context(|| format!("Failed to fetch {}@{} into cache", module_url, version_str))?;

    // Handle fork directory
    let fork_existed = fork_dir.exists();
    if fork_existed {
        if args.force {
            println!("  {} Removing existing fork (--force)...", "→".dimmed());
            fs::remove_dir_all(&fork_dir)
                .with_context(|| format!("Failed to remove {}", fork_dir.display()))?;
        } else {
            println!(
                "  {} Fork directory already exists, skipping copy",
                "→".dimmed()
            );
        }
    }

    // Copy to fork directory (if needed)
    if !fork_dir.exists() {
        println!("  {} Copying to fork directory...", "→".dimmed());
        copy_dir_all(&cache_dir, &fork_dir).with_context(|| {
            format!(
                "Failed to copy from {} to {}",
                cache_dir.display(),
                fork_dir.display()
            )
        })?;
    }

    // Validate package has pcb.toml
    let fork_pcb_toml = fork_dir.join("pcb.toml");
    if !fork_pcb_toml.exists() {
        // Clean up if we just created it
        if !fork_existed {
            let _ = fs::remove_dir_all(&fork_dir);
        }
        anyhow::bail!(
            "pcb fork only supports packages with pcb.toml.\n\
             {} has no pcb.toml (likely an asset).\n\
             For assets, use 'pcb vendor' or clone manually.",
            module_url
        );
    }

    // Write updated config
    if patch_updated {
        println!("  {} Updating pcb.toml [patch] section...", "→".dimmed());
        let new_toml = toml::to_string_pretty(&config)?;
        fs::write(&pcb_toml_path, new_toml)
            .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;
    }

    // Success message
    println!();
    println!("{} Forked successfully!", "✓".green().bold());
    println!();
    println!(
        "  {} {}",
        "Fork location:".dimmed(),
        fork_dir.display().to_string().with_style(Style::Cyan)
    );
    println!(
        "  {} [patch].\"{}\" = {{ path = \"{}\" }}",
        "Patch entry:".dimmed(),
        module_url,
        relative_fork_path
    );
    println!();
    println!(
        "{}",
        "You can now edit files in the fork directory. Changes will be used by 'pcb build'."
            .dimmed()
    );

    Ok(())
}

/// Update the [patch] section in the config. Returns true if config was modified.
fn update_patch_section(
    config: &mut PcbToml,
    module_url: &str,
    relative_fork_path: &str,
    force: bool,
) -> Result<bool> {
    if let Some(existing_patch) = config.patch.get(module_url) {
        // Check if it's already pointing to the same path
        if existing_patch.path.as_deref() == Some(relative_fork_path)
            && existing_patch.branch.is_none()
            && existing_patch.rev.is_none()
        {
            // Already correctly configured
            return Ok(false);
        }

        // There's a different patch
        if !force {
            anyhow::bail!(
                "A patch for '{}' already exists in pcb.toml (path={:?}, branch={:?}, rev={:?}).\n\
                 Remove it manually or use --force to override.",
                module_url,
                existing_patch.path,
                existing_patch.branch,
                existing_patch.rev
            );
        }
    }

    // Add or update the patch entry
    config.patch.insert(
        module_url.to_string(),
        PatchSpec {
            path: Some(relative_fork_path.to_string()),
            branch: None,
            rev: None,
        },
    );

    Ok(true)
}
