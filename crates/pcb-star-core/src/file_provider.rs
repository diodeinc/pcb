use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{FileProvider, FileProviderError};

/// In-memory file provider that stores files in a HashMap.
/// Useful for testing and WASM environments where file system access is not available.
#[derive(Clone, Debug)]
pub struct InMemoryFileProvider {
    files: HashMap<PathBuf, String>,
}

impl InMemoryFileProvider {
    /// Create a new InMemoryFileProvider with the given files.
    /// Keys should be file paths (can be relative or absolute).
    /// Relative paths will be converted to absolute paths with "/" as root.
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
            path_files.insert(absolute_path, content);
        }
        Self { files: path_files }
    }

    /// Create an empty InMemoryFileProvider
    pub fn empty() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    /// Add a file to the provider
    pub fn add_file(&mut self, path: impl Into<PathBuf>, content: String) {
        let path_buf: PathBuf = path.into();
        let absolute_path = if path_buf.is_absolute() {
            path_buf
        } else {
            PathBuf::from("/").join(path_buf)
        };
        self.files.insert(absolute_path, content);
    }

    /// Get a reference to all files
    pub fn files(&self) -> &HashMap<PathBuf, String> {
        &self.files
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize() {
        let mut files = HashMap::new();
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
    }

    #[test]
    fn test_list_directory() {
        let mut files = HashMap::new();
        files.insert("file1.txt".to_string(), "content1".to_string());
        files.insert("dir1/file3.txt".to_string(), "content3".to_string());
        files.insert("dir1/file4.txt".to_string(), "content4".to_string());

        let provider = InMemoryFileProvider::new(files);

        // Test listing root directory
        let mut root_entries = provider.list_directory(Path::new("/")).unwrap();
        root_entries.sort();
        assert_eq!(
            root_entries,
            vec![PathBuf::from("/dir1"), PathBuf::from("/file1.txt"),]
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
    }
}
