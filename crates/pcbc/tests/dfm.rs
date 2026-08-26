#![cfg(not(target_os = "windows"))]

use std::path::Path;

use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;

const PCB_TOML: &str = include_str!("../../pcb-layout/tests/resources/simple/pcb.toml");
const BOARD_ZEN: &str = include_str!("../../pcb-layout/tests/resources/simple/MyBoard.zen");
const MODULE_ZEN: &str = include_str!("../../pcb-layout/tests/resources/simple/BMI270.zen");
const FOOTPRINT: &str =
    include_str!("../../pcb-layout/tests/resources/simple/eda/BMI270.kicad_mod");
const SYMBOL: &str = include_str!("../../pcb-layout/tests/resources/simple/eda/BMI270.kicad_sym");

#[test]
fn dfm_resolves_zen_exports_temporary_ipc_and_checks_standard_pdk() {
    let mut sandbox = Sandbox::new();
    sandbox
        .env("SOURCE_DATE_EPOCH", "1787702400")
        .write("pcb.toml", PCB_TOML)
        .write("MyBoard.zen", BOARD_ZEN)
        .write("BMI270.zen", MODULE_ZEN)
        .write("eda/BMI270.kicad_mod", FOOTPRINT)
        .write("eda/BMI270.kicad_sym", SYMBOL);

    let output = sandbox
        .run("pcbc", ["dfm", "MyBoard.zen", "--pdk", "standard"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("DFM command should run");
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("stdout should contain only the DFM report");

    assert_eq!(report["pdk"]["path"], "builtin:standard");
    assert_eq!(report["layout_target"], "board");
    assert!(
        report["input"]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("ipc2581.xml"))
    );
    assert!(
        sandbox
            .default_cwd()
            .join("build/layout.kicad_pcb")
            .exists()
    );
    assert!(!Path::new(report["input"]["path"].as_str().unwrap()).exists());
    assert!(!sandbox.default_cwd().join(".pcb/releases").exists());
}
