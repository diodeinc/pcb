mod common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_component_factory() {
    let env = TestProject::new();

    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    // Compose the Starlark code with the absolute path.
    env.add_file(
        "test.star",
        r#"
# Import factory and instantiate.
load(".", M123 = "C146731")

M123(
    name = "M123",
    pins = {
        "ICLK": Net("ICLK"),
        "Q1": Net("Q1"),
        "Q2": Net("Q2"),
        "Q3": Net("Q3"),
        "Q4": Net("Q4"),
        "GND": Net("GND"),
        "VDD": Net("VDD"),
        "OE": Net("OE"),
    },
    footprint = "SMD:0805",
)
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn test_net_passing() {
    let env = TestProject::new();

    env.add_file(
        "MyComponent.star",
        r#"
ComponentInterface = interface(p1 = Net, p2 = Net)
input = io("input", ComponentInterface)

Component(
    name = "capacitor",
    type = "capacitor",
    pin_defs = { "P1": "1", "P2": "2" },
    footprint = "SMD:0805",
    pins = { "P1": input.p1, "P2": input.p2 },
)
        "#,
    );

    env.add_file(
        "test.star",
        r#"
load("MyComponent.star", "ComponentInterface")
load(".", MyComponent = "MyComponent")

MyComponent(
    name = "MyComponent",
    input = ComponentInterface("INTERFACE"),
)
        "#,
    );

    env.add_file(
        "top.star",
        r#"
load(".", Test = "test")

Test(
    name = "Test",
)
        "#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
fn snapshot_unused_inputs_should_error() {
    let env = TestProject::new();

    // Create a simple module that does not declare any io()/config() placeholders.
    env.add_file("my_module.star", "\n# empty module with no inputs\n");

    // Top-level file instantiates the module while passing an unexpected argument.
    env.add_file(
        "top.star",
        r#"
load(".", MyModule = "my_module")

MyModule(
    name = "MyModule",
    unused = 123,
)
"#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_missing_pins_should_error() {
    let env = TestProject::new();

    // Include symbol resource used for components.
    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "test_missing.star",
        r#"
load(".", COMP = "C146731")

# Instantiate the component while omitting several required pins.
COMP(
    name = "Component",
    pins = {
        "ICLK": Net("ICLK"),
        "Q1": Net("Q1"),
    },
    footprint = "SMD:0805",
)
"#,
    );

    star_snapshot!(env, "test_missing.star");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_unknown_pin_should_error() {
    let env = TestProject::new();

    // Include symbol resource used for components.
    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "test_unknown.star",
        r#"
load(".", COMP = "C146731")

# Instantiate the component with an invalid pin included.
COMP(
    name = "Comp",
    pins = {
        "ICLK": Net("ICLK"),
        "Q1": Net("Q1"),
        "Q2": Net("Q2"),
        "Q3": Net("Q3"),
        "Q4": Net("Q4"),
        "GND": Net("GND"),
        "VDD": Net("VDD"),
        "OE": Net("OE"),
        "INVALID": Net("X"),
    },
    footprint = "SMD:0805",
)
"#,
    );

    star_snapshot!(env, "test_unknown.star");
}

#[test]
fn test_nested_components() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- Component.star
Component(
    name = "Component",
    pin_defs = {
        "P1": "1",
        "P2": "2",
    },
    pins = {
        "P1": Net("P1"),
        "P2": Net("P2"),
    },
    footprint = "SMD:0805",
)

# --- Module.star
load(".", MyComponent = "Component")

MyComponent(
    name = "MyComponent",
)

# --- Top.star
load(".", MyModule = "Module")

MyModule(
    name = "MyModule",
)
        "#,
    );

    star_snapshot!(env, "Top.star");
}

#[test]
fn test_net_name_deduplication() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- MyModule.star
_internal_net = Net("INTERNAL")
Component(
    name = "Component",
    pin_defs = {
        "P1": "1",
    },
    pins = {
        "P1": _internal_net,
    },
    footprint = "SMD:0805",
)

# --- Top.star
load(".", MyModule = "MyModule")
MyModule(name = "MyModule1")
MyModule(name = "MyModule2")
MyModule(name = "MyModule3")
    "#,
    );

    star_snapshot!(env, "Top.star");
}
