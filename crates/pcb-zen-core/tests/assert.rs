#[macro_use]
mod common;

// Error case – evaluating `error()` should propagate the message.
snapshot_eval!(error_function_should_error, {
    "test.zen" => r#"
        error("boom")
    "#
});

// `check()` with a true condition should pass and produce a schematic/netlist.
snapshot_eval!(check_true_should_pass, {
    "test.zen" => r#"
        # check should not raise when condition is true
        check(True, "all good")
    "#
});

// `check()` with a false condition should raise and surface the message.
snapshot_eval!(check_false_should_error, {
    "test.zen" => r#"
        # check should raise when condition is false
        check(False, "failing condition")
    "#
});

// `warn()` should emit a warning and continue execution.
snapshot_eval!(warn_function_should_warn, {
    "test.zen" => r#"
        warn("this is a warning")
    "#
});

// `warn()` with multiple calls should emit multiple warnings and continue execution.
snapshot_eval!(warn_function_multiple_warnings, {
    "test.zen" => r#"
        warn("first warning")
        warn("second warning")
    "#
});

// `warn()` should not stop execution - code after warn() should still run.
snapshot_eval!(warn_function_continues_execution, {
    "test.zen" => r#"
        warn("warning message")
        x = 42
    "#
});
