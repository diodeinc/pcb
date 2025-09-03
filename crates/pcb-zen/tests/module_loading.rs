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
load("//nested/file/import.zen", DummyFunction = "DummyFunction")

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

// Module loading with local package aliases
#[test]
fn module_with_local_alias() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[packages]
local = "./modules"
"#,
    );

    env.add_file(
        "modules/MyModule.zen",
        r#"
# A simple module
input = io("input", Net)
output = io("output", Net)
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Test that Module() works with local package alias
MyModule = Module("@local/MyModule.zen")

MyModule(
    name = "M1",
    input = Net("IN"),
    output = Net("OUT"),
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Module loading with multiple package aliases
#[test]
#[cfg(not(target_os = "windows"))]
#[serial_test::serial]
fn module_with_multiple_aliases() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[packages]
stdlib_v5 = "@github/diodeinc/stdlib:v0.0.5"
stdlib_v4 = "@github/diodeinc/stdlib:v0.0.4"
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Test loading from different package versions
Resistor_v5 = Module("@stdlib_v5/generics/Resistor.star")
Resistor_v4 = Module("@stdlib_v4/generics/Resistor.star")

# Create instances to verify they load correctly
Resistor_v5(
    name = "R2",
    value = "2kohm",
    package = "0603",
    P1 = Net("P3"),
    P2 = Net("P4"),
)

Resistor_v4(
    name = "R3",
    value = "3kohm",
    package = "0805",
    P1 = Net("P5"),
    P2 = Net("P6"),
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Module loading with workspace root references
#[test]
fn module_with_workspace_root() {
    let env = TestProject::new();

    env.add_file("pcb.toml", "[workspace]");

    env.add_file(
        "submodule.zen",
        r#"
P1 = io("P1", Net)
"#,
    );

    env.add_file(
        "nested/test.zen",
        r#"
# Test workspace root reference
Submodule = Module("//submodule.zen")

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
Units = Module("@stdlib:v0.2.4/units.zen")

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

// Error case: nonexistent package alias
#[test]
#[cfg(not(target_os = "windows"))]
#[serial_test::serial]
fn module_nonexistent_alias() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[packages]
missing = "does_not_exist"
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# This should fail - alias points to nonexistent directory
MissingModule = Module("@missing/something.zen")
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Test Module() with github package syntax
#[test]
#[cfg(not(target_os = "windows"))]
fn module_with_github_package() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test Module() with full github package path
# Note: This will try to download from GitHub, so it requires network access
Resistor = Module("@github/diodeinc/stdlib:v0.0.6/generics/Resistor.star")

# Create an instance to verify it loads
Resistor(
    name = "R1",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

// Test loading remote modules with aliased packages
#[test]
#[cfg(not(target_os = "windows"))]
#[serial_test::serial]
fn module_load_remote_with_alias() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[module]
name = "test"

[packages]
test_package = "@github/hexdae/6d919d810f4a3a238688cfd59de8b7ea"
"#,
    );

    env.add_file(
        "top.zen",
        r#"
# Load a type from a remote repository
load("@github/hexdae/6d919d810f4a3a238688cfd59de8b7ea/Capacitor.star", "Package")

# Load a component from aliased package
Capacitor = Module("@test_package/Capacitor.star")

package = Package("0402")

# Instantiate Capacitor
Capacitor(name = "C1", package = package.value, value = 10e-6, P1 = Net("P1"), P2 = Net("P2"))
"#,
    );

    star_snapshot!(env, "top.zen");
}

// Test loading KiCad symbols from default @kicad-symbols alias
#[test]
#[cfg(not(target_os = "windows"))]
#[serial_test::serial]
fn module_load_kicad_symbol() {
    let env = TestProject::new();

    env.add_file(
        "top.zen",
        r#"
# Create a resistor instance using @kicad-symbols alias
Component(
    name = "R1",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name = "R_US"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pins = {
        "1": Net("IN"),
        "2": Net("OUT")
    }
)
"#,
    );

    star_snapshot!(env, "top.zen");
}

// Test Module() with relative paths from subdirectories
#[test]
fn module_relative_from_subdir() {
    let env = TestProject::new();

    env.add_file(
        "modules/MyModule.zen",
        r#"
# A simple module
input = io("input", Net)
output = io("output", Net)
"#,
    );

    env.add_file(
        "pcb.toml",
        r#"
[workspace]
[packages]
local = "./modules"
"#,
    );

    env.add_file(
        "src/test.zen",
        r#"
# Test that Module() works with relative path alias from subdirectory
MyModule = Module("@local/MyModule.zen")

MyModule(
    name = "M1",
    input = Net("IN"),
    output = Net("OUT"),
)
"#,
    );

    star_snapshot!(env, "src/test.zen");
}
