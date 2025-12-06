use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::{
    AssetDependencySpec, EvalContext, FileProvider, FileProviderError, Lockfile, PcbToml,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;
use zip::ZipArchive;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).ok();
}

/// File provider backed by an in-memory zip archive
struct ZipFileProvider {
    archive: Mutex<ZipArchive<Cursor<Vec<u8>>>>,
    cache: Mutex<HashMap<String, String>>,
    file_index: HashSet<String>,
}

impl ZipFileProvider {
    fn new(zip_bytes: Vec<u8>) -> Result<Self, zip::result::ZipError> {
        let cursor = Cursor::new(zip_bytes);
        let mut archive = ZipArchive::new(cursor)?;

        let file_index = (0..archive.len())
            .filter_map(|i| Some(archive.by_index(i).ok()?.name().to_string()))
            .collect();

        Ok(Self {
            archive: Mutex::new(archive),
            cache: Mutex::new(HashMap::new()),
            file_index,
        })
    }

    fn normalize(path: &Path) -> String {
        path.to_string_lossy().trim_start_matches('/').to_string()
    }

    fn has_prefix(&self, prefix: &str) -> bool {
        self.file_index.iter().any(|f| f.starts_with(prefix))
    }
}

impl FileProvider for ZipFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        let normalized = Self::normalize(path);

        if let Some(cached) = self.cache.lock().unwrap().get(&normalized).cloned() {
            return Ok(cached);
        }

        let contents = {
            let mut archive = self.archive.lock().unwrap();
            let mut file = archive.by_name(&normalized).map_err(|e| match e {
                zip::result::ZipError::FileNotFound => {
                    FileProviderError::NotFound(path.to_path_buf())
                }
                _ => FileProviderError::IoError(format!("Zip error for {normalized}: {e}")),
            })?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|e| FileProviderError::IoError(e.to_string()))?;
            contents
        };

        self.cache
            .lock()
            .unwrap()
            .insert(normalized, contents.clone());
        Ok(contents)
    }

    fn exists(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        self.file_index.contains(&normalized)
            || self.has_prefix(&format!("{}/", normalized.trim_end_matches('/')))
    }

    fn is_directory(&self, path: &Path) -> bool {
        self.has_prefix(&format!("{}/", Self::normalize(path).trim_end_matches('/')))
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        let normalized = Self::normalize(path).trim_end_matches('/').to_string();
        let prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{normalized}/")
        };

        let entries: HashSet<_> = self
            .file_index
            .iter()
            .filter_map(|name| name.strip_prefix(&prefix))
            .filter_map(|rest| rest.split('/').next())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(entries.into_iter().map(|name| path.join(name)).collect())
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileProviderError> {
        let components: Vec<_> = path.components().fold(Vec::new(), |mut acc, c| {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    acc.pop();
                }
                Component::Normal(name) => acc.push(name),
                Component::RootDir | Component::Prefix(_) => acc.clear(),
            }
            acc
        });

        let mut result = if path.is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::new()
        };
        result.extend(components);
        Ok(result)
    }
}

/// Build V2 package resolution map from lockfile and vendored dependencies
fn resolve_v2_packages(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
) -> Option<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let lockfile = Lockfile::parse(
        &file_provider
            .read_file(&workspace_root.join("pcb.sum"))
            .ok()?,
    )
    .ok()?;
    let pcb_toml = PcbToml::parse(
        &file_provider
            .read_file(&workspace_root.join("pcb.toml"))
            .ok()?,
    )
    .ok()?;

    let vendor_dir = workspace_root.join("vendor");

    // Build URL -> vendored path mapping from lockfile
    let url_to_path: HashMap<String, PathBuf> = lockfile
        .iter()
        .filter_map(|entry| {
            let path = vendor_path(&vendor_dir, &entry.module_path, &entry.version);
            file_provider
                .exists(&path)
                .then(|| (entry.module_path.clone(), path))
        })
        .collect();

    // Workspace repository URL allows self-referential imports
    let workspace_repo_url = pcb_toml
        .workspace
        .as_ref()
        .and_then(|w| w.repository.clone());

    let build_pkg_map = |config: &PcbToml| -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();

        if let Some(ref repo_url) = workspace_repo_url {
            map.insert(repo_url.clone(), workspace_root.to_path_buf());
        }

        for url in config.dependencies.keys() {
            if let Some(path) = url_to_path.get(url) {
                map.insert(url.clone(), path.clone());
            }
        }

        for (asset_key, asset_spec) in &config.assets {
            if let Some(ref_str) = extract_asset_ref(asset_spec) {
                let path = vendor_path(&vendor_dir, asset_key, &ref_str);
                if file_provider.exists(&path) {
                    map.insert(asset_key.clone(), path);
                }
            }
        }

        map
    };

    let mut results: HashMap<PathBuf, BTreeMap<String, PathBuf>> = HashMap::new();
    results.insert(workspace_root.to_path_buf(), build_pkg_map(&pcb_toml));

    // Add vendored packages
    for vendor_path in url_to_path.values() {
        if let Ok(content) = file_provider.read_file(&vendor_path.join("pcb.toml")) {
            if let Ok(pkg_config) = PcbToml::parse(&content) {
                results.insert(vendor_path.clone(), build_pkg_map(&pkg_config));
            }
        }
    }

    // Discover workspace member packages by scanning for pcb.toml files
    let mut dirs_to_scan = vec![workspace_root.to_path_buf()];
    while let Some(dir) = dirs_to_scan.pop() {
        if let Ok(entries) = file_provider.list_directory(&dir) {
            for entry in entries {
                if entry == workspace_root.join("vendor") {
                    continue;
                }
                if file_provider.is_directory(&entry) {
                    if let Ok(content) = file_provider.read_file(&entry.join("pcb.toml")) {
                        if let Ok(member_config) = PcbToml::parse(&content) {
                            results.insert(entry.clone(), build_pkg_map(&member_config));
                        }
                    }
                    dirs_to_scan.push(entry);
                }
            }
        }
    }

    Some(results)
}

fn vendor_path(vendor_dir: &Path, module_path: &str, version: &str) -> PathBuf {
    let (repo_url, subpath) = pcb_zen_core::config::split_asset_repo_and_subpath(module_path);
    let base = vendor_dir.join(repo_url).join(version);
    if subpath.is_empty() {
        base
    } else {
        base.join(subpath)
    }
}

fn extract_asset_ref(spec: &AssetDependencySpec) -> Option<String> {
    match spec {
        AssetDependencySpec::Ref(r) if r != "HEAD" => Some(r.clone()),
        AssetDependencySpec::Detailed(d) => d
            .version
            .clone()
            .or_else(|| d.branch.clone())
            .or_else(|| d.rev.clone())
            .filter(|r| r != "HEAD"),
        _ => None,
    }
}

fn diagnostic_to_json(diag: &pcb_zen_core::Diagnostic) -> DiagnosticInfo {
    let level = match diag.severity {
        starlark::errors::EvalSeverity::Error => "error",
        starlark::errors::EvalSeverity::Warning => "warning",
        starlark::errors::EvalSeverity::Advice => "info",
        starlark::errors::EvalSeverity::Disabled => "info",
    }
    .to_string();

    DiagnosticInfo {
        level,
        message: diag.body.clone(),
        file: Some(diag.path.clone()),
        line: diag.span.as_ref().map(|s| s.begin.line as u32),
        child: diag.child.as_ref().map(|c| Box::new(diagnostic_to_json(c))),
    }
}

/// Evaluate a Zener module from a zip archive.
///
/// Works with both V1 (no pcb.sum) and V2 (with pcb.sum) release zips.
/// All dependencies must be vendored in the zip.
#[wasm_bindgen]
pub fn evaluate(
    zip_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<JsValue, JsValue> {
    let file_provider = Arc::new(
        ZipFileProvider::new(zip_bytes)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse zip: {e}")))?,
    );

    let main_path = PathBuf::from(main_file);
    let workspace_root = find_workspace_root(file_provider.as_ref(), &main_path);

    let v2_resolutions = resolve_v2_packages(file_provider.as_ref(), &workspace_root);

    let load_resolver = Arc::new(pcb_zen_core::CoreLoadResolver::new(
        file_provider.clone(),
        Arc::new(pcb_zen_core::NoopRemoteFetcher),
        workspace_root,
        true,
        v2_resolutions,
    ));

    let inputs: HashMap<String, serde_json::Value> = serde_json::from_str(inputs_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse inputs: {e}")))?;

    let mut ctx = EvalContext::new(load_resolver).set_source_path(main_path);

    if !inputs.is_empty() {
        let json_map = starlark::collections::SmallMap::from_iter(inputs);
        ctx.set_json_inputs(json_map);
    }

    let result = ctx.eval();

    let schematic_opt = result.output.as_ref().and_then(|o| o.to_schematic().ok());

    let parameters = result.output.as_ref().map(|o| o.signature.clone());

    let bom_json = schematic_opt.as_ref().map(|s| s.bom().ungrouped_json());

    let evaluation_result = EvaluationResult {
        success: result.output.is_some(),
        parameters,
        schematic: schematic_opt.and_then(|s| serde_json::to_string(&s).ok()),
        bom: bom_json,
        diagnostics: result
            .diagnostics
            .into_iter()
            .map(|d| diagnostic_to_json(&d))
            .collect(),
    };

    serde_wasm_bindgen::to_value(&evaluation_result)
        .map_err(|e| JsValue::from_str(&format!("Failed to serialize result: {e}")))
}

#[derive(Serialize, Deserialize)]
pub struct DiagnosticInfo {
    pub level: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub child: Option<Box<DiagnosticInfo>>,
}

#[derive(Serialize, Deserialize)]
pub struct EvaluationResult {
    pub success: bool,
    pub parameters: Option<Vec<pcb_zen_core::lang::type_info::ParameterInfo>>,
    pub schematic: Option<String>,
    pub bom: Option<String>,
    pub diagnostics: Vec<DiagnosticInfo>,
}
