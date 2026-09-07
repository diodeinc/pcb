use std::process::Command;

use pcb_ir::geom::{GeometryAccuracy, Resolution};

#[test]
fn accuracy_reaches_gerber_normalize_compare_and_render() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("curve.gbr");
    // A round-ended arc requires preparation rather than just copying vertices.
    let source = "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.2*%\nD10*\nG75*\nX1000000Y0D02*\nG03*\nX0Y1000000I-1000000J0D01*\nM02*\n";
    std::fs::write(&file, source).unwrap();
    let parsed = gerberx2::GerberX2::parse(source).unwrap();
    let mut renders = Vec::new();
    let mut comparisons = Vec::new();
    for accuracy_um in [1, 30, 100] {
        let resolution =
            Resolution::default().with_accuracy(GeometryAccuracy::micrometres(accuracy_um));
        let accuracy_arg = accuracy_um.to_string();
        let normalize = Command::new(env!("CARGO_BIN_EXE_pcbc"))
            .args(["--accuracy-um", &accuracy_arg, "gerber", "normalize"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            normalize.status.success(),
            "{}",
            String::from_utf8_lossy(&normalize.stderr)
        );
        assert_eq!(
            normalize.stdout,
            gerberx2::from_artwork::normalize_layer(&parsed, resolution.accuracy)
                .unwrap()
                .as_bytes()
        );

        let compare = Command::new(env!("CARGO_BIN_EXE_pcbc"))
            .args(["gerber", "compare"])
            .arg(&file)
            .arg(&file)
            .args(["--accuracy-um", &accuracy_arg])
            .output()
            .unwrap();
        assert!(
            compare.status.success(),
            "{}",
            String::from_utf8_lossy(&compare.stderr)
        );
        comparisons.push(compare.stdout);

        let render = Command::new(env!("CARGO_BIN_EXE_pcbc"))
            .args(["gerber", "render"])
            .arg(&file)
            .args(["--format", "svg", "--accuracy-um", &accuracy_arg])
            .output()
            .unwrap();
        assert!(
            render.status.success(),
            "{}",
            String::from_utf8_lossy(&render.stderr)
        );
        let artwork = gerberx2::geometry::extract_document(&parsed, resolution.accuracy).unwrap();
        let mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork, resolution).unwrap();
        assert_eq!(
            render.stdout,
            pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::default()).as_bytes()
        );
        renders.push(render.stdout);
    }
    assert_ne!(
        renders[0], renders[1],
        "fixture must exercise accuracy-dependent preparation"
    );
    assert_ne!(
        comparisons[0], comparisons[1],
        "compare must measure the requested preparation"
    );
}
