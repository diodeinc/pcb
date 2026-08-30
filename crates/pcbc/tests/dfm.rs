#![cfg(not(target_os = "windows"))]

use std::{collections::BTreeMap, ffi::OsStr, io::Read, path::Path, process::Output};

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

    let bundle_output = run_pcbc(
        &mut sandbox,
        [
            "dfm",
            "MyBoard.zen",
            "--pdk",
            "standard",
            "--format",
            "bundle",
            "--output",
            "report.dfm.tar.zst",
        ],
    );
    assert!(bundle_output.stdout.is_empty());
    let bundle = read_bundle(&sandbox, "report.dfm.tar.zst");
    let scene_report = &bundle.report;
    assert!(report.get("scene").is_none());
    assert_eq!(scene_report["scene"]["schema_version"], 1);
    assert_eq!(scene_report["pdk"], report["pdk"]);
    assert_eq!(scene_report["summary"], report["summary"]);
    assert_eq!(scene_report["layout_target"], "board");
    assert_eq!(scene_report["layout"]["coordinate_frame"], "selected_board");
    assert!(!Path::new(scene_report["input"]["path"].as_str().unwrap()).exists());
    let xml = &bundle.files["source/design.ipc2581.xml"];
    pcb_ipc2581_tools::ipc2581::Ipc2581::parse(std::str::from_utf8(xml).unwrap())
        .expect("the temporary checked XML must survive as a usable bundle source");
    assert_eq!(scene_report["input"]["sha256"], sha256(xml));
    assert_eq!(scene_report["input"]["size_bytes"], xml.len());
    let standard_pdk = include_bytes!("../../pcb-ipc2581-tools/pdks/standard.toml");
    assert_eq!(
        bundle.files["source/pdk.toml"].as_slice(),
        standard_pdk.as_slice()
    );
    assert_eq!(scene_report["pdk"]["sha256"], sha256(standard_pdk));
    assert_eq!(bundle.files.len(), 4);
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

const REPORT_PDK: &str = r#"schema_version = 1

[pdk]
id = "report-fixture"
name = "Report fixture"
revision = "1"

[capabilities.stackup]
minimum_copper_layer_count = 3
maximum_copper_layer_count = 4

[capabilities.copper]
minimum_board_edge_clearance = "1 mm"
"#;

fn read_report(sandbox: &Sandbox, path: &str) -> Value {
    let json = std::fs::read_to_string(sandbox.default_cwd().join(path)).unwrap();
    serde_json::from_str(&json).expect("output should contain a JSON report")
}

struct Bundle {
    files: BTreeMap<String, Vec<u8>>,
    report: Value,
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Read through independent archive libraries and verify that the manifest
/// inventories the actual bytes emitted by the CLI, including partial sources.
fn read_bundle(sandbox: &Sandbox, path: &str) -> Bundle {
    let compressed = std::fs::read(sandbox.default_cwd().join(path)).unwrap();
    let decoder = zstd::stream::read::Decoder::new(compressed.as_slice()).unwrap();
    let mut archive = tar::Archive::new(decoder);
    let mut files = BTreeMap::new();
    let mut paths = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_str().unwrap().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(files.insert(path.clone(), bytes).is_none());
        paths.push(path);
    }
    assert_eq!(paths.first().map(String::as_str), Some("manifest.json"));
    assert_eq!(paths.get(1).map(String::as_str), Some("report.json"));
    let manifest: Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    assert_eq!(manifest["format"], "pcb-dfm-report");
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["report"], "report.json");
    let inventory = manifest["files"].as_array().unwrap();
    assert_eq!(inventory.len(), paths.len() - 1);
    for (file, path) in inventory.iter().zip(&paths[1..]) {
        assert_eq!(file["path"], path.as_str());
        assert_eq!(file["size_bytes"], files[path].len());
        assert_eq!(file["sha256"], sha256(&files[path]));
        assert_eq!(
            file["media_type"],
            match path.as_str() {
                "report.json" => "application/json",
                "source/design.ipc2581.xml" => "application/xml",
                "source/pdk.toml" | "source/waivers.toml" => "application/toml",
                _ => panic!("unexpected bundle member: {path}"),
            }
        );
    }
    let report = serde_json::from_slice(&files["report.json"]).unwrap();
    Bundle { files, report }
}

#[test]
fn ipc_dfm_geometry_supports_stdout_without_changing_plain_json_error_behavior() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("board.xml", IPC_BOARD)
        .write("pdk.toml", REPORT_PDK);
    let output = run_pcbc(
        &mut sandbox,
        [
            "ipc",
            "dfm",
            "check",
            "board.xml",
            "--pdk",
            "pdk.toml",
            "--include-geometry",
        ],
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["verdict"], "fail");
    assert_eq!(report["scene"]["schema_version"], 1);

    for include_geometry in [false, true] {
        let mut arguments = vec!["ipc", "dfm", "check", "missing.xml", "--pdk", "pdk.toml"];
        if include_geometry {
            arguments.push("--include-geometry");
        }
        let output = run_pcbc(&mut sandbox, arguments);
        assert!(!output.status.success());
        if include_geometry {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["verdict"], "incomplete");
            assert!(report.get("findings").is_none());
        } else {
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn ipc_dfm_geometry_preserves_json_and_waivers_and_shares_full_scene() {
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
            "--include-geometry",
            "--output",
            "report.dfm.json",
        ],
    );
    assert!(!scene_output.status.success());
    assert!(scene_output.stdout.is_empty());
    assert!(expected.get("scene").is_none());
    let mut exported = read_report(&sandbox, "report.dfm.json");
    let scene = exported.as_object_mut().unwrap().remove("scene").unwrap();
    assert_eq!(exported, expected);
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
            "--include-geometry",
            "--output",
            "waived.dfm.json",
            "--waivers",
            "waivers.toml",
        ],
    );
    assert!(waived_output.status.success());
    let waived = read_report(&sandbox, "waived.dfm.json");
    assert_eq!(waived["scene"], scene);
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
fn ipc_dfm_bundles_preserve_checked_bytes_and_compressed_input_identity() {
    // Comments and CRLFs must survive; archiving a reserialized source is not
    // equivalent to capturing the exact input used for this check.
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
        let mut json_arguments = arguments.clone();
        json_arguments.extend(["--format", "json", "--include-geometry"]);
        let json_output = run_pcbc(&mut sandbox, json_arguments);
        assert!(!json_output.status.success());
        let expected: Value = serde_json::from_slice(&json_output.stdout).unwrap();

        arguments.extend(["--format", "bundle", "--output", "report.dfm.tar.zst"]);
        let output = run_pcbc(&mut sandbox, &arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let bundle = read_bundle(&sandbox, "report.dfm.tar.zst");
        assert_eq!(bundle.report, expected);
        // Plain JSON's final newline is stdout framing, outside the serialized
        // report object embedded in the bundle.
        assert_eq!(
            bundle.files["report.json"].as_slice(),
            json_output.stdout.strip_suffix(b"\n").unwrap()
        );
        assert_eq!(bundle.report["verdict"], "fail");
        assert_eq!(bundle.report["summary"]["waived"], 1);
        assert_eq!(bundle.report["waivers"]["applied"], 1);
        assert_eq!(bundle.report["scene"]["schema_version"], 1);
        assert_eq!(
            bundle.report["scene"]["passes"].as_array().unwrap().len(),
            3
        );
        assert_eq!(bundle.files.len(), 5);
        assert_eq!(bundle.files["source/design.ipc2581.xml"], xml.as_bytes());
        assert_eq!(bundle.files["source/pdk.toml"], pdk.as_bytes());
        assert_eq!(bundle.files["source/waivers.toml"], waiver.as_bytes());
        assert_eq!(bundle.report["input"]["path"], input);
        assert_eq!(bundle.report["input"]["sha256"], sha256(input_bytes));
        assert_eq!(bundle.report["input"]["size_bytes"], input_bytes.len());
        assert_eq!(bundle.report["pdk"]["sha256"], sha256(pdk.as_bytes()));
        assert_eq!(
            bundle.report["waivers"]["sha256"],
            sha256(waiver.as_bytes())
        );
        if input.ends_with(".zst") {
            assert_ne!(bundle.report["input"]["sha256"], sha256(xml.as_bytes()));
        }

        let archive = std::fs::read(sandbox.default_cwd().join("report.dfm.tar.zst")).unwrap();
        // Explicit geometry is redundant with bundle format. Neither it nor
        // replacing an existing output may perturb a reproducible archive.
        arguments.push("--include-geometry");
        let repeated = run_pcbc(&mut sandbox, arguments);
        assert!(!repeated.status.success());
        assert!(repeated.stdout.is_empty());
        assert_eq!(
            std::fs::read(sandbox.default_cwd().join("report.dfm.tar.zst")).unwrap(),
            archive
        );
    }
}

#[test]
fn ipc_dfm_bundle_replaces_stale_report_with_incomplete_partial_sources() {
    let bad_pdk = REPORT_PDK.replace(
        "minimum_board_edge_clearance",
        "minimum_bord_edge_clearance",
    );
    let mut sandbox = Sandbox::new();
    sandbox
        .write("board.xml", IPC_BOARD)
        .write("pdk.toml", REPORT_PDK)
        .write("bad-pdk.toml", &bad_pdk)
        .write("bad-waivers.toml", "[[waiver]]\nfinding = ");
    let complete = run_pcbc(
        &mut sandbox,
        [
            "ipc",
            "dfm",
            "check",
            "board.xml",
            "--pdk",
            "pdk.toml",
            "--format",
            "bundle",
            "--output",
            "report.dfm.tar.zst",
        ],
    );
    assert!(!complete.status.success());
    assert_eq!(
        read_bundle(&sandbox, "report.dfm.tar.zst").report["verdict"],
        "fail"
    );
    let stale = std::fs::read(sandbox.default_cwd().join("report.dfm.tar.zst")).unwrap();

    for (input, pdk, waivers, expected_error, sources) in [
        (
            "missing.xml",
            "pdk.toml",
            None,
            "failed to read IPC-2581",
            &[][..],
        ),
        (
            "board.xml",
            "missing-pdk.toml",
            None,
            "failed to read PDK",
            &[("source/design.ipc2581.xml", "board.xml")][..],
        ),
        (
            "board.xml",
            "bad-pdk.toml",
            None,
            "failed to parse PDK",
            &[
                ("source/design.ipc2581.xml", "board.xml"),
                ("source/pdk.toml", "bad-pdk.toml"),
            ][..],
        ),
        (
            "board.xml",
            "pdk.toml",
            Some("bad-waivers.toml"),
            "failed to parse waiver file",
            &[
                ("source/design.ipc2581.xml", "board.xml"),
                ("source/pdk.toml", "pdk.toml"),
                ("source/waivers.toml", "bad-waivers.toml"),
            ][..],
        ),
    ] {
        sandbox.write("report.dfm.tar.zst", &stale);
        let mut arguments = vec![
            "ipc",
            "dfm",
            "check",
            input,
            "--pdk",
            pdk,
            "--format",
            "bundle",
            "--output",
            "report.dfm.tar.zst",
        ];
        if let Some(waivers) = waivers {
            arguments.extend(["--waivers", waivers]);
        }
        let output = run_pcbc(&mut sandbox, arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let bundle = read_bundle(&sandbox, "report.dfm.tar.zst");
        assert_eq!(bundle.report["verdict"], "incomplete");
        assert_eq!(bundle.report["input"]["path"], input);
        assert_eq!(bundle.report["pdk"]["path"], pdk);
        assert!(
            bundle.report["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected_error)
        );
        for field in ["summary", "rules", "findings", "scene"] {
            assert!(bundle.report.get(field).is_none(), "unexpected {field}");
        }
        assert_eq!(bundle.files.len(), 2 + sources.len());
        for (member, source) in sources {
            assert_eq!(
                bundle.files[*member],
                std::fs::read(sandbox.default_cwd().join(source)).unwrap()
            );
        }
    }
}

#[test]
fn dfm_bundle_requires_safe_output_before_reading_or_preparing_inputs() {
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
            sandbox.default_cwd().join("layout-report.tar.zst"),
        )
        .unwrap();
        zen_destinations.push("layout-report.tar.zst");
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
        arguments.extend(["--pdk", "pdk.toml", "--format", "bundle"]);
        let missing_output = run_pcbc(&mut sandbox, &arguments);
        assert_eq!(missing_output.status.code(), Some(2));
        assert!(missing_output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&missing_output.stderr).contains("--output"));
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
                "{REPORT_PDK}\n[capabilities.panelization]\nminimum_board_array_spacing = \"7.62 mm\"\n"
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
                "--include-geometry",
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
fn ipc_dfm_scene_reports_incomplete_input_and_extraction_errors() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("broken.xml", "this is not IPC-2581")
        .write("unattributed.xml", IPC_BOARD)
        .write("pdk.toml", REPORT_PDK)
        .write(
            "bad-pdk.toml",
            REPORT_PDK.replace(
                "minimum_board_edge_clearance",
                "minimum_bord_edge_clearance",
            ),
        );

    for (input, pdk, expected_error) in [
        ("missing.xml", "pdk.toml", "failed to read IPC-2581"),
        ("broken.xml", "pdk.toml", "failed to parse IPC-2581"),
        ("unattributed.xml", "standard", "without net attribution"),
        ("unattributed.xml", "bad-pdk.toml", "failed to parse PDK"),
    ] {
        let output_path = format!("{input}.dfm.json");
        sandbox.write(&output_path, r#"{"verdict":"pass"}"#);
        let output = run_pcbc(
            &mut sandbox,
            [
                "ipc",
                "dfm",
                "check",
                input,
                "--pdk",
                pdk,
                "--include-geometry",
                "--output",
                &output_path,
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let incomplete = read_report(&sandbox, &output_path);
        assert_eq!(incomplete["verdict"], "incomplete");
        assert!(
            incomplete["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected_error)
        );
        assert!(incomplete.get("summary").is_none());
        assert!(incomplete.get("findings").is_none());
        assert!(incomplete.get("scene").is_none());
    }
}

#[test]
fn zen_dfm_geometry_and_bundle_report_preparation_errors() {
    let mut sandbox = Sandbox::new();
    for (format, path) in [
        ("json", "report.dfm.json"),
        ("bundle", "report.dfm.tar.zst"),
    ] {
        let output = run_pcbc(
            &mut sandbox,
            [
                "dfm",
                "missing.zen",
                "--pdk",
                "standard",
                "--format",
                format,
                "--include-geometry",
                "--output",
                path,
                "--offline",
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let incomplete = if format == "bundle" {
            let bundle = read_bundle(&sandbox, path);
            assert_eq!(bundle.files.len(), 2, "no source was prepared");
            bundle.report
        } else {
            read_report(&sandbox, path)
        };
        assert_eq!(incomplete["verdict"], "incomplete");
        assert_eq!(incomplete["input"]["path"], "missing.zen");
        assert!(
            incomplete["error"]["message"]
                .as_str()
                .unwrap()
                .contains("missing.zen")
        );
        for field in ["summary", "rules", "findings", "scene"] {
            assert!(incomplete.get(field).is_none(), "unexpected {field}");
        }
    }
}
