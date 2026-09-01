#![cfg(not(target_os = "windows"))]

use std::{ffi::OsStr, path::Path, process::Output};

use pcb_ipc2581_tools::commands::{
    EdgeInsetsMm,
    board_array::{BoardArrayCreateOptions, create_board_array},
    fab_panel::{self, FabPanelSpec},
};
use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn run_pcbc<I>(sandbox: &mut Sandbox, args: I) -> Output
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    sandbox
        .run("pcbc", args)
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("pcbc command should execute")
}

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

    let output = run_pcbc(&mut sandbox, ["dfm", "MyBoard.zen", "--pdk", "standard"]);
    let mut report: Value =
        serde_json::from_slice(&output.stdout).expect("stdout should contain only the DFM report");

    assert_eq!(report["pdk"]["path"], "builtin:standard");
    assert_eq!(report["layout_target"], "board");
    assert_eq!(report["scene"]["schema_version"], 1);
    assert_eq!(report["layout"]["coordinate_frame"], "selected_board");
    let standard_pdk = include_str!("../../pcb-ipc2581-tools/pdks/standard.toml");
    assert_eq!(report["pdk"]["source"], standard_pdk);
    assert_eq!(report["pdk"]["sha256"], sha256(standard_pdk.as_bytes()));
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

    let file_output = run_pcbc(
        &mut sandbox,
        [
            "dfm",
            "MyBoard.zen",
            "--pdk",
            "standard",
            "--output",
            "report.dfm.json",
        ],
    );
    assert!(file_output.stdout.is_empty());
    assert_eq!(file_output.status.code(), output.status.code());
    let mut file_report = read_report(&sandbox, "report.dfm.json");
    assert!(!Path::new(file_report["input"]["path"].as_str().unwrap()).exists());
    // Each .zen run asks KiCad for new IPC, which can reorder independent
    // features. Compare scene metadata here; the fixed-IPC test below checks
    // exact SVG parity as well as the full checked geometry.
    for generated in [&mut report, &mut file_report] {
        for pass in generated["scene"]["passes"].as_array_mut().unwrap() {
            let svg = pass.as_object_mut().unwrap().remove("svg").unwrap();
            assert!(svg.as_str().unwrap().starts_with("<svg "));
        }
    }
    for field in [
        "pdk",
        "layout_target",
        "layout",
        "summary",
        "rules",
        "findings",
        "scene",
    ] {
        assert_eq!(file_report[field], report[field], "{field} differs");
    }
}

const IPC_BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <LayerRef name="F.Mask"/>
    <LayerRef name="BOTTOM"/>
    <LayerRef name="B.Mask"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="0.07" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
        <StackupGroup name="Primary_Group" thickness="0.07" tolPlus="0" tolMinus="0">
          <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
          <StackupLayer layerOrGroupRef="BOTTOM" thickness="0.035" tolPlus="0" tolMinus="0" sequence="1"/>
        </StackupGroup>
      </Stackup>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="30" y="0"/>
            <PolyStepSegment x="30" y="30"/>
            <PolyStepSegment x="0" y="30"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="1" startY="1" endX="29" endY="1">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
              <Line startX="8" startY="20" endX="22" endY="20">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

const REPORT_PDK: &str = r#"schema_version = 2
default_profile = "test"

[pdk]
id = "report-fixture"
name = "Report fixture"
revision = "1"

[profiles.test]
name = "Test"

[[rules.stackup.copper_layer_count]]
id = "stackup.minimum_copper_layer_count"
minimum = 3

[[rules.stackup.copper_layer_count]]
id = "stackup.maximum_copper_layer_count"
maximum = 4

[[rules.copper.board_edge_clearance]]
id = "copper.minimum_board_edge_clearance"
minimum = "1 mm"
"#;

fn read_report(sandbox: &Sandbox, path: &str) -> Value {
    let json = std::fs::read_to_string(sandbox.default_cwd().join(path)).unwrap();
    serde_json::from_str(&json).expect("output should contain a JSON report")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn ipc_dfm_json_matches_stdout_and_preserves_full_scene_with_waivers() {
    let mut sandbox = Sandbox::new();
    sandbox
        .env("SOURCE_DATE_EPOCH", "1787702400")
        .write("board.xml", IPC_BOARD)
        .write("pdk.toml", REPORT_PDK);

    let json_output = run_pcbc(
        &mut sandbox,
        ["ipc", "dfm", "check", "board.xml", "--pdk", "pdk.toml"],
    );
    assert!(!json_output.status.success());
    let expected: Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(expected["verdict"], "fail");
    assert!(expected["summary"]["findings"].as_u64().unwrap() >= 2);

    let scene_output = run_pcbc(
        &mut sandbox,
        [
            "ipc",
            "dfm",
            "check",
            "board.xml",
            "--pdk",
            "pdk.toml",
            "--output",
            "report.dfm.json",
        ],
    );
    assert!(!scene_output.status.success());
    assert!(scene_output.stdout.is_empty());
    let exported = read_report(&sandbox, "report.dfm.json");
    assert_eq!(exported, expected);
    assert_eq!(
        std::fs::read(sandbox.default_cwd().join("report.dfm.json")).unwrap(),
        json_output.stdout
    );
    let scene = &exported["scene"];
    assert_eq!(scene["schema_version"], 1);
    let passes = scene["passes"].as_array().unwrap();
    // Both whole copper layers and one shared outline are exported, regardless
    // of which sites currently fail. There are no per-finding crop assets.
    assert_eq!(passes.len(), 3);
    let top = passes.iter().find(|pass| pass["layer"] == "TOP").unwrap();
    let svg = top["svg"].as_str().unwrap();
    assert!(
        svg.contains("M1 1 L29 1"),
        "the whole trace must remain visible"
    );
    assert!(
        svg.contains("M8 20 L22 20"),
        "passing geometry far outside the finding must remain visible"
    );
    let bounds = &scene["bounds"];
    let x = bounds["min"]["x"].as_f64().unwrap();
    let y = bounds["min"]["y"].as_f64().unwrap();
    let right = bounds["max"]["x"].as_f64().unwrap();
    let top = bounds["max"]["y"].as_f64().unwrap();
    assert!(x <= 0.0 && y <= 0.0 && right >= 30.0 && top >= 30.0);
    for pass in passes {
        let viewport = pass["svg"]
            .as_str()
            .unwrap()
            .split_once("viewBox='")
            .unwrap()
            .1
            .split_once('\'')
            .unwrap()
            .0
            .split_whitespace()
            .map(|value| value.parse::<f64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(viewport, [x, -top, right - x, top - y]);
    }
    assert_eq!(svg.matches("scale(1 -1)").count(), 1);
    assert!(!svg.contains("<image"));
    assert!(passes.iter().all(|pass| pass.get("assets").is_none()));

    let findings = expected["findings"].as_array().unwrap();
    let waivers = findings
        .iter()
        .map(|finding| {
            format!(
                "[[waiver]]\nfinding = \"{}\"\nreason = \"Approved test fixture\"\n",
                finding["id"].as_str().unwrap()
            )
        })
        .collect::<String>();
    sandbox.write("waivers.toml", waivers);
    let waived_output = run_pcbc(
        &mut sandbox,
        [
            "ipc",
            "dfm",
            "check",
            "board.xml",
            "--pdk",
            "pdk.toml",
            "--output",
            "waived.dfm.json",
            "--waivers",
            "waivers.toml",
        ],
    );
    assert!(waived_output.status.success());
    let waived = read_report(&sandbox, "waived.dfm.json");
    assert_eq!(&waived["scene"], scene);
    assert_eq!(waived["verdict"], "pass");
    assert_eq!(waived["summary"]["waived"], findings.len());
    assert_eq!(waived["findings"].as_array().unwrap().len(), findings.len());
    assert!(
        waived["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["waived"] == true)
    );
}

#[test]
fn ipc_dfm_passing_json_still_includes_native_scene_and_pdk() {
    let pdk = REPORT_PDK
        .replace("minimum = 3", "minimum = 2")
        .replace("minimum = \"1 mm\"", "minimum = \"0.5 mm\"");
    let mut sandbox = Sandbox::new();
    sandbox
        .write("board.xml", IPC_BOARD)
        .write("pdk.toml", &pdk);
    let output = run_pcbc(
        &mut sandbox,
        ["ipc", "dfm", "check", "board.xml", "--pdk", "pdk.toml"],
    );
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["verdict"], "pass");
    assert_eq!(report["summary"]["findings"], 0);
    assert!(report["findings"].as_array().unwrap().is_empty());
    assert_eq!(report["pdk"]["source"], pdk);
    assert_eq!(report["scene"]["schema_version"], 1);
    let top = report["scene"]["passes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pass| pass["layer"] == "TOP")
        .unwrap();
    assert!(top["svg"].as_str().unwrap().contains("M1 1 L29 1"));
}

#[test]
fn ipc_dfm_json_preserves_pdk_source_and_compressed_input_identity() {
    // The embedded PDK must retain comments and CRLFs, not be reserialized.
    let xml = format!("{}\r\n", IPC_BOARD.replace('\n', "\r\n"));
    let pdk = format!(
        "# Retain this source comment\r\n{}",
        REPORT_PDK.replace('\n', "\r\n")
    );
    let compressed = zstd::encode_all(xml.as_bytes(), 3).unwrap();
    let mut sandbox = Sandbox::new();
    sandbox
        .env("SOURCE_DATE_EPOCH", "1787702400")
        .write("board.xml", &xml)
        .write("board.xml.zst", &compressed)
        .write("pdk.toml", &pdk);
    let baseline = run_pcbc(
        &mut sandbox,
        ["ipc", "dfm", "check", "board.xml", "--pdk", "pdk.toml"],
    );
    assert!(!baseline.status.success());
    let baseline: Value = serde_json::from_slice(&baseline.stdout).unwrap();
    let findings = baseline["findings"].as_array().unwrap();
    assert!(findings.len() >= 2);
    let waiver = format!(
        "# Only one finding is waived\r\n[[waiver]]\r\nfinding = \"{}\"\r\nreason = \"Accepted for this fixture\"\r\n",
        findings[0]["id"].as_str().unwrap()
    );
    sandbox.write("waivers.toml", &waiver);

    for (input, input_bytes) in [
        ("board.xml", xml.as_bytes()),
        ("board.xml.zst", compressed.as_slice()),
    ] {
        let mut arguments = vec![
            "ipc",
            "dfm",
            "check",
            input,
            "--pdk",
            "pdk.toml",
            "--waivers",
            "waivers.toml",
        ];
        let json_output = run_pcbc(&mut sandbox, &arguments);
        assert!(!json_output.status.success());
        let expected: Value = serde_json::from_slice(&json_output.stdout).unwrap();

        arguments.extend(["--output", "report.dfm.json"]);
        let output = run_pcbc(&mut sandbox, &arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let report = read_report(&sandbox, "report.dfm.json");
        assert_eq!(report, expected);
        assert_eq!(
            std::fs::read(sandbox.default_cwd().join("report.dfm.json")).unwrap(),
            json_output.stdout
        );
        assert_eq!(report["verdict"], "fail");
        assert_eq!(report["summary"]["waived"], 1);
        assert_eq!(report["waivers"]["applied"], 1);
        assert_eq!(report["waivers"]["path"], "waivers.toml");
        assert_eq!(report["waivers"]["sha256"], sha256(waiver.as_bytes()));
        assert_eq!(
            report["findings"][0]["waiver_reason"],
            "Accepted for this fixture"
        );
        assert_eq!(report["scene"], baseline["scene"]);
        assert_eq!(report["input"]["path"], input);
        assert_eq!(report["input"]["sha256"], sha256(input_bytes));
        assert_eq!(report["input"]["size_bytes"], input_bytes.len());
        assert_eq!(report["pdk"]["source"], pdk);
        assert_eq!(report["pdk"]["sha256"], sha256(pdk.as_bytes()));
        if input.ends_with(".zst") {
            assert_ne!(report["input"]["sha256"], sha256(xml.as_bytes()));
        }

        // Replacing an existing output must preserve reproducible report bytes.
        let repeated = run_pcbc(&mut sandbox, arguments);
        assert!(!repeated.status.success());
        assert!(repeated.stdout.is_empty());
        assert_eq!(
            std::fs::read(sandbox.default_cwd().join("report.dfm.json")).unwrap(),
            json_output.stdout
        );
    }
}

fn assert_incomplete(report: &Value, input: &str, pdk: &str, expected_error: &str) {
    assert_eq!(report["verdict"], "incomplete");
    assert_eq!(report["input"], serde_json::json!({ "path": input }));
    assert_eq!(report["pdk"], serde_json::json!({ "path": pdk }));
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains(expected_error)
    );
    for field in [
        "coordinate_system",
        "layout",
        "waivers",
        "summary",
        "rules",
        "findings",
        "scene",
    ] {
        assert!(report.get(field).is_none(), "unexpected {field}");
    }
}

#[test]
fn ipc_dfm_json_replaces_stale_report_and_reports_all_incomplete_runs() {
    let mut sandbox = Sandbox::new();
    sandbox
        .env("SOURCE_DATE_EPOCH", "1787702400")
        .write("board.xml", IPC_BOARD)
        .write("broken.xml", "this is not IPC-2581")
        .write("pdk.toml", REPORT_PDK)
        .write(
            "bad-pdk.toml",
            REPORT_PDK.replace(
                "[[rules.copper.board_edge_clearance]]",
                "[[rules.copper.board_edge_clearence]]",
            ),
        )
        .write("bad-waivers.toml", "[[waiver]]\nfinding = ");
    let complete = run_pcbc(
        &mut sandbox,
        ["ipc", "dfm", "check", "board.xml", "--pdk", "pdk.toml"],
    );
    assert!(!complete.status.success());
    let complete_report: Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert_eq!(complete_report["verdict"], "fail");
    assert_eq!(complete_report["scene"]["schema_version"], 1);

    for (input, pdk, waivers, expected_error) in [
        ("missing.xml", "pdk.toml", None, "failed to read IPC-2581"),
        ("broken.xml", "pdk.toml", None, "failed to parse IPC-2581"),
        ("board.xml", "missing-pdk.toml", None, "failed to read PDK"),
        ("board.xml", "bad-pdk.toml", None, "failed to parse PDK"),
        ("board.xml", "standard", None, "without net attribution"),
        (
            "board.xml",
            "pdk.toml",
            Some("bad-waivers.toml"),
            "failed to parse waiver file",
        ),
    ] {
        let mut arguments = vec!["ipc", "dfm", "check", input, "--pdk", pdk];
        if let Some(waivers) = waivers {
            arguments.extend(["--waivers", waivers]);
        }
        let stdout = run_pcbc(&mut sandbox, &arguments);
        assert!(!stdout.status.success());
        let incomplete: Value = serde_json::from_slice(&stdout.stdout).unwrap();
        assert_incomplete(&incomplete, input, pdk, expected_error);

        sandbox.write("report.dfm.json", &complete.stdout);
        arguments.extend(["--output", "report.dfm.json"]);
        let output = run_pcbc(&mut sandbox, arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(read_report(&sandbox, "report.dfm.json"), incomplete);
        assert_eq!(
            std::fs::read(sandbox.default_cwd().join("report.dfm.json")).unwrap(),
            stdout.stdout
        );
    }
}

#[test]
fn dfm_json_requires_safe_output_before_reading_or_preparing_inputs() {
    let sources = [
        ("board.xml", IPC_BOARD),
        ("pdk.toml", REPORT_PDK),
        ("waivers.toml", "# No waivers\n"),
        ("MyBoard.zen", "This must not be evaluated\n"),
        (
            "layout.kicad_pcb",
            "Existing layout must remain untouched\n",
        ),
        ("other.KICAD_PCB", "Upper-case board extension\n"),
    ];
    let mut sandbox = Sandbox::new();
    for (path, bytes) in sources {
        sandbox.write(path, bytes);
    }
    let mut ipc_destinations = vec!["board.xml", "./board.xml", "pdk.toml", "waivers.toml"];
    let mut zen_destinations = vec![
        "MyBoard.zen",
        "./MyBoard.zen",
        "pdk.toml",
        "layout.kicad_pcb",
        "other.KICAD_PCB",
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("pdk.toml", sandbox.default_cwd().join("pdk-alias.toml"))
            .unwrap();
        ipc_destinations.push("pdk-alias.toml");
        std::os::unix::fs::symlink(
            "layout.kicad_pcb",
            sandbox.default_cwd().join("layout-report.dfm.json"),
        )
        .unwrap();
        zen_destinations.push("layout-report.dfm.json");
    }
    for (mut arguments, destinations) in [
        (
            vec![
                "ipc",
                "dfm",
                "check",
                "board.xml",
                "--waivers",
                "waivers.toml",
            ],
            ipc_destinations,
        ),
        (vec!["dfm", "MyBoard.zen", "--offline"], zen_destinations),
    ] {
        arguments.extend(["--pdk", "pdk.toml"]);
        for destination in destinations {
            let mut arguments = arguments.clone();
            arguments.extend(["--output", destination]);
            let output = run_pcbc(&mut sandbox, arguments);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("DFM report output would overwrite"),
                "{stderr}"
            );
            for (source, bytes) in sources {
                assert_eq!(
                    std::fs::read(sandbox.default_cwd().join(source)).unwrap(),
                    bytes.as_bytes(),
                    "{source} changed when output was {destination}"
                );
            }
        }
    }
    assert!(!sandbox.default_cwd().join("build").exists());
}

#[test]
fn ipc_dfm_geometry_distinguishes_canonical_board_arrays_and_mixed_fab_scope() {
    let array_options = BoardArrayCreateOptions {
        columns: 2,
        rows: 2,
        board_margin_mm: EdgeInsetsMm::all(5.0),
        edge_rail_mm: EdgeInsetsMm::all(5.0),
    };
    let first = create_board_array(IPC_BOARD, &array_options, false).unwrap();
    let second = create_board_array(
        &IPC_BOARD.replace("name=\"board\"", "name=\"other\""),
        &array_options,
        false,
    )
    .unwrap();
    let mut sandbox = Sandbox::new();
    sandbox
        .write("first.xml", first.xml)
        .write("second.xml", second.xml)
        .write(
            "pdk.toml",
            format!(
                "{REPORT_PDK}\n[[rules.panelization.board_spacing]]\nid = \"panelization.minimum_board_array_spacing\"\nminimum = \"7.62 mm\"\n"
            ),
        );
    let mut spec = FabPanelSpec::INCHES_12_X_18;
    spec.panel_gap_mm = 5.0;
    fab_panel::execute(
        &[
            sandbox.default_cwd().join("first.xml"),
            sandbox.default_cwd().join("second.xml"),
        ],
        &sandbox.default_cwd().join("fab.xml"),
        spec,
        false,
    )
    .unwrap();

    for (input, scope, kind, step, frame, instance_count, board_count) in [
        (
            "fab.xml",
            "board",
            "board",
            "fab_0_board",
            "selected_board",
            0,
            0,
        ),
        (
            "first.xml",
            "board-array",
            "board_array",
            "array",
            "root_layout",
            8,
            4,
        ),
        (
            "fab.xml",
            "board-array",
            "fab_panel",
            "fab_panel",
            "root_layout",
            18,
            8,
        ),
    ] {
        let path = format!("{input}-{scope}.dfm.json");
        let output = run_pcbc(
            &mut sandbox,
            [
                "ipc",
                "dfm",
                "check",
                input,
                "--pdk",
                "pdk.toml",
                "--layout-target",
                scope,
                "--output",
                &path,
            ],
        );
        assert!(!output.status.success());
        let report = read_report(&sandbox, &path);
        assert_eq!(report["scene"]["schema_version"], 1);
        let layout = &report["layout"];
        assert_eq!(layout["kind"], kind);
        assert_eq!(layout["selected_step"], step);
        assert_eq!(layout["coordinate_frame"], frame);
        if scope == "board" {
            let scene = &report["scene"];
            assert!(scene["bounds"]["max"]["x"].as_f64().unwrap() < 40.0);
            assert!(scene["bounds"]["max"]["y"].as_f64().unwrap() < 40.0);
            assert!(
                scene["passes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|pass| { pass["feature"] != "array_outlines" })
            );
        }
        let instances = layout["instances"].as_array().unwrap();
        assert_eq!(instances.len(), instance_count);
        assert_eq!(
            instances
                .iter()
                .filter(|instance| instance["kind"] == "board")
                .count(),
            board_count
        );
        let spacing = report["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rule| rule["id"] == "panelization.minimum_board_array_spacing")
            .unwrap();
        assert_eq!(
            spacing["status"],
            if kind == "fab_panel" {
                "fail"
            } else {
                "skipped"
            }
        );
    }
}

#[test]
fn zen_dfm_json_reports_preparation_errors_to_stdout_and_file() {
    let mut sandbox = Sandbox::new();
    sandbox.env("SOURCE_DATE_EPOCH", "1787702400");
    let mut arguments = vec!["dfm", "missing.zen", "--pdk", "standard", "--offline"];
    let stdout = run_pcbc(&mut sandbox, &arguments);
    assert!(!stdout.status.success());
    let incomplete: Value = serde_json::from_slice(&stdout.stdout).unwrap();
    assert_incomplete(&incomplete, "missing.zen", "standard", "missing.zen");
    assert_eq!(incomplete["layout_target"], "board");

    sandbox.write("report.dfm.json", r#"{"verdict":"pass"}"#);
    arguments.extend(["--output", "report.dfm.json"]);
    let output = run_pcbc(&mut sandbox, arguments);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(read_report(&sandbox, "report.dfm.json"), incomplete);
    assert_eq!(
        std::fs::read(sandbox.default_cwd().join("report.dfm.json")).unwrap(),
        stdout.stdout
    );
}
