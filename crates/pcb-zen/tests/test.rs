mod common;
use common::TestProject;

#[test]
fn test_net_passing() {
    let env = TestProject::new();

    env.add_file(
        "MyComponent.zen",
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
        "test.zen",
        r#"
load("MyComponent.zen", "ComponentInterface")
MyComponent = Module("MyComponent.zen")

MyComponent(
    name = "MyComponent",
    input = ComponentInterface("INTERFACE"),
)
        "#,
    );

    env.add_file(
        "top.zen",
        r#"
Test = Module("test.zen")

Test(
    name = "Test",
)
        "#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
fn snapshot_unused_inputs_should_error() {
    let env = TestProject::new();

    // Create a simple module that does not declare any io()/config() placeholders.
    env.add_file("my_module.zen", "\n# empty module with no inputs\n");

    // Top-level file instantiates the module while passing an unexpected argument.
    env.add_file(
        "top.zen",
        r#"
MyModule = Module("my_module.zen")

MyModule(
    name = "MyModule",
    unused = 123,
)
"#,
    );

    star_snapshot!(env, "top.zen");
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
        "test_missing.zen",
        r#"
# Instantiate the component while omitting several required pins.
Component(
    name = "Component",
    pins = {
        "ICLK": Net("ICLK"),
        "Q1": Net("Q1"),
    },
    symbol = Symbol(library = "C146731.kicad_sym", name = "NB3N551DG"),
    footprint = "SMD:0805",
)
"#,
    );

    star_snapshot!(env, "test_missing.zen");
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
        "test_unknown.zen",
        r#"
# Instantiate the component with an invalid pin included.
Component(
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
    symbol = Symbol(library = "C146731.kicad_sym", name = "NB3N551DG"),
    footprint = "SMD:0805",
)
"#,
    );

    star_snapshot!(env, "test_unknown.zen");
}

#[test]
fn test_nested_components() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- Component.zen
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

# --- Module.zen
MyComponent = Module("Component.zen")

MyComponent(
    name = "MyComponent",
)

# --- Top.zen
MyModule = Module("Module.zen")

MyModule(
    name = "MyModule",
)
        "#,
    );

    star_snapshot!(env, "Top.zen");
}

#[test]
fn test_net_name_deduplication() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- MyModule.zen
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

# --- Top.zen
MyModule = Module("MyModule.zen")
MyModule(name = "MyModule1")
MyModule(name = "MyModule2")
MyModule(name = "MyModule3")
    "#,
    );

    star_snapshot!(env, "Top.zen");
}
