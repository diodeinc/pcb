use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_zen_core::config::{
    PcbToml, parse_pcb_version, pcb_version_from_cargo, pcb_version_is_older,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, value};

type PcbLane = (u32, u32);

struct Migration {
    target: PcbLane,
    apply: fn(&Path) -> Result<()>,
}

// Add migrations by target lane; the manifest version is bumped only after all apply.
const MIGRATIONS: &[Migration] = &[];

/// Arguments for the `migrate` command
#[derive(Args, Debug, Default, Clone)]
#[command(about = "Run available PCB project migrations")]
pub struct MigrateArgs {
    /// One or more paths to consider for migration.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

/// Execute the `migrate` command
pub fn execute(args: MigrateArgs) -> Result<()> {
    let roots = migration_roots(args.paths)?;
    for root in roots {
        migrate_workspace(&root)?;
    }
    Ok(())
}

fn migration_roots(paths: Vec<PathBuf>) -> Result<BTreeSet<PathBuf>> {
    let starts = if paths.is_empty() {
        vec![std::env::current_dir()?]
    } else {
        paths
    };

    let mut roots = BTreeSet::new();
    for path in starts {
        let root = find_migration_root(&path)
            .with_context(|| format!("Failed to find workspace for {}", path.display()))?;
        let pcb_toml_path = root.join("pcb.toml");
        if !pcb_toml_path.is_file() {
            bail!(
                "No pcb.toml found for migration target {}\n  Start from a workspace or pass a path inside one.",
                path.display()
            );
        }
        roots.insert(root);
    }
    Ok(roots)
}

fn find_migration_root(start: &Path) -> Result<PathBuf> {
    let abs_start = fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let start_dir = if abs_start.is_dir() {
        abs_start
    } else {
        abs_start.parent().unwrap_or(&abs_start).to_path_buf()
    };

    let mut candidates = Vec::new();
    for dir in std::iter::successors(Some(start_dir.as_path()), |dir| dir.parent()) {
        let pcb_toml = dir.join("pcb.toml");
        if !pcb_toml.is_file() {
            continue;
        }

        let content = fs::read_to_string(&pcb_toml)
            .with_context(|| format!("Failed to read {}", pcb_toml.display()))?;
        let document = content
            .parse::<DocumentMut>()
            .with_context(|| format!("Failed to parse {}", pcb_toml.display()))?;
        let is_workspace = document
            .get("workspace")
            .and_then(Item::as_table_like)
            .is_some();
        candidates.push((dir.to_path_buf(), is_workspace));
    }

    Ok(candidates
        .iter()
        .find(|(_, is_workspace)| *is_workspace)
        .or_else(|| candidates.first())
        .map(|(path, _)| path.clone())
        .unwrap_or(start_dir))
}

fn migrate_workspace(root: &Path) -> Result<()> {
    let pcb_toml_path = root.join("pcb.toml");
    let original = fs::read_to_string(&pcb_toml_path)
        .with_context(|| format!("Failed to read {}", pcb_toml_path.display()))?;
    let (content, removed_members) = remove_workspace_members(&original)
        .with_context(|| format!("Failed to update {}", pcb_toml_path.display()))?;
    if removed_members {
        fs::write(&pcb_toml_path, &content)
            .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;
        println!(
            "pcb: removed deprecated [workspace].members from {}",
            pcb_toml_path.display()
        );
    }

    let config = PcbToml::parse_with_path(&content, &pcb_toml_path)?;
    let workspace = config.workspace.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Migration target is not a workspace manifest: {}",
            pcb_toml_path.display()
        )
    })?;

    let target = pcb_version_from_cargo();
    let target_lane = parse_pcb_version(&target).expect("running pcb version must be major.minor");
    let existing = workspace.pcb_version.as_deref();

    if let Some(required) = existing
        && pcb_version_is_older(&target, required) == Some(true)
    {
        bail!(
            "Workspace requires pcb-version = \"{}\" but the current pcbc is {}\n  \
             Run `pcb migrate` without a +toolchain override, or install a newer toolchain.\n  \
             Manifest: {}",
            required,
            target,
            pcb_toml_path.display()
        );
    }

    if existing == Some(target.as_str()) {
        println!(
            "pcb: {} already uses pcb-version = \"{}\"",
            pcb_toml_path.display(),
            target
        );
        return Ok(());
    }

    let from_lane = existing.and_then(parse_pcb_version);
    run_versioned_migrations(root, from_lane, target_lane)?;

    write_workspace_pcb_version(&pcb_toml_path, &target)?;

    if let Some(previous) = existing {
        println!(
            "pcb: migrated {} from pcb-version = \"{}\" to \"{}\"",
            pcb_toml_path.display(),
            previous,
            target
        );
    } else {
        println!(
            "pcb: set {} pcb-version = \"{}\"",
            pcb_toml_path.display(),
            target
        );
    }
    Ok(())
}

fn write_workspace_pcb_version(pcb_toml_path: &Path, target: &str) -> Result<()> {
    let content = fs::read_to_string(pcb_toml_path)
        .with_context(|| format!("Failed to read {}", pcb_toml_path.display()))?;
    let (content, _) = remove_workspace_members(&content)
        .with_context(|| format!("Failed to update {}", pcb_toml_path.display()))?;
    let config = PcbToml::parse_with_path(&content, pcb_toml_path)?;
    let workspace = config.workspace.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Migration target is not a workspace manifest: {}",
            pcb_toml_path.display()
        )
    })?;
    let updated = set_workspace_pcb_version(&content, target, workspace.pcb_version.is_none())
        .with_context(|| format!("Failed to update {}", pcb_toml_path.display()))?;
    PcbToml::parse_with_path(&updated, pcb_toml_path)?;
    fs::write(pcb_toml_path, updated)
        .with_context(|| format!("Failed to write {}", pcb_toml_path.display()))?;
    Ok(())
}

fn remove_workspace_members(content: &str) -> Result<(String, bool)> {
    let mut document = content
        .parse::<DocumentMut>()
        .context("Failed to parse manifest for editing")?;
    let Some(workspace) = document
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
    else {
        return Ok((content.to_owned(), false));
    };

    let removed = workspace.remove("members").is_some();
    if removed {
        Ok((document.to_string(), true))
    } else {
        Ok((content.to_owned(), false))
    }
}

fn run_versioned_migrations(root: &Path, from: Option<PcbLane>, to: PcbLane) -> Result<()> {
    for migration in MIGRATIONS {
        if from.is_none_or(|from| from < migration.target) && migration.target <= to {
            (migration.apply)(root).with_context(|| {
                format!(
                    "Failed to apply migration for pcb-version {}",
                    format_lane(migration.target)
                )
            })?;
            println!(
                "pcb: applied migration for pcb-version {}",
                format_lane(migration.target)
            );
        }
    }
    Ok(())
}

fn format_lane((major, minor): PcbLane) -> String {
    format!("{major}.{minor}")
}

fn set_workspace_pcb_version(
    content: &str,
    version: &str,
    insert_if_missing: bool,
) -> Result<String> {
    let mut document = content
        .parse::<DocumentMut>()
        .context("Failed to parse manifest for editing")?;
    let workspace = document
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("Could not locate a [workspace] table"))?;

    match workspace.get_mut("pcb-version") {
        Some(item) => replace_string_value(item, version)?,
        None if insert_if_missing => {
            workspace.insert("pcb-version", value(version));
        }
        None => bail!("Could not locate the existing [workspace].pcb-version assignment"),
    }

    Ok(document.to_string())
}

fn replace_string_value(item: &mut Item, version: &str) -> Result<()> {
    let original = item
        .as_value()
        .filter(|value| value.as_str().is_some())
        .ok_or_else(|| anyhow::anyhow!("existing [workspace].pcb-version is not a string"))?;
    let decor = original.decor().clone();

    let mut replacement = value(version);
    if let Some(value) = replacement.as_value_mut() {
        *value.decor_mut() = decor;
    }
    *item = replacement;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_workspace_pcb_version_without_reformatting_manifest() {
        let input = r#"# keep
[workspace] # root
name = "demo"
pcb-version = "0.3" # old lane

[dependencies]
pcb-version = "not this"
"#;

        let output = set_workspace_pcb_version(input, "0.4", false).unwrap();

        assert!(output.contains("# keep\n[workspace] # root\nname = \"demo\"\n"));
        assert!(output.contains("pcb-version = \"0.4\" # old lane"));
        assert!(output.contains("[dependencies]\npcb-version = \"not this\""));
    }

    #[test]
    fn inserts_missing_workspace_pcb_version() {
        let input = "[workspace]\nname = \"demo\"\n";

        let output = set_workspace_pcb_version(input, "0.4", true).unwrap();

        assert_eq!(
            output,
            "[workspace]\nname = \"demo\"\npcb-version = \"0.4\"\n"
        );
    }

    #[test]
    fn removes_workspace_members_without_touching_other_tables() {
        let input = r#"# keep
[workspace] # root
name = "demo"
members = ["boards/*"] # old
pcb-version = "0.3"

[dependencies]
members = "not this"
"#;

        let (output, removed) = remove_workspace_members(input).unwrap();

        assert!(removed);
        assert!(output.contains("# keep\n[workspace] # root\nname = \"demo\"\n"));
        assert!(!output.contains("members = [\"boards/*\"]"));
        assert!(output.contains("[dependencies]\nmembers = \"not this\""));
    }

    #[test]
    fn writes_workspace_pcb_version_from_current_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pcb.toml");
        fs::write(&path, "[workspace]\npcb-version = \"0.3\"\n").unwrap();

        fs::write(
            &path,
            "[workspace]\npcb-version = \"0.3\"\nname = \"from-migration\"\n",
        )
        .unwrap();
        write_workspace_pcb_version(&path, "0.4").unwrap();

        let output = fs::read_to_string(path).unwrap();
        assert!(output.contains("name = \"from-migration\""));
        assert!(output.contains("pcb-version = \"0.4\""));
    }
}
