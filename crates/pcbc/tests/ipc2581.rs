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
    let expected: Value = serde_json::from_str(include_str!(
        "../../pcb-ipc2581-tools/src/assembly/testdata/report-v1.json"
    ))
    .unwrap();

    assert_eq!(first_json, second_json);
    assert_eq!(report, expected);
}

#[test]
fn assembly_command_selects_canonical_board_scope() {
    let (_, report) = assembly_report("board");

    assert_eq!(report["scope"]["kind"], "board");
    assert_eq!(report["summary"]["board_occurrences"], 1);
    assert_eq!(report["summary"]["components"]["total"], 4);
    assert_eq!(report["summary"]["terminations"]["total"], 3);
}
