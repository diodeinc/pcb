use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const BOARD_WITHOUT_LAYOUT_ZEN: &str = r#"
vin = Power("VIN")
gnd = Ground("GND")
signal = Net("SIGNAL")
"#;

const BOARD_WITH_NON_STRING_LAYOUT_PATH_ZEN: &str = r#"
add_property("layout_path", 1)

vin = Power("VIN")
gnd = Ground("GND")
signal = Net("SIGNAL")
"#;

#[test]
fn missing_board_macro_warning() {
    let mut sb = Sandbox::new();
    sb.write("boost_5v.zen", BOARD_WITHOUT_LAYOUT_ZEN);

    let output = sb.snapshot_run("pcb", ["layout", "--no-open", "boost_5v.zen"]);
    assert_snapshot!("missing_board_macro_warning", output);
}

#[test]
fn non_string_layout_path_warning() {
    let mut sb = Sandbox::new();
    sb.write("bad_layout_path.zen", BOARD_WITH_NON_STRING_LAYOUT_PATH_ZEN);

    let output = sb.snapshot_run("pcb", ["layout", "--no-open", "bad_layout_path.zen"]);
    assert_snapshot!("non_string_layout_path_warning", output);
}
