use pcb_eda::Symbol;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub fn setup_symbol(name: &str) -> Symbol {
    let temp_dir = setup_test_env();
    // Find the kicad symbol file by searching in the directory structure
    let temp_path = temp_dir.keep();
    let kicad_dir = temp_path.join("kicad");
    let name_dir = kicad_dir.join(name);

    // Look for any .kicad_sym file in the component directory
    let lib_path = fs::read_dir(&name_dir)
        .expect("Failed to read component directory")
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "kicad_sym")
        })
        .map(|entry| entry.path())
        .unwrap_or_else(|| panic!("Could not find .kicad_sym file for {name}"));

    Symbol::from_file(&lib_path).unwrap()
}

pub fn setup_test_env() -> TempDir {
    let _ = env_logger::builder().is_test(true).try_init();

    let resources_dir = if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(manifest_dir).join("tests/resources")
    } else {
        // Try relative path first
        let mut path = PathBuf::from("tests/resources");
        if !path.exists() {
            path = PathBuf::from("projects/cli/crates/diode_eda/tests/resources");
        }
        path
    };

    // Create a temporary directory
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Copy the project files to the temporary directory
    copy_dir_all(&resources_dir, temp_dir.path())
        .unwrap_or_else(|e| panic!("Failed to copy project files from {resources_dir:?}: {e}"));

    temp_dir
}

// Helper function to recursively copy directories
pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        // To avoid dealing with nested git repositories, we disguise these git repos by renaming
        // the .git folder to _git, and rename it back when we set up the test environment.
        let file_name = entry.file_name();
        let destination = if file_name == "_git" {
            dst.as_ref().join(".git")
        } else {
            dst.as_ref().join(&file_name)
        };

        if ty.is_dir() {
            copy_dir_all(entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}
