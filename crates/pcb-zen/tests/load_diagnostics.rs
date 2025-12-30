mod common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_nonexistent_file() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# This load should fail and the error should point to this line
load("./nonexistent.zen", "foo")

print("This shouldn't execute")
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn snapshot_load_file_with_syntax_error() {
    let env = TestProject::new();

    env.add_file(
        "broken.zen",
        r#"
# This file has a syntax error
def broken_function(
    # Missing closing parenthesis
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Loading a file with syntax errors should show error at this load statement
load("./broken.zen", "broken_function")

print("This shouldn't execute")
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn snapshot_nested_load_errors() {
    let env = TestProject::new();

    env.add_file(
        "level3.zen",
        r#"
# This file has an actual error
undefined_variable + 1
"#,
    );

    env.add_file(
        "level2.zen",
        r#"
# This loads a file with an error
load("./level3.zen", "something")
"#,
    );

    env.add_file(
        "level1.zen",
        r#"
# This loads a file that loads a file with an error
load("./level2.zen", "something")
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Top level load - error should propagate up with proper spans
load("./level1.zen", "something")
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn snapshot_cyclic_load_error() {
    let env = TestProject::new();

    env.add_file(
        "a.zen",
        r#"
# This creates a cycle: a -> b -> a
load("./b.zen", "b_func")

def a_func():
    return "a"
"#,
    );

    env.add_file(
        "b.zen",
        r#"
# This completes the cycle
load("./a.zen", "a_func")

def b_func():
    return "b"
"#,
    );

    star_snapshot!(env, "a.zen");
}
