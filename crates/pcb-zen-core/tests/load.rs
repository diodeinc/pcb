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

snapshot_eval!(directory_with_errors, {
    "modules/GoodModule.zen" => r#"
        def hello():
            return "Hello from GoodModule"
    "#,
    "modules/BadModule.zen" => r#"
        # This module has an error - trying to load a non-existent file
        load("does_not_exist.zen", "something")

        def world():
            return "World"
    "#,
    "modules/SyntaxError.zen" => r#"
        # This module has a syntax error
        def broken(
            # Missing closing parenthesis
    "#,
    "test.zen" => r#"
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

snapshot_eval!(load_directory_mixed_symbols, {
    "modules/Working.zen" => r#"
        def working_function():
            return "This module works fine"
    "#,
    "modules/Broken.zen" => r#"
        # This module has a runtime error
        undefined_variable + 1

        def broken_function():
            return "This won't be reached"
    "#,
    "modules/AlsoWorking.zen" => r#"
        def also_working():
            return "This also works"
    "#,
    "test.zen" => r#"
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
    "Module.zen" => r#"
        TestExport = "test"
    "#,
    "top.zen" => r#"
        load(".", "Module")

        check(Module.TestExport == "test", "TestExport should be 'test'")
    "#
});
