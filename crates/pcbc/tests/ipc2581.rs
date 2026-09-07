use std::{path::PathBuf, process::Command};

use serde_json::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pcb-ipc2581-tools/src/assembly/testdata/report.xml")
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
    assert_eq!(report["schema_version"], 4);
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
