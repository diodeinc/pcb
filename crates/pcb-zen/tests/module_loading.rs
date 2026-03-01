mod common;
use common::TestProject;

// Module loading with relative paths
#[test]
fn module_with_relative_paths() {
    let env = TestProject::new();

    env.add_file(
        "MyModule.zen",
        r#"
# A simple module
P1 = io("P1", Net)
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Test that Module() works with relative paths
MyModule = Module("./MyModule.zen")

MyModule(
    name = "MyModule",
    P1 = Net("P1"),
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Module loading with nested directories
#[test]
fn module_with_nested_directories() {
    let env = TestProject::new();

    env.add_file(
        "nested/file/import.zen",
        r#"
def DummyFunction():
    pass
"#,
    );

    env.add_file(
        "sub.zen",
        r#"
load("./nested/file/import.zen", DummyFunction = "DummyFunction")

DummyFunction()

Component(
    name = "TestComponent",
    footprint = "SMD:0805",
    symbol = Symbol(
        definition = [ 
            ("1" , ["1", "N1"]),
            ("2" , ["2", "N2"]),
        ],
    ),
    pins = {
        "1": Net("N1"),
        "2": Net("N2"),
    },
)
"#,
    );

    env.add_file(
        "top.zen",
        r#"
Sub = Module("sub.zen")
Sub(name = "sub")
"#,
    );

    star_snapshot!(env, "top.zen");
}

// Module loading with workspace root references
#[test]
fn module_with_workspace_root() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[workspace]
pcb-version = "0.3"
"#,
    );

    env.add_file(
        "submodule.zen",
        r#"
P1 = io("P1", Net)
"#,
    );

    env.add_file(
        "nested/test.zen",
        r#"
# Test that Module() can load a sibling file from a nested directory via relative paths
Submodule = Module("../submodule.zen")

Submodule(
    name = "Submodule",
    P1 = Net("P1"),
)
"#,
    );

    star_snapshot!(env, "nested/test.zen");
}

// Module loading with @stdlib default alias
#[test]
#[cfg(not(target_os = "windows"))]
#[serial_test::serial]
fn module_with_stdlib_alias() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test that Module() can resolve @stdlib imports (using units as an example)
Units = Module("@stdlib/units.zen")

# We don't instantiate Units since it's just definitions,
# but the Module() call should resolve correctly
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Error case: nonexistent module file
#[test]
fn module_nonexistent_file() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# This should fail - module file doesn't exist
MissingModule = Module("does_not_exist.zen")
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Test Module() with relative paths from subdirectories
#[test]
fn module_relative_from_subdir() {
    let env = TestProject::new();

    env.add_file(
        "modules/MyModule.zen",
        r#"
# A simple module
INPUT = io("INPUT", Net)
OUTPUT = io("OUTPUT", Net)
"#,
    );

    env.add_file(
        "pcb.toml",
        r#"
[workspace]
pcb-version = "0.3"
"#,
    );

    env.add_file(
        "src/test.zen",
        r#"
# Test that Module() works with relative paths from a subdirectory
MyModule = Module("../modules/MyModule.zen")

MyModule(
    name = "M1",
    INPUT = Net("IN"),
    OUTPUT = Net("OUT"),
)
"#,
    );

    star_snapshot!(env, "src/test.zen");
}
