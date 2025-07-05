mod common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_remote_module_and_component() {
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

    // `top.zen` loads a remote Starlark module from GitHub and instantiates
    // a component from it.
    env.add_file(
        "top.zen",
        r#"

# Load a type from a remote repository.
load("@github/hexdae/6d919d810f4a3a238688cfd59de8b7ea/Capacitor.star", "Package")

# Load a component from aliased package, should be equivalent to the next line.
# load("@github/hexdae/6d919d810f4a3a238688cfd59de8b7ea", "Capacitor")
load("@test_package", "Capacitor")

package = Package("0402")

# Instantiate `Capacitor` so we still get a non-empty schematic for snapshotting.
Capacitor(name = "C1", package = package.value, value = 10e-6, P1 = Net("P1"), P2 = Net("P2"))
"#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn load_kicad_symbol_from_default_alias() {
    let env = TestProject::new();
    // Test loading a resistor from the default @kicad-symbols alias
    env.add_file(
        "top.zen",
        r#"
# Create a resistor instance
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
