#![cfg(not(target_os = "windows"))]

use std::fs;

use pcb_kicad_sch::{KicadProject, SchItem};
use pcb_test_utils::sandbox::Sandbox;

const BOARD_ZEN: &str = r#"
Project(name="ApplyTest", path="hardware", layout=False, bom_profile=None)

Resistor = Module("@stdlib/generics/Resistor.zen")

LEFT = Net("LEFT")
MID = Net("MID")
RIGHT = Net("RIGHT")

Resistor(name="R1", value="1kohms", package="0402", P1=LEFT, P2=MID)
Resistor(name="R2", value="2kohms", package="0402", P1=MID, P2=RIGHT)
"#;

#[test]
fn apply_schematic_creates_repairs_and_clears_build_diagnostics() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_ZEN);

    sandbox
        .run("pcbc", ["apply", "schematic", "--no-open", "board.zen"])
        .run()
        .expect("create schematic project");

    let project_dir = sandbox.root_path().join("hardware");
    let mut project = KicadProject::load(&project_dir).unwrap();
    assert!(project.project_file.is_file());
    assert_eq!(project.schematic_files.len(), 1);

    let page = &mut project.document.pages[0];
    let label_index = page
        .items
        .iter()
        .position(|item| matches!(item, SchItem::Label(label) if label.text == "MID"))
        .expect("generated MID label");
    page.items.remove(label_index);
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    let broken = sandbox
        .run("pcbc", ["build", "--diagnostics", "-", "board.zen"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("build with schematic warning");
    let diagnostics = String::from_utf8(broken.stdout).unwrap();
    assert!(
        diagnostics.contains("sch.disconnected_net"),
        "{diagnostics}"
    );

    sandbox
        .run("pcbc", ["apply", "schematic", "--no-open", "board.zen"])
        .run()
        .expect("repair schematic project");
    let repaired = sandbox
        .run("pcbc", ["build", "--diagnostics", "-", "board.zen"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("build repaired schematic");
    let diagnostics = String::from_utf8(repaired.stdout).unwrap();
    assert!(!diagnostics.contains("\"kind\": \"sch."), "{diagnostics}");
}

#[test]
fn complete_apply_reports_schematic_and_layout_artifacts_consistently() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_ZEN.replace("layout=False, ", ""));

    let output = sandbox
        .run("pcbc", ["apply", "--no-open", "board.zen"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("apply complete project");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("board.zen schematic created (")
            && stdout.contains("ApplyTest.kicad_sch")
            && stdout.contains("board.zen layout created (")
            && stdout.contains("layout.kicad_pcb"),
        "{stdout}"
    );

    let mut json_sandbox = Sandbox::new().with_workspace();
    json_sandbox.write("board.zen", BOARD_ZEN.replace("layout=False, ", ""));
    let output = json_sandbox
        .run("pcbc", ["apply", "--no-open", "-f", "json", "board.zen"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("apply complete project as JSON");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["sourceFile"], "board.zen");
    assert!(
        json["schematic"]["rootSchematic"]
            .as_str()
            .is_some_and(|path| path.ends_with("ApplyTest.kicad_sch"))
    );
    assert_eq!(json["layout"]["action"], "created");
    assert!(
        json["layout"]["pcbFile"]
            .as_str()
            .is_some_and(|path| path.ends_with("layout.kicad_pcb"))
    );
}
