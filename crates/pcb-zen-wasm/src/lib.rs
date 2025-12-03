use log::debug;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::{AssetDependencySpec, EvalContext, FileProvider, Lockfile, PcbToml};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

// JavaScript callback interface for remote fetching
#[wasm_bindgen]
extern "C" {
    /// JavaScript function that fetches a single file from a remote source.
    /// Takes a FetchRequest object and returns the file content.
    /// Returns the file content, or a string starting with "ERROR:" if the file doesn't exist.
    #[wasm_bindgen(js_namespace = ["__zen"], js_name = "fetchRemoteFile")]
    fn js_fetch_remote_file(request: JsValue) -> JsValue;

    /// JavaScript function that loads a single file.
    /// Takes a file path and returns the file content or an error.
    /// Returns the file content, or a string starting with "ERROR:" if the file doesn't exist.
    #[wasm_bindgen(js_namespace = ["__zen"], js_name = "loadFile")]
    fn js_load_file(path: &str) -> JsValue;
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).expect("Failed to initialize console log");
    debug!("Initialized pcb-zen-wasm logger");
}

/// Remote fetcher that uses JavaScript functions to fetch remote files
struct WasmRemoteFetcher {
    file_provider: Arc<Mutex<pcb_zen_core::InMemoryFileProvider>>,
}

impl WasmRemoteFetcher {
    fn new(file_provider: Arc<Mutex<pcb_zen_core::InMemoryFileProvider>>) -> Self {
        Self { file_provider }
    }
}

impl pcb_zen_core::RemoteFetcher for WasmRemoteFetcher {
    fn fetch_remote(
        &self,
        spec: &pcb_zen_core::LoadSpec,
        _workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        debug!("WasmRemoteFetcher::fetch_remote - Fetching spec: {spec:?}");

        match spec {
            pcb_zen_core::LoadSpec::Package { package, tag, path } => {
                self.fetch_and_cache(path, |req| {
                    req.spec_type = "package".to_string();
                    req.package = Some(package.to_string());
                    req.version = Some(tag.to_string());
                    req.path = Some(path.to_string_lossy().to_string());
                })
            }

            pcb_zen_core::LoadSpec::Github {
                user,
                repo,
                rev,
                path,
            } => self.fetch_and_cache(path, |req| {
                req.spec_type = "github".to_string();
                req.owner = Some(user.to_string());
                req.repo = Some(repo.to_string());
                req.git_ref = Some(rev.to_string());
                req.path = Some(path.to_string_lossy().to_string());
            }),

            pcb_zen_core::LoadSpec::Gitlab {
                project_path,
                rev,
                path,
            } => self.fetch_and_cache(path, |req| {
                req.spec_type = "gitlab".to_string();
                req.owner = Some(project_path.to_string());
                req.git_ref = Some(rev.to_string());
                req.path = Some(path.to_string_lossy().to_string());
            }),

            pcb_zen_core::LoadSpec::Path { path, .. } => {
                // Regular path - just return it
                Ok(path.clone())
            }
        }
    }
}

impl WasmRemoteFetcher {
    fn fetch_and_cache<F>(
        &self,
        path: &Path,
        configure_request: F,
    ) -> Result<PathBuf, anyhow::Error>
    where
        F: FnOnce(&mut FetchRequest),
    {
        debug!(
            "WasmRemoteFetcher::fetch_and_cache - Fetching file: {}",
            path.display()
        );
        debug!(
            "WasmRemoteFetcher::fetch_and_cache - Existing files: {:?}",
            self.file_provider.lock().unwrap().files().keys()
        );

        // Check if the file already exists in our file provider
        if let Ok(provider) = self.file_provider.lock() {
            if provider.exists(path) {
                return Ok(path.to_path_buf());
            }
        }

        // Build the fetch request
        let mut req = FetchRequest::new();
        configure_request(&mut req);

        // Fetch the content
        let content = self.fetch_with_request(req)?;

        // Store in the file provider
        if let Ok(mut provider) = self.file_provider.lock() {
            provider.add_file(path.to_path_buf(), content);
        }

        Ok(path.to_path_buf())
    }

    fn fetch_with_request(&self, fetch_request: FetchRequest) -> Result<String, anyhow::Error> {
        // Convert to JsValue
        let js_request = serde_wasm_bindgen::to_value(&fetch_request)
            .map_err(|e| anyhow::anyhow!("Failed to serialize fetch request: {}", e))?;

        // Call JavaScript to fetch the remote file
        let result = js_fetch_remote_file(js_request);

        if let Some(content) = result.as_string() {
            if content.starts_with("ERROR:") {
                let error_msg = content.trim_start_matches("ERROR:");
                Err(anyhow::anyhow!("{}", error_msg))
            } else {
                Ok(content)
            }
        } else {
            Err(anyhow::anyhow!("Invalid response from JavaScript"))
        }
    }
}

/// Request structure sent to JavaScript for remote fetching
#[wasm_bindgen]
#[derive(Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    /// Type of the load spec (package, github, gitlab)
    #[wasm_bindgen(getter_with_clone)]
    pub spec_type: String,

    /// Package name (for package specs)
    #[wasm_bindgen(getter_with_clone)]
    pub package: Option<String>,

    /// Version (for package specs)
    #[wasm_bindgen(getter_with_clone)]
    pub version: Option<String>,

    /// Owner (for github/gitlab specs)
    #[wasm_bindgen(getter_with_clone)]
    pub owner: Option<String>,

    /// Repo (for github/gitlab specs)
    #[wasm_bindgen(getter_with_clone)]
    pub repo: Option<String>,

    /// Ref (for github/gitlab specs)
    #[wasm_bindgen(getter_with_clone)]
    pub git_ref: Option<String>,

    /// Path within the repo (for github/gitlab specs)
    #[wasm_bindgen(getter_with_clone)]
    pub path: Option<String>,

    /// Workspace root path (if available)
    #[wasm_bindgen(getter_with_clone)]
    pub workspace_root: Option<String>,
}

#[wasm_bindgen]
impl FetchRequest {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            spec_type: String::new(),
            package: None,
            version: None,
            owner: None,
            repo: None,
            git_ref: None,
            path: None,
            workspace_root: None,
        }
    }
}

/// Custom file provider that wraps InMemoryFileProvider and adds JavaScript fallback
struct WasmFileProvider {
    inner: Arc<Mutex<pcb_zen_core::InMemoryFileProvider>>,
}

impl WasmFileProvider {
    fn new(inner: Arc<Mutex<pcb_zen_core::InMemoryFileProvider>>) -> Self {
        Self { inner }
    }
}

impl pcb_zen_core::FileProvider for WasmFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, pcb_zen_core::FileProviderError> {
        let path_str = path.to_string_lossy();

        // Try the inner provider first
        if let Ok(provider) = self.inner.lock() {
            match provider.read_file(path) {
                Ok(content) => {
                    return Ok(content);
                }
                Err(_) => {
                    // File not in memory, continue to JavaScript fallback
                }
            }
        }

        // For files not in memory, call JavaScript to load them
        let result = js_load_file(&path_str);

        if let Some(content) = result.as_string() {
            if content.starts_with("ERROR:") {
                Err(pcb_zen_core::FileProviderError::NotFound(
                    path.to_path_buf(),
                ))
            } else {
                // Cache the loaded file for future use
                if let Ok(mut provider) = self.inner.lock() {
                    provider.add_file(path, content.clone());
                }

                Ok(content)
            }
        } else {
            Err(pcb_zen_core::FileProviderError::IoError(
                "Invalid response from JavaScript".to_string(),
            ))
        }
    }

    fn exists(&self, path: &Path) -> bool {
        // Check the inner provider first
        if let Ok(provider) = self.inner.lock() {
            if provider.exists(path) {
                return true;
            }
        }

        // Otherwise, try to read it via JavaScript
        self.read_file(path).is_ok()
    }

    fn is_directory(&self, path: &Path) -> bool {
        if let Ok(provider) = self.inner.lock() {
            provider.is_directory(path)
        } else {
            false
        }
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, pcb_zen_core::FileProviderError> {
        if let Ok(provider) = self.inner.lock() {
            provider.list_directory(path)
        } else {
            Ok(Vec::new())
        }
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, pcb_zen_core::FileProviderError> {
        if let Ok(provider) = self.inner.lock() {
            provider.canonicalize(path)
        } else {
            Ok(path.to_path_buf())
        }
    }
}

/// Lightweight V2 resolution from pcb.sum + vendor/
///
/// Returns package resolutions if pcb.sum exists (implying V2 mode).
/// Assumes vendor/ contains all dependencies.
fn resolve_from_lockfile_and_vendor(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
) -> Option<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    // Read pcb.sum - presence implies V2
    let lockfile_content = file_provider
        .read_file(&workspace_root.join("pcb.sum"))
        .ok()?;
    let lockfile = Lockfile::parse(&lockfile_content).ok()?;

    // Read workspace pcb.toml
    let pcb_toml_content = file_provider
        .read_file(&workspace_root.join("pcb.toml"))
        .ok()?;
    let pcb_toml = PcbToml::parse(&pcb_toml_content).ok()?;

    let vendor_dir = workspace_root.join("vendor");

    // Build url -> vendor path lookup from lockfile
    let url_to_path: HashMap<String, PathBuf> = lockfile
        .iter()
        .filter_map(|entry| {
            let vendor_path = vendor_dir.join(&entry.module_path).join(&entry.version);
            file_provider
                .exists(&vendor_path)
                .then(|| (entry.module_path.clone(), vendor_path))
        })
        .collect();

    // Helper to build resolution map for a package config
    let build_pkg_map = |config: &PcbToml| -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();

        // Dependencies
        for url in config.dependencies.keys() {
            if let Some(path) = url_to_path.get(url) {
                map.insert(url.clone(), path.clone());
            }
        }

        // Assets: vendor/<repo_url>/<ref>/
        for (asset_key, asset_spec) in &config.assets {
            if let Some(ref_str) = extract_asset_ref(asset_spec) {
                let asset_path = vendor_dir.join(asset_key).join(&ref_str);
                if file_provider.exists(&asset_path) {
                    map.insert(asset_key.clone(), asset_path);
                }
            }
        }

        map
    };

    let mut results: HashMap<PathBuf, BTreeMap<String, PathBuf>> = HashMap::new();

    // Workspace root
    results.insert(workspace_root.to_path_buf(), build_pkg_map(&pcb_toml));

    // Vendored packages
    for vendor_path in url_to_path.values() {
        if let Ok(content) = file_provider.read_file(&vendor_path.join("pcb.toml")) {
            if let Ok(pkg_config) = PcbToml::parse(&content) {
                results.insert(vendor_path.clone(), build_pkg_map(&pkg_config));
            }
        }
    }

    // Workspace members
    if let Some(workspace) = &pcb_toml.workspace {
        for member_pattern in &workspace.members {
            let member_dir = workspace_root.join(member_pattern.trim_end_matches("/*"));
            if let Ok(content) = file_provider.read_file(&member_dir.join("pcb.toml")) {
                if let Ok(member_config) = PcbToml::parse(&content) {
                    results.insert(member_dir, build_pkg_map(&member_config));
                }
            }
        }
    }

    Some(results)
}

/// Extract ref string from AssetDependencySpec (version/branch/rev, excluding HEAD)
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

/// Convert a Diagnostic to DiagnosticInfo
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

/// A module that can be introspected or evaluated
#[wasm_bindgen]
pub struct Module {
    id: String,
    main_file: String,
    #[allow(dead_code)]
    module_name: String,
    load_resolver: Arc<pcb_zen_core::CoreLoadResolver>,
}

#[wasm_bindgen]
impl Module {
    /// Create a module from a single file path
    #[wasm_bindgen(js_name = fromPath)]
    pub fn from_path(file_path: &str, options: JsValue) -> Result<Module, JsValue> {
        // Extract module name from the file path
        let path = PathBuf::from(file_path);
        let module_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string();

        // Generate unique ID
        let id = format!("module_{}", uuid::Uuid::new_v4());

        // Create shared inner provider
        let inner_provider = Arc::new(Mutex::new(pcb_zen_core::InMemoryFileProvider::new(
            HashMap::new(),
        )));

        // Create file provider and remote fetcher that share the same inner provider
        let file_provider = Arc::new(WasmFileProvider::new(inner_provider.clone()));
        // Parse options for resolver configuration
        #[derive(Deserialize)]
        struct ModuleOptions {
            #[serde(rename = "useVendorDir")]
            use_vendor_dir: Option<bool>,
            offline: Option<bool>,
        }

        let (use_vendor_dir, offline) = if !options.is_undefined() && !options.is_null() {
            match serde_wasm_bindgen::from_value::<ModuleOptions>(options) {
                Ok(opts) => (
                    opts.use_vendor_dir.unwrap_or(true),
                    opts.offline.unwrap_or(false),
                ),
                Err(e) => {
                    return Err(JsValue::from_str(&format!(
                        "Failed to parse module options: {e}"
                    )));
                }
            }
        } else {
            (true, false)
        };

        let remote_fetcher: Arc<dyn pcb_zen_core::RemoteFetcher> = if offline {
            Arc::new(pcb_zen_core::NoopRemoteFetcher)
        } else {
            Arc::new(WasmRemoteFetcher::new(inner_provider))
        };

        // Determine workspace root using pcb.toml discovery
        let workspace_root = find_workspace_root(file_provider.as_ref(), &path);

        // Try lightweight V2 resolution from lockfile + vendor
        let v2_resolutions =
            resolve_from_lockfile_and_vendor(file_provider.as_ref(), &workspace_root);

        // Create load resolver
        let load_resolver = Arc::new(pcb_zen_core::CoreLoadResolver::new(
            file_provider.clone(),
            remote_fetcher.clone(),
            workspace_root,
            use_vendor_dir,
            v2_resolutions,
        ));

        Ok(Module {
            id,
            main_file: file_path.to_string(),
            module_name,
            load_resolver,
        })
    }

    /// Create a module from individual files
    #[wasm_bindgen(js_name = fromFiles)]
    pub fn from_files(
        files_json: &str,
        main_file: &str,
        module_name: &str,
        options: JsValue,
    ) -> Result<Module, JsValue> {
        let files: std::collections::HashMap<String, String> = serde_json::from_str(files_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse files JSON: {e}")))?;

        // Generate unique ID
        let id = format!("module_{}", uuid::Uuid::new_v4());

        // Create shared inner provider with the provided files
        let inner_provider = Arc::new(Mutex::new(pcb_zen_core::InMemoryFileProvider::new(
            files.clone(),
        )));

        // Create file provider and remote fetcher that share the same inner provider
        let file_provider = Arc::new(WasmFileProvider::new(inner_provider.clone()));
        // Parse options for resolver configuration
        #[derive(Deserialize)]
        struct ModuleOptions {
            #[serde(rename = "useVendorDir")]
            use_vendor_dir: Option<bool>,
            offline: Option<bool>,
        }

        let (use_vendor_dir, offline) = if !options.is_undefined() && !options.is_null() {
            match serde_wasm_bindgen::from_value::<ModuleOptions>(options) {
                Ok(opts) => (
                    opts.use_vendor_dir.unwrap_or(true),
                    opts.offline.unwrap_or(false),
                ),
                Err(e) => {
                    return Err(JsValue::from_str(&format!(
                        "Failed to parse module options: {e}"
                    )));
                }
            }
        } else {
            (true, false)
        };

        let remote_fetcher: Arc<dyn pcb_zen_core::RemoteFetcher> = if offline {
            Arc::new(pcb_zen_core::NoopRemoteFetcher)
        } else {
            Arc::new(WasmRemoteFetcher::new(inner_provider))
        };

        // Determine workspace root using pcb.toml discovery
        let main_path = PathBuf::from(main_file);
        let workspace_root = find_workspace_root(file_provider.as_ref(), &main_path);

        // Try lightweight V2 resolution from lockfile + vendor
        let v2_resolutions =
            resolve_from_lockfile_and_vendor(file_provider.as_ref(), &workspace_root);

        // Create load resolver
        let load_resolver = Arc::new(pcb_zen_core::CoreLoadResolver::new(
            file_provider.clone(),
            remote_fetcher.clone(),
            workspace_root,
            use_vendor_dir,
            v2_resolutions,
        ));

        Ok(Module {
            id,
            main_file: main_file.to_string(),
            module_name: module_name.to_string(),
            load_resolver,
        })
    }

    /// Evaluate the module with the given inputs
    #[wasm_bindgen]
    pub fn evaluate(&self, inputs_json: &str) -> Result<JsValue, JsValue> {
        // Parse inputs
        let inputs: HashMap<String, serde_json::Value> = serde_json::from_str(inputs_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse inputs JSON: {e}")))?;

        // Create evaluation context using the stored providers
        let main_path = PathBuf::from(&self.main_file);
        let mut ctx = EvalContext::new(self.load_resolver.clone()).set_source_path(main_path);

        // Convert JSON inputs directly to heap values (no serialization!)
        if !inputs.is_empty() {
            let json_map = starlark::collections::SmallMap::from_iter(inputs);
            ctx.set_json_inputs(json_map);
        }

        // Evaluate the module
        let result = ctx.eval();

        // Extract schematic from the result
        let schematic_opt = result
            .output
            .as_ref()
            .and_then(|output| output.to_schematic().ok());

        let parameters = result
            .output
            .as_ref()
            .map(|output| output.signature.clone());

        // Generate BOM JSON from the schematic if available
        let bom_json = schematic_opt
            .as_ref()
            .map(|schematic| schematic.bom().ungrouped_json());

        // Build evaluation result
        let evaluation_result = EvaluationResult {
            success: result.output.is_some(),
            parameters,
            schematic: schematic_opt.and_then(|s| match serde_json::to_string(&s) {
                Ok(json) => Some(json),
                Err(e) => {
                    log::error!("Failed to serialize schematic to JSON: {e}");
                    None
                }
            }),
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

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Free the module from memory
    #[wasm_bindgen]
    pub fn free_module(&self) {
        // TODO: Send a cleanup message to the worker
        debug!("Freeing module {}", self.id);
    }

    /// Read a file from the module's file system
    #[wasm_bindgen(js_name = readFile)]
    pub fn read_file(&self, _path: &str) -> Result<String, JsValue> {
        // TODO: Implement file reading through worker
        Err(JsValue::from_str("File reading not yet implemented"))
    }

    /// Write a file to the module's file system
    #[wasm_bindgen(js_name = writeFile)]
    pub fn write_file(&self, _path: &str, _content: &str) -> Result<(), JsValue> {
        // TODO: Implement file writing through worker
        Err(JsValue::from_str("File writing not yet implemented"))
    }

    /// Delete a file from the module's file system
    #[wasm_bindgen(js_name = deleteFile)]
    pub fn delete_file(&self, _path: &str) -> Result<(), JsValue> {
        // TODO: Implement file deletion through worker
        Err(JsValue::from_str("File deletion not yet implemented"))
    }

    /// List all files in the module's file system
    #[wasm_bindgen(js_name = listFiles)]
    pub fn list_files(&self) -> Result<String, JsValue> {
        // TODO: Implement file listing through worker
        Ok("[]".to_string())
    }

    /// Save positions - currently unsupported in WASM
    #[wasm_bindgen(js_name = savePositions)]
    pub fn save_positions(
        &self,
        _file_path: &str,
        _positions_json: JsValue,
    ) -> Result<(), JsValue> {
        Err(JsValue::from_str(
            "Position saving is not supported in WASM environment",
        ))
    }
}

// Data structures for serialization

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
