#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

#[test]
fn test_electrical_check_error_severity() {
    let output = Sandbox::new()
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
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("error_severity", output);
}

#[test]
fn test_electrical_check_warning_severity() {
    let output = Sandbox::new()
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
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("warning_severity", output);
}

#[test]
fn test_electrical_check_passing() {
    let output = Sandbox::new()
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
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("passing_check", output);
}

#[test]
fn test_electrical_check_invalid_severity() {
    let output = Sandbox::new()
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
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("invalid_severity", output);
}

#[test]
fn test_electrical_check_with_inputs() {
    let output = Sandbox::new()
        .write(
            "board.zen",
            r#"
def check_range(module, min_val, max_val, name):
    actual = 150
    if actual < min_val or actual > max_val:
        error("{} out of range {}-{}: got {}".format(name, min_val, max_val, actual))

builtin.add_electrical_check(
    name="range_check",
    check_fn=check_range,
    inputs={"min_val": 0, "max_val": 100, "name": "voltage"},
    severity="warning",
)
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("with_inputs", output);
}
