#![allow(dead_code)]

use pcb_zen_core::{FileProvider, FileProviderError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// In-memory file provider for tests
#[derive(Clone)]
pub struct InMemoryFileProvider {
    files: HashMap<PathBuf, String>,
}

impl InMemoryFileProvider {
    pub fn new(files: HashMap<String, String>) -> Self {
        let mut path_files = HashMap::new();
        for (path, content) in files {
            // Ensure all paths are stored as absolute paths
            let path_buf = PathBuf::from(path);
            let absolute_path = if path_buf.is_absolute() {
                path_buf
            } else {
                // Convert relative paths to absolute by prepending /
                PathBuf::from("/").join(path_buf)
            };
            path_files.insert(absolute_path, dedent(&content));
        }
        Self { files: path_files }
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

        Ok(canonical_path)
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
            use std::sync::Arc;
            use pcb_zen_core::{EvalContext, InputMap, WorkspaceLoadResolver};
            use $crate::common::InMemoryFileProvider;

            let mut files = std::collections::HashMap::new();
            let file_list = vec![$(($file.to_string(), $content.to_string())),+];

            for (file, content) in &file_list {
                files.insert(file.clone(), content.clone());
            }

            // The last file in the list is the main file
            let main_file = file_list.last().unwrap().0.clone();

            let file_provider = Arc::new(InMemoryFileProvider::new(files));
            let load_resolver = Arc::new(WorkspaceLoadResolver::new(
                std::path::PathBuf::from("/"),
            ));


            let ctx = EvalContext::new()
                .set_file_provider(file_provider)
                .set_load_resolver(load_resolver)
                .set_source_path(std::path::PathBuf::from(&main_file))
                .set_module_name("<root>")
                .set_inputs(InputMap::new());

            let result = ctx.eval();

            // Format the output similar to the original tests
            let output = if result.is_success() {
                if let Some(eval_output) = result.output {
                    // Sort the signature HashMap by keys for deterministic output
                    let mut sorted_signature: Vec<_> = eval_output.signature.into_iter().collect();
                    sorted_signature.sort_by(|a, b| a.0.cmp(&b.0));

                    let mut output_parts = vec![];

                    // Include print output if there was any
                    if !eval_output.print_output.is_empty() {
                        for line in eval_output.print_output {
                            output_parts.push(line);
                        }
                    }

                    output_parts.push(format!("{:#?}", eval_output.sch_module));
                    output_parts.push(format!("{:#?}", sorted_signature));

                    output_parts.join("\n") + "\n"
                } else {
                    String::new()
                }
            } else {
                // Format diagnostics
                result.diagnostics.iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            };

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
