// Error case – evaluating `error()` should propagate the message.
snapshot_eval!(error_function_should_error, {
    "test.zen" => r#"
        error("boom")
    "#
});

// `check()` with a false condition should raise and surface the message.
snapshot_eval!(check_false_should_error, {
    "test.zen" => r#"
        # check should raise when condition is false
        check(False, "failing condition")
    "#
});

// `warn()` with multiple calls should emit multiple warnings and continue execution.
snapshot_eval!(warn_function_multiple_warnings, {
    "test.zen" => r#"
        warn("first warning")
        warn("second warning")
    "#
});
