use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Find all pcb-* executables, checking both the directory containing the
/// current executable and directories on PATH.
///
/// Returns full absolute paths to the binaries, which allows spawning them
/// even when PATH doesn't include their directory (common when spawned by IDEs).
pub fn find_pcb_binaries() -> Vec<String> {
    let mut seen = HashSet::new();
    let mut binaries = Vec::new();

    // Collect directories to search: current exe's dir first, then PATH
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // Add directory containing the current executable (highest priority)
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            search_dirs.push(exe_dir.to_path_buf());
        }
    }

    // Add PATH directories
    let path_var = env::var_os("PATH").unwrap_or_default();
    search_dirs.extend(env::split_paths(&path_var));

    for dir in search_dirs {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Skip if not pcb-* pattern or is just "pcb"
                    if !name.starts_with("pcb-") {
                        continue;
                    }

                    // Skip if not executable
                    if !is_executable(&path) {
                        continue;
                    }

                    // Skip duplicates by name (first one wins)
                    if seen.insert(name.to_string()) {
                        // Return full path so we can spawn without relying on PATH
                        binaries.push(path.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }

    binaries
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    // On Windows, check for common executable extensions
    path.is_file()
        && path
            .extension()
            .map(|ext| {
                let ext = ext.to_string_lossy().to_lowercase();
                ext == "exe" || ext == "cmd" || ext == "bat"
            })
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_pcb_binaries() {
        // This test just ensures the function runs without panicking
        // Actual binaries found will depend on the system
        let binaries = find_pcb_binaries();
        // Should not contain "pcb" itself (check filename, not full path)
        for bin in &binaries {
            let filename = Path::new(bin)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(bin);
            assert_ne!(filename, "pcb", "Should not include 'pcb' itself");
            assert!(
                filename.starts_with("pcb-"),
                "Found non-pcb binary: {}",
                bin
            );
        }
    }
}
