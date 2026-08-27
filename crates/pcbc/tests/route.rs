use pcb_test_utils::sandbox::Sandbox;
use std::path::PathBuf;
use std::process::Command;

const BOARD_NO_LAYOUT_ZEN: &str = r#"
p1 = Net("P1")
"#;

const BOARD_WITH_LAYOUT_ZEN: &str = r#"
LoadCap = Module("@stdlib/generics/Capacitor.zen")
vcc = Net("VCC")
gnd = Net("GND")
LoadCap(name = "C1", value = "100nF", package = "0402", P1 = vcc, P2 = gnd)
Layout(name="TestBoard", path="build/TestBoard")
"#;

/// Unlike `BOARD_WITH_LAYOUT_ZEN`, has nets that actually need a routed
/// trace, so FreeRouting produces a real output object.
#[cfg(not(target_os = "windows"))]
const BOARD_WITH_ROUTABLE_NETS_ZEN: &str = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")

n1 = Net("N1")
n2 = Net("N2")
n3 = Net("N3")

Resistor(name = "R1", value = "1k", package = "0402", P1 = n1, P2 = n2)
Resistor(name = "R2", value = "1k", package = "0402", P1 = n2, P2 = n3)

Layout(name="TestBoard", path="build/TestBoard")
"#;

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

/// Run `pcbc route --engine freerouting <extra_args...> board.zen` in
/// `sandbox` and return (exit_code, stdout, stderr).
fn run_route_freerouting(sandbox: &mut Sandbox, extra_args: &[&str]) -> (i32, String, String) {
    let mut args = vec!["route", "--engine", "freerouting"];
    args.extend_from_slice(extra_args);
    args.push("board.zen");

    let output = sandbox
        .run("pcbc", args)
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("failed to run pcbc route --engine freerouting");

    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// ---------------------------------------------------------------------------
// Error-path tests
// ---------------------------------------------------------------------------

#[test]
fn test_freerouting_no_layout() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_NO_LAYOUT_ZEN);
    let (code, stdout, stderr) = run_route_freerouting(&mut sandbox, &[]);
    assert_ne!(code, 0, "expected failure with no layout defined");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("No layout path defined") || combined.contains("layout"),
        "expected a layout-related error, got:\n{combined}"
    );
}

#[test]
fn test_freerouting_java_not_found() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.env("PATH", "");
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let (code, stdout, stderr) = run_route_freerouting(&mut sandbox, &[]);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("Java 25+ not found"),
        "expected Java-not-found error, got:\n{combined}"
    );
    assert_ne!(code, 0);
}

#[test]
fn test_freerouting_bad_jar_path() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    sandbox.env("FREEROUTING_JAR", "/nonexistent/freerouting.jar");
    let (code, stdout, stderr) = run_route_freerouting(&mut sandbox, &[]);
    assert_ne!(
        code, 0,
        "expected failure for a nonexistent FREEROUTING_JAR path"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("FreeRouting JAR not found"),
        "expected a FreeRouting JAR error, got:\n{combined}"
    );
}

// ---------------------------------------------------------------------------
// Integration tests — need Java + FreeRouting JAR on the host. `#[ignore]`d
// so a plain `cargo test -p pcbc` never downloads or runs an external JAR;
// run explicitly with `--ignored` and prerequisites must be present, or the
// test fails loudly rather than silently passing.
// ---------------------------------------------------------------------------

/// Resolve the FreeRouting JAR path from `FREEROUTING_TEST_JAR`. Panics if
/// it isn't set to an existing path, rather than silently skipping — these
/// tests are only run explicitly.
#[cfg(not(target_os = "windows"))]
fn resolve_freerouting_jar() -> PathBuf {
    let path = std::env::var("FREEROUTING_TEST_JAR").unwrap_or_else(|_| {
        panic!(
            "FREEROUTING_TEST_JAR must be set to a FreeRouting jar to run this ignored integration test"
        )
    });
    let p = PathBuf::from(&path);
    assert!(p.exists(), "FREEROUTING_TEST_JAR does not exist: {path}");
    p
}

#[cfg(not(target_os = "windows"))]
fn java_compatible() -> bool {
    let output = match Command::new("java").arg("-version").output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    // java -version writes to stderr, not stdout
    let stderr = String::from_utf8_lossy(&output.stderr);
    let major = stderr
        .lines()
        .find(|l| l.contains("version"))
        .and_then(|l| l.split('"').nth(1))
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    major >= 25
}

#[test]
#[ignore]
#[cfg(not(target_os = "windows"))]
fn test_freerouting_cli() {
    assert!(
        java_compatible(),
        "Java 25+ is required to run this ignored integration test"
    );

    let jar_path = resolve_freerouting_jar();

    let kicad_missing = Command::new("kicad-cli")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);
    assert!(
        !kicad_missing,
        "kicad-cli is required to run this ignored integration test"
    );

    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_ROUTABLE_NETS_ZEN);

    let build_output = sandbox
        .run("pcbc", ["layout", "--no-open", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("layout command failed");

    assert!(
        build_output.status.success(),
        "layout generation failed (KiCad Python not available?):\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&build_output.stdout),
        String::from_utf8_lossy(&build_output.stderr)
    );

    sandbox.env("FREEROUTING_JAR", jar_path.to_string_lossy().to_string());
    let (code, stdout, stderr) =
        run_route_freerouting(&mut sandbox, &["--no-open", "--timeout", "1"]);

    assert!(
        code == 0,
        "route --engine freerouting failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

    // Only assert on unconnected items, not overall DRC: this fixture has no
    // board outline, which trips an unrelated `invalid_outline` violation.
    let board_path = sandbox
        .default_cwd()
        .join("build/TestBoard/layout.kicad_pcb");
    let drc_report_path = sandbox.default_cwd().join("layout-drc.rpt");
    let drc_output = Command::new("kicad-cli")
        .args(["pcb", "drc", "-o"])
        .arg(&drc_report_path)
        .arg(&board_path)
        .output()
        .expect("failed to run kicad-cli pcb drc");
    let drc_stdout = String::from_utf8_lossy(&drc_output.stdout);

    assert!(
        drc_stdout.contains("Found 0 unconnected items"),
        "expected no unconnected items after routing:\nstdout:{}\nstderr:{}",
        drc_stdout,
        String::from_utf8_lossy(&drc_output.stderr)
    );
}
