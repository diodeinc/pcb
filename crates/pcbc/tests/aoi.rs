#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;

const PCB_TOML_MIN: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
vcc = Net("VCC")
gnd = Net("GND")
"#;

#[test]
fn aoi_help_describes_inspection() {
    let output = Sandbox::new()
        .run("pcbc", ["aoi", "--help"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();

    assert!(output.status.success(), "`aoi --help` should exit cleanly");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Automated Optical Inspection"),
        "help should describe AOI, got:\n{stdout}"
    );
    assert!(stdout.contains("--image"), "help should document --image");
    assert!(
        stdout.contains("--threshold"),
        "help should document --threshold"
    );
}

#[test]
fn aoi_command_is_recognized() {
    let output = Sandbox::new()
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/Board.zen", SIMPLE_BOARD_ZEN)
        .write("photo.png", "not-a-real-image")
        .run("pcbc", ["aoi", "boards/Board.zen", "--image", "photo.png"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Unknown command"),
        "`aoi` must be a recognized subcommand, got stderr:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "`aoi` scaffold should exit gracefully, stderr:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not implemented"),
        "report should flag that inspection did not run, got stdout:\n{stdout}"
    );
    // Progress/notice text goes to stderr, keeping stdout report-only.
    assert!(
        stderr.contains("not yet implemented"),
        "pending-diff notice should be on stderr, got stderr:\n{stderr}"
    );
}

#[test]
fn aoi_output_dash_emits_pure_json_to_stdout() {
    let output = Sandbox::new()
        .write("pcb.toml", PCB_TOML_MIN)
        .write("boards/Board.zen", SIMPLE_BOARD_ZEN)
        .write("photo.png", "not-a-real-image")
        .run(
            "pcbc",
            [
                "aoi",
                "boards/Board.zen",
                "--image",
                "photo.png",
                "--output",
                "-",
            ],
        )
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // stdout must be the JSON report only; progress notes are on stderr.
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not pure JSON ({e}):\n{stdout}"));
    assert_eq!(value["status"], "not_implemented");
    assert_eq!(value["findings"].as_array().map(Vec::len), Some(0));
}
