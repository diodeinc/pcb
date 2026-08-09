use anyhow::Result;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// Gets the path to test resources
pub fn get_resource_path(resource_name: &str) -> PathBuf {
    PathBuf::from(format!("tests/resources/{resource_name}"))
}

macro_rules! assert_test_snapshot {
    ($name:expr, $content:expr) => {{
        let module = module_path!()
            .rsplit("::")
            .next()
            .expect("snapshot assertion must have a module");
        let name = format!("{module}__{}", $name);
        insta::with_settings!({ prepend_module_to_snapshot => false }, {
            insta::assert_snapshot!(name, $content);
        });
    }};
}
pub(crate) use assert_test_snapshot;

/// Creates a normalized snapshot of a log file's contents.
///
/// Normalizes non-deterministic content for stable snapshots:
/// - Timing values (e.g., "0.123 seconds" → "X.XXX seconds", "0.123s" → "X.XXXs")
/// - Temp directory paths (matches pcb-test-utils/sandbox.rs patterns)
/// - Time-of-day stamps from KiCad (e.g., "08:07:31 PM:")
/// - KiCad build paths
pub fn create_log_snapshot<P: AsRef<Path>>(file_path: P) -> Result<String> {
    let content = fs::read_to_string(file_path)?;

    // Normalize timing values like "0.123 seconds" or "0.123s"
    let timing_re = Regex::new(r"\d+\.\d+ ?seconds?").unwrap();
    let content = timing_re.replace_all(&content, "X.XXX seconds");

    let timing_short_re = Regex::new(r"\d+\.\d+s\b").unwrap();
    let content = timing_short_re.replace_all(&content, "X.XXXs");

    // Replace temp directory paths with a placeholder (same patterns as pcb-test-utils/sandbox.rs)
    // macOS: /private/var/folders/XX/YY/T/.tmpZZZ or /var/folders/XX/YY/T/.tmpZZZ
    let macos_pattern = Regex::new(r"(?:/private)?/var/folders/[^/]+/[^/]+/T/[^\s]+").unwrap();
    let content = macos_pattern.replace_all(&content, "<TEMP_DIR>");

    // Linux: /tmp/.tmpXXX or /tmp/pcb-layout-XXX
    let linux_pattern = Regex::new(r"/tmp/[^\s]+").unwrap();
    let content = linux_pattern.replace_all(&content, "<TEMP_DIR>");

    // Remove KiCad time-of-day debug lines (e.g., "08:07:31 PM: Debug: Adding duplicate image handler...")
    // These are non-deterministic and not useful for testing
    let kicad_time_debug_re =
        Regex::new(r"(?m)^\d{2}:\d{2}:\d{2} [AP]M: Debug:.*(?:\n|$)").unwrap();
    let content = kicad_time_debug_re.replace_all(&content, "");

    // Remove KiCad/wxWidgets internal assert/warning lines (paths vary by installation and OS)
    // Matches patterns like:
    // - /Users/kicad/remoteroot/workspace/.../file.cpp(123): assert ...
    // - /home/runner/work/.../file.cpp(123): assert ...
    // - ./src/common/stdpbase.cpp(59): assert ...
    let cpp_assert_re = Regex::new(r"(?m)^[^\n]*\.cpp\(\d+\): assert.*(?:\n|$)").unwrap();
    let content = cpp_assert_re.replace_all(&content, "");

    Ok(content.to_string())
}

/// Macro to generate a snapshot test of a log file's contents (normalized)
macro_rules! assert_log_snapshot {
    ($name:expr, $file:expr) => {
        let content = create_log_snapshot($file)?;
        assert_test_snapshot!($name, content);
    };
}
pub(crate) use assert_log_snapshot;
