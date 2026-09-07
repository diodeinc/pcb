use pcb_corpus::*;
use std::path::Path;

fn fixture(name: &str) -> Fixture {
    load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/v1/{name}.json")),
        "geometry",
    )
    .unwrap()
}

#[test]
fn analytical_area_and_hole_survive_replay() {
    let f = fixture("curves-hole");
    let r = replay(&f, &Geometry);
    assert_eq!(r.status, Status::Completed);
    // Circle approximation is inscribed; allow canonical chord error, not arbitrary golden values.
    let area = r.before_area_mm2.unwrap();
    assert!(area <= std::f64::consts::PI * 100.0);
    assert!(
        (area - std::f64::consts::PI * 100.0).abs()
            < 2.0 * std::f64::consts::PI * 10.0 * f.flatten_mm
    );
    assert!((area - r.after_area_mm2.unwrap() - 16.0).abs() < 1e-8);
    assert!(
        !region(&r.after, f.tolerance_mm)
            .unwrap()
            .contains_point(pcb_ir::geom::Point::new(0.0, 0.0))
    );
    assert_eq!(
        serde_json::to_vec(&r).unwrap(),
        serde_json::to_vec(&replay(&f, &Geometry)).unwrap()
    );
}

#[test]
fn narrow_clearance_is_not_removed_as_noise() {
    let f = fixture("concave-narrow-overhang");
    let r = replay(&f, &Geometry);
    assert_eq!(r.status, Status::Completed);
    assert!((r.before_area_mm2.unwrap() - 244.0).abs() < 1e-8);
    assert!((r.after_area_mm2.unwrap() - 232.6).abs() < 1e-6);
    assert!(
        region(&r.after, f.tolerance_mm)
            .unwrap()
            .contains_point(pcb_ir::geom::Point::new(10.0, 1.95))
    );
    assert!(r.physical_metrics.is_none());
}

#[test]
fn bad_input_and_rejection_are_distinct() {
    let mut f = fixture("complete-removal");
    assert_eq!(replay(&f, &Geometry).status, Status::GeometryRejected);
    f.version = 999;
    assert_eq!(replay(&f, &Geometry).status, Status::MalformedSource);
    assert_eq!(
        load(Path::new("not-a-corpus-file.json"), "geometry")
            .unwrap_err()
            .status,
        Status::MissingSource
    );
    f.version = VERSION;
    f.substrate[0][0][0] = f64::NAN;
    assert_eq!(replay(&f, &Geometry).status, Status::MalformedSource);
}

#[test]
fn unmet_polygon_accuracy_is_a_numerical_failure() {
    let mut f = fixture("complete-removal");
    for point in f.substrate.iter_mut().flatten() {
        point[0] += 1e15;
    }
    let report = replay(&f, &Geometry);
    assert_eq!(report.status, Status::NumericalFailure);
    assert!(report.message.contains("accuracy budget"));
    assert!(report.after_area_mm2.is_none());
}

#[test]
fn independent_component_outcomes_are_not_reclassified() {
    struct Failure(Status);
    impl Component for Failure {
        fn name(&self) -> &str {
            "test-component"
        }
        fn evaluate(&self, f: &Fixture) -> Report {
            Report::outcome(
                &f.id,
                self.name(),
                self.0.clone(),
                "explicit component result",
            )
        }
    }
    let f = fixture("curves-hole");
    for status in [
        Status::PhysicalFailure,
        Status::NumericalFailure,
        Status::SearchFailure,
        Status::Timeout,
        Status::ToleranceAmbiguous,
        Status::Unavailable,
    ] {
        let r = replay(&f, &Failure(status.clone()));
        assert_eq!(r.status, status);
        let page = html(&[(Some(f.clone()), r)]);
        assert!(page.contains("After geometry unavailable"));
        assert!(page.contains("Physical metrics: unavailable"));
    }
}

#[test]
fn source_text_is_escaped_and_failed_geometry_not_rendered() {
    let mut f = fixture("curves-hole");
    f.id = "<script>alert(1)</script>".into();
    f.tolerance_mm = -1.0;
    let r = replay(&f, &Geometry);
    let page = html(&[(Some(f), r)]);
    assert!(!page.contains("<script>"));
    assert!(!page.contains("<svg"));
    assert!(page.contains("malformed_source"));
}

#[test]
fn compressed_real_sources_replay_without_source_files() {
    for name in [
        "demo-dm0003",
        "workspace-dm0002",
        "demo-bramble",
        "demo-demeter",
        "demo-feign",
        "demo-governor",
        "demo-marlow",
        "demo-renfield",
        "demo-seward",
    ] {
        let f = load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/v1/{name}.json.zst")),
            "geometry",
        )
        .unwrap();
        assert_eq!(replay(&f, &Geometry).status, Status::Completed);
        assert_eq!(f.provenance.sha256.as_ref().unwrap().len(), 64);
        assert!(f.overlays.iter().any(|o| o.name.starts_with("copper")));
    }
}

#[test]
fn cli_order_is_deterministic_and_failures_still_write_reports() {
    let dir = std::env::temp_dir().join(format!("pcb-corpus-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/v1/curves-hole.json");
    let bad = dir.join("malformed.json");
    std::fs::write(&bad, "not json").unwrap();
    let missing = dir.join("missing.json");
    let run = |paths: &[&Path]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_pcb-corpus"))
            .arg(&dir)
            .args(paths)
            .output()
            .unwrap()
    };
    assert!(!run(&[&good, &bad, &missing]).status.success());
    let first = std::fs::read(dir.join("results.json")).unwrap();
    assert!(!run(&[&missing, &bad, &good]).status.success());
    assert_eq!(first, std::fs::read(dir.join("results.json")).unwrap());
    let reports: Vec<Report> = serde_json::from_slice(&first).unwrap();
    assert!(reports.iter().any(|r| r.status == Status::MalformedSource));
    assert!(reports.iter().any(|r| r.status == Status::MissingSource));
    assert!(dir.join("index.html").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
