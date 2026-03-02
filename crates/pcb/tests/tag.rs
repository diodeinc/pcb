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

[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
"#;

const BOARD_TB0001_PCB_TOML: &str = r#"
[board]
name = "TB0001"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")
load("@stdlib/properties.zen", "Layout")

Layout(name="TB0001", path="build/TB0001", bom_profile=None)

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_publish_board_simple_workspace() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", BOARD_TB0001_PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/netlist.json"])
        .ignore_globs([
            "layout/*",
            "**/vendor/**",
            "**/build/**",
            "**/manufacturing/**",
            "**/3d/**",
            "**/bom/**",
            "**/drc.json",
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
            "boards/Test/TB0001.zen",
            "--bump=minor",
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
    let mut sb = Sandbox::new();
    let output = sb
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", BOARD_TB0001_PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run(
            "pcb",
            [
                "publish",
                "boards/NonExistent.zen",
                "--bump=minor",
                "--no-push",
                "--force",
            ],
        );
    assert_snapshot!("publish_board_invalid_path", output);
}
