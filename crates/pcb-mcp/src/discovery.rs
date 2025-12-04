use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

/// Find all pcb-* executables on PATH, excluding `pcb` itself
pub fn find_pcb_binaries() -> Vec<String> {
    let path_var = env::var_os("PATH").unwrap_or_default();
    let mut seen = HashSet::new();
    let mut binaries = Vec::new();

    for dir in env::split_paths(&path_var) {
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

                    // Skip duplicates (first one in PATH wins)
                    if seen.insert(name.to_string()) {
                        binaries.push(name.to_string());
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
        // Should not contain "pcb" itself
        assert!(!binaries.contains(&"pcb".to_string()));
        // All results should start with "pcb-"
        for bin in &binaries {
            assert!(bin.starts_with("pcb-"), "Found non-pcb binary: {}", bin);
        }
    }
}
