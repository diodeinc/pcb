use super::*;
use pcb_ir::geom::attachment::outline::{OutlineObstacle, eligible_outline};

fn config() -> Config {
    Config {
        routing_gap_mm: 0.5,
        frame_landing_mm: 0.3,
        max_span_mm: 3.0,
        candidate_pitch_mm: 3.5,
        max_candidates: 32,
        bending: [[10.0, 2.0, 0.0], [2.0, 10.0, 0.0], [0.0, 0.0, 4.0]],
        connection_stiffness: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
        scales: [1.0, 0.1],
        mesh_max_area_mm2: 20.0,
        mesh_min_angle_degrees: 0.0,
        mesh_max_additional_vertices: 100,
        max_dofs: 500,
        max_subsets: 16,
        clamp_sides: [false, true, false, false],
        load_cases: vec![Case {
            resultant_per_board: [1.0, 0.3, -0.7],
            compliance_limit: 100.0,
        }],
    }
}

fn footprint() -> OutlineFootprint {
    OutlineFootprint {
        width_mm: 1.0,
        inward_mm: 0.25,
        outward_mm: 1.0,
    }
}

fn options() -> BoardArrayCreateOptions {
    BoardArrayCreateOptions {
        columns: 1,
        rows: 1,
        board_margin_mm: super::super::BoardMarginMm::all(2.0),
        edge_rail_mm: super::super::BoardMarginMm::all(1.0),
    }
}

#[test]
fn sampling_uses_connected_arclength_not_polygon_fragment_count() {
    let seed = prepared(false, false).intervals[0].clone();
    let fragments = (0..20)
        .map(|edge| OutlineInterval {
            edge,
            start_mm: edge as f64 * 0.65,
            end_mm: (edge + 1) as f64 * 0.65,
            state: OutlineState::Eligible,
            ..seed.clone()
        })
        .collect::<Vec<_>>();
    let samples = sample_intervals(&fragments, 4.0)
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        samples.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
        vec![1.625, 4.875, 8.125, 11.375]
    );
    assert!(sample_intervals(&fragments, f64::MIN_POSITIVE).is_err());
    let mut gap = fragments;
    for i in &mut gap[7..11] {
        i.state = OutlineState::Unknown;
    }
    let samples = sample_intervals(&gap, 4.0).unwrap().collect::<Vec<_>>();
    assert_eq!(samples.len(), 4);
    assert!(samples.iter().all(|(_, s)| *s < 4.55 || *s > 7.15));
}

fn prepared(missing: bool, hole: bool) -> eligibility::Prepared {
    let resolution = Resolution::default().strict();
    let mut substrate = ContourSet::rectangle(
        BBox::new(
            Point::new(0.0, 0.0),
            if hole {
                Point::new(8.0, 7.0)
            } else {
                Point::new(4.0, 3.0)
            },
        ),
        resolution,
    );
    if hole {
        substrate = substrate
            .difference(&ContourSet::rectangle(
                BBox::new(Point::new(2.0, 2.0), Point::new(6.0, 5.0)),
                resolution,
            ))
            .unwrap();
    }
    let evidence = if missing {
        vec![eligibility::Evidence {
            id: "missing-bottom".into(),
            region: None,
        }]
    } else {
        vec![]
    };
    let obstacles = evidence
        .iter()
        .map(|e| OutlineObstacle {
            id: &e.id,
            region: e.region.as_ref(),
        })
        .collect::<Vec<_>>();
    let intervals = eligible_outline(
        &substrate,
        &obstacles,
        footprint(),
        QueryTolerance {
            boundary_mm: 0.0,
            numerical_mm: 1e-9,
        },
    )
    .unwrap();
    eligibility::Prepared {
        report: json!({}),
        substrate,
        evidence,
        intervals,
    }
}

#[test]
fn frame_only_sites_select_a_verified_count_under_asymmetric_loading() {
    let result = plan(
        prepared(false, false),
        footprint(),
        &options(),
        &config(),
        Resolution::default().strict(),
    )
    .unwrap();
    let candidates = result["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 4, "{result}");
    for candidate in candidates {
        assert_eq!(candidate["board"], 0);
        assert!((candidate["span_mm"].as_f64().unwrap() - 0.5).abs() < 1e-12);
        let p = &candidate["frame_point"];
        let x = p[0].as_f64().unwrap();
        let y = p[1].as_f64().unwrap();
        assert!(x == 2.5 || x == 7.5 || y == 2.5 || y == 6.5);
    }
    // Three-channel positive stiffness can restrain the three board rigid modes
    // with one tab; the very loose supplied limit makes count one feasible.
    assert_eq!(result["mechanics"]["tab_count"], 1, "{result}");
    let response = &result["mechanics"]["responses"][0];
    assert_eq!(response["status"], "Stable");
    assert!(response["compliance_n_mm"].as_f64().unwrap() <= 100.0);
    assert!(response["relative_residual"].as_f64().unwrap() < 1e-8);
    assert_eq!(result["manufacturing_ready"], false);
    assert!(result.get("xml").is_none());
}

#[test]
fn missing_evidence_and_unreachable_frame_do_not_fall_back_to_board_connections() {
    for (missing, span) in [(true, 3.0), (false, 0.4)] {
        let mut cfg = config();
        cfg.max_span_mm = span;
        let mut layout = options();
        layout.columns = 2;
        let report = plan(
            prepared(missing, false),
            footprint(),
            &layout,
            &cfg,
            Resolution::default().strict(),
        )
        .unwrap();
        assert!(report["candidates"].as_array().unwrap().is_empty());
        assert_eq!(report["mechanics"]["status"], "no-proven-frame-candidates");
        assert!(report["mechanics"]["selected_ids"].is_null());
        if !missing {
            assert!(!report["rejected"].as_array().unwrap().is_empty());
        }
    }
    let mut layout = options();
    layout.board_margin_mm = super::super::BoardMarginMm::all(0.5);
    let error = plan(
        prepared(false, false),
        footprint(),
        &layout,
        &config(),
        Resolution::default().strict(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("connected internal frame"));
}

#[test]
fn candidates_from_holes_cannot_cross_their_own_board_to_reach_frame() {
    let mut cfg = config();
    cfg.max_subsets = 1;
    cfg.max_span_mm = 10.0;
    let report = plan(
        prepared(false, true),
        footprint(),
        &options(),
        &cfg,
        Resolution::default().strict(),
    )
    .unwrap();
    assert!(
        report["rejected"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["reason"]
                .as_str()
                .unwrap()
                .contains("re-enters its board")),
        "{report}"
    );
    assert!(
        report["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["ring"] == 0)
    );
    assert_eq!(report["mechanics"]["status"], "BudgetExhausted");
    assert_eq!(
        report["mechanics"]["selected_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn complete_connection_checks_obstacles_beyond_the_eligibility_band() {
    let mut source = prepared(false, false);
    source.evidence.push(eligibility::Evidence {
        id: "remote-bottom-overhang".into(),
        region: Some(ContourSet::rectangle(
            BBox::new(Point::new(-1.6, 1.0), Point::new(-1.1, 2.0)),
            Resolution::default().strict(),
        )),
    });
    // This obstacle lies entirely beyond the 1 mm eligibility band but inside
    // the proposed 1.5 mm connection. A center/band-only check would accept it.
    let mut cfg = config();
    cfg.routing_gap_mm = 1.5;
    // Four sampled sites, but the rejected overhang must not consume a slot.
    cfg.max_candidates = 3;
    let report = plan(
        source,
        footprint(),
        &options(),
        &cfg,
        Resolution::default().strict(),
    )
    .unwrap();
    assert_eq!(
        report["candidates"].as_array().unwrap().len(),
        3,
        "{report}"
    );
    assert!(report["rejected"].as_array().unwrap().iter().any(|r| {
        r["reason"]
            .as_str()
            .unwrap()
            .contains("remote-bottom-overhang")
    }));
}

#[test]
fn slanted_board_edges_have_full_width_frame_landings() {
    let resolution = Resolution::default().strict();
    let mut source = prepared(false, false);
    source.substrate = transform_region(
        &source.substrate,
        Affine2::placement(Point::new(0.0, 0.0), 12.0, Default::default(), 1.0),
    )
    .unwrap();
    source.intervals = eligible_outline(
        &source.substrate,
        &[],
        footprint(),
        QueryTolerance {
            boundary_mm: 0.0,
            numerical_mm: 1e-9,
        },
    )
    .unwrap();
    let mut cfg = config();
    cfg.max_subsets = 1;
    let report = plan(source, footprint(), &options(), &cfg, resolution).unwrap();
    assert!(
        !report["candidates"].as_array().unwrap().is_empty(),
        "{report}"
    );
    for c in report["candidates"].as_array().unwrap() {
        assert!((c["frame_landing_area_mm2"].as_f64().unwrap() - 0.3).abs() < 1e-12);
    }
}

#[test]
fn continuous_landing_search_handles_unsampled_notches_and_later_gaps() {
    let resolution = Resolution::default().strict();
    let rect = |x0, y0, x1, y1| {
        ContourSet::rectangle(
            BBox::new(Point::new(x0, y0), Point::new(x1, y1)),
            resolution,
        )
    };
    let origin = Point::new(1.0, -0.5);
    let normal = Point::new(0.0, 1.0);
    let corridor = rect(0.0, -0.5, 2.0, 4.0);
    let stock = rect(-1.0, 0.0, 3.0, 4.0);
    // The notch lies between all three former probes (x=0,1,2).
    let frame = stock.difference(&rect(0.2, 0.0, 0.3, 1.0)).unwrap();
    let start = landing_start(&frame, &corridor, origin, normal, 0.3, 0.01).unwrap();
    assert!((start - 1.51).abs() < 1e-12);
    // A later void leaves a 0.4 mm rail. A 0.3 mm landing fits before it;
    // a 0.5 mm landing must skip it. Neither a ray nor a max projection suffices.
    let frame = frame.difference(&rect(-1.0, 1.4, 3.0, 2.0)).unwrap();
    for (depth, expected) in [(0.3, 1.51), (0.5, 2.51)] {
        let start = landing_start(&frame, &corridor, origin, normal, depth, 0.01).unwrap();
        assert!((start - expected).abs() < 1e-12);
        let landing = rect(0.0, origin.y + start, 2.0, origin.y + start + depth);
        assert!(landing.difference(&frame).unwrap().is_empty());
    }
    assert_eq!(
        landing_start(&corridor, &corridor, origin, normal, 0.3, 0.01).unwrap(),
        0.01
    );
}

#[test]
fn mechanical_budget_failure_preserves_candidate_evidence() {
    let mut cfg = config();
    cfg.max_dofs = 1;
    let report = plan(
        prepared(false, false),
        footprint(),
        &options(),
        &cfg,
        Resolution::default().strict(),
    )
    .unwrap();
    assert!(!report["candidates"].as_array().unwrap().is_empty());
    assert_eq!(report["mechanics"]["status"], "analysis-failed");
    assert!(
        report["mechanics"]["error"]
            .as_str()
            .unwrap()
            .contains("exceeding max_dofs 1")
    );
    assert!(report["mechanics"]["selected_ids"].is_null());
    assert_eq!(report["manufacturing_ready"], false);
}

#[test]
fn every_repeated_board_is_connected_to_frame_not_to_its_neighbor() {
    let mut layout = options();
    layout.columns = 2;
    let mut cfg = config();
    cfg.max_subsets = 64;
    let report = plan(
        prepared(false, false),
        footprint(),
        &layout,
        &cfg,
        Resolution::default().strict(),
    )
    .unwrap();
    assert_eq!(report["mechanics"]["tab_count"], 2, "{report}");
    let candidates = report["candidates"].as_array().unwrap();
    let selected = report["mechanics"]["selected_ids"].as_array().unwrap();
    let mut owners = selected
        .iter()
        .map(|id| {
            candidates[id.as_u64().unwrap() as usize]["board"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    owners.sort();
    assert_eq!(owners, vec![0, 1]);
}

#[test]
fn imported_repeated_board_has_a_verified_placement() {
    // IPC preparation has a different cyclic origin from rectangle(). This
    // exposed QR nonconvergence on a zero-padded rank-three connection matrix.
    let xml = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
      <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="board"/></Content>
      <Ecad><CadHeader units="MILLIMETER"/><CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="board" type="BOARD"><Datum x="0" y="0"/>
      <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="4" y="0"/>
      <PolyStepSegment x="4" y="3"/><PolyStepSegment x="0" y="3"/>
      <PolyStepSegment x="0" y="0"/></Polygon></Profile></Step></CadData></Ecad></IPC-2581>"#;
    let mut layout = options();
    layout.columns = 2;
    let mut cfg = config();
    cfg.max_subsets = 64;
    let report = analyze(xml, footprint(), 0.0, &layout, &cfg, Resolution::default()).unwrap();
    assert_eq!(report["mechanics"]["tab_count"], 2, "{report}");
    assert_eq!(report["mechanics"]["responses"][0]["status"], "Stable");
}
