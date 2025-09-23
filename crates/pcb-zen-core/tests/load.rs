#[macro_use]
mod common;

snapshot_eval!(nonexistent_file, {
    "test.zen" => r#"
        # This load should fail and the error should point to this line
        load("nonexistent.zen", "foo")
    "#
});

snapshot_eval!(file_with_syntax_error, {
    "broken.zen" => r#"
        # This file has a syntax error
        def broken_function(
            # Missing closing parenthesis
    "#,
    "test.zen" => r#"
        # Loading a file with syntax errors should show error at this load statement
        load("broken.zen", "broken_function")
    "#
});

snapshot_eval!(nested_load_errors, {
    "level3.zen" => r#"
        # This file has an actual error
        undefined_variable + 1
    "#,
    "level2.zen" => r#"
        # This loads a file with an error
        load("level3.zen", "something")
    "#,
    "level1.zen" => r#"
        # This loads a file that loads a file with an error
        load("level2.zen", "something")
    "#,
    "test.zen" => r#"
        # Top level load - error should propagate up with proper spans
        load("level1.zen", "something")
    "#
});

snapshot_eval!(cyclic_load_error, {
    "a.zen" => r#"
        # This creates a cycle: a -> b -> a
        load("b.zen", "b_func")

        def a_func():
            return "a"
    "#,
    "b.zen" => r#"
        # This completes the cycle
        load("a.zen", "a_func")

        def b_func():
            return "b"
    "#
});

snapshot_eval!(module_loader_attrs, {
    "Module.zen" => r#"
        TestExport = "test"
    "#,
    "top.zen" => r#"
        MyModule = Module("Module.zen")

        check(MyModule.TestExport == "test", "TestExport should be 'test'")
    "#
});
