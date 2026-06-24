use std::collections::BTreeMap;

use anyhow::{Context, Result};
use pcb_zen::package_resolver::PackageResolution;
use pcb_zen_core::config::{DependencySpec, PcbToml};

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
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}
