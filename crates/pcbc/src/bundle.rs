use anyhow::{Context, Result, bail};
use chrono::Utc;
use pcb_zen::{copy_dir_all, git};
use pcb_zen_core::resolution::{FrozenPackageIdentity, FrozenResolutionMap, ResolutionResult};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

pub(crate) struct MetadataInput<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub git_hash: &'a str,
    pub workspace_root: &'a Path,
    pub staging_dir: &'a Path,
    pub zen_path: &'a Path,
    pub layout_path: Option<&'a Path>,
    pub description: Option<&'a str>,
    pub include_kicad_version: bool,
    pub bom_strict: bool,
}

pub(crate) struct SourceBundlePlan<'a> {
    pub resolution: &'a ResolutionResult,
    pub root_package_url: Option<&'a str>,
    pub staged_src: &'a Path,
}

#[instrument(name = "write_bundle_metadata", skip_all)]
pub(crate) fn write_metadata_json(input: &MetadataInput<'_>) -> Result<()> {
    let metadata = create_metadata_json(input);
    let metadata_str = serde_json::to_string_pretty(&metadata)?;
    fs::write(input.staging_dir.join("metadata.json"), metadata_str)?;
    Ok(())
}

#[instrument(name = "stage_source_bundle", skip_all)]
pub(crate) fn stage_source_bundle(plan: &SourceBundlePlan<'_>) -> Result<()> {
    let root_package_url = plan
        .root_package_url
        .context("Source bundling requires a workspace package target")?;
    let frozen = plan
        .resolution
        .frozen_root(root_package_url)
        .with_context(|| {
            format!("{root_package_url} is missing hydrated dependency state; run `pcb sync` first")
        })?;
    stage_frozen_source_bundle(plan, frozen)
}

fn stage_frozen_source_bundle(
    plan: &SourceBundlePlan<'_>,
    frozen: &FrozenResolutionMap,
) -> Result<()> {
    let workspace_info = &plan.resolution.workspace_info;
    fs::create_dir_all(plan.staged_src)?;

    let root_pcb_toml = workspace_info.root.join("pcb.toml");
    if root_pcb_toml.exists() {
        fs::copy(&root_pcb_toml, plan.staged_src.join("pcb.toml"))?;
    }

    let excluded_roots = source_bundle_excluded_roots(workspace_info);

    for (root, package) in &frozen.packages {
        match &package.identity {
            FrozenPackageIdentity::Workspace(url) => {
                let rel_path = workspace_package_rel_path(workspace_info, url)?;
                copy_dir_all(root, &plan.staged_src.join(rel_path), &excluded_roots)?;
            }
            FrozenPackageIdentity::Remote { dep_id, version } => {
                let dst = plan
                    .staged_src
                    .join("vendor")
                    .join(&dep_id.path)
                    .join(version.to_string());
                copy_dir_all(root, &dst, &HashSet::new())?;
            }
            FrozenPackageIdentity::Stdlib => {}
        }
    }

    Ok(())
}

pub(crate) fn remove_dir_all_with_permissions(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    if let Ok(mut perms) = fs::metadata(dir).map(|m| m.permissions()) {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(dir, perms);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_symlink() {
            fs::remove_file(&path)?;
        } else if path.is_dir() {
            remove_dir_all_with_permissions(&path)?;
        } else {
            if let Ok(mut perms) = fs::metadata(&path).map(|m| m.permissions()) {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = fs::set_permissions(&path, perms);
            }
            fs::remove_file(&path)?;
        }
    }

    fs::remove_dir(dir)?;
    Ok(())
}

fn source_bundle_excluded_roots(workspace_info: &pcb_zen::WorkspaceInfo) -> HashSet<PathBuf> {
    let mut excluded: HashSet<_> = workspace_info
        .packages
        .values()
        .map(|pkg| canonicalize(&pkg.dir(&workspace_info.root)))
        .collect();
    excluded.insert(canonicalize(&workspace_info.root.join("vendor")));
    excluded
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn workspace_package_rel_path(
    workspace_info: &pcb_zen::WorkspaceInfo,
    package_url: &str,
) -> Result<PathBuf> {
    if let Some(pkg) = workspace_info.packages.get(package_url) {
        return Ok(pkg.rel_path.clone());
    }
    if workspace_info.workspace_base_url().as_deref() == Some(package_url) {
        return Ok(PathBuf::new());
    }
    bail!("Unknown workspace package {}", package_url)
}

fn create_metadata_json(input: &MetadataInput<'_>) -> serde_json::Value {
    let rfc3339_timestamp = Utc::now().to_rfc3339();

    let mut release_obj = serde_json::json!({
        "schema_version": "1",
        "board_name": input.name,
        "git_version": input.version,
        "created_at": rfc3339_timestamp,
        "zen_file": input.zen_path.strip_prefix(input.workspace_root).expect("zen_file must be within workspace_root"),
        "workspace_root": input.workspace_root,
        "staging_directory": input.staging_dir
    });

    if let Some(layout_path) = input.layout_path {
        release_obj["layout_path"] = serde_json::json!(layout_path);
    }

    if let Some(description) = input.description
        && !description.is_empty()
    {
        release_obj["description"] = serde_json::json!(description);
    }

    if input.bom_strict {
        release_obj["bom"] = serde_json::json!({ "strict": true });
    }

    let workspace_root = input.workspace_root;
    let (branch, remotes) = {
        let _span = info_span!("collect_git_metadata").entered();
        (
            git::rev_parse_abbrev_ref_head(workspace_root),
            get_git_remotes(workspace_root),
        )
    };

    let mut git_obj = serde_json::json!({
        "describe": input.version,
        "hash": input.git_hash,
        "workspace": workspace_root.display().to_string(),
        "remotes": remotes
    });

    if let Some(branch) = branch {
        git_obj["branch"] = serde_json::Value::String(branch);
    }

    let mut system_obj = serde_json::json!({
        "user": std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cli_version": env!("CARGO_PKG_VERSION"),
    });

    if input.include_kicad_version {
        let kicad_version = {
            let _span = info_span!("detect_kicad_version").entered();
            pcb_kicad::get_kicad_version()
                .ok()
                .unwrap_or_else(|| "unknown".to_string())
        };
        system_obj["kicad_version"] = serde_json::Value::String(kicad_version);
    }

    serde_json::json!({
        "release": release_obj,
        "system": system_obj,
        "git": git_obj
    })
}

fn get_git_remotes(path: &Path) -> serde_json::Value {
    let mut remotes = serde_json::Map::new();
    let Some(remote_list) = git::run_output_opt(path, &["remote"]) else {
        return serde_json::Value::Object(remotes);
    };

    for name in remote_list.lines() {
        if let Ok(url) = git::get_remote_url_for(path, name) {
            remotes.insert(name.to_string(), serde_json::Value::String(url));
        }
    }

    serde_json::Value::Object(remotes)
}
