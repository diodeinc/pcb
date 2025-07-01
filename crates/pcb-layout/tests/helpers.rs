use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Gets the path to test resources
#[allow(unused)]
pub fn get_resource_path(resource_name: &str) -> PathBuf {
    let relative_path = format!("tests/resources/{resource_name}");

    // Return the relative path - tests will be run from the crate root
    PathBuf::from(relative_path)
}

/// Normalizes path separators to forward slashes
#[allow(unused)]
pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Creates a directory structure snapshot representation
#[allow(unused)]
pub fn create_dir_snapshot<P: AsRef<Path>>(dir_path: P) -> Result<Vec<String>> {
    let mut dirs = Vec::new();
    for entry in WalkDir::new(&dir_path)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel_path = entry
            .path()
            .strip_prefix(&dir_path)?
            .to_string_lossy()
            .to_string();
        if rel_path.is_empty() {
            continue;
        }
        let prefix = if entry.file_type().is_dir() { "d" } else { "f" };
        dirs.push(format!("{} {}", prefix, normalize_path(&rel_path)));
    }
    Ok(dirs)
}

/// Macro to generate a snapshot test of a directory structure
#[macro_export]
macro_rules! assert_dir_snapshot {
    ($name:expr, $dir:expr) => {
        let dirs = create_dir_snapshot($dir)?;
        insta::assert_snapshot!($name, dirs.join("\n"));
    };
}

/// Creates a snapshot of a file's contents
#[allow(unused)]
pub fn create_file_snapshot<P: AsRef<Path>>(file_path: P) -> Result<String> {
    Ok(fs::read_to_string(file_path)?)
}

/// Macro to generate a snapshot test of a file's contents
#[macro_export]
macro_rules! assert_file_snapshot {
    ($name:expr, $file:expr) => {
        let content = create_file_snapshot($file)?;
        insta::assert_snapshot!($name, content);
    };
}

/// Creates a snapshot of a binary file's contents
#[allow(unused)]
pub fn create_binary_snapshot<P: AsRef<Path>>(file_path: P) -> Result<Vec<u8>> {
    Ok(fs::read(file_path)?)
}

/// Macro to generate a snapshot test of a binary file's contents
#[macro_export]
macro_rules! assert_binary_snapshot {
    ($name:expr, $file:expr) => {
        let content = create_binary_snapshot($file)?;
        insta::assert_binary_snapshot!(&$name, content);
    };
}

/// Creates a structured snapshot representation of a zip file's contents
#[allow(unused)]
pub fn create_zip_snapshot<P, F>(file_path: P, redact: F) -> Result<Vec<String>>
where
    P: AsRef<Path>,
    F: Fn(&str) -> bool,
{
    use std::collections::BTreeMap;
    use zip::ZipArchive;

    let file = fs::File::open(file_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entries = BTreeMap::new();

    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = normalize_path(file.name());
        let info = if redact(&name) {
            "size=REDACTED, compressed_size=REDACTED".to_string()
        } else {
            format!(
                "size={}, compressed_size={}",
                file.size(),
                file.compressed_size()
            )
        };
        entries.insert(name, info);
    }

    Ok(entries
        .into_iter()
        .map(|(name, info)| format!("{name}: {info}"))
        .collect())
}

/// Macro to generate a snapshot test of a zip file's contents
#[macro_export]
macro_rules! assert_zip_snapshot {
    // Default case - no redaction
    ($name:expr, $file:expr) => {
        let contents = create_zip_snapshot($file, |_| false)?;
        insta::assert_snapshot!($name, contents.join("\n"));
    };
    // Case with redaction function
    ($name:expr, $file:expr, $redact:expr) => {
        let contents = create_zip_snapshot($file, $redact)?;
        insta::assert_snapshot!($name, contents.join("\n"));
    };
}
