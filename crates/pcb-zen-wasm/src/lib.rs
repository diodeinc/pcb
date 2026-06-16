use pcb_zen_core::config::PcbToml;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::resolution::{
    FrozenDepId, FrozenPackage, FrozenPackageIdentity, FrozenResolutionMap, FrozenResolutionSet,
    ModuleLine, VendoredPathResolver, build_resolution_map, selected_remote_from_hydrated_manifest,
};
use pcb_zen_core::workspace::WorkspaceInfo;
use pcb_zen_core::workspace::get_workspace_info;
use pcb_zen_core::{EvalContext, FileProvider, FileProviderError};
use ruzstd::decoding::StreamingDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
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

struct MemoryFiles {
    files: HashMap<String, Vec<u8>>,
}

impl MemoryFiles {
    fn new(files: HashMap<String, Vec<u8>>) -> Self {
        Self { files }
    }

    fn read_utf8(
        &self,
        normalized: &str,
        original_path: &Path,
    ) -> Result<String, FileProviderError> {
        self.files
            .get(normalized)
            .ok_or_else(|| FileProviderError::NotFound(original_path.to_path_buf()))
            .and_then(|bytes| {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|e| FileProviderError::IoError(e.to_string()))
            })
    }

    fn contains_or_dir(&self, normalized: &str) -> bool {
        self.files.contains_key(normalized) || self.is_dir(normalized)
    }

    fn is_dir(&self, normalized: &str) -> bool {
        let prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{}/", normalized.trim_end_matches('/'))
        };
        self.files.keys().any(|f| f.starts_with(&prefix))
    }

    fn list_dir(&self, normalized: &str) -> Vec<String> {
        let prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{}/", normalized.trim_end_matches('/'))
        };
        self.files
            .keys()
            .filter_map(|name| name.strip_prefix(&prefix))
            .filter_map(|rest| rest.split('/').next())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
}

/// File provider backed by in-memory source and stdlib bundles.
///
/// Supports plain source zips, release zips, and canonical `.tar.zst` bundles.
struct BundleFileProvider {
    project: MemoryFiles,
    stdlib: MemoryFiles,
    hinted_main_file: Option<String>,
    stdlib_root: String,
}

impl BundleFileProvider {
    fn new(bundle_bytes: Vec<u8>, stdlib_tar_zst_bytes: Vec<u8>) -> Result<Self, String> {
        let parsed = parse_bundle(bundle_bytes)?;
        let stdlib = MemoryFiles::new(parse_stdlib_archive(&stdlib_tar_zst_bytes)?);
        Ok(Self {
            project: MemoryFiles::new(parsed.files),
            stdlib,
            hinted_main_file: parsed.hinted_main_file,
            stdlib_root: Self::normalize(&wasm_stdlib_root()),
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

    fn stdlib_rel<'a>(&'a self, normalized: &'a str) -> Option<&'a str> {
        normalized
            .strip_prefix(&self.stdlib_root)
            .and_then(|s| s.strip_prefix('/'))
    }

    /// Auto-detect the main .zen file in the bundle.
    ///
    /// Looks in `boards/` for a single subdirectory containing a single .zen file.
    /// Returns the path like "boards/LG0002/LG0002.zen" if found.
    fn detect_main_file(&self) -> Option<String> {
        if let Some(main_file) = self.hinted_main_file.as_ref()
            && self.project.files.contains_key(main_file)
        {
            return Some(main_file.clone());
        }

        let board_dirs: HashSet<_> = self
            .project
            .files
            .keys()
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
            .project
            .files
            .keys()
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

        if normalized == self.stdlib_root {
            return Err(FileProviderError::NotFound(path.to_path_buf()));
        }
        if let Some(rel) = self.stdlib_rel(&normalized) {
            if pcb_zen_core::stdlib::include_path(Path::new(rel)) {
                return self.stdlib.read_utf8(rel, path);
            }
            return Err(FileProviderError::NotFound(path.to_path_buf()));
        }

        self.project.read_utf8(&normalized, path)
    }

    fn exists(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        if normalized == self.stdlib_root {
            return true;
        }
        if let Some(rel) = self.stdlib_rel(&normalized) {
            return pcb_zen_core::stdlib::include_path(Path::new(rel))
                && self.stdlib.contains_or_dir(rel);
        }
        self.project.contains_or_dir(&normalized)
    }

    fn is_directory(&self, path: &Path) -> bool {
        let normalized = Self::normalize(path);
        if normalized == self.stdlib_root {
            return true;
        }
        if let Some(rel) = self.stdlib_rel(&normalized) {
            return pcb_zen_core::stdlib::include_path(Path::new(rel)) && self.stdlib.is_dir(rel);
        }
        self.project.is_dir(&normalized)
    }

    fn is_symlink(&self, _path: &Path) -> bool {
        false
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        let normalized = Self::normalize(path).trim_end_matches('/').to_string();

        if normalized == self.stdlib_root {
            return Ok(self
                .stdlib
                .list_dir("")
                .into_iter()
                .map(|name| path.join(name))
                .collect());
        }
        if let Some(rel) = self.stdlib_rel(&normalized) {
            if !pcb_zen_core::stdlib::include_path(Path::new(rel)) || !self.stdlib.is_dir(rel) {
                return Err(FileProviderError::NotFound(path.to_path_buf()));
            }
            return Ok(self
                .stdlib
                .list_dir(rel)
                .into_iter()
                .map(|name| path.join(name))
                .collect());
        }

        Ok(self
            .project
            .list_dir(&normalized)
            .into_iter()
            .map(|name| path.join(name))
            .collect())
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
    let (is_release_bundle, hinted_main_file) = match archive.by_name("metadata.json") {
        Ok(mut file) => {
            let mut metadata = String::new();
            let hinted_main_file = if file.read_to_string(&mut metadata).is_ok() {
                extract_main_file_from_metadata(&metadata)
            } else {
                None
            };
            (true, hinted_main_file)
        }
        Err(_) => (false, None),
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

fn parse_stdlib_archive(stdlib_tar_zst_bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let decoder = StreamingDecoder::new(Cursor::new(stdlib_tar_zst_bytes))
        .map_err(|e| format!("stdlib zstd decode error: {e}"))?;
    let mut archive = Archive::new(decoder);
    let mut files = HashMap::new();

    for entry_result in archive
        .entries()
        .map_err(|e| format!("stdlib tar read error: {e}"))?
    {
        let mut entry = entry_result.map_err(|e| format!("stdlib tar entry error: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let path = {
            let raw_path = entry
                .path()
                .map_err(|e| format!("invalid stdlib tar path: {e}"))?;
            normalize_archive_path(raw_path.as_ref())
                .ok_or_else(|| format!("invalid stdlib archive path: {}", raw_path.display()))?
        };
        if path.is_empty() || !pcb_zen_core::stdlib::include_path(Path::new(&path)) {
            continue;
        }

        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|e| format!("stdlib tar entry read error: {e}"))?;
        files.insert(path, contents);
    }

    for required in ["pcb.toml", "interfaces.zen"] {
        if !files.contains_key(required) {
            return Err(format!("stdlib archive is missing {required}"));
        }
    }

    Ok(files)
}

fn normalize_archive_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => parts.push(name.to_str()?.to_string()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
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
            append_tar_file(&mut builder, "metadata.json", br#"{"kind":"bundle"}"#);
            append_tar_file(&mut builder, "src/boards/demo/demo.zen", b"print('demo')");
            builder.finish().expect("finish tar");
        }

        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(&tar_bytes).expect("encode tar");
        encoder.finish().expect("finish zstd")
    }

    fn append_tar_file(builder: &mut Builder<&mut Vec<u8>>, path: &str, contents: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents)
            .expect("append tar file");
    }

    fn stdlib_tar_zst_bytes() -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = Builder::new(&mut tar_bytes);
            append_tar_file(&mut builder, "pcb.toml", b"[dependencies]\n");
            append_tar_file(&mut builder, "interfaces.zen", b"# interfaces\n");
            append_tar_file(&mut builder, "units.zen", b"# units\n");
            append_tar_file(&mut builder, "generics/Resistor.zen", b"# resistor\n");
            append_tar_file(&mut builder, "test/test_checks.zen", b"# excluded\n");
            builder.finish().expect("finish tar");
        }

        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(&tar_bytes).expect("encode tar");
        encoder.finish().expect("finish zstd")
    }

    fn provider(bundle: Vec<u8>) -> BundleFileProvider {
        BundleFileProvider::new(bundle, stdlib_tar_zst_bytes()).expect("create provider")
    }

    #[test]
    fn list_stdlib_root_includes_archive_top_level_entries() {
        let provider = provider(empty_zip_bytes());
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
        let provider = provider(empty_zip_bytes());
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
    fn missing_stdlib_archive_errors() {
        let err = match BundleFileProvider::new(empty_zip_bytes(), Vec::new()) {
            Ok(_) => panic!("empty stdlib archive should fail"),
            Err(err) => err,
        };
        assert!(err.contains("stdlib"));
    }

    #[test]
    fn tar_zst_bundle_is_normalized_to_src_contents() {
        let provider = provider(tar_zst_bundle_bytes());
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
                append_tar_file(
                    &mut builder,
                    "metadata.json",
                    br#"{"release":{"zen_file":"reference/demo/demo.zen"}}"#,
                );
                append_tar_file(&mut builder, "src/pcb.toml", b"[workspace]\n");
                append_tar_file(
                    &mut builder,
                    "src/reference/demo/demo.zen",
                    b"print('demo')",
                );
                builder.finish().expect("finish tar");
            }

            let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
            encoder.write_all(&tar_bytes).expect("encode tar");
            encoder.finish().expect("finish zstd")
        };

        let provider = provider(tar_bytes);
        assert_eq!(
            provider.detect_main_file().as_deref(),
            Some("reference/demo/demo.zen")
        );
    }
}

/// Build frozen package resolution from hydrated manifests and vendored dependencies.
///
/// Assumes all deps are vendored in `vendor/`. No patches, no cache fallback.
/// Uses shared resolution logic from pcb-zen-core.
fn resolve_packages<F: FileProvider + Clone>(
    file_provider: F,
    workspace_root: &Path,
    main_path: &Path,
) -> Result<pcb_zen_core::resolution::ResolutionResult, String> {
    let vendor_dir = workspace_root.join("vendor");
    let workspace = get_workspace_info(&file_provider, workspace_root)
        .map_err(|e| format!("Failed to discover workspace metadata: {e}"))?;
    let package_url = hydrated_package_url(&workspace, main_path).ok_or_else(|| {
        "Source bundle is missing hydrated dependency state; run `pcb sync` before bundling"
            .to_string()
    })?;
    let resolver = VendoredPathResolver::from_selected_versions(
        vendor_dir,
        selected_versions_from_manifest(&workspace, &package_url)?,
    );

    let package_resolutions =
        build_resolution_map(&file_provider, &resolver, &workspace, resolver.closure());
    let frozen = build_wasm_frozen_resolution(
        &workspace,
        &package_resolutions,
        &file_provider,
        &package_url,
    )?;
    Ok(pcb_zen_core::resolution::ResolutionResult::frozen(
        workspace,
        FrozenResolutionSet::from([(package_url, frozen)]),
        HashMap::new(),
    ))
}

fn hydrated_package_url(workspace: &WorkspaceInfo, main_path: &Path) -> Option<String> {
    package_url_for_zen(workspace, main_path)
}

fn selected_versions_from_manifest(
    workspace: &WorkspaceInfo,
    package_url: &str,
) -> Result<HashMap<ModuleLine, Version>, String> {
    Ok(
        selected_remote_from_hydrated_manifest(workspace, package_url)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(dep_id, version)| (ModuleLine::new(dep_id.path, &version), version))
            .collect(),
    )
}

fn package_url_for_zen(workspace: &WorkspaceInfo, path: &Path) -> Option<String> {
    workspace
        .packages
        .iter()
        .filter(|(_, package)| path.starts_with(package.dir(&workspace.root)))
        .max_by_key(|(_, package)| package.dir(&workspace.root).as_os_str().len())
        .map(|(url, _)| url.clone())
}

fn build_wasm_frozen_resolution<F: FileProvider>(
    workspace: &WorkspaceInfo,
    package_resolutions: &HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    file_provider: &F,
    package_url: &str,
) -> Result<FrozenResolutionMap, String> {
    let selected_remote = selected_remote_from_hydrated_manifest(workspace, package_url)
        .map_err(|e| e.to_string())?;
    let mut packages = BTreeMap::new();

    for (root, deps) in package_resolutions {
        let Some(identity) = frozen_identity_for_root(workspace, &selected_remote, root) else {
            continue;
        };
        let parts = match &identity {
            FrozenPackageIdentity::Workspace(url) => workspace
                .packages
                .get(url)
                .map(|package| package.config.parts.clone())
                .unwrap_or_default(),
            FrozenPackageIdentity::Remote { .. } => read_manifest(file_provider, root)?.parts,
            FrozenPackageIdentity::Stdlib => Vec::new(),
        };
        packages.insert(
            root.clone(),
            FrozenPackage {
                identity,
                deps: deps.clone(),
                parts,
            },
        );
    }

    Ok(FrozenResolutionMap {
        selected_remote,
        packages,
    })
}

fn frozen_identity_for_root(
    workspace: &WorkspaceInfo,
    selected_remote: &BTreeMap<FrozenDepId, Version>,
    root: &Path,
) -> Option<FrozenPackageIdentity> {
    if root == workspace.workspace_stdlib_dir() {
        return Some(FrozenPackageIdentity::Stdlib);
    }

    workspace
        .packages
        .iter()
        .find(|(_, package)| package.dir(&workspace.root) == root)
        .map(|(url, _)| FrozenPackageIdentity::Workspace(url.clone()))
        .or_else(|| {
            selected_remote.iter().find_map(|(dep_id, version)| {
                let package_root = workspace
                    .root
                    .join("vendor")
                    .join(&dep_id.path)
                    .join(version.to_string());
                (package_root == root).then(|| FrozenPackageIdentity::Remote {
                    dep_id: dep_id.clone(),
                    version: version.clone(),
                })
            })
        })
}

fn read_manifest<F: FileProvider>(file_provider: &F, root: &Path) -> Result<PcbToml, String> {
    let path = root.join("pcb.toml");
    let content = file_provider
        .read_file(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    PcbToml::parse(&content).map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
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
    stdlib_tar_zst_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<EvaluationResult, String> {
    let file_provider = Arc::new(BundleFileProvider::new(bundle_bytes, stdlib_tar_zst_bytes)?);

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
    let main_path = Path::new("/").join(requested_main_path);
    let main_path = file_provider
        .canonicalize(&main_path)
        .map_err(|e| format!("Failed to canonicalize main file path: {e}"))?;
    let workspace_root = find_workspace_root(file_provider.as_ref(), &main_path)
        .map_err(|e| format!("Failed to find workspace root: {e}"))?;
    let resolution = resolve_packages(file_provider.clone(), &workspace_root, &main_path)
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
    stdlib_tar_zst_bytes: Vec<u8>,
    main_file: &str,
    inputs_json: &str,
) -> Result<JsValue, JsValue> {
    let result = evaluate_impl(bundle_bytes, stdlib_tar_zst_bytes, main_file, inputs_json)
        .map_err(|e| JsValue::from_str(&e))?;

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
