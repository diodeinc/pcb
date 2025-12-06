use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::{
    vendor_path, EvalContext, FileProvider, FileProviderError, Lockfile, PcbToml,
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
    console_log::init_with_level(log::Level::Warn).ok();
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

/// Build V2 package resolution map from lockfile and vendored dependencies.
///
/// Assumes all deps are vendored in `vendor/`. No patches, no cache fallback.
fn resolve_v2_packages(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
) -> Option<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let lockfile_content = file_provider
        .read_file(&workspace_root.join("pcb.sum"))
        .ok()?;
    let lockfile = Lockfile::parse(&lockfile_content).ok()?;

    let root_toml_content = file_provider
        .read_file(&workspace_root.join("pcb.toml"))
        .ok()?;
    let root_toml = PcbToml::parse(&root_toml_content).ok()?;

    let vendor_dir = workspace_root.join("vendor");

    let workspace_repo_url = root_toml
        .workspace
        .as_ref()
        .and_then(|w| w.repository.clone());

    // Build URL -> vendored path from lockfile (code deps + assets)
    let lockfile_paths: HashMap<String, PathBuf> = lockfile
        .iter()
        .filter_map(|entry| {
            let path = vendor_path(&vendor_dir, &entry.module_path, &entry.version);
            file_provider
                .exists(&path)
                .then(|| (entry.module_path.clone(), path))
        })
        .collect();

    // Discover workspace members
    let mut member_url_to_path: HashMap<String, PathBuf> = HashMap::new();
    let mut member_dirs: Vec<PathBuf> = Vec::new();

    let mut dirs_to_scan = vec![workspace_root.to_path_buf()];
    while let Some(dir) = dirs_to_scan.pop() {
        if let Ok(entries) = file_provider.list_directory(&dir) {
            for entry in entries {
                if entry == workspace_root.join("vendor") {
                    continue;
                }
                if file_provider.is_directory(&entry) {
                    if file_provider.exists(&entry.join("pcb.toml")) {
                        member_dirs.push(entry.clone());

                        if let Some(ref repo_url) = workspace_repo_url {
                            if let Ok(relative) = entry.strip_prefix(workspace_root) {
                                let relative_str = relative.to_string_lossy();
                                if !relative_str.is_empty() {
                                    let member_url = format!("{}/{}", repo_url, relative_str);
                                    member_url_to_path.insert(member_url, entry.clone());
                                }
                            }
                        }
                    }
                    dirs_to_scan.push(entry);
                }
            }
        }
    }

    // Resolve URL -> path: prefer workspace member, then lockfile/vendor
    let resolve_url = |url: &str| -> Option<PathBuf> {
        if let Some(path) = member_url_to_path.get(url) {
            return Some(path.clone());
        }

        // Check if URL is under workspace repo
        if let Some(ref repo_url) = workspace_repo_url {
            if let Some(subpath) = url.strip_prefix(repo_url).and_then(|s| s.strip_prefix('/')) {
                // Try longest matching member prefix
                for (member_url, member_path) in &member_url_to_path {
                    if url.starts_with(member_url)
                        && (url.len() == member_url.len()
                            || url.as_bytes().get(member_url.len()) == Some(&b'/'))
                    {
                        return Some(member_path.clone());
                    }
                }
                return Some(workspace_root.join(subpath));
            }
        }

        lockfile_paths.get(url).cloned()
    };

    // Build per-package resolution map
    let build_pkg_map = |config: &PcbToml| -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();

        if let Some(ref repo_url) = workspace_repo_url {
            map.insert(repo_url.clone(), workspace_root.to_path_buf());
        }

        for (url, path) in &member_url_to_path {
            map.insert(url.clone(), path.clone());
        }

        for url in config.dependencies.keys() {
            if let Some(path) = resolve_url(url) {
                map.insert(url.clone(), path);
            }
        }

        for asset_key in config.assets.keys() {
            if let Some(path) = lockfile_paths.get(asset_key) {
                map.insert(asset_key.clone(), path.clone());
            }
        }

        map
    };

    let mut results: HashMap<PathBuf, BTreeMap<String, PathBuf>> = HashMap::new();

    results.insert(workspace_root.to_path_buf(), build_pkg_map(&root_toml));

    for member_dir in &member_dirs {
        if let Ok(content) = file_provider.read_file(&member_dir.join("pcb.toml")) {
            if let Ok(member_config) = PcbToml::parse(&content) {
                results.insert(member_dir.clone(), build_pkg_map(&member_config));
            }
        }
    }

    for vendor_pkg_path in lockfile_paths.values() {
        if let Ok(content) = file_provider.read_file(&vendor_pkg_path.join("pcb.toml")) {
            if let Ok(pkg_config) = PcbToml::parse(&content) {
                results.insert(vendor_pkg_path.clone(), build_pkg_map(&pkg_config));
            }
        }
    }

    log::warn!(
        "V2 resolution map has {} entries: {:?}",
        results.len(),
        results.keys().collect::<Vec<_>>()
    );

    Some(results)
}

fn diagnostic_to_json(diag: &pcb_zen_core::Diagnostic) -> DiagnosticInfo {
    DiagnosticInfo {
        level: match diag.severity {
            starlark::errors::EvalSeverity::Error => "error",
            starlark::errors::EvalSeverity::Warning => "warning",
            _ => "info",
        }
        .to_string(),
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
        ctx.set_json_inputs(starlark::collections::SmallMap::from_iter(inputs));
    }

    let result = ctx.eval();
    let schematic_opt = result.output.as_ref().and_then(|o| o.to_schematic().ok());

    let evaluation_result = EvaluationResult {
        success: result.output.is_some(),
        parameters: result.output.as_ref().map(|o| o.signature.clone()),
        schematic: schematic_opt
            .as_ref()
            .and_then(|s| serde_json::to_string(s).ok()),
        bom: schematic_opt.as_ref().map(|s| s.bom().ungrouped_json()),
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
