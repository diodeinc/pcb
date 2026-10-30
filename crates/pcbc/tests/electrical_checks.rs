#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

#[test]
fn test_electrical_check_error_severity() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
def check_critical(module):
    error("Critical check failed")

builtin.add_electrical_check(
    name="critical",
    check_fn=check_critical,
)
"#,
        )
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("error_severity", output);
}

#[test]
fn test_electrical_check_warning_severity() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
def check_recommended(module, min_value):
    if min_value > 5:
        error("Recommended value should be at least {}".format(min_value))

builtin.add_electrical_check(
    name="recommended",
    check_fn=check_recommended,
    inputs={"min_value": 10},
    severity="warning",
)
"#,
        )
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("warning_severity", output);
}

#[test]
fn test_electrical_check_passing() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
def check_passes(module):
    # No error means check passes
    pass

builtin.add_electrical_check(
    name="passing_check",
    check_fn=check_passes,
)
"#,
        )
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("passing_check", output);
}

#[test]
fn test_electrical_check_invalid_severity() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
def check_fn(module):
    pass

builtin.add_electrical_check(
    name="test",
    check_fn=check_fn,
    severity="invalid",
)
"#,
        )
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("invalid_severity", output);
}

#[test]
fn test_diagnostics_json_uses_effective_electrical_check_severity() {
    for severity in ["advice", "warning", "error"] {
        let mut sandbox = Sandbox::new().with_workspace();
        sandbox.write(
            "board.zen",
            format!(
                r#"
def check_rating(module):
    error("Rating is outside the recommended range")

builtin.add_electrical_check(
    name="rating",
    check_fn=check_rating,
    severity="{severity}",
)
"#
            ),
        );
        let output = sandbox
            .run(
                "pcbc",
                ["build", "board.zen", "--diagnostics", "diagnostics.json"],
            )
            .unchecked()
            .run()
            .expect("build should run");
        assert_eq!(output.status.success(), severity != "error");

        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(sandbox.root_path().join("diagnostics.json")).unwrap(),
        )
        .unwrap();
        let diagnostics = report["board.zen"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["severity"], severity);
        assert_eq!(
            diagnostics[0]["body"],
            "Rating is outside the recommended range"
        );
        assert_eq!(
            diagnostics[0]["stack"][0]["message"],
            "Electrical check 'rating' failed"
        );
    }
}
