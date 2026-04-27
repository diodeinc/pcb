use std::collections::BTreeMap;

use anyhow::{Context, Result};
use pcb_zen_core::config::{DependencySpec, PcbToml};

use super::mvs::PackageResolution;
use super::target::AddTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WritebackSummary {
    pub(crate) changed: bool,
}

pub(crate) fn write_package_manifest(
    target: &AddTarget,
    resolution: &PackageResolution,
) -> Result<WritebackSummary> {
    let original = std::fs::read_to_string(&target.pcb_toml_path)
        .with_context(|| format!("Failed to read {}", target.pcb_toml_path.display()))?;
    let mut config: PcbToml = toml::from_str(&original)
        .with_context(|| format!("Failed to parse {}", target.pcb_toml_path.display()))?;

    config.dependencies.direct = resolution.direct.clone();
    config.dependencies.indirect = indirect_dependencies(resolution);

    let rendered = render_manifest(&config)?;
    let changed = rendered != original;
    if changed {
        std::fs::write(&target.pcb_toml_path, rendered)
            .with_context(|| format!("Failed to write {}", target.pcb_toml_path.display()))?;
    }

    Ok(WritebackSummary { changed })
}

fn indirect_dependencies(resolution: &PackageResolution) -> BTreeMap<String, DependencySpec> {
    resolution
        .resolved_remote
        .iter()
        .filter(|(dep_id, _)| !resolution.direct_remote_ids.contains(*dep_id))
        .map(|(dep_id, version)| {
            (
                dep_id.indirect_key(),
                DependencySpec::Version(version.to_string()),
            )
        })
        .collect()
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
    use std::collections::BTreeMap;

    use semver::Version;
    use tempfile::tempdir;

    use super::*;
    use crate::pcb_mod::dep_id::ResolvedDepId;

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
            direct: BTreeMap::from([
                (
                    "github.com/example/direct".to_string(),
                    DependencySpec::Version("0.2.0".to_string()),
                ),
                (
                    "github.com/example/workspace/components/Local".to_string(),
                    DependencySpec::Version("0.4.5".to_string()),
                ),
            ]),
            direct_remote_ids: std::collections::BTreeSet::from([ResolvedDepId::new(
                "github.com/example/direct",
                "0.2",
            )]),
            resolved_remote: BTreeMap::from([
                (
                    ResolvedDepId::new("github.com/example/direct", "0.2"),
                    Version::parse("0.2.0").unwrap(),
                ),
                (
                    ResolvedDepId::new("github.com/example/transitive", "1"),
                    Version::parse("1.2.3").unwrap(),
                ),
            ]),
        };

        let summary = write_package_manifest(&target, &resolution).unwrap();
        assert!(summary.changed);

        let updated = std::fs::read_to_string(pcb_toml_path).unwrap();
        assert!(updated.contains("[board]"));
        assert!(updated.contains("\"github.com/example/direct\" = \"0.2.0\""));
        assert!(updated.contains("\"github.com/example/workspace/components/Local\" = \"0.4.5\""));
        assert!(updated.contains("[dependencies.indirect]"));
        assert!(updated.contains("\"github.com/example/transitive@1\" = \"1.2.3\""));
        assert!(!updated.contains("stale"));
        assert!(!updated.contains("old-transitive"));
    }

    #[test]
    fn writeback_keeps_indirect_lane_for_same_module_path() {
        let temp = tempdir().unwrap();
        let pcb_toml_path = temp.path().join("pcb.toml");
        std::fs::write(
            &pcb_toml_path,
            r#"[board]
name = "Demo"
"#,
        )
        .unwrap();

        let target = AddTarget {
            package_url: "github.com/example/workspace/boards/Demo".to_string(),
            pcb_toml_path: pcb_toml_path.clone(),
        };
        let resolution = PackageResolution {
            direct: BTreeMap::from([(
                "github.com/example/foo".to_string(),
                DependencySpec::Version("0.1.7".to_string()),
            )]),
            direct_remote_ids: std::collections::BTreeSet::from([ResolvedDepId::new(
                "github.com/example/foo",
                "0.1",
            )]),
            resolved_remote: BTreeMap::from([
                (
                    ResolvedDepId::new("github.com/example/foo", "0.1"),
                    Version::parse("0.1.7").unwrap(),
                ),
                (
                    ResolvedDepId::new("github.com/example/foo", "0.8"),
                    Version::parse("0.8.3").unwrap(),
                ),
            ]),
        };

        write_package_manifest(&target, &resolution).unwrap();

        let updated = std::fs::read_to_string(pcb_toml_path).unwrap();
        assert!(updated.contains("\"github.com/example/foo\" = \"0.1.7\""));
        assert!(updated.contains("\"github.com/example/foo@0.8\" = \"0.8.3\""));
    }
}
