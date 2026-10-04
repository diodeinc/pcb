use pcb_kicad::run_drc;
use pcb_zen_core::Diagnostics;
use std::fs;
use std::path::Path;
use std::process::Command;

// Run environment-dependent tests in a child process: KICAD_CLI must not race
// other tests using the real installation.
#[test]
fn drc_child() {
    let Ok(root) = std::env::var("PCB_DRC_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let mut diagnostics = Diagnostics::default();
    let report = run_drc(
        "layout.kicad_pcb",
        false,
        Some(Path::new("work")),
        Path::new("report.json"),
        Path::new("source/layout.kicad_pcb"),
        &mut diagnostics,
    )
    .unwrap();
    if let Ok(expected) = std::env::var("PCB_DRC_TEST_ERROR") {
        assert!(report.is_none());
        assert_eq!(diagnostics.len(), 1);
        let error = &diagnostics[0].body;
        assert!(error.contains(expected.as_str()), "{error}");
        assert!(error.contains("rule \"isolation\""), "{error}");
        assert_eq!(diagnostics[0].path, "source/layout.kicad_dru");
        assert!(diagnostics[0].span.is_some());
        assert!(!root.join("invoked").exists());
        assert!(!root.join("work/report.json").exists());
    } else {
        assert_eq!(report.unwrap().source, "layout.kicad_pcb");
        assert!(root.join("invoked").exists());
        assert!(root.join("work/report.json").exists());
        assert!(!root.join("report.json").exists());
    }
}

#[cfg(unix)]
#[test]
fn preflight_blocks_cli_and_resolves_its_actual_working_directory() {
    use std::os::unix::fs::PermissionsExt;
    for (rules, error) in [
        (
            Some("(version 1)\n(rule \"isolation\" (constraint clearance (min 0.11)))"),
            Some("missing units"),
        ),
        (
            Some("(version 1)\n(rule \"isolation\" (constraint clearance (min 5mm))"),
            Some("unclosed '('"),
        ),
        (
            Some("(version 1)\n(rule \"isolation\" (constraint clearance (min 2 * 2.5mm)))"),
            None,
        ),
        (None, None),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("work")).unwrap();
        fs::write(root.path().join("work/layout.kicad_pcb"), "board").unwrap();
        // Wrong-directory decoys ensure the preflight checks the subprocess
        // input, not the similarly named file relative to the caller.
        fs::write(root.path().join("layout.kicad_pcb"), "decoy").unwrap();
        fs::write(
            root.path().join("layout.kicad_dru"),
            "(rule \"decoy\" (constraint clearance (min 0.11)))",
        )
        .unwrap();
        if let Some(rules) = rules {
            fs::write(root.path().join("work/layout.kicad_dru"), rules).unwrap();
        }
        let cli = root.path().join("kicad-cli");
        fs::write(&cli, r#"#!/bin/sh
set -eu
: > "$PCB_DRC_TEST_ROOT/invoked"
while [ "$1" != '--output' ]; do shift; done
shift
printf '%s' '{"coordinate_units":"mm","date":"","kicad_version":"test","source":"layout.kicad_pcb","violations":[]}' > "$1"
"#).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "drc_child", "--nocapture"])
            .current_dir(root.path())
            .env("PCB_DRC_TEST_ROOT", root.path())
            .env("KICAD_CLI", &cli);
        if let Some(error) = error {
            child.env("PCB_DRC_TEST_ERROR", error);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "requires installed kicad-cli; run explicitly for KiCad compatibility"]
fn real_kicad_custom_rules_and_exclusions() {
    let root = tempfile::tempdir().unwrap();
    let board = root.path().join("layout.kicad_pcb");
    fs::write(
        &board,
        include_str!("../../pcb-layout/tests/resources/graphics/module/layout.kicad_pcb"),
    )
    .unwrap();
    let mut project: serde_json::Value = serde_json::from_str(include_str!(
        "../../pcb-layout/tests/resources/graphics/module/layout.kicad_pro"
    ))
    .unwrap();
    project["board"]["design_settings"]["drc_exclusions"] = serde_json::json!([[
        "lib_footprint_issues|147000000|104000000|b4348631-462e-477f-bbb9-7190db41b065|00000000-0000-0000-0000-000000000000",
        "preflight exclusion control"
    ]]);
    fs::write(board.with_extension("kicad_pro"), project.to_string()).unwrap();
    let rules = board.with_extension("kicad_dru");
    let report_path = root.path().join("report.json");
    // The two pads are 0.85mm apart: the implicit 0.2mm rule cannot produce
    // this violation. This proves the named custom rule actually loaded.
    fs::write(&rules, "# KiCad comment\n(version 1)\n(rule \"preflight-positive-control\"\n (condition \"A.memberOfFootprint('R1')\")\n (constraint clearance (min \"2 * (1mm + 1.5mm)\")))").unwrap();
    let mut diagnostics = Diagnostics::default();
    let report = run_drc(
        &board,
        false,
        Some(root.path()),
        &report_path,
        &board,
        &mut diagnostics,
    )
    .unwrap()
    .unwrap();
    assert_eq!(diagnostics.warning_count(), 0);
    eprintln!("Executed real KiCad {}", report.kicad_version);
    let clearances = report
        .violations
        .iter()
        .filter(|v| v.violation_type == "clearance")
        .collect::<Vec<_>>();
    assert_eq!(clearances.len(), 1);
    assert!(
        clearances[0]
            .description
            .contains("preflight-positive-control")
    );
    assert!(clearances[0].description.contains("5.0000 mm"));
    assert!(clearances[0].description.contains("0.8500 mm"));
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.violation_type == "lib_footprint_issues" && v.excluded)
    );

    // CLI rules follow the board filename, even when the only project has a
    // different name and references this board. Invalid project-named rules
    // must not block checks of the valid, active board-named rules.
    project["project_refs"] = serde_json::json!(["layout.kicad_pcb"]);
    fs::write(
        root.path().join("controller.kicad_pro"),
        project.to_string(),
    )
    .unwrap();
    fs::remove_file(board.with_extension("kicad_pro")).unwrap();
    fs::write(
        root.path().join("controller.kicad_dru"),
        "(version 1)\n(rule \"inactive\" (constraint clearance (min 0.11)))",
    )
    .unwrap();
    diagnostics = Diagnostics::default();
    let report = run_drc(
        &board,
        false,
        Some(root.path()),
        &report_path,
        &board,
        &mut diagnostics,
    )
    .unwrap()
    .unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.description.contains("preflight-positive-control"))
    );

    let original = fs::read(&report_path).unwrap();
    fs::write(
        &rules,
        "(version 1)\n(rule \"invalid\" (constraint clearance (min 0.11)))",
    )
    .unwrap();
    diagnostics = Diagnostics::default();
    assert!(
        run_drc(
            &board,
            false,
            Some(root.path()),
            &report_path,
            &board,
            &mut diagnostics
        )
        .unwrap()
        .is_none()
    );
    assert!(diagnostics[0].body.contains("missing units"));
    // A previous report is not overwritten or returned as a successful run.
    assert_eq!(fs::read(&report_path).unwrap(), original);
}
