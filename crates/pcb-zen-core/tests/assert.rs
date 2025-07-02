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
