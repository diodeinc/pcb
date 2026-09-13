use std::path::Path;
use std::process::Command;

use base64::Engine;

const HEAD: &str = r#"(kicad_pcb (version 20241229) (generator "pcbnew")
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (25 "Edge.Cuts" user) (37 "F.SilkS" user))
"#;

fn rect_outline(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    format!(
        r#"(gr_line (start {x0} {y0}) (end {x1} {y0}) (layer "Edge.Cuts"))
(gr_line (start {x1} {y0}) (end {x1} {y1}) (layer "Edge.Cuts"))
(gr_line (start {x1} {y1}) (end {x0} {y1}) (layer "Edge.Cuts"))
(gr_line (start {x0} {y1}) (end {x0} {y0}) (layer "Edge.Cuts"))
"#
    )
}

/// Encode bytes the way KiCad embeds files: zstd, then base64.
fn embed(name: &str, data: &[u8]) -> String {
    let compressed = zstd::bulk::compress(data, 3).unwrap();
    let text = base64::engine::general_purpose::STANDARD.encode(compressed);
    format!(r#"(embedded_files (file (name "{name}") (type model) (data |{text}|)))"#)
}

fn export(board: &Path, output: &Path, flags: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args(["step", "export"])
        .arg(board)
        .arg("-o")
        .arg(output)
        .args(flags)
        .output()
        .unwrap()
}

fn export_ok(board: &Path, output: &Path, flags: &[&str]) -> String {
    let result = export(board, output, flags);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stderr).into_owned()
}

/// A board with an outline, silkscreen text, one footprint whose model is
/// embedded (itself an export of a 2x2 mm board) and one DNP footprint
/// whose model is not on disk.
fn write_board(dir: &Path) -> std::path::PathBuf {
    let donor_board = dir.join("donor.kicad_pcb");
    std::fs::write(
        &donor_board,
        format!("{HEAD}{})", rect_outline(0.0, 0.0, 2.0, 2.0)),
    )
    .unwrap();
    let donor = dir.join("donor.step");
    export_ok(&donor_board, &donor, &[]);
    let donor = std::fs::read(&donor).unwrap();

    let board = dir.join("board.kicad_pcb");
    std::fs::write(
        &board,
        format!(
            r#"{HEAD}
(footprint "Lib:A" (layer "F.Cu") (at 10 10) (property "Reference" "U1")
  (model "kicad-embed://cube.step" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
(footprint "Lib:B" (layer "F.Cu") (at 20 10) (property "Reference" "J1") (attr dnp)
  (model "${{KICAD10_3DMODEL_DIR}}/Nowhere.3dshapes/missing.step"))
(gr_text "${{PCB_VERSION}}" (at 15 5) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
{}{})"#,
            rect_outline(0.0, 0.0, 30.0, 20.0),
            embed("cube.step", &donor)
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("board.kicad_pro"),
        r#"{"text_variables": {"PCB_VERSION": "1.2.3"}}"#,
    )
    .unwrap();
    board
}

#[test]
fn step_export_places_embedded_models_and_names_the_assembly_after_the_board() {
    let directory = tempfile::tempdir().unwrap();
    let board = write_board(directory.path());
    let output = directory.path().join("out.step");
    let stderr = export_ok(&board, &output, &[]);
    assert!(
        stderr.contains(
            "warning: could not find 3D model: ${KICAD10_3DMODEL_DIR}/Nowhere.3dshapes/missing.step"
        ),
        "{stderr}"
    );

    let step = std::fs::read_to_string(&output).unwrap();
    assert!(step.starts_with("ISO-10303-21;"));
    assert!(step.ends_with("END-ISO-10303-21;\n"));
    assert!(step.contains("PRODUCT('board',"), "{step}");
    assert!(step.contains("PRODUCT('board_PCB',"), "{step}");
    // U1's model and the board body: the donor's own board solid is copied once.
    assert_eq!(step.matches("MANIFOLD_SOLID_BREP").count(), 2);
    assert!(
        step.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','U1'"),
        "{step}"
    );
    assert!(
        step.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE('2','PCB'"),
        "{step}"
    );
}

#[test]
fn step_export_honours_kicad_cli_flags() {
    let directory = tempfile::tempdir().unwrap();
    let board = write_board(directory.path());
    let output = directory.path().join("out.step");
    let stderr = export_ok(&board, &output, &["--no-dnp", "--drill-origin"]);
    assert!(!stderr.contains("could not find 3D model"), "{stderr}");

    let step = std::fs::read_to_string(&output).unwrap();
    assert!(step.contains("PRODUCT('board_PCB',"));
    assert!(step.contains("PRODUCT('board_silkscreen',"), "{step}");
    assert!(
        step.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','U1'"),
        "{step}"
    );

    let body_only = directory.path().join("body.step");
    export_ok(
        &board,
        &body_only,
        &["--no-components", "--no-silkscreen", "--no-soldermask"],
    );
    let step = std::fs::read_to_string(&body_only).unwrap();
    assert_eq!(step.matches("NEXT_ASSEMBLY_USAGE_OCCURRENCE").count(), 1);
}

#[test]
fn step_export_rejects_a_board_without_an_outline_and_leaves_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let board = directory.path().join("empty.kicad_pcb");
    std::fs::write(&board, format!("{HEAD})")).unwrap();
    let output = directory.path().join("empty.step");
    let result = export(&board, &output, &[]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("no Edge.Cuts outline"), "{stderr}");
    assert!(!output.exists());
}

#[test]
fn step_export_refuses_the_board_as_output_and_keeps_the_previous_output() {
    let directory = tempfile::tempdir().unwrap();
    let board = write_board(directory.path());
    let before = std::fs::read(&board).unwrap();
    let result = export(&board, &board, &[]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("is the board itself"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&board).unwrap(), before);

    // A failed export leaves the previous output alone and no partial file.
    let output = directory.path().join("out.step");
    export_ok(&board, &output, &[]);
    let good = std::fs::read(&output).unwrap();
    let empty = directory.path().join("empty.kicad_pcb");
    std::fs::write(&empty, format!("{HEAD})")).unwrap();
    assert!(!export(&empty, &output, &[]).status.success());
    assert_eq!(std::fs::read(&output).unwrap(), good);
    assert!(!directory.path().join("out.step.part").exists());
}
