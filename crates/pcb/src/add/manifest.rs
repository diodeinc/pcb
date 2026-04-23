use std::collections::BTreeMap;

use anyhow::{Context, Result};
use pcb_zen::cache_index::CacheIndex;
use pcb_zen_core::config::DependencySpec;
use pcb_zen_core::kicad_library::effective_kicad_library_for_repo;
use semver::Version;

pub(crate) struct ManifestLoader {
    workspace: pcb_zen::WorkspaceInfo,
    cache: BTreeMap<(String, String), BTreeMap<String, DependencySpec>>,
}

impl ManifestLoader {
    pub(crate) fn new(workspace: pcb_zen::WorkspaceInfo) -> Self {
        Self {
            workspace,
            cache: BTreeMap::new(),
        }
    }

    pub(crate) fn load(
        &mut self,
        index: &CacheIndex,
        module_path: &str,
        version: &Version,
    ) -> Result<BTreeMap<String, DependencySpec>> {
        let key = (module_path.to_string(), version.to_string());
        if let Some(loaded) = self.cache.get(&key) {
            return Ok(loaded.clone());
        }

        let loaded =
            load_manifest_for_module_version(&self.workspace, index, module_path, version)?;
        self.cache.insert(key, loaded.clone());
        Ok(loaded)
    }
}

pub(crate) fn load_manifest_for_module_version(
    workspace: &pcb_zen::WorkspaceInfo,
    index: &CacheIndex,
    module_path: &str,
    version: &Version,
) -> Result<BTreeMap<String, DependencySpec>> {
    if let Some(loaded) = synthetic_kicad_manifest(workspace, module_path, version)? {
        return Ok(loaded);
    }

    let pcb_toml_path =
        pcb_zen::resolve::ensure_package_manifest_in_cache(module_path, version, index)
            .with_context(|| format!("Failed to materialize {}@{}", module_path, version))?;
    let content = std::fs::read_to_string(&pcb_toml_path)
        .with_context(|| format!("Failed to read {}", pcb_toml_path.display()))?;
    let manifest = pcb_zen_core::config::PcbToml::parse(&content)
        .with_context(|| format!("Failed to parse {}", pcb_toml_path.display()))?;
    let has_indirect_table = manifest_has_indirect_table(&content)?;

    let mut requirements = manifest.dependencies.direct.clone();
    if has_indirect_table {
        requirements.extend(manifest.dependencies.indirect.clone());
    }

    Ok(requirements)
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
) -> Result<Option<BTreeMap<String, DependencySpec>>> {
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

    Ok(Some(requirements))
}
