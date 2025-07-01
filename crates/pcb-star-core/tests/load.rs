#[macro_use]
mod common;

snapshot_eval!(nonexistent_file, {
    "test.star" => r#"
        # This load should fail and the error should point to this line
        load("nonexistent.star", "foo")
    "#
});

snapshot_eval!(file_with_syntax_error, {
    "broken.star" => r#"
        # This file has a syntax error
        def broken_function(
            # Missing closing parenthesis
    "#,
    "test.star" => r#"
        # Loading a file with syntax errors should show error at this load statement
        load("broken.star", "broken_function")
    "#
});

snapshot_eval!(directory_with_errors, {
    "modules/GoodModule.star" => r#"
        def hello():
            return "Hello from GoodModule"
    "#,
    "modules/BadModule.star" => r#"
        # This module has an error - trying to load a non-existent file
        load("does_not_exist.star", "something")

        def world():
            return "World"
    "#,
    "modules/SyntaxError.star" => r#"
        # This module has a syntax error
        def broken(
            # Missing closing parenthesis
    "#,
    "test.star" => r#"
        # Loading a directory with problematic modules should show errors
        load("modules", "GoodModule", "BadModule", "SyntaxError")

        # Try to use the good module - this should work
        GoodModule.hello()

        # These shouldn't work
        # BadModule.world()
        # SyntaxError.broken()
    "#
});

snapshot_eval!(nested_load_errors, {
    "level3.star" => r#"
        # This file has an actual error
        undefined_variable + 1
    "#,
    "level2.star" => r#"
        # This loads a file with an error
        load("level3.star", "something")
    "#,
    "level1.star" => r#"
        # This loads a file that loads a file with an error
        load("level2.star", "something")
    "#,
    "test.star" => r#"
        # Top level load - error should propagate up with proper spans
        load("level1.star", "something")
    "#
});

snapshot_eval!(cyclic_load_error, {
    "a.star" => r#"
        # This creates a cycle: a -> b -> a
        load("b.star", "b_func")

        def a_func():
            return "a"
    "#,
    "b.star" => r#"
        # This completes the cycle
        load("a.star", "a_func")

        def b_func():
            return "b"
    "#
});

snapshot_eval!(load_directory_mixed_symbols, {
    "modules/Working.star" => r#"
        def working_function():
            return "This module works fine"
    "#,
    "modules/Broken.star" => r#"
        # This module has a runtime error
        undefined_variable + 1

        def broken_function():
            return "This won't be reached"
    "#,
    "modules/AlsoWorking.star" => r#"
        def also_working():
            return "This also works"
    "#,
    "test.star" => r#"
        # Loading multiple symbols from a directory - only Broken should show an error
        load("modules", "Working", "Broken", "AlsoWorking")

        # These should work
        Working.working_function()
        AlsoWorking.also_working()

        # This would fail if we tried to use it
        # Broken.broken_function()
    "#
});

snapshot_eval!(module_loader_attrs, {
    "Module.star" => r#"
        TestExport = "test"
    "#,
    "top.star" => r#"
        load(".", "Module")

        check(Module.TestExport == "test", "TestExport should be 'test'")
    "#
});
