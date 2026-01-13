//! Generate documentation from Zener source files.
//!
//! This crate parses `.zen` files from a package directory, extracts docstrings
//! and module signatures, and generates markdown documentation.

mod parser;
mod render;
mod signature;
mod types;

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub use types::*;

/// Generate documentation for a Zener package.
///
/// - `package_url`: Used as the h1 header (e.g. "github.com/diodeinc/stdlib")
/// - `display_path`: Path shown in source comment; defaults to package_root if None
pub fn generate_docs(
    package_root: &Path,
    package_url: Option<&str>,
    display_path: Option<&str>,
) -> Result<DocsResult> {
    // Canonicalize to ensure consistent path handling
    let package_root = package_root
        .canonicalize()
        .unwrap_or_else(|_| package_root.to_path_buf());
    let zen_files = collect_zen_files(&package_root)?;

    let mut files = Vec::new();

    for path in zen_files {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let file_path = get_file_path(&package_root, &path);

        match signature::try_get_signature(&path) {
            signature::SignatureResult::Module(sig) => {
                files.push(FileDoc::Module(ModuleDoc {
                    path: file_path,
                    file_doc: parser::extract_file_docstring(&content),
                    signature: sig,
                }));
            }
            signature::SignatureResult::Library => {
                files.push(FileDoc::Library(parser::parse_library(
                    file_path, &content,
                )?));
            }
            signature::SignatureResult::Error(e) => {
                eprintln!("Warning: Failed to parse {}: {}", file_path, e);
            }
        }
    }

    // Sort by path
    files.sort_by(|a, b| a.path().cmp(b.path()));

    let default_path = package_root.to_string_lossy();
    let local_path = display_path.unwrap_or(&default_path);
    let markdown = render::render_docs(&files, package_url, Some(local_path));

    let (library_count, module_count) = files.iter().fold((0, 0), |(l, m), f| match f {
        FileDoc::Library(_) => (l + 1, m),
        FileDoc::Module(_) => (l, m + 1),
    });

    Ok(DocsResult {
        markdown,
        library_count,
        module_count,
    })
}

/// Collect all .zen files, excluding test/ and hidden directories.
fn collect_zen_files(root: &Path) -> Result<Vec<PathBuf>> {
    // Canonicalize root to ensure strip_prefix works correctly with WalkDir paths
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut files: Vec<_> = WalkDir::new(&canonical_root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "zen"))
        .filter(|e| {
            // Only check path components relative to root, not the absolute path
            let rel_path = e.path().strip_prefix(&canonical_root).unwrap_or(e.path());
            !rel_path.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s == "test" || s.starts_with('.')
            })
        })
        .map(|e| e.into_path())
        .collect();

    files.sort();
    Ok(files)
}

/// Get the file path relative to package root.
fn get_file_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Result of the documentation generation.
pub struct DocsResult {
    pub markdown: String,
    pub library_count: usize,
    pub module_count: usize,
}
