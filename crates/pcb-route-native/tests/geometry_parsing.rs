use pcb_route_native::parse_board;

const TWO_PAD_BOARD: &str = include_str!("fixtures/two_pad_board.kicad_pcb");

#[test]
fn parses_footprint_count_and_references() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    assert_eq!(board.footprints.len(), 2);
    let refs: Vec<_> = board
        .footprints
        .iter()
        .map(|f| f.reference.clone().unwrap())
        .collect();
    assert_eq!(refs, vec!["R1", "R2"]);
}

#[test]
fn resolves_pad_position_through_unrotated_footprint() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    let r1 = &board.footprints[0];
    assert_eq!(r1.rotation, 0.0);
    // R1 at (5, 5), rot 0; pad 1 local (-0.51, 0) -> absolute (4.49, 5.0).
    let pad1 = r1.pads.iter().find(|p| p.number == "1").unwrap();
    assert!((pad1.position.x - 4.49).abs() < 1e-9);
    assert!((pad1.position.y - 5.0).abs() < 1e-9);
    assert_eq!(pad1.net_name.as_deref(), Some("VCC"));
}

#[test]
fn resolves_pad_position_through_rotated_footprint() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    let r2 = &board.footprints[1];
    assert_eq!(r2.rotation, 90.0);
    // R2 at (15, 10), rot 90; pad 1 local (-0.51, 0).
    // Clockwise rotation by 90 degrees maps local (x, y) -> (-y, x), so
    // (-0.51, 0) -> (0, -0.51) -> absolute (15.0, 9.49).
    let pad1 = r2.pads.iter().find(|p| p.number == "1").unwrap();
    assert!(
        (pad1.position.x - 15.0).abs() < 1e-9,
        "x = {}",
        pad1.position.x
    );
    assert!(
        (pad1.position.y - 9.49).abs() < 1e-9,
        "y = {}",
        pad1.position.y
    );
}

#[test]
fn parses_edge_cuts_outline() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    let outline = board.outline.expect("fixture has an Edge.Cuts gr_rect");
    assert_eq!(outline.min.x, 0.0);
    assert_eq!(outline.min.y, 0.0);
    assert_eq!(outline.max.x, 20.0);
    assert_eq!(outline.max.y, 15.0);
}
