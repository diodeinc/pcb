use anyhow::{Context, Result, bail};
use chrono::Utc;
use pcb_kicad::KiCadCliBuilder;
use pcb_zen::{copy_dir_all, git, vendor_deps};
use pcb_zen_core::resolution::{PackageClosure, ResolutionResult};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

const BUNDLE_ZSTD_LEVEL: i32 = 9;

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
}

pub(crate) struct SourceBundlePlan<'a> {
    pub resolution: &'a ResolutionResult,
    pub closure: Option<&'a PackageClosure>,
    pub staged_src: &'a Path,
    pub resolved_paths: &'a [PathBuf],
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
    let workspace_root = &plan.resolution.workspace_info.root;
    fs::create_dir_all(plan.staged_src)?;

    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        fs::copy(&root_pcb_toml, plan.staged_src.join("pcb.toml"))?;
    }

    let all_pkg_roots: HashSet<PathBuf> = plan
        .resolution
        .workspace_info
        .packages
        .values()
        .map(|pkg| workspace_root.join(&pkg.rel_path))
        .collect();

    {
        let _span = info_span!("copy_local_packages").entered();
        if let Some(closure) = plan.closure {
            let mut local_packages: Vec<_> = closure.local_packages.iter().collect();
            local_packages.sort();
            for pkg_url in local_packages {
                let Some(pkg) = plan.resolution.workspace_info.packages.get(pkg_url) else {
                    continue;
                };
                let dest = plan.staged_src.join(&pkg.rel_path);
                copy_dir_all(&pkg.dir(workspace_root), &dest, &all_pkg_roots)?;
            }
        }
    }

    {
        let _span = info_span!("copy_remote_packages").entered();
        let vendor_dir = plan.staged_src.join("vendor");
        if let Some(closure) = plan.closure {
            vendor_remote_closure_packages(plan.resolution, closure, &vendor_dir)?;
        } else {
            vendor_deps(
                plan.resolution,
                &["**".to_string()],
                Some(&vendor_dir),
                true,
            )?;
        }
    }

    {
        let _span = info_span!("stage_resolved_assets").entered();
        let package_roots = plan.resolution.package_roots();
        for resolved_path in plan.resolved_paths {
            stage_resolved_file_for_source_bundle(plan.staged_src, &package_roots, resolved_path)?;
        }
    }

    let lockfile_src = workspace_root.join("pcb.sum");
    if lockfile_src.exists() {
        fs::copy(&lockfile_src, plan.staged_src.join("pcb.sum"))?;
    }

    Ok(())
}

#[instrument(name = "write_canonical_bundle", skip_all)]
pub(crate) fn write_canonical_bundle(staging_dir: &Path, output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bundle_file = fs::File::create(output_path)?;
    let buffered = BufWriter::with_capacity(256 * 1024, bundle_file);
    let mut encoder = zstd::stream::write::Encoder::new(buffered, BUNDLE_ZSTD_LEVEL)?;
    pcb_canonical::create_canonical_tar(
        staging_dir,
        &mut encoder,
        Some(pcb_canonical::CanonicalTarOptions {
            exclude_nested_packages: false,
        }),
    )?;
    encoder.finish()?;
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

pub(crate) fn stage_resolved_file_for_source_bundle(
    staged_src: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
    resolved_path: &Path,
) -> Result<()> {
    let Some((repo, version, dep_root)) =
        pcb_zen_core::kicad_library::package_coord_for_path(resolved_path, package_roots)
    else {
        return Ok(());
    };

    if dep_root.join("pcb.toml").exists() {
        return Ok(());
    }

    let Ok(rel_path) = resolved_path.strip_prefix(&dep_root) else {
        return Ok(());
    };
    if rel_path.as_os_str().is_empty() {
        return Ok(());
    }
    if !resolved_path.exists() {
        log::warn!(
            "Skipping missing referenced library path during source bundle staging: {}",
            resolved_path.display()
        );
        return Ok(());
    }

    let dst = staged_src
        .join("vendor")
        .join(repo)
        .join(version)
        .join(rel_path);
    if resolved_path == dst || dst.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }

    let copy_result: Result<()> = if resolved_path.is_dir() {
        copy_dir_all(resolved_path, &dst, &HashSet::new())
    } else {
        fs::copy(resolved_path, &dst)
            .map(|_| ())
            .map_err(Into::into)
    };

    copy_result.with_context(|| {
        format!(
            "Failed to copy {} to {}",
            resolved_path.display(),
            dst.display()
        )
    })?;
    Ok(())
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
            get_kicad_version()
        };
        system_obj["kicad_version"] = serde_json::Value::String(kicad_version);
    }

    serde_json::json!({
        "release": release_obj,
        "system": system_obj,
        "git": git_obj
    })
}

pub(crate) fn get_kicad_version() -> String {
    KiCadCliBuilder::new()
        .command("version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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

#[instrument(name = "vendor_remote_closure_packages", skip_all)]
fn vendor_remote_closure_packages(
    resolution: &ResolutionResult,
    closure: &PackageClosure,
    vendor_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(vendor_dir)?;

    let workspace_info = &resolution.workspace_info;
    let workspace_vendor = workspace_info.root.join("vendor");

    let mut remote_packages: Vec<_> = closure.remote_packages.iter().collect();
    remote_packages.sort();

    for (module_path, version) in remote_packages {
        let vendor_src = workspace_vendor.join(module_path).join(version);
        let cache_src = workspace_info.cache_dir.join(module_path).join(version);
        let src = if vendor_src.exists() {
            vendor_src
        } else {
            cache_src
        };
        if !src.exists() {
            bail!("Missing package source for {}@{}", module_path, version);
        }

        let dst = vendor_dir.join(module_path).join(version);
        if dst.exists() {
            continue;
        }
        copy_dir_all(&src, &dst, &HashSet::new())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SourceBundlePlan, stage_source_bundle};
    use pcb_test_utils::sandbox::Sandbox;
    use pcb_zen::resolve_dependencies;
    use pcb_zen::workspace::get_workspace_info;
    use pcb_zen_core::DefaultFileProvider;

    const ROOT_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
"#;

    #[test]
    fn stage_source_bundle_without_closure_still_copies_root_files() {
        let mut sb = Sandbox::new();
        sb.cwd("src")
            .write("pcb.toml", ROOT_PCB_TOML)
            .write("pcb.sum", "test/package 0.1.0 h1:test\n");

        let mut workspace = get_workspace_info(
            &DefaultFileProvider::new(),
            &sb.root_path().join("src"),
            true,
        )
        .unwrap();
        let resolution = resolve_dependencies(&mut workspace, false, false).unwrap();
        let staged_src = sb.root_path().join("staged/src");

        stage_source_bundle(&SourceBundlePlan {
            resolution: &resolution,
            closure: None,
            staged_src: &staged_src,
            resolved_paths: &[],
        })
        .unwrap();

        assert!(staged_src.join("pcb.toml").exists());
        assert!(staged_src.join("pcb.sum").exists());
    }
}
