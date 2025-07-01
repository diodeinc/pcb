mod common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_nonexistent_file() {
    let env = TestProject::new();

    env.add_file(
        "test.star",
        r#"
# This load should fail and the error should point to this line
load("./nonexistent.star", "foo")

print("This shouldn't execute")
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn snapshot_load_file_with_syntax_error() {
    let env = TestProject::new();

    env.add_file(
        "broken.star",
        r#"
# This file has a syntax error
def broken_function(
    # Missing closing parenthesis
"#,
    );

    env.add_file(
        "test.star",
        r#"
# Loading a file with syntax errors should show error at this load statement
load("./broken.star", "broken_function")

print("This shouldn't execute")
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_directory_with_errors() {
    let env = TestProject::new();

    // Create a directory with some good and bad modules
    env.add_file(
        "modules/GoodModule.star",
        r#"
def hello():
    return "Hello from GoodModule"
"#,
    );

    env.add_file(
        "modules/BadModule.star",
        r#"
# This module has an error - trying to load a non-existent file
load("./does_not_exist.star", "something")

def world():
    return "World"
"#,
    );

    env.add_file(
        "modules/SyntaxError.star",
        r#"
# This module has a syntax error
def broken(
    # Missing closing parenthesis
"#,
    );

    env.add_file(
        "test.star",
        r#"
# Loading a directory with problematic modules should show errors
load("./modules", "GoodModule", "BadModule", "SyntaxError")

# Try to use the good module - this should work
GoodModule.hello()

# These shouldn't work
# BadModule.world()
# SyntaxError.broken()
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn snapshot_nested_load_errors() {
    let env = TestProject::new();

    env.add_file(
        "level3.star",
        r#"
# This file has an actual error
undefined_variable + 1
"#,
    );

    env.add_file(
        "level2.star",
        r#"
# This loads a file with an error
load("./level3.star", "something")
"#,
    );

    env.add_file(
        "level1.star",
        r#"
# This loads a file that loads a file with an error
load("./level2.star", "something")
"#,
    );

    env.add_file(
        "test.star",
        r#"
# Top level load - error should propagate up with proper spans
load("./level1.star", "something")
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn snapshot_cyclic_load_error() {
    let env = TestProject::new();

    env.add_file(
        "a.star",
        r#"
# This creates a cycle: a -> b -> a
load("./b.star", "b_func")

def a_func():
    return "a"
"#,
    );

    env.add_file(
        "b.star",
        r#"
# This completes the cycle
load("./a.star", "a_func")

def b_func():
    return "b"
"#,
    );

    star_snapshot!(env, "a.star");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_directory_mixed_symbols() {
    let env = TestProject::new();

    // Create a directory with some good and bad modules
    env.add_file(
        "modules/Working.star",
        r#"
def working_function():
    return "This module works fine"
"#,
    );

    env.add_file(
        "modules/Broken.star",
        r#"
# This module has a runtime error
undefined_variable + 1

def broken_function():
    return "This won't be reached"
"#,
    );

    env.add_file(
        "modules/AlsoWorking.star",
        r#"
def also_working():
    return "This also works"
"#,
    );

    env.add_file(
        "test.star",
        r#"
# Loading multiple symbols from a directory - only Broken should show an error
load("./modules", "Working", "Broken", "AlsoWorking")

# These should work
Working.working_function()
AlsoWorking.also_working()

# This would fail if we tried to use it
# Broken.broken_function()
"#,
    );

    star_snapshot!(env, "test.star");
}
