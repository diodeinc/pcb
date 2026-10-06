use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pcb-ipc2581-tools/src/assembly/testdata/report.xml")
}

#[test]
fn jlc_bom_command_exports_hydrated_supplier_without_availability() {
    let temp = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pcb-ipc2581-tools/src/commands/testdata/jlc_bom.xml");
    let selections = temp.path().join("selections.json");
    let hydrated = temp.path().join("hydrated.xml");
    std::fs::write(
        &selections,
        r#"[{
        "path":"Board.C1", "refdes":"C1",
        "manufacturerId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        "manufacturer":"Selected Manufacturer", "mpn":"Selected-MPN",
        "distributor":"LCSC", "distributorPartId":"C9999"
    }]"#,
    )
    .unwrap();
    let edit = Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args(["ipc", "edit", "bom"])
        .arg(input)
        .arg("--selections")
        .arg(selections)
        .arg("--output")
        .arg(&hydrated)
        .output()
        .unwrap();
    assert!(
        edit.status.success(),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );

    // No --offline or credentials: JLC output uses only the hydrated file.
    let output = Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args(["ipc", "bom"])
        .arg(hydrated)
        .args(["--format", "jlc"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let csv = String::from_utf8(output.stdout).unwrap();
    assert!(csv.starts_with("Comment,Designator,Footprint,JLCPCB Part #\n"));
    assert!(csv.contains("100nF,C1,0402,C9999\n"));
    assert!(!csv.contains("C16133"));
}

fn assembly_report(scope: &str) -> (Vec<u8>, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .arg("ipc")
        .arg("assembly")
        .arg(fixture())
        .arg("--scope")
        .arg(scope)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "pcbc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output.stdout, report)
}

#[test]
fn assembly_command_matches_the_shared_board_array_report() {
    let (first_json, report) = assembly_report("board-array");
    let (second_json, _) = assembly_report("board-array");

    assert_eq!(first_json, second_json);
    assert_eq!(report["schema_version"], 5);
    assert_eq!(report["scope"]["kind"], "board_array");
    assert_eq!(report["scope"]["area_mm2"], 1_400.0);
    assert_eq!(report["profiles"].as_array().unwrap().len(), 2);
    let package = report["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "pkg-smt")
        .unwrap();
    assert_eq!(package["pickup_point_mm"]["x"], 0.1);
    assert!(package["views"][0]["silkscreen"].is_object());
}

fn assembly_from_report(report: &[u8], args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args(["ipc", "assembly", "--report", "-"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(report).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn assembly_command_changes_the_population_of_a_piped_report() {
    let (json, _) = assembly_report("board");

    let output = assembly_from_report(&json, &["--dnp", "J1", "--populate", "U2"]);

    assert!(
        output.status.success(),
        "pcbc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let population = |designator: &str| {
        report["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|component| component["reference_designator"] == designator)
            .unwrap()["population"]
            .clone()
    };
    assert_eq!(population("J1"), "do_not_populate");
    assert_eq!(population("U2"), "populate");
    assert_eq!(
        report["summary"]["terminations"]["through_on_included_populated_components"],
        0
    );

    // Reporting the file for that population gives the same report.
    let output = Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args([
            "ipc",
            "assembly",
            "--scope",
            "board",
            "--dnp",
            "J1",
            "--populate",
            "U2",
        ])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        report
    );

    let output = assembly_from_report(&json, &["--dnp", "R9"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Designators not in the BOM: R9"));

    let output = assembly_from_report(&json, &["--scope", "board"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn assembly_command_selects_canonical_board_scope() {
    let (_, report) = assembly_report("board");

    assert_eq!(report["scope"]["kind"], "board");
    assert_eq!(report["summary"]["board_occurrences"], 1);
    assert_eq!(report["summary"]["components"]["total"], 4);
    assert_eq!(report["summary"]["terminations"]["total"], 3);
}

#[test]
fn accuracy_reaches_ipc_manufacturing_geometry() {
    use pcb_ipc2581_tools::commands::board_array::{
        BoardArrayCreateOptions, BoardMarginMm, create_board_array,
    };
    use pcb_ir::geom::{GeometryAccuracy, Resolution};
    let source = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
<Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="TOP"/>
<DictionaryStandard units="MILLIMETER"><EntryStandard id="ellipse"><Ellipse width="8" height="4"/></EntryStandard></DictionaryStandard></Content>
<Ecad><CadHeader units="MILLIMETER"/><CadData>
<Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
<Step name="board" type="BOARD"><Profile><Polygon>
<PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/><PolyStepSegment x="10" y="10"/><PolyStepSegment x="0" y="10"/>
</Polygon></Profile>
<LayerFeature layerRef="TOP"><Set polarity="POSITIVE"><Pad padstackDefRef="ellipse">
<Location x="5" y="5"/><StandardPrimitiveRef id="ellipse"/>
</Pad></Set></LayerFeature>
</Step></CadData></Ecad></IPC-2581>"#;
    let directory = tempfile::tempdir().unwrap();
    // Gerber cannot carry a native ellipse, so export must spend the budget.
    let array = create_board_array(
        source,
        &BoardArrayCreateOptions {
            columns: 4,
            rows: 4,
            board_margin_mm: BoardMarginMm::all(5.0),
            edge_rail_mm: BoardMarginMm::all(5.0),
        },
        false,
        pcb_ipc2581_tools::commands::board_array::Separation::VScore,
        Resolution::default(),
    )
    .unwrap();
    let input = directory.path().join("array.xml");
    std::fs::write(&input, &array.xml).unwrap();
    let ipc = pcb_ipc2581_tools::ipc2581::Ipc2581::parse(&array.xml).unwrap();
    let path = directory.path().join("gerbers");
    let mut packages = Vec::new();
    for accuracy_um in [1, 30] {
        let resolution =
            Resolution::default().with_accuracy(GeometryAccuracy::micrometres(accuracy_um));
        let output = Command::new(env!("CARGO_BIN_EXE_pcbc"))
            .args(["ipc", "gerber"])
            .arg(&input)
            .args(["--accuracy-um", &accuracy_um.to_string(), "--output"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let expected = pcb_ipc2581_tools::manufacturing::build_manufacturing_package(
            &imported,
            &pcb_ipc2581_tools::manufacturing::ManufacturingExportOptions {
                view: pcb_ipc2581_tools::LayoutTarget::BoardArray.artwork_scope(),
                relief_debug_dir: None,
                include_auxiliary_layers: false,
            },
            resolution,
        )
        .unwrap();
        let mut contents = Vec::new();
        for file in expected.files {
            let actual = std::fs::read_to_string(path.join(file.filename)).unwrap();
            assert_eq!(actual, file.contents);
            contents.push(actual);
        }
        packages.push(contents);
    }
    assert_ne!(
        packages[0], packages[1],
        "elliptical copper must exercise the requested accuracy"
    );
}
