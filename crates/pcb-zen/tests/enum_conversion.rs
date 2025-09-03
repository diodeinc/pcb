mod common;
use common::TestProject;

#[test]
fn snapshot_enum_config_conversion() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- child.zen
Direction = enum("NORTH", "SOUTH")

# Declare a config placeholder expecting the Direction enum.
heading = config("heading", Direction)

# Add a trivial component so that the schematic/netlist is non-empty.
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = { "V": "1" },
    pins = { "V": Net("VCC") },
)

# --- top.zen
# Bring in the `child` module from the current directory and alias it to `Child`.
Child = Module("child.zen")

# Pass the enum value as a plain string. The implementation should convert this
# into a Direction enum variant automatically.
Child(
    name = "child",
    heading = "NORTH",
)
"#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
fn snapshot_enum_io_conversion() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- child.zen
Direction = enum("NORTH", "SOUTH")

# IO placeholder expecting an enum variant.
hdr_dir = io("hdr_dir", Direction)

MyCap = None  # remove old factory; keep placeholder to avoid undefined
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = { "V": "1" },
    pins = { "V": Net("VCC") },
)

# --- top.zen
# Bring in the `child` module from the current directory and alias it to `Child`.
Child = Module("child.zen")

Child(
    name = "child",
    hdr_dir = "SOUTH",
)
"#,
    );

    star_snapshot!(env, "top.zen");
}
