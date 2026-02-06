use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::resolution::{build_resolution_map, VendoredPathResolver};
use pcb_zen_core::workspace::get_workspace_info;
use pcb_zen_core::{EvalContext, FileProvider, FileProviderError, Lockfile};
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

    /// Auto-detect the main .zen file in the zip.
    ///
    /// Looks in `boards/` for a single subdirectory containing a single .zen file.
    /// Returns the path like "boards/LG0002/LG0002.zen" if found.
    fn detect_main_file(&self) -> Option<String> {
        // Find all entries under boards/
        let board_dirs: HashSet<_> = self
            .file_index
            .iter()
            .filter_map(|path| {
                let path = path.strip_prefix("boards/")?;
                let dir = path.split('/').next()?;
                if !dir.is_empty() {
                    Some(dir.to_string())
                } else {
                    None
                }
            })
            .collect();

        // Must have exactly one board directory
        if board_dirs.len() != 1 {
            return None;
        }

        let board_dir = board_dirs.into_iter().next()?;
        let board_path = format!("boards/{}", board_dir);

        // Find .zen files directly in this board directory (not in subdirs)
        let zen_files: Vec<_> = self
            .file_index
            .iter()
            .filter(|path| {
                if let Some(rest) = path.strip_prefix(&format!("{}/", board_path)) {
                    // Must be a .zen file directly in the board dir (no more slashes)
                    !rest.contains('/') && rest.ends_with(".zen")
                } else {
                    false
                }
            })
            .collect();

        // Must have exactly one .zen file
        if zen_files.len() != 1 {
            return None;
        }

        Some(zen_files[0].clone())
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

    fn is_symlink(&self, _path: &Path) -> bool {
        false
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
/// Uses shared resolution logic from pcb-zen-core.
fn resolve_v2_packages<F: FileProvider + Clone>(
    file_provider: F,
    workspace_root: &Path,
) -> Option<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    // Parse lockfile
    let lockfile_content = file_provider
        .read_file(&workspace_root.join("pcb.sum"))
        .ok()?;
    let lockfile = Lockfile::parse(&lockfile_content).ok()?;

    let vendor_dir = workspace_root.join("vendor");

    // Create the vendored path resolver
    let resolver =
        VendoredPathResolver::from_lockfile(file_provider.clone(), vendor_dir, &lockfile);

    // Discover workspace using shared logic
    let workspace = get_workspace_info(&file_provider, workspace_root).ok()?;

    // Build resolution map for workspace members and all vendored packages
    let results = build_resolution_map(&file_provider, &resolver, &workspace, resolver.closure());

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

/// Evaluate a Zener module from a zip archive (pure Rust implementation).
///
/// Works with both V1 (no pcb.sum) and V2 (with pcb.sum) release zips.
/// All dependencies must be vendored in the zip.
///
/// If `main_file` is empty, attempts to auto-detect by looking for a single
/// board directory with a single .zen file (e.g., "boards/LG0002/LG0002.zen").
///
/// This is the core implementation that can be used from both WASM and native contexts.
pub fn evaluate_impl(
    zip_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<EvaluationResult, String> {
    let file_provider =
        Arc::new(ZipFileProvider::new(zip_bytes).map_err(|e| format!("Failed to parse zip: {e}"))?);

    // Auto-detect main file if not provided
    let main_file = if main_file.is_empty() {
        file_provider.detect_main_file().ok_or_else(|| {
            "Could not auto-detect main file. Expected exactly one board directory \
             in boards/ with exactly one .zen file. Please specify the main file explicitly."
                .to_string()
        })?
    } else {
        main_file.to_string()
    };

    let main_path = PathBuf::from(&main_file);
    let workspace_root = find_workspace_root(file_provider.as_ref(), &main_path)
        .map_err(|e| format!("Failed to find workspace root: {e}"))?;
    let v2_resolutions = resolve_v2_packages(file_provider.clone(), &workspace_root)
        .expect("v2 dependency resolution");

    let load_resolver = Arc::new(pcb_zen_core::CoreLoadResolver::new(
        file_provider.clone(),
        v2_resolutions,
    ));

    let inputs: HashMap<String, serde_json::Value> =
        serde_json::from_str(inputs_json).map_err(|e| format!("Failed to parse inputs: {e}"))?;

    let mut ctx = EvalContext::new(load_resolver).set_source_path(main_path);
    if !inputs.is_empty() {
        ctx.set_json_inputs(starlark::collections::SmallMap::from_iter(inputs));
    }

    let result = ctx.eval();
    let schematic_opt = result.output.as_ref().and_then(|o| o.to_schematic().ok());

    Ok(EvaluationResult {
        success: result.output.is_some(),
        parameters: result.output.as_ref().map(|o| o.signature.clone()),
        schematic: schematic_opt
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok()),
        bom: schematic_opt
            .as_ref()
            .and_then(|s| serde_json::from_str(&s.bom().ungrouped_json()).ok()),
        diagnostics: result
            .diagnostics
            .into_iter()
            .map(|d| diagnostic_to_json(&d))
            .collect(),
    })
}

/// Evaluate a Zener module from a zip archive (WASM binding).
///
/// This is a thin wrapper around `evaluate_impl` for wasm-bindgen.
#[wasm_bindgen]
pub fn evaluate(
    zip_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<JsValue, JsValue> {
    let result =
        evaluate_impl(zip_bytes, main_file, inputs_json).map_err(|e| JsValue::from_str(&e))?;

    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    result
        .serialize(&serializer)
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
    pub schematic: Option<serde_json::Value>,
    pub bom: Option<serde_json::Value>,
    pub diagnostics: Vec<DiagnosticInfo>,
}
