use std::collections::BTreeMap;

use anyhow::{Context, Result};
use pcb_zen_core::config::{DependencySpec, PcbToml};

use super::mvs::PackageResolution;
use super::target::AddTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WritebackSummary {
    pub(crate) changed: bool,
    pub(crate) direct_count: usize,
    pub(crate) indirect_count: usize,
}

pub(crate) fn write_package_manifest(
    target: &AddTarget,
    resolution: &PackageResolution,
) -> Result<WritebackSummary> {
    let original = std::fs::read_to_string(&target.pcb_toml_path)
        .with_context(|| format!("Failed to read {}", target.pcb_toml_path.display()))?;
    let mut config: PcbToml = toml::from_str(&original)
        .with_context(|| format!("Failed to parse {}", target.pcb_toml_path.display()))?;

    let (direct, indirect) = dependency_tables(resolution)?;
    let direct_count = direct.len();
    let indirect_count = indirect.len();

    config.dependencies.direct = direct;
    config.dependencies.indirect = indirect;

    let rendered = render_manifest(&config)?;
    let changed = rendered != original;
    if changed {
        std::fs::write(&target.pcb_toml_path, rendered)
            .with_context(|| format!("Failed to write {}", target.pcb_toml_path.display()))?;
    }

    Ok(WritebackSummary {
        changed,
        direct_count,
        indirect_count,
    })
}

fn dependency_tables(
    resolution: &PackageResolution,
) -> Result<(
    BTreeMap<String, DependencySpec>,
    BTreeMap<String, DependencySpec>,
)> {
    let direct = resolution
        .scanned
        .remote
        .keys()
        .map(|module_path| {
            let version = resolution.resolved_remote.get(module_path).ok_or_else(|| {
                anyhow::anyhow!(
                    "Resolved closure is missing direct dependency {}",
                    module_path
                )
            })?;
            Ok((
                module_path.clone(),
                DependencySpec::Version(version.to_string()),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let indirect = resolution
        .resolved_remote
        .iter()
        .filter(|(module_path, _)| !direct.contains_key(*module_path))
        .map(|(module_path, version)| {
            (
                module_path.clone(),
                DependencySpec::Version(version.to_string()),
            )
        })
        .collect();

    Ok((direct, indirect))
}

fn render_manifest(config: &PcbToml) -> Result<String> {
    let mut rendered = toml::to_string_pretty(config)?;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use semver::Version;
    use tempfile::tempdir;

    use super::*;
    use crate::add::scan::ScannedDirectDeps;

    #[test]
    fn writeback_rebuilds_direct_and_indirect_dependency_tables() {
        let temp = tempdir().unwrap();
        let pcb_toml_path = temp.path().join("pcb.toml");
        std::fs::write(
            &pcb_toml_path,
            r#"[board]
name = "Demo"

[dependencies]
"github.com/example/stale" = "9.9.9"
"github.com/example/direct" = "0.1.0"

[dependencies.indirect]
"github.com/example/old-transitive" = "1.0.0"
"#,
        )
        .unwrap();

        let target = AddTarget {
            package_url: "github.com/example/workspace/boards/Demo".to_string(),
            pcb_toml_path: pcb_toml_path.clone(),
        };
        let resolution = PackageResolution {
            scanned: ScannedDirectDeps {
                remote: BTreeMap::from([(
                    "github.com/example/direct".to_string(),
                    DependencySpec::Version("0.1.0".to_string()),
                )]),
                workspace: BTreeSet::new(),
                implicit_remote: BTreeMap::new(),
            },
            resolved_remote: BTreeMap::from([
                (
                    "github.com/example/direct".to_string(),
                    Version::parse("0.2.0").unwrap(),
                ),
                (
                    "github.com/example/transitive".to_string(),
                    Version::parse("1.2.3").unwrap(),
                ),
            ]),
        };

        let summary = write_package_manifest(&target, &resolution).unwrap();
        assert!(summary.changed);
        assert_eq!(summary.direct_count, 1);
        assert_eq!(summary.indirect_count, 1);

        let updated = std::fs::read_to_string(pcb_toml_path).unwrap();
        assert!(updated.contains("[board]"));
        assert!(updated.contains("\"github.com/example/direct\" = \"0.2.0\""));
        assert!(updated.contains("[dependencies.indirect]"));
        assert!(updated.contains("\"github.com/example/transitive\" = \"1.2.3\""));
        assert!(!updated.contains("stale"));
        assert!(!updated.contains("old-transitive"));
    }
}
