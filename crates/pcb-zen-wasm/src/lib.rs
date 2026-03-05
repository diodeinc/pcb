use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::resolution::{VendoredPathResolver, build_resolution_map};
use pcb_zen_core::workspace::get_workspace_info;
use pcb_zen_core::{EvalContext, FileProvider, FileProviderError, Lockfile};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

fn wasm_stdlib_root() -> PathBuf {
    pcb_zen_core::workspace_stdlib_root(Path::new("."))
}

/// File provider backed by an in-memory zip archive
struct ZipFileProvider {
    archive: Mutex<ZipArchive<Cursor<Vec<u8>>>>,
    cache: Mutex<HashMap<String, String>>,
    file_index: HashSet<String>,
    stdlib_root: String,
}

impl ZipFileProvider {
    fn new(zip_bytes: Vec<u8>) -> Result<Self, zip::result::ZipError> {
        let cursor = Cursor::new(zip_bytes);
        let mut archive = ZipArchive::new(cursor)?;
        let stdlib_root = Self::normalize(&wasm_stdlib_root());

        let file_index = (0..archive.len())
            .filter_map(|i| Some(archive.by_index(i).ok()?.name().to_string()))
            .collect();
        Ok(Self {
            archive: Mutex::new(archive),
            cache: Mutex::new(HashMap::new()),
            file_index,
            stdlib_root,
        })
    }

    fn normalize(path: &Path) -> String {
        path.to_string_lossy().trim_start_matches('/').to_string()
    }

    fn has_prefix(&self, prefix: &str) -> bool {
        self.file_index.iter().any(|f| f.starts_with(prefix))
    }

    fn stdlib_rel_path<'a>(&'a self, normalized: &'a str) -> Option<&'a str> {
        normalized
            .strip_prefix(&self.stdlib_root)
            .and_then(|s| s.strip_prefix('/'))
    }

    fn stdlib_path_exists(&self, normalized: &str) -> bool {
        if let Some(rel) = self.stdlib_rel_path(normalized) {
            let stdlib = pcb_zen_core::embedded_stdlib::embedded_stdlib_dir();
            stdlib.get_file(rel).is_some() || stdlib.get_dir(rel).is_some()
        } else {
            normalized == self.stdlib_root
        }
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

        if let Some(rel) = self.stdlib_rel_path(&normalized)
            && let Some(file) = pcb_zen_core::embedded_stdlib::embedded_stdlib_dir().get_file(rel)
        {
            let contents = std::str::from_utf8(file.contents())
                .map_err(|e| FileProviderError::IoError(e.to_string()))?
                .to_string();
            self.cache
                .lock()
                .unwrap()
                .insert(normalized, contents.clone());
            return Ok(contents);
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
            || self.stdlib_path_exists(&normalized)
    }

    fn is_directory(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        self.has_prefix(&format!("{}/", normalized.trim_end_matches('/')))
            || normalized == self.stdlib_root
            || self.stdlib_rel_path(&normalized).is_some_and(|rel| {
                pcb_zen_core::embedded_stdlib::embedded_stdlib_dir()
                    .get_dir(rel)
                    .is_some()
            })
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

        let mut entries: HashSet<String> = self
            .file_index
            .iter()
            .filter_map(|name| name.strip_prefix(&prefix))
            .filter_map(|rest| rest.split('/').next())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();

        let stdlib_dir = if normalized == self.stdlib_root {
            Some(pcb_zen_core::embedded_stdlib::embedded_stdlib_dir())
        } else {
            self.stdlib_rel_path(&normalized)
                .and_then(|rel| pcb_zen_core::embedded_stdlib::embedded_stdlib_dir().get_dir(rel))
        };

        if let Some(dir) = stdlib_dir {
            entries.extend(
                dir.files()
                    .filter_map(|f| f.path().file_name())
                    .map(|n| n.to_string_lossy().to_string()),
            );
            entries.extend(
                dir.dirs()
                    .filter_map(|d| d.path().file_name())
                    .map(|n| n.to_string_lossy().to_string()),
            );
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn empty_zip_bytes() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            writer
                .start_file("boards/demo/demo.zen", SimpleFileOptions::default())
                .expect("start zip file");
            writer.write_all(b"print('demo')").expect("write zip file");
            writer.finish().expect("finish zip");
        }
        cursor.into_inner()
    }

    #[test]
    fn list_stdlib_root_includes_embedded_top_level_entries() {
        let provider = ZipFileProvider::new(empty_zip_bytes()).expect("create provider");
        let root = wasm_stdlib_root();
        let entries = provider
            .list_directory(&root)
            .expect("list stdlib root directory");

        assert!(
            entries.iter().any(|p| p == &root.join("interfaces.zen")),
            "expected interfaces.zen in stdlib root listing",
        );
        assert!(
            entries.iter().any(|p| p == &root.join("units.zen")),
            "expected units.zen in stdlib root listing",
        );
        assert!(
            entries.iter().any(|p| p == &root.join("generics")),
            "expected generics dir in stdlib root listing",
        );
    }
}

/// Build package resolution map from lockfile and vendored dependencies.
///
/// Assumes all deps are vendored in `vendor/`. No patches, no cache fallback.
/// Uses shared resolution logic from pcb-zen-core.
fn resolve_packages<F: FileProvider + Clone>(
    file_provider: F,
    workspace_root: &Path,
) -> Result<pcb_zen_core::resolution::ResolutionResult, String> {
    let lockfile_path = workspace_root.join("pcb.sum");

    let lockfile_content = file_provider
        .read_file(&lockfile_path)
        .map_err(|e| format!("Failed to read {}: {}", lockfile_path.display(), e))?;
    let lockfile = Lockfile::parse(&lockfile_content)
        .map_err(|e| format!("Failed to parse {}: {}", lockfile_path.display(), e))?;

    let vendor_dir = workspace_root.join("vendor");
    let workspace = get_workspace_info(&file_provider, workspace_root)
        .map_err(|e| format!("Failed to discover workspace metadata: {e}"))?;
    let resolver =
        VendoredPathResolver::from_lockfile(file_provider.clone(), vendor_dir, &lockfile);

    let package_resolutions =
        build_resolution_map(&file_provider, &resolver, &workspace, resolver.closure());
    Ok(pcb_zen_core::resolution::ResolutionResult {
        workspace_info: workspace,
        package_resolutions,
        closure: HashMap::new(),
        lockfile_changed: false,
    })
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
/// Expects release zips with `pcb.sum`.
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
    let resolution = resolve_packages(file_provider.clone(), &workspace_root)
        .map_err(|e| format!("Failed to resolve dependencies: {e}"))?;

    let inputs: HashMap<String, serde_json::Value> =
        serde_json::from_str(inputs_json).map_err(|e| format!("Failed to parse inputs: {e}"))?;

    let mut ctx = EvalContext::new(file_provider.clone(), resolution).set_source_path(main_path);
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
