use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::resolution::{VendoredPathResolver, build_resolution_map};
use pcb_zen_core::workspace::get_workspace_info;
use pcb_zen_core::{EvalContext, FileProvider, FileProviderError, Lockfile};
use ruzstd::decoding::StreamingDecoder;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use tar::Archive;
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

/// File provider backed by an in-memory source bundle.
///
/// Supports plain source zips, release zips, and canonical `.tar.zst` bundles.
struct BundleFileProvider {
    files: HashMap<String, Vec<u8>>,
    cache: Mutex<HashMap<String, String>>,
    file_index: HashSet<String>,
    hinted_main_file: Option<String>,
    stdlib_root: String,
}

enum StdlibPath<'a> {
    NotStdlib,
    Root,
    Included(&'a str),
    Excluded,
}

impl BundleFileProvider {
    fn new(bundle_bytes: Vec<u8>) -> Result<Self, String> {
        let parsed = parse_bundle(bundle_bytes)?;
        let stdlib_root = Self::normalize(&wasm_stdlib_root());
        let file_index = parsed.files.keys().cloned().collect();
        Ok(Self {
            files: parsed.files,
            cache: Mutex::new(HashMap::new()),
            file_index,
            hinted_main_file: parsed.hinted_main_file,
            stdlib_root,
        })
    }

    fn normalize(path: &Path) -> String {
        let mut normalized = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::Normal(name) => normalized.push(name.to_string_lossy().into_owned()),
                Component::RootDir | Component::Prefix(_) => {}
            }
        }
        normalized.join("/")
    }

    fn classify_stdlib_path<'a>(&'a self, normalized: &'a str) -> StdlibPath<'a> {
        if normalized == self.stdlib_root {
            return StdlibPath::Root;
        }

        match normalized
            .strip_prefix(&self.stdlib_root)
            .and_then(|s| s.strip_prefix('/'))
        {
            Some(rel) if pcb_zen_core::embedded_stdlib::include_stdlib_path(Path::new(rel)) => {
                StdlibPath::Included(rel)
            }
            Some(_) => StdlibPath::Excluded,
            None => StdlibPath::NotStdlib,
        }
    }

    /// Auto-detect the main .zen file in the bundle.
    ///
    /// Looks in `boards/` for a single subdirectory containing a single .zen file.
    /// Returns the path like "boards/LG0002/LG0002.zen" if found.
    fn detect_main_file(&self) -> Option<String> {
        if let Some(main_file) = self.hinted_main_file.as_ref()
            && self.file_index.contains(main_file)
        {
            return Some(main_file.clone());
        }

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

        if board_dirs.len() != 1 {
            return None;
        }

        let board_dir = board_dirs.into_iter().next()?;
        let board_path = format!("boards/{}", board_dir);

        let zen_files: Vec<_> = self
            .file_index
            .iter()
            .filter(|path| {
                if let Some(rest) = path.strip_prefix(&format!("{}/", board_path)) {
                    !rest.contains('/') && rest.ends_with(".zen")
                } else {
                    false
                }
            })
            .collect();

        if zen_files.len() != 1 {
            return None;
        }

        Some(zen_files[0].clone())
    }
}

impl FileProvider for BundleFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        let normalized = Self::normalize(path);

        if let Some(cached) = self.cache.lock().unwrap().get(&normalized).cloned() {
            return Ok(cached);
        }

        match self.classify_stdlib_path(&normalized) {
            StdlibPath::Included(rel) => {
                if let Some(file) =
                    pcb_zen_core::embedded_stdlib::embedded_stdlib_dir().get_file(rel)
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
                return Err(FileProviderError::NotFound(path.to_path_buf()));
            }
            StdlibPath::Root | StdlibPath::Excluded => {
                return Err(FileProviderError::NotFound(path.to_path_buf()));
            }
            StdlibPath::NotStdlib => {}
        }

        let contents = self
            .files
            .get(&normalized)
            .ok_or_else(|| FileProviderError::NotFound(path.to_path_buf()))
            .and_then(|bytes| {
                String::from_utf8(bytes.clone())
                    .map_err(|e| FileProviderError::IoError(e.to_string()))
            })?;

        self.cache
            .lock()
            .unwrap()
            .insert(normalized, contents.clone());
        Ok(contents)
    }

    fn exists(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        match self.classify_stdlib_path(&normalized) {
            StdlibPath::NotStdlib => {
                self.file_index.contains(&normalized)
                    || self
                        .file_index
                        .iter()
                        .any(|f| f.starts_with(&format!("{}/", normalized.trim_end_matches('/'))))
            }
            StdlibPath::Root => true,
            StdlibPath::Excluded => false,
            StdlibPath::Included(rel) => {
                let stdlib = pcb_zen_core::embedded_stdlib::embedded_stdlib_dir();
                stdlib.get_file(rel).is_some() || stdlib.get_dir(rel).is_some()
            }
        }
    }

    fn is_directory(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        match self.classify_stdlib_path(&normalized) {
            StdlibPath::NotStdlib => self
                .file_index
                .iter()
                .any(|f| f.starts_with(&format!("{}/", normalized.trim_end_matches('/')))),
            StdlibPath::Root => true,
            StdlibPath::Excluded => false,
            StdlibPath::Included(rel) => pcb_zen_core::embedded_stdlib::embedded_stdlib_dir()
                .get_dir(rel)
                .is_some(),
        }
    }

    fn is_symlink(&self, _path: &Path) -> bool {
        false
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        let normalized = Self::normalize(path).trim_end_matches('/').to_string();

        let stdlib_dir = match self.classify_stdlib_path(&normalized) {
            StdlibPath::Root => Some(pcb_zen_core::embedded_stdlib::embedded_stdlib_dir()),
            StdlibPath::Included(rel) => Some(
                pcb_zen_core::embedded_stdlib::embedded_stdlib_dir()
                    .get_dir(rel)
                    .ok_or_else(|| FileProviderError::NotFound(path.to_path_buf()))?,
            ),
            StdlibPath::Excluded => return Err(FileProviderError::NotFound(path.to_path_buf())),
            StdlibPath::NotStdlib => None,
        };
        if let Some(dir) = stdlib_dir {
            let mut entries = HashSet::new();
            entries.extend(
                dir.files()
                    .filter(|f| pcb_zen_core::embedded_stdlib::include_stdlib_path(f.path()))
                    .filter_map(|f| f.path().file_name())
                    .map(|n| n.to_string_lossy().to_string()),
            );
            entries.extend(
                dir.dirs()
                    .filter(|d| pcb_zen_core::embedded_stdlib::include_stdlib_path(d.path()))
                    .filter_map(|d| d.path().file_name())
                    .map(|n| n.to_string_lossy().to_string()),
            );
            return Ok(entries.into_iter().map(|name| path.join(name)).collect());
        }

        let prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{normalized}/")
        };

        let entries: HashSet<String> = self
            .file_index
            .iter()
            .filter_map(|name| name.strip_prefix(&prefix))
            .filter_map(|rest| rest.split('/').next())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
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

struct ParsedBundle {
    files: HashMap<String, Vec<u8>>,
    hinted_main_file: Option<String>,
}

fn parse_bundle(bundle_bytes: Vec<u8>) -> Result<ParsedBundle, String> {
    match parse_zip_bundle(&bundle_bytes) {
        Ok(files) => Ok(files),
        Err(zip_err) => parse_tar_zst_bundle(&bundle_bytes).map_err(|tar_err| {
            format!("Failed to parse bundle as zip ({zip_err}) or .tar.zst ({tar_err})")
        }),
    }
}

fn parse_zip_bundle(bundle_bytes: &[u8]) -> Result<ParsedBundle, zip::result::ZipError> {
    let mut archive = ZipArchive::new(Cursor::new(bundle_bytes))?;
    let is_release_bundle = archive.by_name("metadata.json").is_ok();
    let hinted_main_file = if is_release_bundle {
        archive.by_name("metadata.json").ok().and_then(|mut file| {
            let mut metadata = String::new();
            file.read_to_string(&mut metadata).ok()?;
            extract_main_file_from_metadata(&metadata)
        })
    } else {
        None
    };
    let mut files = HashMap::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.is_dir() {
            continue;
        }

        let path = if is_release_bundle {
            let Some(stripped) = file.name().strip_prefix("src/") else {
                continue;
            };
            stripped.to_string()
        } else {
            file.name().to_string()
        };
        if path.is_empty() {
            continue;
        }

        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        files.insert(path, contents);
    }

    Ok(ParsedBundle {
        files,
        hinted_main_file,
    })
}

fn parse_tar_zst_bundle(bundle_bytes: &[u8]) -> Result<ParsedBundle, String> {
    let decoder = StreamingDecoder::new(Cursor::new(bundle_bytes))
        .map_err(|e| format!("zstd decode error: {e}"))?;
    let mut archive = Archive::new(decoder);
    let mut files = HashMap::new();
    let mut hinted_main_file = None;

    for entry_result in archive
        .entries()
        .map_err(|e| format!("tar read error: {e}"))?
    {
        let mut entry = entry_result.map_err(|e| format!("tar entry error: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let path = entry
            .path()
            .map_err(|e| format!("invalid tar path: {e}"))?
            .to_string_lossy()
            .into_owned();
        if path == "metadata.json" {
            let mut metadata = String::new();
            entry
                .read_to_string(&mut metadata)
                .map_err(|e| format!("metadata read error: {e}"))?;
            hinted_main_file = extract_main_file_from_metadata(&metadata);
            continue;
        }
        let Some(stripped) = path.strip_prefix("src/") else {
            continue;
        };
        if stripped.is_empty() {
            continue;
        }

        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|e| format!("tar entry read error: {e}"))?;
        files.insert(stripped.to_string(), contents);
    }

    Ok(ParsedBundle {
        files,
        hinted_main_file,
    })
}

fn extract_main_file_from_metadata(metadata: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct BundleMetadata {
        release: Option<BundleReleaseMetadata>,
    }

    #[derive(Deserialize)]
    struct BundleReleaseMetadata {
        zen_file: Option<String>,
    }

    serde_json::from_str::<BundleMetadata>(metadata)
        .ok()
        .and_then(|m| m.release)
        .and_then(|r| r.zen_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tar::Builder;
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

    fn tar_zst_bundle_bytes() -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = Builder::new(&mut tar_bytes);

            let metadata = br#"{"kind":"bundle"}"#;
            let mut metadata_header = tar::Header::new_gnu();
            metadata_header.set_size(metadata.len() as u64);
            metadata_header.set_mode(0o644);
            metadata_header.set_cksum();
            builder
                .append_data(&mut metadata_header, "metadata.json", &metadata[..])
                .expect("append metadata");

            let source = b"print('demo')";
            let mut source_header = tar::Header::new_gnu();
            source_header.set_size(source.len() as u64);
            source_header.set_mode(0o644);
            source_header.set_cksum();
            builder
                .append_data(&mut source_header, "src/boards/demo/demo.zen", &source[..])
                .expect("append source");

            builder.finish().expect("finish tar");
        }

        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(&tar_bytes).expect("encode tar");
        encoder.finish().expect("finish zstd")
    }

    #[test]
    fn list_stdlib_root_includes_embedded_top_level_entries() {
        let provider = BundleFileProvider::new(empty_zip_bytes()).expect("create provider");
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

    #[test]
    fn stdlib_excluded_paths_are_hidden() {
        let provider = BundleFileProvider::new(empty_zip_bytes()).expect("create provider");
        let root = wasm_stdlib_root();
        let entries = provider
            .list_directory(&root)
            .expect("list stdlib root directory");

        assert!(
            !entries.iter().any(|p| p == &root.join("test")),
            "expected test dir to be excluded from stdlib listing",
        );
        assert!(!provider.exists(&root.join("test")));
        assert!(!provider.is_directory(&root.join("test")));

        let err = provider
            .read_file(&root.join("test/test_checks.zen"))
            .expect_err("excluded stdlib file should not be readable");
        assert!(matches!(err, FileProviderError::NotFound(_)));
    }

    #[test]
    fn tar_zst_bundle_is_normalized_to_src_contents() {
        let provider = BundleFileProvider::new(tar_zst_bundle_bytes()).expect("create provider");
        assert!(provider.exists(Path::new("boards/demo/demo.zen")));
        assert_eq!(
            provider
                .read_file(Path::new("boards/demo/demo.zen"))
                .expect("read source"),
            "print('demo')"
        );
        assert_eq!(
            provider.detect_main_file().as_deref(),
            Some("boards/demo/demo.zen")
        );
    }

    #[test]
    fn metadata_hint_is_used_for_non_board_package_layout() {
        let tar_bytes = {
            let mut tar_bytes = Vec::new();
            {
                let mut builder = Builder::new(&mut tar_bytes);

                let metadata = br#"{"release":{"zen_file":"reference/demo/demo.zen"}}"#;
                let mut metadata_header = tar::Header::new_gnu();
                metadata_header.set_size(metadata.len() as u64);
                metadata_header.set_mode(0o644);
                metadata_header.set_cksum();
                builder
                    .append_data(&mut metadata_header, "metadata.json", &metadata[..])
                    .expect("append metadata");

                let workspace = b"[workspace]\nmembers = [\"reference/*\"]\n";
                let mut workspace_header = tar::Header::new_gnu();
                workspace_header.set_size(workspace.len() as u64);
                workspace_header.set_mode(0o644);
                workspace_header.set_cksum();
                builder
                    .append_data(&mut workspace_header, "src/pcb.toml", &workspace[..])
                    .expect("append workspace");

                let source = b"print('demo')";
                let mut source_header = tar::Header::new_gnu();
                source_header.set_size(source.len() as u64);
                source_header.set_mode(0o644);
                source_header.set_cksum();
                builder
                    .append_data(
                        &mut source_header,
                        "src/reference/demo/demo.zen",
                        &source[..],
                    )
                    .expect("append source");

                builder.finish().expect("finish tar");
            }

            let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
            encoder.write_all(&tar_bytes).expect("encode tar");
            encoder.finish().expect("finish zstd")
        };

        let provider = BundleFileProvider::new(tar_bytes).expect("create provider");
        assert_eq!(
            provider.detect_main_file().as_deref(),
            Some("reference/demo/demo.zen")
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
        symbol_parts: HashMap::new(),
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

/// Evaluate a Zener module from a source bundle (pure Rust implementation).
///
/// Supports source zips, release zips, and canonical `.tar.zst` bundles.
/// All dependencies must already be vendored in the bundle.
///
/// If `main_file` is empty, attempts to auto-detect by looking for a single
/// board directory with a single .zen file (e.g., "boards/LG0002/LG0002.zen").
pub fn evaluate_impl(
    bundle_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<EvaluationResult, String> {
    let file_provider = Arc::new(BundleFileProvider::new(bundle_bytes)?);

    let main_file = if main_file.is_empty() {
        file_provider.detect_main_file().ok_or_else(|| {
            "Could not auto-detect main file. Expected exactly one board directory \
             in boards/ with exactly one .zen file. Please specify the main file explicitly."
                .to_string()
        })?
    } else {
        main_file.to_string()
    };

    let requested_main_path = PathBuf::from(&main_file);
    let main_path = if requested_main_path.is_absolute() {
        requested_main_path
    } else {
        Path::new("/").join(requested_main_path)
    };
    let main_path = file_provider
        .canonicalize(&main_path)
        .map_err(|e| format!("Failed to canonicalize main file path: {e}"))?;
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

/// Evaluate a Zener module from an in-memory source bundle (WASM binding).
#[wasm_bindgen]
pub fn evaluate(
    bundle_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<JsValue, JsValue> {
    let result =
        evaluate_impl(bundle_bytes, main_file, inputs_json).map_err(|e| JsValue::from_str(&e))?;

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
