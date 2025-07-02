mod common;
use common::TestProject;

#[test]
fn snapshot_io_and_config_placeholders() {
    let env = TestProject::new();

    env.add_file(
        "my_sub.zen",
        r#"
# Declare input placeholders
pwr = io("pwr", Net)
baud = config("baud", int)

# Very small dummy component that ties to the power net so that the schematic is non-empty.
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {
        "V": "1",
    },
    pins = {"V": pwr},
)
"#,
    );

    env.add_file(
        "top.zen",
        r#"
Sub = Module("my_sub.zen")

Sub(
    name = "sub",
    pwr = Net("VCC"),
    baud = 9600,
)
"#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
fn snapshot_undefined_placeholder() {
    let env = TestProject::new();

    env.add_file(
        "my_sub.zen",
        r#"
# Declare input placeholders
pwr = io("pwr", Net, optional = True)

Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {"V": "1"},
    pins = {"V": pwr},
)
"#,
    );

    env.add_file(
        "top.zen",
        r#"
Sub = Module("my_sub.zen")

Sub(
    name = "sub",
    # Missing `pwr`
)
"#,
    );

    star_snapshot!(env, "top.zen");
}
