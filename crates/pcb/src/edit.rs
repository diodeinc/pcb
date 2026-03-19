use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_zen::cache_index::ensure_edit_reference_repo;
use pcb_zen::git;
use pcb_zen::{MemberPackage, WorkspaceInfo, get_workspace_info};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencyDetail, DependencySpec, PcbToml, split_repo_and_subpath};

struct EditCheckout {
    dir: PathBuf,
    rev: String,
}

struct PendingEdit {
    target_url: String,
    checkout_dir: PathBuf,
    rev: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageScope {
    pub package: MemberPackage,
    pub package_dir: PathBuf,
    pub pcb_toml_path: PathBuf,
}

pub(crate) fn find_member_package_scope(
    workspace: &WorkspaceInfo,
    start_path: &Path,
) -> Option<PackageScope> {
    let candidate_dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };
    let candidate_dir = candidate_dir
        .canonicalize()
        .unwrap_or_else(|_| candidate_dir.to_path_buf());

    workspace
        .packages
        .values()
        .filter_map(|package| {
            let package_dir = package
                .dir(&workspace.root)
                .canonicalize()
                .unwrap_or_else(|_| package.dir(&workspace.root));
            if candidate_dir != package_dir && !candidate_dir.starts_with(&package_dir) {
                return None;
            }
            Some(PackageScope {
                package: package.clone(),
                package_dir: package_dir.clone(),
                pcb_toml_path: package_dir.join("pcb.toml"),
            })
        })
        .max_by_key(|scope| scope.package.rel_path.as_os_str().len())
}

#[derive(Args, Debug)]
#[command(about = "Create a managed edit checkout for a remote package")]
pub struct EditArgs {
    /// Path used to determine the active package context. Defaults to current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Remote package URL to edit. Repeat `-p` to edit multiple packages.
    #[arg(long, short = 'p', value_name = "URL", required = true)]
    pub packages: Vec<String>,
}

pub fn execute(args: EditArgs) -> Result<()> {
    let path = args.path.canonicalize()?;
    let mut target_urls = Vec::new();
    for package in args.packages {
        let target_url = package.trim().to_string();
        if target_url.is_empty() {
            anyhow::bail!("Package URL cannot be empty");
        }
        target_urls.push(target_url);
    }

    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info(&file_provider, &path, true)?;
    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        anyhow::bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }
    let scope = find_member_package_scope(&workspace, &path).ok_or_else(|| {
        anyhow::anyhow!(
            "pcb edit requires a package-scoped path.\n\
             '{}' did not resolve to a workspace package directory.",
            path.display()
        )
    })?;
    let _manifest_lock = git::lock_manifest(&scope.pcb_toml_path)?;

    let mut config = PcbToml::from_file(&file_provider, &scope.pcb_toml_path)?;
    if !config.is_v2() {
        anyhow::bail!(
            "pcb edit only supports V2 package manifests (pcb-version >= 0.3)\n{}",
            scope.pcb_toml_path.display()
        );
    }

    let branch = compute_edit_branch_name(&workspace.root, &scope.package.rel_path)?;
    let mut pending = Vec::new();
    for target_url in target_urls {
        let (repo_url, _) = split_repo_and_subpath(&target_url);
        let checkout = ensure_edit_checkout(&scope.package_dir, repo_url, &branch)?;
        let package_manifest = package_manifest_in_checkout(&checkout.dir, &target_url);
        if !package_manifest.exists() {
            anyhow::bail!(
                "Package '{}' does not exist on branch '{}' in {}\nExpected to find {}",
                target_url,
                branch,
                checkout.dir.display(),
                package_manifest.display()
            );
        }

        let rev = existing_managed_rev(&config, &target_url, &branch).unwrap_or(checkout.rev);
        pending.push(PendingEdit {
            target_url,
            checkout_dir: checkout.dir,
            rev,
        });
    }

    for edit in &pending {
        config.dependencies.insert(
            edit.target_url.clone(),
            DependencySpec::Detailed(DependencyDetail {
                version: None,
                branch: Some(branch.clone()),
                rev: Some(edit.rev.clone()),
                path: None,
            }),
        );
    }

    std::fs::write(&scope.pcb_toml_path, toml::to_string_pretty(&config)?)
        .with_context(|| format!("Failed to write {}", scope.pcb_toml_path.display()))?;

    let manifest_rel = scope
        .pcb_toml_path
        .strip_prefix(&workspace.root)
        .unwrap_or(&scope.pcb_toml_path);

    println!("{} Checkout ready", "✓".green().bold());
    println!();
    println!("  {} {}", "Branch:".dimmed(), branch);
    for checkout_dir in pending
        .iter()
        .map(|edit| &edit.checkout_dir)
        .collect::<BTreeSet<_>>()
    {
        let checkout_rel = checkout_dir
            .strip_prefix(&workspace.root)
            .unwrap_or(checkout_dir);
        println!("  {} {}", "Edit at:".dimmed(), checkout_rel.display());
    }
    println!();
    println!(
        "  {} {}:",
        "Dependency updated in".dimmed(),
        manifest_rel.display()
    );
    for edit in pending {
        println!(
            "    \"{}\" = {{ branch = \"{}\", rev = \"{}\" }}",
            edit.target_url, branch, edit.rev
        );
    }

    Ok(())
}

pub(crate) fn warn_for_managed_packages(workspace: &WorkspaceInfo, package_urls: &[String]) {
    let mut seen = BTreeSet::new();

    for package_url in package_urls {
        let Some(package) = workspace.packages.get(package_url) else {
            continue;
        };

        let Ok(expected_branch) = compute_edit_branch_name(&workspace.root, &package.rel_path)
        else {
            continue;
        };

        for (dep_url, spec) in &package.config.dependencies {
            let DependencySpec::Detailed(detail) = spec else {
                continue;
            };
            if detail.path.is_some() || detail.branch.as_deref() != Some(expected_branch.as_str()) {
                continue;
            }

            let package_dir = package.dir(&workspace.root);
            let checkout_dir = managed_checkout_dir(&package_dir, dep_url);
            if !checkout_dir.exists() || !seen.insert(checkout_dir.clone()) {
                continue;
            }

            if git::has_uncommitted_changes(&checkout_dir).unwrap_or(true) {
                eprintln!(
                    "  Warning: Managed edit checkout has uncommitted changes: {}",
                    checkout_dir.display()
                );
            }

            let remote_ref = format!("refs/remotes/origin/{expected_branch}");
            if git::ref_exists(&checkout_dir, &remote_ref) {
                if let Some((ahead, _behind)) = git::ahead_behind_ref(&checkout_dir, &remote_ref)
                    && ahead > 0
                {
                    eprintln!(
                        "  Warning: Managed edit checkout has unpushed commits: {}",
                        checkout_dir.display()
                    );
                }
            } else {
                eprintln!(
                    "  Warning: Managed edit checkout branch has not been pushed to origin: {}",
                    checkout_dir.display()
                );
            }
        }
    }
}

fn ensure_edit_checkout(package_dir: &Path, repo_url: &str, branch: &str) -> Result<EditCheckout> {
    let checkout_dir = edit_checkout_dir(package_dir, repo_url);
    let _checkout_lock = git::lock_dir(&checkout_dir)?;
    let remote_branch_ref = format!("refs/remotes/origin/{branch}");

    if checkout_dir.exists() {
        // TODO: keep repair manual for now. If this checkout is tampered with, docs should
        // tell users to fix it with normal git commands instead of adding repair logic here.
        let repo_root = git::get_repo_root(&checkout_dir).with_context(|| {
            format!(
                "Managed edit path exists but is not a git checkout: {}",
                checkout_dir.display()
            )
        })?;
        let repo_root = repo_root.canonicalize().unwrap_or(repo_root);
        let expected_root = checkout_dir
            .canonicalize()
            .unwrap_or_else(|_| checkout_dir.clone());
        if repo_root != expected_root {
            anyhow::bail!(
                "Managed edit path already exists but is not a checkout root: {}",
                checkout_dir.display()
            );
        }

        let current_branch = git::symbolic_ref_short_head(&checkout_dir).ok_or_else(|| {
            anyhow::anyhow!(
                "Managed edit checkout is not on a branch: {}",
                checkout_dir.display()
            )
        })?;
        if current_branch != branch {
            anyhow::bail!(
                "Managed edit checkout is on branch '{}' but expected '{}': {}",
                current_branch,
                branch,
                checkout_dir.display()
            );
        }

        let rev = if git::fetch_tracking_branch(&checkout_dir, "origin", branch).is_ok() {
            git::rev_parse(&checkout_dir, &remote_branch_ref).with_context(|| {
                format!(
                    "Failed to resolve remote branch '{}' in {}",
                    branch,
                    checkout_dir.display()
                )
            })?
        } else {
            git::rev_parse_head(&checkout_dir).with_context(|| {
                format!("Failed to determine HEAD for {}", checkout_dir.display())
            })?
        };
        return Ok(EditCheckout {
            dir: checkout_dir,
            rev,
        });
    }

    let reference_repo = ensure_edit_reference_repo(repo_url)?;
    let remote_url = git::get_remote_url(&reference_repo)?;
    git::clone_repo_with_reference(&remote_url, &reference_repo, &checkout_dir).with_context(|| {
        format!(
            "Failed to create edit checkout at {}",
            checkout_dir.display()
        )
    })?;

    let rev = if git::fetch_tracking_branch(&checkout_dir, "origin", branch).is_ok() {
        git::checkout_branch_reset(&checkout_dir, branch, &remote_branch_ref).with_context(
            || {
                format!(
                    "Failed to create edit checkout for existing branch '{}' at {}",
                    branch,
                    checkout_dir.display()
                )
            },
        )?;
        git::rev_parse(&checkout_dir, &remote_branch_ref).with_context(|| {
            format!(
                "Failed to resolve remote branch '{}' in {}",
                branch,
                checkout_dir.display()
            )
        })?
    } else {
        git::checkout_branch_reset(&checkout_dir, branch, "HEAD").with_context(|| {
            format!(
                "Failed to create edit branch '{}' in {}",
                branch,
                checkout_dir.display()
            )
        })?;
        git::rev_parse_head(&checkout_dir)
            .with_context(|| format!("Failed to determine HEAD for {}", checkout_dir.display()))?
    };

    Ok(EditCheckout {
        dir: checkout_dir,
        rev,
    })
}

fn edit_checkout_dir(package_dir: &Path, repo_url: &str) -> PathBuf {
    package_dir.join(".pcb/edit").join(repo_url)
}

fn managed_checkout_dir(package_dir: &Path, dep_url: &str) -> PathBuf {
    let (repo_url, _) = split_repo_and_subpath(dep_url);
    edit_checkout_dir(package_dir, repo_url)
}

fn package_manifest_in_checkout(checkout_dir: &Path, package_url: &str) -> PathBuf {
    let (_, subpath) = split_repo_and_subpath(package_url);
    if subpath.is_empty() {
        checkout_dir.join("pcb.toml")
    } else {
        checkout_dir.join(subpath).join("pcb.toml")
    }
}

fn existing_managed_rev(config: &PcbToml, target_url: &str, branch: &str) -> Option<String> {
    let spec = config.dependencies.get(target_url)?;
    let DependencySpec::Detailed(detail) = spec else {
        return None;
    };
    if detail.version.is_some() || detail.path.is_some() {
        return None;
    }
    if detail.branch.as_deref() != Some(branch) {
        return None;
    }
    detail.rev.clone()
}

// Deterministic branch naming keeps one long-lived edit branch per workspace package.
//
// We intentionally accept some collision risk here to keep branches short:
// - drop the old `pcb-edit/` prefix
// - if the owner is `dioderobot`, drop that too
//
// Result:
// - most repos: `<owner>/<repo>/<package-relpath>`
// - `dioderobot/*`: `<repo>/<package-relpath>`
fn compute_edit_branch_name(workspace_root: &Path, package_rel_path: &Path) -> Result<String> {
    let (owner, repo) = git::remote_owner_repo(workspace_root)
        .context("Failed to determine workspace git remote owner/repo")?;
    let owner = sanitize_branch_component(&owner);
    let repo = sanitize_branch_component(&repo);
    let package_path = sanitize_branch_path(package_rel_path);
    if owner == "dioderobot" {
        return Ok(format!("{repo}/{package_path}"));
    }
    Ok(format!("{owner}/{repo}/{package_path}"))
}

fn sanitize_branch_path(path: &Path) -> String {
    path.components()
        .map(|component| sanitize_branch_component(&component.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitize_branch_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let valid = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        out.push(if valid { ch } else { '-' });
    }
    let trimmed = out.trim_matches('.');
    if trimmed.is_empty() {
        "pkg".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_test_utils::sandbox::Sandbox;
    use pcb_zen_core::workspace::WorkspaceInfo;
    use std::collections::BTreeMap;

    const TEST_PACKAGE_PCB_TOML: &str = r#"
[board]
name = "MainBoard"
path = "MainBoard.zen"
"#;

    const TEST_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]
"#;

    #[test]
    fn test_branch_name_is_deterministic() {
        let mut sb = Sandbox::new();
        sb.write("pcb.toml", TEST_WORKSPACE_PCB_TOML)
            .write("boards/MainBoard/pcb.toml", TEST_PACKAGE_PCB_TOML)
            .init_git();
        sb.cmd(
            "git",
            [
                "remote",
                "set-url",
                "origin",
                "https://github.com/dioderobot/diode.git",
            ],
        )
        .run()
        .unwrap();

        let branch =
            compute_edit_branch_name(sb.root_path(), Path::new("boards/MainBoard")).unwrap();
        assert_eq!(branch, "diode/boards/MainBoard");
    }

    #[test]
    fn test_branch_component_sanitization() {
        assert_eq!(sanitize_branch_component("MainBoard"), "MainBoard");
        assert_eq!(sanitize_branch_component(".hidden."), "hidden");
        assert_eq!(sanitize_branch_component("board name"), "board-name");
        assert_eq!(sanitize_branch_component("x:y?z*"), "x-y-z-");
        assert_eq!(sanitize_branch_component("..."), "pkg");
    }

    #[test]
    fn test_branch_path_sanitization() {
        assert_eq!(
            sanitize_branch_path(Path::new("boards/Foo Bar/.hidden")),
            "boards/Foo-Bar/hidden"
        );
    }

    #[test]
    fn test_find_member_package_scope_from_nested_subdirectory() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let member_rel = PathBuf::from("packages/foo");
        let nested = root.join(&member_rel).join("src/nested");

        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(&member_rel).join("pcb.toml"), "").unwrap();

        let mut packages = BTreeMap::new();
        packages.insert(
            "github.com/example/foo".to_string(),
            MemberPackage {
                rel_path: member_rel.clone(),
                config: PcbToml::default(),
                version: None,
                published_at: None,
                preferred: false,
                dirty: false,
            },
        );

        let ws = WorkspaceInfo {
            root,
            cache_dir: PathBuf::new(),
            config: None,
            packages,
            lockfile: None,
            errors: vec![],
        };

        let scope = find_member_package_scope(&ws, &nested).expect("scope");
        assert_eq!(scope.package.rel_path, member_rel);
    }
}
