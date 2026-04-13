#![allow(dead_code)]

use pcb_zen_core::{FileProvider, FileProviderError};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEST_KICAD_LIBRARY_VERSION: &str = "9.0.3";
const TEST_KICAD_ADDITIONAL_LIBRARY_VERSION: &str = "10.0.0";
const TEST_KICAD_SYMBOLS_REPO: &str = "gitlab.com/kicad/libraries/kicad-symbols";
const TEST_KICAD_FOOTPRINTS_REPO: &str = "gitlab.com/kicad/libraries/kicad-footprints";
const TEST_KICAD_MODELS_REPO: &str = "gitlab.com/kicad/libraries/kicad-packages3D";

fn test_kicad_symbols_root() -> PathBuf {
    PathBuf::from("/.pcb/cache")
        .join(TEST_KICAD_SYMBOLS_REPO)
        .join(TEST_KICAD_LIBRARY_VERSION)
}

fn test_kicad_footprints_root() -> PathBuf {
    PathBuf::from("/.pcb/cache")
        .join(TEST_KICAD_FOOTPRINTS_REPO)
        .join(TEST_KICAD_LIBRARY_VERSION)
}

fn test_kicad_models_root() -> PathBuf {
    PathBuf::from("/.pcb/cache")
        .join(TEST_KICAD_MODELS_REPO)
        .join(TEST_KICAD_LIBRARY_VERSION)
}

fn power_symbol_library() -> String {
    r##"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "VCC" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "#PWR" (id 0) (at 0 0 0))
    (symbol "VCC_1_1")
  )
  (symbol "GND" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "#PWR" (id 0) (at 0 0 0))
    (symbol "GND_1_1")
  )
)"##
    .to_string()
}

pub fn default_test_kicad_resolution_map() -> BTreeMap<String, PathBuf> {
    BTreeMap::from([
        (
            TEST_KICAD_SYMBOLS_REPO.to_string(),
            test_kicad_symbols_root(),
        ),
        (
            TEST_KICAD_FOOTPRINTS_REPO.to_string(),
            test_kicad_footprints_root(),
        ),
        (TEST_KICAD_MODELS_REPO.to_string(), test_kicad_models_root()),
    ])
}

fn fake_kicad_test_files() -> HashMap<String, String> {
    [
        PathBuf::from("/.pcb/cache")
            .join(TEST_KICAD_SYMBOLS_REPO)
            .join(TEST_KICAD_LIBRARY_VERSION),
        PathBuf::from("/.pcb/cache")
            .join(TEST_KICAD_SYMBOLS_REPO)
            .join(TEST_KICAD_ADDITIONAL_LIBRARY_VERSION),
    ]
    .into_iter()
    .map(|root| {
        (
            root.join("power.kicad_sym").to_string_lossy().into_owned(),
            power_symbol_library(),
        )
    })
    .collect()
}

/// Return stdlib `.zen` files keyed by their absolute in-memory path
/// (e.g. `"/.pcb/stdlib/interfaces.zen"` or `"/workspace/.pcb/stdlib/interfaces.zen"`).
/// Also includes the minimal fake KiCad symbol library needed by stdlib prelude defaults.
pub fn stdlib_test_files_at(workspace_root: &Path) -> HashMap<String, String> {
    pcb_zen_core::embedded_stdlib::stdlib_files_for_tests()
        .into_iter()
        .map(|(rel, contents)| {
            (
                workspace_root
                    .join(".pcb/stdlib")
                    .join(rel)
                    .to_string_lossy()
                    .into_owned(),
                contents,
            )
        })
        .chain(fake_kicad_test_files())
        .collect()
}

/// Return stdlib `.zen` files keyed by their absolute in-memory path
/// (e.g. `"/.pcb/stdlib/interfaces.zen"`). Intended to be merged into the
/// files map passed to [`InMemoryFileProvider`].
pub fn stdlib_test_files() -> HashMap<String, String> {
    stdlib_test_files_at(Path::new("/"))
}

/// Preamble prepended to every `.zen` file in `eval_zen` so tests don't have
/// to repeat the Net definition in every inline snippet. This must match the
/// production definition in stdlib/interfaces.zen (symbol, voltage, impedance).
const ZEN_TEST_PREAMBLE: &str = "\
Voltage = builtin.physical_value(\"V\")\n\
Impedance = builtin.physical_value(\"Ohm\")\n\
Net = builtin.net_type(\"Net\", symbol=Symbol, voltage=field(Voltage | None, default=None), impedance=field(Impedance | None, default=None)); io = builtin.io; input = partial(io, direction=\"input\"); output = partial(io, direction=\"output\")\n";

/// Prepend `ZEN_TEST_PREAMBLE` to a `.zen` source string, matching the
/// existing indentation so that `dedent` still works correctly.
fn prepend_preamble(content: &str) -> String {
    // Find the indentation of the first non-empty line
    let indent = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| &l[..l.len() - l.trim_start().len()])
        .unwrap_or("");
    // Indent each line of the preamble to match
    let indented_preamble: String = ZEN_TEST_PREAMBLE
        .lines()
        .map(|l| format!("{indent}{l}\n"))
        .collect();
    // Preserve any leading newline (common in r#"\n  ..."# literals)
    let leading_newline = if content.starts_with('\n') { "\n" } else { "" };
    let rest = content.strip_prefix('\n').unwrap_or(content);
    format!("{leading_newline}{indented_preamble}{rest}")
}

/// Evaluate a set of `.zen` files using an in-memory file provider with stdlib
/// materialized. The last file in `user_files` is used as the entry point.
///
/// This is the common test harness used by both snapshot macros and manual test
/// helpers.  It handles stdlib materialization, resolution setup, and context
/// creation so callers don't have to repeat the boilerplate.
///
/// Every `.zen` file automatically gets the production-equivalent Net
/// definition (with symbol, voltage, impedance fields) prepended.
pub fn eval_zen(
    user_files: Vec<(String, String)>,
) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput> {
    let main_file = user_files.last().expect("need at least one file").0.clone();
    let mut files = stdlib_test_files();
    for (path, content) in user_files {
        if path.ends_with(".zen") {
            files.insert(path, prepend_preamble(&content));
        } else {
            files.insert(path, content);
        }
    }
    eval_zen_raw(files, &main_file)
}

/// Like [`eval_zen`] but accepts a pre-built files map (already including
/// stdlib) and an explicit main file path.
pub fn eval_zen_raw(
    files: HashMap<String, String>,
    main_file: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput> {
    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(files));
    let resolution = test_resolution();
    pcb_zen_core::EvalContext::new(file_provider, resolution)
        .set_source_path(PathBuf::from(main_file))
        .set_inject_prelude(false)
        .eval()
}

/// Build a minimal `ResolutionResult` suitable for most in-memory tests.
/// Sets workspace root to `/` with a single "test" package.
pub fn test_resolution() -> pcb_zen_core::resolution::ResolutionResult {
    test_resolution_at(Path::new("/"))
}

/// Build a minimal `ResolutionResult` suitable for in-memory tests at an arbitrary workspace root.
pub fn test_resolution_at(workspace_root: &Path) -> pcb_zen_core::resolution::ResolutionResult {
    let mut resolution = pcb_zen_core::resolution::ResolutionResult::empty();
    resolution.workspace_info.root = workspace_root.to_path_buf();
    resolution.workspace_info.packages.insert(
        "test".to_string(),
        pcb_zen_core::workspace::MemberPackage {
            rel_path: PathBuf::new(),
            config: Default::default(),
            version: None,
            published_at: None,
            preferred: false,
            dirty: false,
        },
    );
    let default_deps = default_test_kicad_resolution_map();
    resolution
        .package_resolutions
        .insert(workspace_root.to_path_buf(), default_deps.clone());
    resolution
        .package_resolutions
        .insert(workspace_root.join(".pcb/stdlib"), default_deps);
    resolution
}

/// In-memory file provider for tests
#[derive(Clone)]
pub struct InMemoryFileProvider {
    files: HashMap<PathBuf, String>,
}

impl InMemoryFileProvider {
    pub fn new(files: HashMap<String, String>) -> Self {
        let mut path_files = HashMap::new();
        for (path, content) in files {
            // Normalize separators so the virtual FS stays platform-independent.
            let normalized = path.replace('\\', "/");
            let path_buf = PathBuf::from(normalized);
            let absolute_path = if path_buf.is_absolute() {
                path_buf
            } else {
                // Convert relative paths to absolute by prepending /
                PathBuf::from("/").join(path_buf)
            };
            path_files.insert(Self::normalize_path(absolute_path), dedent(&content));
        }
        Self { files: path_files }
    }

    fn normalize_path(path: PathBuf) -> PathBuf {
        PathBuf::from(path.to_string_lossy().replace('\\', "/"))
    }
}

impl FileProvider for InMemoryFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        let path = self.canonicalize(path)?;

        if self.is_directory(&path) {
            return Err(FileProviderError::IoError(format!(
                "Is a directory: {}",
                path.display()
            )));
        }

        self.files
            .get(&path)
            .cloned()
            .ok_or_else(|| FileProviderError::NotFound(path.to_path_buf()))
    }

    fn exists(&self, path: &Path) -> bool {
        match self.canonicalize(path) {
            Ok(path) => self.files.contains_key(&path) || self.is_directory(&path),
            Err(_) => false,
        }
    }

    fn is_directory(&self, path: &Path) -> bool {
        match self.canonicalize(path) {
            Ok(path) => {
                // Special case for root directories
                if path == Path::new("/") || path == Path::new(".") || path == Path::new("") {
                    // Root is a directory if we have any files
                    return !self.files.is_empty();
                }

                // A path is a directory if any file has it as a prefix
                let path_str = path.to_string_lossy();
                self.files.keys().any(|file_path| {
                    let file_str = file_path.to_string_lossy();
                    file_str.starts_with(&format!("{path_str}/"))
                        || file_str.starts_with(&format!("{path_str}\\"))
                })
            }
            Err(_) => false,
        }
    }

    fn is_symlink(&self, _path: &Path) -> bool {
        false
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        let path = self.canonicalize(path)?;

        if !self.is_directory(&path) {
            return Err(FileProviderError::NotFound(path.to_path_buf()));
        }

        let mut entries = std::collections::HashSet::new();

        // Normalize the directory path for comparison
        let is_root = path == Path::new("/");
        let path_str = if is_root {
            "/".to_string()
        } else {
            format!("{}/", path.to_string_lossy())
        };

        for file_path in self.files.keys() {
            let file_str = file_path.to_string_lossy();

            // For root directory, all files should start with "/"
            // For other directories, check if the file is under this directory
            if is_root {
                // For root, we want immediate children only
                if file_str.starts_with('/') && file_str.len() > 1 {
                    let relative = &file_str[1..]; // Skip the leading /
                    if let Some(sep_pos) = relative.find('/') {
                        // It's in a subdirectory - add the subdirectory
                        let subdir = &relative[..sep_pos];
                        entries.insert(PathBuf::from("/").join(subdir));
                    } else {
                        // It's a file in the root directory
                        entries.insert(file_path.clone());
                    }
                }
            } else {
                // For non-root directories
                if file_str.starts_with(&path_str) {
                    let relative = &file_str[path_str.len()..];

                    if let Some(sep_pos) = relative.find('/') {
                        // It's in a subdirectory - add the subdirectory
                        let subdir = &relative[..sep_pos];
                        entries.insert(path.join(subdir));
                    } else if !relative.is_empty() {
                        // It's a file in this directory
                        entries.insert(file_path.clone());
                    }
                }
            }
        }

        let mut result: Vec<_> = entries.into_iter().collect();
        result.sort();

        Ok(result)
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileProviderError> {
        let mut path_buf = path.to_path_buf();
        if !path_buf.is_absolute() {
            path_buf = Path::new("/").join(path_buf);
        }

        // Normalize the path by removing . and .. components
        let mut components = Vec::new();

        for component in path_buf.components() {
            match component {
                std::path::Component::CurDir => {
                    // Skip "." components
                }
                std::path::Component::ParentDir => {
                    // Handle ".." by popping the last component if possible
                    if !components.is_empty() {
                        components.pop();
                    }
                }
                std::path::Component::Normal(name) => {
                    components.push(name);
                }
                std::path::Component::RootDir => {
                    // Start from root
                    components.clear();
                }
                std::path::Component::Prefix(_) => {
                    // Handle Windows prefixes if needed
                    components.clear();
                }
            }
        }

        // Reconstruct the path from normalized components
        let mut canonical_path = PathBuf::new();
        canonical_path.push("/");

        for component in components {
            canonical_path.push(component);
        }

        Ok(Self::normalize_path(canonical_path))
    }
}

/// Macro to create a test that evaluates Starlark code and compares the output to a snapshot.
///
/// # Example
///
/// ```rust
/// snapshot_eval!(test_name, {
///     "file1.zen" => "content1",
///     "file2.zen" => "content2",
///     "main.zen" => "main content"
/// });
/// ```
///
/// This will:
/// 1. Create an in-memory file system with the specified files
/// 2. Evaluate "main.zen" (the last file in the list)
/// 3. Compare the output to a snapshot
#[macro_export]
macro_rules! snapshot_eval {
    ($name:ident, { $($file:expr => $content:expr),+ $(,)? }) => {
        #[test]
        #[cfg(not(target_os = "windows"))]
        fn $name() {
            use pcb_zen_core::{SortPass, DiagnosticsPass};

            let file_list = vec![$(($file.to_string(), $content.to_string())),+];
            let result = $crate::common::eval_zen(file_list);

            // Format the output similar to the original tests
            let mut output = if result.is_success() {
                if let Some(eval_output) = result.output {
                    let mut output_parts = vec![];

                    // Include print output if there was any
                    if !eval_output.print_output.is_empty() {
                        for line in &eval_output.print_output {
                            output_parts.push(line.clone());
                        }
                    }

                    // Include warnings if there were any (sorted for determinism)
                    let mut diagnostics = result.diagnostics.clone();
                    SortPass.apply(&mut diagnostics);
                    let warnings = diagnostics.warnings();
                    if !warnings.is_empty() {
                        for warning in warnings {
                            output_parts.push(warning.to_string());
                        }
                    }

                    output_parts.push(format!("{:#?}", eval_output.module_tree()));
                    output_parts.push(format!("{:#?}", eval_output.signature));

                    output_parts.join("\n") + "\n"
                } else {
                    String::new()
                }
            } else {
                // Sort diagnostics for deterministic output, then format
                let mut diagnostics = result.diagnostics;
                SortPass.apply(&mut diagnostics);
                diagnostics.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            // Sanitize net IDs for stable snapshots (apply to both success and error output)
            // Replace patterns like id: "123" with id: "<ID>"
            output = regex::Regex::new(r#"id: "\d+""#)
                .unwrap()
                .replace_all(&output, r#"id: "<ID>""#)
                .to_string();
            // Replace patterns like "id: 123" with "id: <ID>"
            output = regex::Regex::new(r#"id: \d+"#)
                .unwrap()
                .replace_all(&output, "id: <ID>")
                .to_string();
            // Replace patterns like "net_id": Number(123) with "net_id": Number(<ID>)
            output = regex::Regex::new(r#""net_id": Number\(\d+\)"#)
                .unwrap()
                .replace_all(&output, r#""net_id": Number(<ID>)"#)
                .to_string();

            insta::assert_snapshot!(output);
        }
    };
}

/// Macro to create a test that evaluates Starlark code and snapshots the generated KiCad netlist.
///
/// Unlike `snapshot_eval!`, this validates the post-eval conversion path by
/// calling `to_schematic_with_diagnostics()` and serializing with
/// `pcb_sch::kicad_netlist::to_kicad_netlist`.
#[macro_export]
macro_rules! snapshot_netlist_eval {
    ($name:ident, { $($file:expr => $content:expr),+ $(,)? }) => {
        #[test]
        #[cfg(not(target_os = "windows"))]
        fn $name() {
            use pcb_zen_core::{DiagnosticsPass, SortPass};

            let file_list = vec![$(($file.to_string(), $content.to_string())),+];
            let eval_result = $crate::common::eval_zen(file_list);
            let mut output = String::new();

            let mut diagnostics = eval_result.diagnostics.clone();
            if let Some(eval_output) = eval_result.output {
                let sch_result = eval_output.to_schematic_with_diagnostics();
                diagnostics.extend(sch_result.diagnostics);

                if let Some(schematic) = sch_result.output {
                    output.push_str(&pcb_sch::kicad_netlist::to_kicad_netlist(&schematic));
                    output.push('\n');
                }
            }

            SortPass.apply(&mut diagnostics);
            if !diagnostics.is_empty() {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(
                    &diagnostics
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }

            insta::assert_snapshot!(output);
        }
    };
}

/// Strips common leading indentation from a string.
/// This allows test code to be indented nicely without affecting the actual content.
fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    // Find the minimum indentation (ignoring empty lines)
    let min_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);

    // Remove the common indentation from all lines
    lines
        .iter()
        .map(|line| {
            if line.len() > min_indent {
                &line[min_indent..]
            } else {
                line.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_canonicalize() {
        let mut files = HashMap::new();
        // Files will be automatically converted to absolute paths
        files.insert("foo/bar.txt".to_string(), "content".to_string());
        files.insert("baz.txt".to_string(), "content".to_string());

        let provider = InMemoryFileProvider::new(files);

        // Test basic canonicalization with absolute path
        assert_eq!(
            provider.canonicalize(Path::new("/foo/bar.txt")).unwrap(),
            PathBuf::from("/foo/bar.txt")
        );

        // Test with current directory in absolute path
        assert_eq!(
            provider.canonicalize(Path::new("/./foo/bar.txt")).unwrap(),
            PathBuf::from("/foo/bar.txt")
        );

        // Test with parent directory in absolute path
        assert_eq!(
            provider.canonicalize(Path::new("/foo/../baz.txt")).unwrap(),
            PathBuf::from("/baz.txt")
        );

        // Test with multiple parent directories
        assert_eq!(
            provider
                .canonicalize(Path::new("/foo/bar/../../baz.txt"))
                .unwrap(),
            PathBuf::from("/baz.txt")
        );

        // Test root path
        assert_eq!(
            provider.canonicalize(Path::new("/")).unwrap(),
            PathBuf::from("/")
        );
    }

    #[test]
    fn test_list_directory() {
        let mut files = HashMap::new();
        files.insert("file1.txt".to_string(), "content1".to_string());
        files.insert("file2.txt".to_string(), "content2".to_string());
        files.insert("dir1/file3.txt".to_string(), "content3".to_string());
        files.insert("dir1/file4.txt".to_string(), "content4".to_string());
        files.insert("dir2/subdir/file5.txt".to_string(), "content5".to_string());

        let provider = InMemoryFileProvider::new(files);

        // Test listing root directory
        let mut root_entries = provider.list_directory(Path::new("/")).unwrap();
        root_entries.sort();
        assert_eq!(
            root_entries,
            vec![
                PathBuf::from("/dir1"),
                PathBuf::from("/dir2"),
                PathBuf::from("/file1.txt"),
                PathBuf::from("/file2.txt"),
            ]
        );

        // Test listing subdirectory
        let mut dir1_entries = provider.list_directory(Path::new("/dir1")).unwrap();
        dir1_entries.sort();
        assert_eq!(
            dir1_entries,
            vec![
                PathBuf::from("/dir1/file3.txt"),
                PathBuf::from("/dir1/file4.txt"),
            ]
        );

        // Test listing directory with subdirectory
        let mut dir2_entries = provider.list_directory(Path::new("/dir2")).unwrap();
        dir2_entries.sort();
        assert_eq!(dir2_entries, vec![PathBuf::from("/dir2/subdir")]);

        // Test listing non-existent directory
        assert!(provider.list_directory(Path::new("/nonexistent")).is_err());
    }
}
