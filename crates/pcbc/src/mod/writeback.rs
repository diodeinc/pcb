use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_zen::package_resolver::PackageResolution;
use pcb_zen_core::config::{DependencySpec, PcbToml};

use super::target::AddTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestEdit {
    pub(crate) path: PathBuf,
    pub(crate) rendered: String,
}

impl ManifestEdit {
    pub(crate) fn apply(&self) -> Result<()> {
        std::fs::write(&self.path, &self.rendered)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

pub(crate) fn plan_package_manifest(
    target: &AddTarget,
    resolution: &PackageResolution,
) -> Result<Option<ManifestEdit>> {
    let original = std::fs::read_to_string(&target.pcb_toml_path)
        .with_context(|| format!("Failed to read {}", target.pcb_toml_path.display()))?;
    let mut config: PcbToml = toml::from_str(&original)
        .with_context(|| format!("Failed to parse {}", target.pcb_toml_path.display()))?;

    config.dependencies.direct = resolution.direct.clone();
    config.dependencies.indirect = indirect_dependencies(resolution);

    let rendered = render_manifest(&config)?;
    let changed = rendered != original;
    if !changed {
        return Ok(None);
    }

    Ok(Some(ManifestEdit {
        path: target.pcb_toml_path.clone(),
        rendered,
    }))
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
