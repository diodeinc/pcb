use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache_index::CacheIndex;
use anyhow::{Context, Result};
use pcb_zen_core::config::{DependencySpec, ManifestPart};
use semver::Version;

use super::{ResolvedDepId, compatibility_lane, parse_lane_qualified_key};

#[derive(Debug, Clone, Default)]
pub(crate) struct ManifestRequirements {
    pub(crate) direct: BTreeMap<String, DependencySpec>,
    pub(crate) indirect: BTreeMap<ResolvedDepId, Version>,
    pub(crate) parts: Vec<ManifestPart>,
}

pub(crate) struct ManifestLoader {
    workspace: crate::WorkspaceInfo,
    offline: bool,
    cache: BTreeMap<(String, String), ManifestRequirements>,
}

impl ManifestLoader {
    pub(crate) fn new(workspace: crate::WorkspaceInfo, offline: bool) -> Self {
        Self {
            workspace,
            offline,
            cache: BTreeMap::new(),
        }
    }

    pub(crate) fn load(
        &mut self,
        index: &CacheIndex,
        module_path: &str,
        version: &Version,
    ) -> Result<ManifestRequirements> {
        let key = (module_path.to_string(), version.to_string());
        if let Some(loaded) = self.cache.get(&key) {
            return Ok(loaded.clone());
        }

        let loaded = load_manifest_for_module_version(
            &self.workspace,
            index,
            module_path,
            version,
            self.offline,
        )?;
        self.cache.insert(key, loaded.clone());
        Ok(loaded)
    }
}

pub(crate) fn load_manifest_for_module_version(
    workspace: &crate::WorkspaceInfo,
    index: &CacheIndex,
    module_path: &str,
    version: &Version,
    offline: bool,
) -> Result<ManifestRequirements> {
    let vendor_toml_path =
        package_version_root(workspace.root.join("vendor"), module_path, version).join("pcb.toml");
    let pcb_toml_path = if vendor_toml_path.exists() {
        vendor_toml_path
    } else if offline {
        package_version_root(workspace.workspace_cache_dir(), module_path, version).join("pcb.toml")
    } else {
        crate::resolve::ensure_package_manifest_in_cache(module_path, version, index)
            .with_context(|| format!("Failed to materialize {}@{}", module_path, version))?
    };
    let content = std::fs::read_to_string(&pcb_toml_path)
        .with_context(|| format!("Failed to read {}", pcb_toml_path.display()))?;
    let manifest = pcb_zen_core::config::PcbToml::parse(&content)
        .with_context(|| format!("Failed to parse {}", pcb_toml_path.display()))?;
    let has_indirect_table = manifest_has_indirect_table(&content)?;

    let indirect = if has_indirect_table {
        manifest
            .dependencies
            .indirect
            .into_iter()
            .map(|(raw_key, spec)| parse_indirect_dependency(&raw_key, spec))
            .collect::<Result<BTreeMap<_, _>>>()?
    } else {
        BTreeMap::new()
    };

    Ok(ManifestRequirements {
        direct: manifest.dependencies.direct,
        indirect,
        parts: manifest.parts,
    })
}

fn parse_indirect_dependency(
    raw_key: &str,
    spec: DependencySpec,
) -> Result<(ResolvedDepId, Version)> {
    let dep_id = parse_lane_qualified_key(raw_key)?;
    let DependencySpec::Version(raw_version) = spec else {
        anyhow::bail!(
            "Indirect dependency {} must be an exact version string",
            dep_id.indirect_key()
        );
    };
    let version = pcb_zen_core::parse_relaxed_version(&raw_version).ok_or_else(|| {
        anyhow::anyhow!(
            "Indirect dependency {} has invalid version '{}'",
            dep_id.indirect_key(),
            raw_version
        )
    })?;
    let expected_lane = compatibility_lane(&version);
    if dep_id.lane != expected_lane {
        anyhow::bail!(
            "Indirect dependency {} resolved to lane {}, not {}",
            dep_id.path,
            expected_lane,
            dep_id.lane
        );
    }
    Ok((dep_id, version))
}

fn manifest_has_indirect_table(content: &str) -> Result<bool> {
    let value: toml::Value = toml::from_str(content)?;
    Ok(value
        .get("dependencies")
        .and_then(|deps| deps.get("indirect"))
        .is_some())
}

pub(crate) fn package_version_root(
    root: impl AsRef<Path>,
    module_path: &str,
    version: &Version,
) -> PathBuf {
    root.as_ref().join(module_path).join(version.to_string())
}
