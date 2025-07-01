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

    // `top.star` loads a remote Starlark module from GitHub and instantiates
    // a component from it.
    env.add_file(
        "top.star",
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

    star_snapshot!(env, "top.star");
}
