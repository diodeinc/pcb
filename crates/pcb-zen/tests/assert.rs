mod common;
use common::TestProject;

/// Error case – evaluating `error()` should propagate the message.
#[test]
fn snapshot_error_function_should_error() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- test.zen
error("boom")
"#,
    );

    star_snapshot!(env, "test.zen");
}

/// `check()` with a true condition should pass and produce a schematic/netlist.
#[test]
fn snapshot_check_true_should_pass() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- test.zen
# check should not raise when condition is true
check(True, "all good")

# Dummy component so schematic isn't empty
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {"P": "1"},
    pins = {"P": Net("")},
)
"#,
    );

    // Should evaluate successfully and produce a netlist.
    star_snapshot!(env, "test.zen");
}

/// `check()` with a false condition should raise and surface the message.
#[test]
fn snapshot_check_false_should_error() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- test.zen
# check should raise when condition is false
check(False, "failing condition")
"#,
    );

    star_snapshot!(env, "test.zen");
}
