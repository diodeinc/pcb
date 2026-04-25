use std::collections::BTreeMap;

use anyhow::{Context, Result};
use pcb_zen::cache_index::CacheIndex;
use pcb_zen::tags;
use pcb_zen_core::config::DependencySpec;
use pcb_zen_core::kicad_library::effective_kicad_library_for_repo;
use semver::Version;

use super::dep_id::{ResolvedDepId, parse_lane_qualified_key};

#[derive(Debug, Clone, Default)]
pub(crate) struct ManifestRequirements {
    pub(crate) direct: BTreeMap<String, DependencySpec>,
    pub(crate) indirect: BTreeMap<ResolvedDepId, Version>,
}

pub(crate) struct ManifestLoader {
    workspace: pcb_zen::WorkspaceInfo,
    offline: bool,
    cache: BTreeMap<(String, String), ManifestRequirements>,
}

impl ManifestLoader {
    pub(crate) fn new(workspace: pcb_zen::WorkspaceInfo, offline: bool) -> Self {
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
    workspace: &pcb_zen::WorkspaceInfo,
    index: &CacheIndex,
    module_path: &str,
    version: &Version,
    offline: bool,
) -> Result<ManifestRequirements> {
    if let Some(loaded) = synthetic_kicad_manifest(workspace, module_path, version)? {
        return Ok(loaded);
    }

    let pcb_toml_path = if offline {
        workspace
            .workspace_cache_dir()
            .join(module_path)
            .join(version.to_string())
            .join("pcb.toml")
    } else {
        pcb_zen::resolve::ensure_package_manifest_in_cache(module_path, version, index)
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
    let version = tags::parse_relaxed_version(&raw_version).ok_or_else(|| {
        anyhow::anyhow!(
            "Indirect dependency {} has invalid version '{}'",
            dep_id.indirect_key(),
            raw_version
        )
    })?;
    let expected_lane = super::dep_id::compatibility_lane(&version);
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

fn synthetic_kicad_manifest(
    workspace: &pcb_zen::WorkspaceInfo,
    module_path: &str,
    version: &Version,
) -> Result<Option<ManifestRequirements>> {
    let workspace_cfg = workspace.workspace_config();
    let Some(entry) =
        effective_kicad_library_for_repo(&workspace_cfg.kicad_library, module_path, version)
    else {
        return Ok(None);
    };

    let mut requirements = BTreeMap::new();
    let spec = || DependencySpec::Version(version.to_string());

    if module_path == entry.symbols {
        requirements.insert(entry.footprints.clone(), spec());
    }

    if module_path == entry.footprints {
        for model_repo in entry.models.values() {
            requirements.insert(model_repo.clone(), spec());
        }
    }

    Ok(Some(ManifestRequirements {
        direct: requirements,
        indirect: BTreeMap::new(),
    }))
}
