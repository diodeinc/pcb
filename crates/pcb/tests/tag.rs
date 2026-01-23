#![cfg(not(target_os = "windows"))]

use pcb_test_utils::{assert_snapshot, sandbox::Sandbox};

fn find_staging_dir(sb: &Sandbox, board_name: &str) -> String {
    let releases_dir = sb.root_path().join(".pcb/releases");
    let staging_dir_name = std::fs::read_dir(&releases_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&format!("{}-", board_name)) && !name.ends_with(".zip")
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .expect("Staging directory not found");
    format!(".pcb/releases/{}", staging_dir_name)
}

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
name = "test_workspace"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")

add_property("layout_path", "build/TB0001")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_publish_board_simple_workspace() {
    let mut sb = Sandbox::new().allow_network();
    sb.write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/netlist.json"])
        .ignore_globs([
            "layout/*",
            "**/vendor/**",
            "**/build/**",
            "**/manufacturing/**",
            "**/3d/**",
            "**/bom/**",
        ])
        .init_git()
        .commit("Initial commit");

    // Generate layout files before release (full releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/Test/TB0001.zen"])
        .run()
        .expect("layout generation failed");

    sb.run(
        "pcb",
        [
            "publish",
            "--board",
            "boards/Test/TB0001.zen",
            "--bump",
            "minor",
            "--no-push",
            "--force", // Skip preflight checks
            "-S",
            "layout.drc.invalid_outline",
        ],
    )
    .run()
    .expect("publish failed");

    let staging_dir = find_staging_dir(&sb, "TB0001");
    assert_snapshot!(
        "publish_board_simple_workspace",
        sb.snapshot_dir(&staging_dir)
    );
}

#[test]
fn test_publish_board_invalid_path() {
    let output = Sandbox::new()
        .allow_network()
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run(
            "pcb",
            [
                "publish",
                "--board",
                "boards/NonExistent.zen",
                "--bump",
                "minor",
                "--no-push",
                "--force",
            ],
        );
    assert_snapshot!("publish_board_invalid_path", output);
}
