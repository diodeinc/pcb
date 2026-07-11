#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use std::path::PathBuf;
use std::process::Command;

const BOARD_NO_LAYOUT_ZEN: &str = r#"
p1 = Net("P1")
"#;

/// A .zen file that declares a layout path but whose board files have not
/// been generated yet. resolve_board() will fail with a "run pcb layout"
/// error.
const BOARD_WITH_LAYOUT_ZEN: &str = r#"
LoadCap = Module("@stdlib/generics/Capacitor.zen")
vcc = Net("VCC")
gnd = Net("GND")
LoadCap(name = "C1", value = "100nF", package = "0402", P1 = vcc, P2 = gnd)
Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)
"#;

/// Helper: create a minimal KiCad layout directory (kicad_pro + kicad_pcb)
/// inside the sandbox so that resolve_board() passes and routing can proceed
/// until the Java/JAR prerequisite check.
fn scaffold_layout(sandbox: &mut Sandbox) {
    sandbox.write(
        "build/TestBoard/test.kicad_pro",
        "(kicad_pro (version 20231010))\n",
    );
    sandbox.write(
        "build/TestBoard/test.kicad_pcb",
        "(kicad_pcb (version 20231010))\n",
    );
}

// ---------------------------------------------------------------------------
// Error-path tests
// ---------------------------------------------------------------------------

#[test]
fn test_route_missing_file() {
    let output = Sandbox::new()
        .with_workspace()
        .snapshot_run("pcbc", ["route", "nonexistent.zen"]);
    assert_snapshot!("missing_file", output);
}

#[test]
fn test_route_local_no_layout() {
    let output = Sandbox::new()
        .with_workspace()
        .write("board.zen", BOARD_NO_LAYOUT_ZEN)
        .snapshot_run("pcbc", ["route", "board.zen"]);
    assert_snapshot!("local_no_layout", output);
}

#[test]
fn test_route_local_java_not_found() {
    // Sandbox passes through the host PATH, but we can simulate a missing
    // Java by pointing to an empty directory first in PATH. The simplest
    // approach is to rely on the sandbox env — if Java is not on the host,
    // this test naturally covers the error.
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let output = sandbox.snapshot_run("pcbc", ["route", "board.zen"]);
    assert_snapshot!("local_java_not_found", output);
}

#[test]
fn test_route_local_jar_not_found() {
    // Same as java_not_found — on hosts with Java, this will hit the
    // JAR lookup instead.
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let output = sandbox.snapshot_run("pcbc", ["route", "board.zen"]);
    assert_snapshot!("local_jar_not_found", output);
}

#[test]
fn test_route_local_bad_jar_path() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let output = sandbox.snapshot_run(
        "pcbc",
        [
            "route",
            "--fr-jar",
            "/nonexistent/freerouting.jar",
            "board.zen",
        ],
    );
    assert_snapshot!("local_bad_jar_path", output);
}

#[test]
fn test_route_cloud_timeout_exceeded() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let output = sandbox.snapshot_run("pcbc", ["route", "--remote", "-t", "999", "board.zen"]);
    assert_snapshot!("cloud_timeout_exceeded", output);
}

// ---------------------------------------------------------------------------
// Integration tests — need Java + FreeRouting JAR on the host
// ---------------------------------------------------------------------------

/// Resolve the FreeRouting JAR path for integration tests.
///
/// Priority:
/// 1. `FREEROUTING_TEST_JAR` env var (test-specific override)
/// 2. `FREEROUTING_JAR` env var
/// 3. Cached download (`~/.cache/pcb/test-cache/freerouting-cli.jar`)
/// 4. Download to cache from GitHub releases
///
/// Returns `None` (and prints a diagnostic) so the calling test can skip.
fn resolve_freerouting_jar() -> Option<PathBuf> {
    for var in &["FREEROUTING_TEST_JAR", "FREEROUTING_JAR"] {
        if let Ok(path) = std::env::var(var) {
            let p = PathBuf::from(&path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pcb")
        .join("test-cache");
    let cached = cache_dir.join("freerouting-2.0.1.jar");

    if cached.exists() {
        return Some(cached);
    }

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        eprintln!("[route test] Skipping: failed to create cache dir: {e}");
        return None;
    }

    let urls = [
        "https://github.com/freerouting/freerouting/releases/download/v2.0.1/freerouting-2.0.1.jar",
    ];

    for url in &urls {
        eprintln!("[route test] Downloading FreeRouting JAR from {url} ...");
        if let Ok(status) = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&cached)
            .arg(url)
            .status()
        {
            if status.success()
                && cached.exists()
                && cached.metadata().map(|m| m.len() > 0).unwrap_or(false)
            {
                eprintln!("[route test] Downloaded to {}", cached.display());
                return Some(cached);
            }
        }
    }

    eprintln!(
        "[route test] Skipping: could not download FreeRouting JAR. \
         Set FREEROUTING_TEST_JAR to a pre-downloaded jar."
    );
    None
}

fn java_compatible() -> bool {
    let output = match Command::new("java").arg("-version").output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    // Parse major version from stderr (java -version writes to stderr)
    let stderr = String::from_utf8_lossy(&output.stderr);
    let major = stderr
        .lines()
        .find(|l| l.contains("version"))
        .and_then(|l| l.split('"').nth(1))
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    major >= 21
}

#[test]
fn test_route_local_integration_download_jar() {
    if !java_compatible() {
        eprintln!("[route test] Skipping: Java 21+ not available");
        return;
    }

    let jar_path = match resolve_freerouting_jar() {
        Some(j) => j,
        None => return,
    };

    let output = Command::new("java")
        .args(["-jar", &jar_path.to_string_lossy(), "--help"])
        .output()
        .expect("failed to run FreeRouting");

    assert!(
        output.status.success(),
        "FreeRouting --help failed:\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let help_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        help_text.contains("-de") || help_text.contains("Usage"),
        "FreeRouting --help does not look like the CLI tool:\n{help_text}"
    );
}

#[test]
fn test_route_local_integration_freerouting_cli() {
    // Full integration test: requires Java, FreeRouting JAR, and KiCad
    // (for DSN export and SES import).
    if !java_compatible() {
        eprintln!("[route test] Skipping: Java 21+ not available");
        return;
    }

    let jar_path = match resolve_freerouting_jar() {
        Some(j) => j,
        None => return,
    };

    // KiCad is needed for DSN export and SES import
    let kicad_missing = Command::new("kicad-cli")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);

    if kicad_missing {
        eprintln!("[route test] Skipping full integration: KiCad not installed");
        eprintln!("  (JAR downloaded to {})", jar_path.display());
        return;
    }

    // Build a board then route it locally
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);

    let build_output = sandbox
        .run("pcbc", ["layout", "--no-open", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("layout command failed");

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        if stderr.contains("Python") || stderr.contains("kicad") || stderr.contains("KiCad") {
            eprintln!("[route test] Skipping: KiCad Python not available");
            return;
        }
        panic!(
            "layout generation failed:\nstdout:{}\nstderr:{}",
            String::from_utf8_lossy(&build_output.stdout),
            stderr,
        );
    }

    let fr_jar = jar_path.to_string_lossy().to_string();
    let route_output = sandbox
        .run(
            "pcbc",
            [
                "route",
                "--fr-jar",
                &fr_jar,
                "--fr-timeout",
                "60",
                "--keep",
                "board.zen",
            ],
        )
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("route command failed");

    let output_str = sandbox.sanitize_output(&format!(
        "--- route ---\n{}{}",
        String::from_utf8_lossy(&route_output.stdout),
        String::from_utf8_lossy(&route_output.stderr),
    ));

    assert_snapshot!("route_local_integration", output_str);
}
