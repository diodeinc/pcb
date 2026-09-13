use std::f64::consts::PI;

use crate::board::Board;
use crate::donor::{Donor, push_real};
use crate::geom::{Vec2, Vec3};
use crate::holes::RoundHole;
use crate::outline::{Edge, Frame, Loop, board_solids, cut_holes, cut_round};
use crate::{Options, export};

fn real(v: f64) -> String {
    let mut out = Vec::new();
    push_real(&mut out, v);
    String::from_utf8(out).unwrap()
}

#[test]
fn reals_are_written_like_occt() {
    assert_eq!(real(0.0), "0.");
    assert_eq!(real(1.0), "1.");
    assert_eq!(real(-0.5), "-0.5");
    assert_eq!(real(150.16), "150.16");
    assert_eq!(real(0.807843143701), "0.807843143701");
    assert_eq!(real(0.0980392180264), "9.80392180264E-02");
    assert_eq!(real(1e-6), "1.E-06");
    assert_eq!(real(1234567890123.0), "1.23456789012E+12");
    assert_eq!(real(9.9999999999999), "10.");
    assert_eq!(real(0.123456789012345), "0.123456789012");
    assert_eq!(real(100.0), "100.");
}

const HEAD: &str = r#"(kicad_pcb (version 20241229) (generator "pcbnew")
  (general (thickness 1.6) (legacy_teardrops no))
  (layers (0 "F.Cu" signal) (4 "In1.Cu" signal) (6 "In2.Cu" signal) (2 "B.Cu" signal) (25 "Edge.Cuts" user))
  (setup (aux_axis_origin 10 20) (grid_origin 1 2)
    (stackup
      (layer "F.Mask" (type "Top Solder Mask") (color "Black") (thickness 0.01))
      (layer "F.Cu" (type "copper") (thickness 0.035))
      (layer "dielectric 1" (type "prepreg") (thickness 0.2104))
      (layer "In1.Cu" (type "copper") (thickness 0.0152))
      (layer "dielectric 2" (type "core") (thickness 1.065) (addsublayer) (thickness 0.1))
      (layer "In2.Cu" (type "copper") (thickness 0.0152))
      (layer "dielectric 3" (type "prepreg") (thickness 0.2104))
      (layer "B.Cu" (type "copper") (thickness 0.035))
      (layer "B.Mask" (type "Bottom Solder Mask") (color "Black") (thickness 0.01))))
"#;

fn rect_outline(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    format!(
        r#"(gr_line (start {x0} {y0}) (end {x1} {y0}) (layer "Edge.Cuts"))
(gr_line (start {x1} {y0}) (end {x1} {y1}) (stroke (width 0.05) (type default)) (layer "Edge.Cuts"))
(gr_line (start {x1} {y1}) (end {x0} {y1}) (layer "Edge.Cuts"))
(gr_line (start {x0} {y1}) (end {x0} {y0}) (layer "Edge.Cuts"))
"#
    )
}

fn parse(body: &str) -> String {
    format!("{HEAD}{body})")
}

#[test]
fn parses_board_semantics() {
    let text = parse(&format!(
        r#"{}
(footprint "Lib:Part" (layer "B.Cu") (at 10 20 90)
  (property "Reference" "R1" (at 0 0) (layer "F.SilkS"))
  (property "Description" "quoted \"stuff\" (with parens)")
  (attr smd dnp)
  (fp_line (start 0 0) (end 1 0) (layer "F.SilkS"))
  (pad "1" thru_hole circle (at 1 0) (size 1 1) (drill 0.6 (offset 5 5)) (layers "*.Cu"))
  (pad "2" thru_hole oval (at 0 2 30) (size 1 2) (drill oval 0.6 1.4) (layers "*.Cu"))
  (pad "3" smd rect (at 3 3) (size 1 1) (layers "F.Cu"))
  (pad "4" np_thru_hole circle (at 0 0) (size 0 0) (drill 0) (layers "*.Cu"))
  (model "kicad-embed://part.step" (offset (xyz 1 2 3)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 45)))
  (model "hidden.step" (hide yes) (offset (xyz 0 0 0)))
  (model "${{KIPRJMOD}}/ext.stp" (offset (xyz 0 0 0)) (scale (xyz 2 2 2)) (rotate (xyz 0 0 0)))
  (model (type extruded) (overall_height 1))
  (model "vrml.wrl"))
(via (at 5 5) (size 0.45) (drill 0.2) (layers "F.Cu" "B.Cu") (net 1))
(via blind (at 6 6) (size 0.45) (drill 0.2) (layers "In1.Cu" "F.Cu"))
(via (at 7 7) (size 0.45) (drill 0) (layers "F.Cu" "B.Cu"))
(embedded_files (file (name "part.step") (type model) (data |AAAA|) (checksum "x")))
"#,
        rect_outline(0.0, 0.0, 30.0, 20.0)
    ));
    let board = Board::parse(text.as_bytes()).unwrap();
    assert_eq!(board.thickness, 1.6);
    assert_eq!(board.copper_layers, ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]);
    assert_eq!(board.aux_origin, Vec2::new(10.0, 20.0));
    assert_eq!(board.edges.len(), 4);

    let fp = &board.footprints[0];
    assert_eq!(fp.reference, "R1");
    assert!(fp.back && fp.dnp && !fp.unspecified);
    assert_eq!(fp.rotation, 90.0);
    assert_eq!(fp.models, 0..2);
    assert_eq!(board.models[0].name, "kicad-embed://part.step");
    assert_eq!(board.models[0].rotate, Vec3::new(0.0, 0.0, 45.0));
    assert_eq!(board.models[1].name, "${KIPRJMOD}/ext.stp");
    assert_eq!(board.models[1].scale, Vec3::splat(2.0));

    // Pad 1 is at footprint-local (1, 0); the footprint is rotated 90
    // degrees counter-clockwise on screen, which in y-down coordinates
    // moves it to (10, 20 - 1). The drill offset does not move the hole.
    assert_eq!(board.holes.len(), 2);
    let round = board.holes[0];
    assert!((round.a - Vec2::new(10.0, 19.0)).length() < 1e-9);
    assert_eq!(round.a, round.b);
    assert_eq!(round.r, 0.3);
    // The pad's 30 degrees is absolute, so the slot runs 30 degrees from
    // the board's y axis whatever the footprint's rotation.
    let slot = board.holes[1];
    let axis = slot.b - slot.a;
    assert!((axis.length() - 0.8).abs() < 1e-9);
    assert!((axis.y.atan2(axis.x).to_degrees() - 60.0).abs() < 1e-9);
    assert_eq!(slot.r, 0.3);

    assert_eq!(board.vias.len(), 2);
    assert_eq!((board.vias[0].top, board.vias[0].bottom), (0, 3));
    assert_eq!((board.vias[1].top, board.vias[1].bottom), (0, 1));
    assert_eq!(board.embedded[0].name, "part.step");
    assert_eq!(board.embedded[0].data, b"AAAA");
}

#[test]
fn stackup_gives_kicad_body_extents() {
    let text = parse("");
    let board = Board::parse(text.as_bytes()).unwrap();
    let physical = board.physical();
    assert!((physical.body_top - 1.6162).abs() < 1e-9);
    assert_eq!(physical.front_copper, 0.035);
    assert_eq!(physical.back_copper, 0.035);
    assert_eq!(physical.copper_z.len(), 4);
    assert_eq!(physical.copper_z[3], (-0.035, 0.0));
    assert!((physical.copper_z[0].0 - 1.6162).abs() < 1e-9);
    // Black mask, darkened by 0.2, encoded to sRGB: what KiCad writes.
    let color = board.body_color();
    assert!((color[0] - 0.2044548).abs() < 1e-5);
}

#[test]
fn default_stackup_matches_kicad_construction() {
    let text =
        r#"(kicad_pcb (general (thickness 1.6)) (layers (0 "F.Cu" signal) (2 "B.Cu" signal)))"#;
    let board = Board::parse(text.as_bytes()).unwrap();
    let physical = board.physical();
    assert!((physical.body_top - 1.51).abs() < 1e-9);
    assert_eq!(physical.front_copper, 0.035);
    assert!((board.body_color()[1] - 0.2).abs() < 0.3);
}

fn solids_for(edges: &str) -> Vec<crate::outline::Solid> {
    let text = parse(edges);
    let board = Board::parse(text.as_bytes()).unwrap();
    board_solids(&board, Frame { origin: Vec2::ZERO }).unwrap()
}

#[test]
fn chains_outline_and_classifies_cutouts() {
    let edges = format!(
        "{}{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(gr_circle (center 5 5) (end 6 5) (layer "Edge.Cuts"))"#,
        rect_outline(40.0, 0.0, 50.0, 10.0),
    );
    let solids = solids_for(&edges);
    assert_eq!(solids.len(), 2);
    let big = solids.iter().find(|s| s.outer.area() > 500.0).unwrap();
    assert!((big.outer.area() - 600.0).abs() < 1e-9);
    assert_eq!(big.holes.len(), 1);
    assert!((big.holes[0].area() + PI).abs() < 0.02);
    assert_eq!(big.holes[0].edges.len(), 1);
    let small = solids.iter().find(|s| s.outer.area() < 500.0).unwrap();
    assert!((small.outer.area() - 100.0).abs() < 1e-9);
    assert!(small.holes.is_empty());
}

#[test]
fn arcs_stay_analytic_with_the_right_sense() {
    // A 10x10 square with its top-right corner rounded by a 2 mm arc.
    let edges = r#"(gr_line (start 0 0) (end 8 0) (layer "Edge.Cuts"))
(gr_arc (start 8 0) (mid 9.414214 0.585786) (end 10 2) (layer "Edge.Cuts"))
(gr_line (start 10 2) (end 10 10) (layer "Edge.Cuts"))
(gr_line (start 10 10) (end 0 10) (layer "Edge.Cuts"))
(gr_line (start 0 10) (end 0 0) (layer "Edge.Cuts"))
"#;
    let solids = solids_for(edges);
    let outer = &solids[0].outer;
    assert_eq!(outer.edges.len(), 5);
    let arcs: Vec<_> = outer
        .edges
        .iter()
        .filter(|e| matches!(e, crate::outline::Edge::Arc { .. }))
        .collect();
    assert_eq!(arcs.len(), 1);
    let crate::outline::Edge::Arc { c, ccw, .. } = arcs[0] else {
        unreachable!()
    };
    // Board y is flipped, so the centre lands at (8, -2) and the convex
    // corner runs counter-clockwise like the rest of the outline.
    assert!((*c - Vec2::new(8.0, -2.0)).length() < 1e-4);
    assert!(*ccw);
    assert!((outer.area() - (100.0 - 4.0 + PI)).abs() < 0.02);
}

#[test]
fn open_outline_is_an_error() {
    let text = parse(
        r#"(gr_line (start 0 0) (end 10 0) (layer "Edge.Cuts")) (gr_line (start 10 0) (end 10 10) (layer "Edge.Cuts"))"#,
    );
    let board = Board::parse(text.as_bytes()).unwrap();
    assert!(matches!(
        board_solids(&board, Frame { origin: Vec2::ZERO }),
        Err(crate::Error::OpenOutline(_))
    ));
}

#[test]
fn drills_become_holes_notches_and_merges() {
    let solids = solids_for(&rect_outline(0.0, 0.0, 30.0, 20.0));
    let mut warnings = Vec::new();
    let circle = |x, y, r| Loop::stadium(Vec2::new(x, y), Vec2::new(x, y), r);
    let drills = vec![
        // Interior drill: a hole.
        circle(5.0, -5.0, 1.0),
        // A slot overlapping it: the two merge into one hole.
        Loop::stadium(Vec2::new(5.5, -5.0), Vec2::new(8.0, -5.0), 0.5),
        // A drill entirely inside that hole changes nothing.
        circle(5.0, -5.0, 0.2),
        // A drill straddling the left edge notches the outline instead.
        circle(0.0, -10.0, 1.0),
        // A drill tangent to the bottom edge from inside stays a hole.
        circle(15.0, -19.0, 1.0),
        // A drill outside every solid is ignored.
        circle(100.0, 100.0, 1.0),
    ];
    let mut solids = cut_holes(solids, drills, &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(solids.len(), 1);
    assert_eq!(solids[0].holes.len(), 2);
    assert!((solids[0].outer.area() - (600.0 - PI / 2.0)).abs() < 0.02);
    // The notch is one exact arc on the drill's circle.
    let arcs: Vec<&Edge> = solids[0]
        .outer
        .edges
        .iter()
        .filter(|e| matches!(e, Edge::Arc { .. }))
        .collect();
    assert_eq!(arcs.len(), 1);
    let Edge::Arc { a, b, c, .. } = *arcs[0] else {
        unreachable!()
    };
    assert!(c.distance(Vec2::new(0.0, -10.0)) < 1e-9);
    assert!((a.distance(c) - 1.0).abs() < 1e-9 && (b.distance(c) - 1.0).abs() < 1e-9);
    assert!(
        a.x.abs() < 1e-9 && b.x.abs() < 1e-9,
        "notch ends on the edge line"
    );
    let merged = solids[0]
        .holes
        .iter()
        .map(|h| h.area())
        .fold(0.0_f64, f64::min);
    assert!(
        -merged > PI && -merged < PI + PI * 0.25 + 2.5,
        "merged area {merged}"
    );
    assert!(solids[0].holes.iter().all(|h| h.area() < 0.0));

    let pocket = |r: f64, floor: f64| RoundHole {
        center: Vec2::new(20.0, -10.0),
        profile: vec![(1.5, r), (floor, r), (floor, 0.0)],
        fallback: None,
    };
    assert!(cut_round(&mut solids, pocket(0.2, 1.0), &mut warnings).is_none());
    assert_eq!(solids[0].round.len(), 1);
    assert!(cut_round(&mut solids, pocket(0.3, 0.5), &mut warnings).is_none());
    assert_eq!(solids[0].round.len(), 1);
    assert_eq!(warnings.len(), 1);
}

#[test]
fn rounded_rect_outline_keeps_its_corner_arcs() {
    let solids = solids_for(
        r#"(gr_rect (start 0 0) (end 12 2) (radius 1) (stroke (width 0.05) (type default)) (fill no) (layer "Edge.Cuts"))"#,
    );
    assert_eq!(solids.len(), 1);
    let outer = &solids[0].outer;
    assert_eq!(
        outer
            .edges
            .iter()
            .filter(|e| matches!(e, Edge::Arc { .. }))
            .count(),
        4
    );
    // Area of a 12 x 2 rectangle less the four corner cut-offs.
    assert!(
        (outer.area() - (24.0 - (4.0 - PI))).abs() < 0.02,
        "area {}",
        outer.area()
    );
    // A radius beyond half the short side is clamped: a stadium.
    let solids = solids_for(
        r#"(gr_rect (start 0 0) (end 12 2) (radius 5) (stroke (width 0.05) (type default)) (fill no) (layer "Edge.Cuts"))"#,
    );
    assert!((solids[0].outer.area() - (20.0 + PI)).abs() < 0.02);
}

#[test]
fn castellation_coincident_with_the_outline_is_absorbed() {
    // An outline with a semicircular notch, and a drill on the same circle.
    let outline = r#"
        (gr_line (start 0 0) (end 10 0) (layer "Edge.Cuts"))
        (gr_line (start 10 0) (end 10 10) (layer "Edge.Cuts"))
        (gr_line (start 10 10) (end 6 10) (layer "Edge.Cuts"))
        (gr_arc (start 6 10) (mid 5 9) (end 4 10) (layer "Edge.Cuts"))
        (gr_line (start 4 10) (end 0 10) (layer "Edge.Cuts"))
        (gr_line (start 0 10) (end 0 0) (layer "Edge.Cuts"))"#;
    let solids = solids_for(outline);
    let mut warnings = Vec::new();
    let drill = Loop::stadium(Vec2::new(5.0, -10.0), Vec2::new(5.0, -10.0), 1.0);
    let solids = cut_holes(solids, vec![drill], &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(solids.len(), 1);
    assert!(solids[0].holes.is_empty());
    assert!(
        (solids[0].outer.area() - (100.0 - PI / 2.0)).abs() < 0.02,
        "area {}",
        solids[0].outer.area()
    );
    let arcs = solids[0]
        .outer
        .edges
        .iter()
        .filter(|e| matches!(e, Edge::Arc { .. }))
        .count();
    assert_eq!(arcs, 1);
}

/// Options without the silkscreen and mask faces, for tests that count
/// the board's own faces.
fn bare() -> Options {
    Options {
        silkscreen: false,
        soldermask: false,
        ..Options::default()
    }
}

fn export_text(board: &str, options: &Options) -> (String, crate::Report) {
    let board = Board::parse(board.as_bytes()).unwrap();
    let mut out = Vec::new();
    let report = export(&board, options, &mut out).unwrap();
    (String::from_utf8(out).unwrap(), report)
}

fn count(text: &str, entity: &str) -> usize {
    text.matches(&format!(" = {entity}(")).count()
}

#[test]
fn board_body_is_one_closed_solid() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10)
  (pad "1" thru_hole circle (at 0 0) (size 1 1) (drill 0.6) (layers "*.Cu"))
  (pad "2" thru_hole oval (at 5 0) (size 1 2) (drill oval 0.6 1.4) (layers "*.Cu")))
(via (at 20 10) (size 0.45) (drill 0.2) (layers "F.Cu" "B.Cu"))
"#
    );
    let (text, report) = export_text(&parse(&edges), &bare());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(count(&text, "MANIFOLD_SOLID_BREP"), 1);
    assert_eq!(count(&text, "CLOSED_SHELL"), 1);
    // Two caps, four outline walls, one cylinder for the round drill, and
    // two lines plus two cylinders for the slot. The via is not cut.
    assert_eq!(count(&text, "ADVANCED_FACE"), 2 + 4 + 1 + 4);
    assert_eq!(count(&text, "CYLINDRICAL_SURFACE"), 3);
    assert_eq!(count(&text, "FACE_BOUND"), 4);
    assert!(text.contains("COLOUR_RGB('',0.204454873"));
    assert!(text.contains("PRODUCT('board_PCB'"));

    let (text, _) = export_text(
        &parse(&edges),
        &Options {
            cut_vias: true,
            ..bare()
        },
    );
    assert_eq!(count(&text, "CYLINDRICAL_SURFACE"), 4);
}

/// Encode bytes the way KiCad embeds files: zstd, then base64 wrapped
/// in bar text.
fn embed(name: &str, data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let compressed = zstd::bulk::compress(data, 3).unwrap();
    let mut text = String::new();
    for chunk in compressed.chunks(3) {
        let mut n = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            n |= (*b as u32) << (16 - 8 * i);
        }
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
        if text.len().is_multiple_of(77) {
            text.push('\n');
        }
    }
    format!(r#"(embedded_files (file (name "{name}") (type model) (data |{text}|)))"#)
}

/// A donor STEP file: our own export of a 2x2 board, which has product
/// structure and one solid.
fn donor_step() -> Vec<u8> {
    let (text, _) = export_text(
        &parse(&rect_outline(0.0, 0.0, 2.0, 2.0)),
        &Options {
            name: "donor".to_owned(),
            ..bare()
        },
    );
    text.into_bytes()
}

#[test]
fn components_are_copied_once_and_placed_per_footprint() {
    let donor = donor_step();
    let board = parse(&format!(
        r#"{}
(footprint "Lib:A" (layer "F.Cu") (at 10 10 90) (property "Reference" "U1")
  (model "kicad-embed://cube.step" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
(footprint "Lib:A" (layer "B.Cu") (at 20 10) (property "Reference" "U2") (attr dnp)
  (model "kicad-embed://CUBE.STEP" (offset (xyz 0 0 1)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
(footprint "Lib:B" (layer "F.Cu") (at 5 5) (property "Reference" "J1")
  (model "kicad-embed://cube.step" (offset (xyz 0 0 0)) (scale (xyz 2 2 2)) (rotate (xyz 0 0 0)))
  (model "missing.step"))
{}"#,
        rect_outline(0.0, 0.0, 30.0, 20.0),
        embed("cube.step", &donor)
    ));
    let (text, report) = export_text(&board, &bare());
    assert_eq!(report.warnings, ["could not find 3D model: missing.step"]);
    assert_eq!(report.failed_models, 0);
    // One copy of the donor solid at scale 1, one at scale 2, the board.
    assert_eq!(count(&text, "MANIFOLD_SOLID_BREP"), 3);
    assert_eq!(count(&text, "NEXT_ASSEMBLY_USAGE_OCCURRENCE"), 4);
    assert!(text.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','U1'"));
    assert!(text.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE('4','PCB'"));
    // The scaled copy has its coordinates doubled.
    assert!(text.contains("CARTESIAN_POINT('',(4.,-4.,"));

    // Front component: z = body top + F.Cu + standoff; rotated 90.
    let physical = Board::parse(board.as_bytes()).unwrap().physical();
    let z = physical.body_top + physical.front_copper + 0.05;
    let expected = format!("CARTESIAN_POINT('',(10.0,-10.0,{}));", fmt9(z));
    assert!(text.contains(&expected), "{expected}");
    let u1_axis = text
        .lines()
        .find(|l| l.contains("AXIS2_PLACEMENT_3D('U1'"))
        .unwrap();
    let ids: Vec<&str> = u1_axis
        .split('#')
        .skip(2)
        .map(|s| s.trim_end_matches([',', ')', ';']))
        .collect();
    let x_axis = text
        .lines()
        .find(|l| l.starts_with(&format!("#{} = ", ids[2])))
        .unwrap();
    assert!(x_axis.contains("(0.0,1.0,0.0)"), "{x_axis}");
    // Back component: below the board, flipped.
    let z = -(physical.back_copper + 0.05 + 1.0);
    let expected = format!("CARTESIAN_POINT('',(20.0,-10.0,{}));", fmt9(z));
    assert!(text.contains(&expected), "{expected}");

    let (text, _) = export_text(
        &board,
        &Options {
            include_dnp: false,
            component_filter: vec!["U*".to_owned()],
            board_body: false,
            ..bare()
        },
    );
    assert_eq!(count(&text, "NEXT_ASSEMBLY_USAGE_OCCURRENCE"), 1);
    assert_eq!(count(&text, "MANIFOLD_SOLID_BREP"), 1);
}

#[test]
fn title_block_fields_are_text_variables() {
    let text = parse(r#"(title_block (title "Demo") (rev "${PCB_VERSION}") (comment 1 "one"))"#);
    let board = Board::parse(text.as_bytes()).unwrap();
    let owned = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    assert_eq!(
        board.title_block,
        owned(&[
            ("TITLE", "Demo"),
            ("REVISION", "${PCB_VERSION}"),
            ("COMMENT1", "one")
        ])
    );
    let variables = [board.title_block.clone(), owned(&[("PCB_VERSION", "1.2")])].concat();
    assert_eq!(
        crate::faces::expand("rev ${REVISION}", &variables),
        "rev 1.2"
    );
}

#[test]
fn crossing_outline_loops_are_an_error() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 10.0, 10.0),
        r#"(gr_line (start 9 1) (end 1 9) (layer "Edge.Cuts"))
(gr_line (start 1 9) (end -7 1) (layer "Edge.Cuts"))
(gr_line (start -7 1) (end 1 -7) (layer "Edge.Cuts"))
(gr_line (start 1 -7) (end 9 1) (layer "Edge.Cuts"))
"#
    );
    let text = parse(&edges);
    let board = Board::parse(text.as_bytes()).unwrap();
    let err = export(&board, &bare(), &mut Vec::new()).unwrap_err();
    assert!(matches!(err, crate::Error::Outline(_)), "{err}");
}

fn fmt9(v: f64) -> String {
    let mut out = Vec::new();
    crate::step::push_float(&mut out, v);
    String::from_utf8(out).unwrap()
}

#[test]
fn donor_units_and_styles_are_converted() {
    let step = br#"ISO-10303-21;
HEADER;
FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('a #99 quote''s',(1.,2.,3.));
#2 = DIRECTION('',(0.,0.,1.));
#3 = DIRECTION('',(1.,0.,0.));
#4 = AXIS2_PLACEMENT_3D('',#1,#2,#3);
#5 = CONICAL_SURFACE('',#4,2.,45.);
#6 = CIRCLE('',#4,0.5);
#7 = SURFACE_CURVE('',#6,(#8),.PCURVE_S1.);
#8 = PCURVE('',#5,#9);
#9 = DEFINITIONAL_REPRESENTATION('',(#10),#11);
#10 = LINE('',#1,#12);
#11 = ( GEOMETRIC_REPRESENTATION_CONTEXT(2) PARAMETRIC_REPRESENTATION_CONTEXT() REPRESENTATION_CONTEXT('','') );
#12 = VECTOR('',#3,1.);
#13 = VERTEX_POINT('',#1);
#14 = EDGE_CURVE('',#13,#13,#7,.T.);
#15 = GEOMETRIC_SET('',(#14,#5));
#16 = STYLED_ITEM('',(#17),#15);
#17 = PRESENTATION_STYLE_ASSIGNMENT((#18));
#18 = CURVE_STYLE('',#19,POSITIVE_LENGTH_MEASURE(0.1),#20);
#19 = DRAUGHTING_PRE_DEFINED_CURVE_FONT('continuous');
#20 = COLOUR_RGB('',1.,0.,0.);
#21 = STYLED_ITEM('',(#22),#15);
#22 = PRESENTATION_STYLE_ASSIGNMENT((#23));
#23 = SURFACE_STYLE_USAGE(.BOTH.,#24);
#24 = SURFACE_SIDE_STYLE('',(#25));
#25 = SURFACE_STYLE_FILL_AREA(#26);
#26 = FILL_AREA_STYLE('',(#27));
#27 = FILL_AREA_STYLE_COLOUR('',#20);
#30 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.) );
#31 = ( CONVERSION_BASED_UNIT('DEGREE',#32) NAMED_UNIT(*) PLANE_ANGLE_UNIT() );
#32 = PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.0174532925199),#33);
#33 = ( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) );
ENDSEC;
END-ISO-10303-21;
"#;
    let donor = Donor::parse(step.to_vec()).unwrap();
    let analysis = donor.analyze().unwrap();
    assert!(!analysis.is_empty());
    let mut w = crate::step::Writer::new(100);
    let root = crate::step::Root::reserve(&mut crate::step::Writer::new(1));
    analysis.emit(&donor, &mut w, &root.context, 1.0).unwrap();
    let text = String::from_utf8(w.buf).unwrap();
    // Metres become millimetres, degrees become radians.
    assert!(text.contains("CARTESIAN_POINT('a #99 quote''s',(1000.,2000.,3000.))"));
    assert!(
        text.contains("CONICAL_SURFACE('',#105,2000.,0.785398163397)"),
        "{}",
        text.lines()
            .find(|l| l.contains("CONICAL"))
            .unwrap_or("no cone")
    );
    assert!(text.contains("CIRCLE('',#105,500.)"));
    // The edge references the basis circle directly; pcurves are gone.
    assert_eq!(count(&text, "PCURVE"), 0);
    assert_eq!(count(&text, "SURFACE_CURVE"), 0);
    assert_eq!(count(&text, "DEFINITIONAL_REPRESENTATION"), 0);
    assert!(text.contains("EDGE_CURVE('',#108,#108,#107,.T.)"));
    // The surface style survives, the curve style does not.
    assert_eq!(count(&text, "STYLED_ITEM"), 1);
    assert_eq!(count(&text, "CURVE_STYLE"), 0);
    assert_eq!(count(&text, "SURFACE_STYLE_USAGE"), 1);
}

#[test]
fn glob_matches_reference_designators() {
    use crate::glob_match;
    assert!(glob_match("R*", "R12"));
    assert!(glob_match("R?", "R1"));
    assert!(!glob_match("R?", "R12"));
    assert!(glob_match("*", "anything"));
    assert!(!glob_match("C*", "R1"));
    assert!(glob_match("U1*2", "U1abc2"));
}

#[test]
fn donor_assemblies_follow_product_structure() {
    // A root product with two occurrences of one child product, whose
    // shape is a geometric set. The relationships are written child-first
    // and parent-first respectively, so only the occurrences can say
    // which side is the child.
    let step = br#"ISO-10303-21;
HEADER;
FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));
ENDSEC;
DATA;
#1 = APPLICATION_CONTEXT('');
#2 = PRODUCT_CONTEXT('',#1,'mechanical');
#3 = PRODUCT_DEFINITION_CONTEXT('',#1,'design');
#10 = PRODUCT('root','root','',(#2));
#11 = PRODUCT_DEFINITION_FORMATION('','',#10);
#12 = PRODUCT_DEFINITION('','',#11,#3);
#13 = PRODUCT_DEFINITION_SHAPE('','',#12);
#14 = SHAPE_DEFINITION_REPRESENTATION(#13,#15);
#15 = SHAPE_REPRESENTATION('',(#16),#90);
#16 = AXIS2_PLACEMENT_3D('',#17,#18,#19);
#17 = CARTESIAN_POINT('',(0.,0.,0.));
#18 = DIRECTION('',(0.,0.,1.));
#19 = DIRECTION('',(1.,0.,0.));
#20 = PRODUCT('child','child','',(#2));
#21 = PRODUCT_DEFINITION_FORMATION('','',#20);
#22 = PRODUCT_DEFINITION('','',#21,#3);
#23 = PRODUCT_DEFINITION_SHAPE('','',#22);
#24 = SHAPE_DEFINITION_REPRESENTATION(#23,#25);
#25 = ADVANCED_BREP_SHAPE_REPRESENTATION('',(#16,#26),#90);
#26 = GEOMETRIC_CURVE_SET('',(#27));
#27 = VERTEX_POINT('',#17);
#30 = NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','a','',#12,#22,$);
#31 = PRODUCT_DEFINITION_SHAPE('','',#30);
#32 = CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#33,#31);
#33 = ( REPRESENTATION_RELATIONSHIP('','',#25,#15) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#34) SHAPE_REPRESENTATION_RELATIONSHIP() );
#34 = ITEM_DEFINED_TRANSFORMATION('','',#16,#35);
#35 = AXIS2_PLACEMENT_3D('',#36,#18,#19);
#36 = CARTESIAN_POINT('',(10.,0.,0.));
#40 = NEXT_ASSEMBLY_USAGE_OCCURRENCE('2','b','',#12,#22,$);
#41 = PRODUCT_DEFINITION_SHAPE('','',#40);
#42 = CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#43,#41);
#43 = ( REPRESENTATION_RELATIONSHIP('','',#15,#25) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#44) SHAPE_REPRESENTATION_RELATIONSHIP() );
#44 = ITEM_DEFINED_TRANSFORMATION('','',#45,#16);
#45 = AXIS2_PLACEMENT_3D('',#46,#18,#19);
#46 = CARTESIAN_POINT('',(0.,20.,0.));
#90 = ( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#91)) GLOBAL_UNIT_ASSIGNED_CONTEXT((#92)) REPRESENTATION_CONTEXT('','') );
#91 = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-04),#92,'distance_accuracy_value','');
#92 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );
ENDSEC;
END-ISO-10303-21;
"#;
    let donor = Donor::parse(step.to_vec()).unwrap();
    let analysis = donor.analyze().unwrap();
    let mut w = crate::step::Writer::new(100);
    let root = crate::step::Root::reserve(&mut crate::step::Writer::new(1));
    let shape = analysis.emit(&donor, &mut w, &root.context, 1.0).unwrap();
    let text = String::from_utf8(w.buf).unwrap();
    // The child geometry is copied once and instanced twice, at the two
    // occurrence placements, under the root representation.
    assert_eq!(count(&text, "GEOMETRIC_CURVE_SET"), 1);
    assert_eq!(count(&text, "MAPPED_ITEM"), 2);
    assert_eq!(count(&text, "REPRESENTATION_MAP"), 2);
    assert!(text.contains("CARTESIAN_POINT('',(10.0,0.0,0.0))"));
    assert!(text.contains("CARTESIAN_POINT('',(0.0,20.0,0.0))"));
    // The donor's 100 µm accuracy is carried into its own context.
    assert!(text.contains("LENGTH_MEASURE(0.0001)"));
    let representation = text
        .lines()
        .find(|l| l.starts_with(&format!("#{} = SHAPE_REPRESENTATION", shape.representation)))
        .unwrap();
    assert_eq!(representation.matches('#').count(), 1 + 1 + 4 + 1);
}

#[test]
fn donor_contexts_scale_independently() {
    // A millimetre root assembly placing an inch child: the child's
    // coordinates convert, the placement in the root does not.
    let step = br#"ISO-10303-21;
HEADER;
FILE_SCHEMA(('AUTOMOTIVE_DESIGN'));
ENDSEC;
DATA;
#1 = APPLICATION_CONTEXT('');
#2 = PRODUCT_CONTEXT('',#1,'mechanical');
#3 = PRODUCT_DEFINITION_CONTEXT('',#1,'design');
#10 = PRODUCT('root','root','',(#2));
#11 = PRODUCT_DEFINITION_FORMATION('','',#10);
#12 = PRODUCT_DEFINITION('','',#11,#3);
#13 = PRODUCT_DEFINITION_SHAPE('','',#12);
#14 = SHAPE_DEFINITION_REPRESENTATION(#13,#15);
#15 = SHAPE_REPRESENTATION('',(#16),#90);
#16 = AXIS2_PLACEMENT_3D('',#17,#18,#19);
#17 = CARTESIAN_POINT('',(0.,0.,0.));
#18 = DIRECTION('',(0.,0.,1.));
#19 = DIRECTION('',(1.,0.,0.));
#20 = PRODUCT('child','child','',(#2));
#21 = PRODUCT_DEFINITION_FORMATION('','',#20);
#22 = PRODUCT_DEFINITION('','',#21,#3);
#23 = PRODUCT_DEFINITION_SHAPE('','',#22);
#24 = SHAPE_DEFINITION_REPRESENTATION(#23,#25);
#25 = ADVANCED_BREP_SHAPE_REPRESENTATION('',(#50,#26),#95);
#26 = GEOMETRIC_CURVE_SET('',(#27));
#27 = VERTEX_POINT('',#28);
#28 = CARTESIAN_POINT('',(1.,2.,3.));
#30 = NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','a','',#12,#22,$);
#31 = PRODUCT_DEFINITION_SHAPE('','',#30);
#32 = CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#33,#31);
#33 = ( REPRESENTATION_RELATIONSHIP('','',#25,#15) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#34) SHAPE_REPRESENTATION_RELATIONSHIP() );
#34 = ITEM_DEFINED_TRANSFORMATION('','',#50,#35);
#35 = AXIS2_PLACEMENT_3D('',#36,#18,#19);
#36 = CARTESIAN_POINT('',(10.,0.,0.));
#50 = AXIS2_PLACEMENT_3D('',#51,#18,#19);
#51 = CARTESIAN_POINT('',(0.,0.,0.));
#90 = ( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#91)) GLOBAL_UNIT_ASSIGNED_CONTEXT((#92)) REPRESENTATION_CONTEXT('','') );
#91 = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-04),#92,'distance_accuracy_value','');
#92 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );
#95 = ( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#96)) GLOBAL_UNIT_ASSIGNED_CONTEXT((#97)) REPRESENTATION_CONTEXT('','') );
#96 = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-05),#97,'distance_accuracy_value','');
#97 = ( CONVERSION_BASED_UNIT('INCH',#98) LENGTH_UNIT() NAMED_UNIT(#99) );
#98 = LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#92);
#99 = DIMENSIONAL_EXPONENTS(1.,0.,0.,0.,0.,0.,0.);
ENDSEC;
END-ISO-10303-21;
"#;
    let donor = Donor::parse(step.to_vec()).unwrap();
    let analysis = donor.analyze().unwrap();
    let mut w = crate::step::Writer::new(100);
    let root = crate::step::Root::reserve(&mut crate::step::Writer::new(1));
    analysis.emit(&donor, &mut w, &root.context, 1.0).unwrap();
    let text = String::from_utf8(w.buf).unwrap();
    assert!(
        text.contains("CARTESIAN_POINT('',(25.4,50.8,76.2))"),
        "{text}"
    );
    assert!(
        text.contains("CARTESIAN_POINT('',(10.0,0.0,0.0))"),
        "{text}"
    );
}

#[test]
fn machining_becomes_radius_profiles() {
    use crate::board::{Backdrill, Machining, Mouth, Via};
    use crate::holes::{pad_hole, via_hole};

    let text = parse("");
    let board = Board::parse(text.as_bytes()).unwrap();
    let physical = board.physical();
    let top = physical.body_top;
    let surface = top + physical.front_copper;
    let c = Vec2::new(5.0, -5.0);
    let near = |a: f64, b: f64| (a - b).abs() < 1e-9;

    // Counterbore from the front: cylinder, shoulder, drill.
    let bore = pad_hole(
        c,
        1.0,
        Machining {
            front: Some(Mouth::Counterbore { r: 2.0, depth: 0.6 }),
            ..Machining::default()
        },
        &physical,
    )
    .unwrap();
    let floor = surface - 0.6;
    assert_eq!(bore.profile.len(), 4);
    assert!(near(bore.profile[0].0, top) && near(bore.profile[0].1, 2.0));
    assert!(near(bore.profile[1].0, floor) && near(bore.profile[1].1, 2.0));
    assert!(near(bore.profile[2].0, floor) && near(bore.profile[2].1, 1.0));
    assert_eq!(bore.profile[3], (0.0, 1.0));
    assert_eq!(bore.fallback, Some(1.0));

    // Countersink from the back with no depth runs to where the cone
    // meets the drill.
    let sink = pad_hole(
        c,
        1.0,
        Machining {
            back: Some(Mouth::Countersink {
                r: 2.0,
                depth: None,
                half_angle: 45f64.to_radians(),
            }),
            ..Machining::default()
        },
        &physical,
    )
    .unwrap();
    assert_eq!(sink.profile.len(), 3);
    assert_eq!(sink.profile[0], (top, 1.0));
    let z_meet = -physical.back_copper + (2.0 - 1.0);
    assert!(near(sink.profile[1].0, z_meet) && near(sink.profile[1].1, 1.0));
    assert!(near(sink.profile[2].0, 0.0) && near(sink.profile[2].1, 2.0 - physical.back_copper));

    // Machining that does not reach the body is ignored.
    let shallow = pad_hole(
        c,
        1.0,
        Machining {
            front: Some(Mouth::Counterbore {
                r: 2.0,
                depth: 0.01,
            }),
            ..Machining::default()
        },
        &physical,
    )
    .unwrap();
    assert!(shallow.is_plain());

    // A through via backdrilled from the back to In2.Cu.
    let mut warnings = Vec::new();
    let via = Via {
        at: c,
        drill: 0.4,
        top: 0,
        bottom: 3,
        filled: false,
        size: 0.8,
        net: 0,
        remove_unused: false,
        keep_ends: false,
        tented: [None; 2],
        machining: Machining {
            backdrills: [
                Some(Backdrill {
                    r: 0.35,
                    start: 3,
                    end: 2,
                }),
                None,
            ],
            ..Machining::default()
        },
    };
    let drilled = via_hole(c, &via, &physical, &mut warnings).unwrap();
    let ceiling = physical.copper_z[2].1;
    assert_eq!(drilled.profile.len(), 4);
    assert_eq!(drilled.profile[0], (top, 0.2));
    assert!(near(drilled.profile[1].0, ceiling) && near(drilled.profile[1].1, 0.2));
    assert!(near(drilled.profile[2].0, ceiling) && near(drilled.profile[2].1, 0.35));
    assert_eq!(drilled.profile[3], (0.0, 0.35));

    // Filled: only the backdrill pocket is cut.
    let filled = Via {
        filled: true,
        ..via
    };
    let pocket = via_hole(c, &filled, &physical, &mut warnings).unwrap();
    assert_eq!(pocket.profile.len(), 3);
    assert!(near(pocket.profile[0].0, ceiling) && pocket.profile[0].1 == 0.0);
    assert_eq!(pocket.fallback, None);
    let plain_filled = Via {
        machining: Machining::default(),
        ..filled
    };
    assert!(via_hole(c, &plain_filled, &physical, &mut warnings).is_none());

    // Blind from the top: drill then a floor disc.
    let blind = Via {
        bottom: 1,
        machining: Machining::default(),
        ..via
    };
    let hole = via_hole(c, &blind, &physical, &mut warnings).unwrap();
    let floor = physical.copper_z[1].0;
    assert_eq!(hole.profile, vec![(top, 0.2), (floor, 0.2), (floor, 0.0)]);
    assert!(warnings.is_empty());
}

#[test]
fn parses_via_fill_state_and_machining() {
    let body = format!(
        r#"{}
(via (at 5 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (filling no) (backdrill (size 0.7) (layers "B.Cu" "In2.Cu")))
(via (at 6 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (capping yes))
(via (at 7 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (front_post_machining countersink (size 1) (angle 90)))
(via (at 8 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu"))
"#,
        rect_outline(0.0, 0.0, 30.0, 20.0)
    );
    let text = parse(&body)
        .replace(
            "(setup (aux_axis_origin 10 20)",
            "(setup (filling yes) (aux_axis_origin 10 20)",
        )
        .replace("(version 20241229)", "(version 20260206)");
    let board = Board::parse(text.as_bytes()).unwrap();
    assert!(board.fill_vias);
    let vias = &board.vias;
    assert!(!vias[0].filled);
    assert_eq!(
        vias[0].machining.backdrills[0],
        Some(crate::board::Backdrill {
            r: 0.35,
            start: 3,
            end: 2
        })
    );
    assert!(vias[1].filled);
    assert!(matches!(
        vias[2].machining.front,
        Some(crate::board::Mouth::Countersink { r, depth: None, .. }) if r == 0.5
    ));
    assert!(vias[3].filled, "board default applies");

    // Pre-2025 files default to unfilled instead of inheriting.
    let legacy = text.replace("(version 20260206)", "(version 20241229)");
    let board = Board::parse(legacy.as_bytes()).unwrap();
    let filled: Vec<bool> = board.vias.iter().map(|v| v.filled).collect();
    assert_eq!(filled, [false, true, false, false]);
}

#[test]
fn machined_holes_are_emitted_as_analytic_faces() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10)
  (pad "1" thru_hole circle (at 0 0) (size 3 3) (drill 2) (layers "*.Cu") (front_post_machining counterbore (size 4) (depth 0.6)))
  (pad "2" thru_hole circle (at 6 0) (size 3 3) (drill 2) (layers "*.Cu") (back_post_machining countersink (size 4) (angle 90))))
(via blind (at 20 10) (size 0.8) (drill 0.4) (layers "F.Cu" "In1.Cu"))
"#
    );
    let (text, report) = export_text(
        &parse(&edges),
        &Options {
            cut_vias: true,
            ..bare()
        },
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(count(&text, "MANIFOLD_SOLID_BREP"), 1);
    // Caps and four walls; counterbore: cylinder, shoulder, cylinder;
    // countersink: cylinder, cone; blind via: cylinder, floor disc.
    assert_eq!(count(&text, "ADVANCED_FACE"), 2 + 4 + 3 + 2 + 2);
    assert_eq!(count(&text, "CONICAL_SURFACE"), 1);
    assert_eq!(count(&text, "CYLINDRICAL_SURFACE"), 4);
    // Three holes on the top cap, two on the bottom, the counterbore's
    // shoulder ring.
    assert_eq!(count(&text, "FACE_BOUND"), 3 + 2 + 1);
}

#[test]
fn parses_copper_items() {
    let body = format!(
        r#"{}
(footprint "Lib:Part" (layer "F.Cu") (at 10 10 90) (property "Reference" "U1")
  (pad "1" smd roundrect (at 1 0) (size 1 2) (layers "F.Cu" "F.Mask") (roundrect_rratio 0.25) (net "GND"))
  (pad "2" thru_hole circle (at 0 0) (size 1.6 1.6) (drill 0.8) (layers "*.Cu" "*.Mask") (net "VCC") (remove_unused_layers yes) (keep_end_layers yes))
  (pad "3" smd custom (at 3 0) (size 1 1) (layers "F.Cu") (options (clearance outline) (anchor circle))
    (primitives (gr_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (width 0) (fill yes)) (gr_line (start 0 0) (end -1 0) (width 0.2)))))
(segment (start 1 1) (end 5 1) (width 0.25) (layer "F.Cu") (net "GND"))
(arc (start 5 1) (mid 6 2) (end 7 1) (width 0.25) (layer "B.Cu") (net "GND"))
(via (at 5 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net "VCC"))
(zone (net "GND") (layers "F.Cu") (filled_polygon (layer "F.Cu") (pts (xy 0 0) (xy 10 0) (xy 10 10) (xy 0 10)))
  (filled_polygon (layer "B.Cu") (island) (pts (xy 0 0) (xy 1 0) (xy 1 1))))
"#,
        rect_outline(0.0, 0.0, 30.0, 20.0)
    );
    let text = parse(&body);
    let board = Board::parse(text.as_bytes()).unwrap();
    assert_eq!(board.pads.len(), 3);
    let pad = &board.pads[0];
    assert!(
        matches!(pad.shape, crate::board::PadShape::RoundRect { round_ratio, .. } if round_ratio == 0.25)
    );
    assert_eq!(pad.layers, 1);
    // The footprint is rotated 90 degrees, so pad-local (1, 0) lands at
    // (10, 9) in y-down board coordinates.
    assert!((pad.at - Vec2::new(10.0, 9.0)).length() < 1e-9);
    let tht = &board.pads[1];
    assert_eq!(tht.layers, 0b1111);
    assert!(tht.has_hole && tht.remove_unused && tht.keep_ends);
    assert_eq!(tht.kind, crate::board::PadKind::ThroughHole);
    let custom = &board.pads[2];
    assert!(matches!(
        custom.shape,
        crate::board::PadShape::Custom {
            anchor_circle: true
        }
    ));
    assert_eq!(custom.primitives, 0..2);
    assert_eq!(board.primitive_points.len(), 3);

    assert_eq!(board.tracks.len(), 2);
    assert_eq!(board.tracks[0].layer, 0);
    assert_eq!(board.tracks[0].mid, board.tracks[0].a);
    assert_eq!(board.tracks[1].layer, 3);
    assert_ne!(board.tracks[1].mid, board.tracks[1].a);
    // Nets are interned by name: GND and VCC are distinct and nonzero.
    assert_ne!(board.tracks[0].net, 0);
    assert_eq!(board.tracks[0].net, board.fills[0].net);
    assert_eq!(board.vias[0].net, tht.net);
    assert_ne!(board.vias[0].net, board.tracks[0].net);
    assert_eq!(board.vias[0].size, 0.6);
    assert_eq!(board.fills.len(), 2);
    assert_eq!(board.fills[1].layer, 3);
    assert_eq!(board.fill_points.len(), 7);
}

#[test]
fn copper_is_unioned_per_layer_and_extruded() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10) (property "Reference" "R1")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" thru_hole circle (at 1 0) (size 1.6 1.6) (drill 0.8) (layers "*.Cu")))
(segment (start 4 4) (end 8 4) (width 0.5) (layer "F.Cu") (net "A"))
(segment (start 8 4) (end 8 8) (width 0.5) (layer "F.Cu") (net "A"))
(segment (start 20 4) (end 24 4) (width 0.5) (layer "F.Cu") (net "B"))
(via (at 15 15) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net "A"))
(zone (net "A") (layers "B.Cu") (filled_polygon (layer "B.Cu") (pts (xy 2 2) (xy 12 2) (xy 12 12) (xy 2 12))))
"#
    );
    let (text, report) = export_text(
        &parse(&edges),
        &Options {
            pads: true,
            tracks: true,
            zones: true,
            ..Options::default()
        },
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(text.contains("PRODUCT('board_copper'"));
    assert!(text.contains("PRODUCT('board_pad'"));
    assert!(text.contains("PRODUCT('board_via'"));
    // Pads: an SMD square on F.Cu, and a through-hole pad on F.Cu and
    // B.Cu plus its plating tube. Vias: one tube. Copper islands: the two
    // joined A segments, the B segment, the via ring on each layer, and
    // the B.Cu zone with the pad drill knocked out.
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('pad ").count(), 4);
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('via ").count(), 1);
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('copper ").count(), 5);
    // Copper sits on top of the body and below it at the copper thickness.
    let body_top = 1.6162;
    assert!(text.contains(&format!(
        "CARTESIAN_POINT('',(0.0,0.0,{}))",
        fmt9(body_top + 0.035)
    )));
    assert!(text.contains("CARTESIAN_POINT('',(0.0,0.0,-0.035))"));
}

#[test]
fn polygonized_arcs_are_refitted() {
    use crate::rings::refit_arcs;
    let n = 36;
    let circle: Vec<Vec2> = (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            Vec2::new(5.0 + 2.0 * a.cos(), 3.0 + 2.0 * a.sin())
        })
        .collect();
    let edges = refit_arcs(&circle);
    assert_eq!(edges.len(), 1);
    assert!(matches!(
        edges[0],
        crate::outline::Edge::Arc { ccw: true, .. }
    ));

    // A rounded slot: two straight runs and two half circles.
    let mut slot = Vec::new();
    for i in 0..=18 {
        let a = -std::f64::consts::FRAC_PI_2 + PI * i as f64 / 18.0;
        slot.push(Vec2::new(4.0 + a.cos(), a.sin()));
    }
    for i in 0..=18 {
        let a = std::f64::consts::FRAC_PI_2 + PI * i as f64 / 18.0;
        slot.push(Vec2::new(a.cos(), a.sin()));
    }
    let edges = refit_arcs(&slot);
    let arcs = edges
        .iter()
        .filter(|e| matches!(e, crate::outline::Edge::Arc { .. }))
        .count();
    assert_eq!(arcs, 2, "{edges:?}");
    assert_eq!(edges.len(), 4, "{edges:?}");
}

#[test]
fn rounded_and_chamfered_pads_cut_inward() {
    let text = parse(&format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10 30) (property "Reference" "U1")
  (pad "1" smd roundrect (at 0 0) (size 1 2) (layers "F.Cu") (roundrect_rratio 0.25))
  (pad "2" smd roundrect (at 3 0) (size 1 1) (layers "F.Cu") (roundrect_rratio 0) (chamfer_ratio 0.25) (chamfer top_left bottom_right))
  (pad "3" smd oval (at 6 0) (size 1 2) (layers "F.Cu")))
"#
    ));
    let board = Board::parse(text.as_bytes()).unwrap();
    let frame = Frame { origin: Vec2::ZERO };
    let mut warnings = Vec::new();
    let mut area = |pad| {
        let edges = crate::copper::pad_outline(&board, frame, pad, &mut warnings).unwrap();
        Loop::new(edges).area().abs()
    };
    let areas = [
        area(&board.pads[0]),
        area(&board.pads[1]),
        area(&board.pads[2]),
    ];
    // Every corner cut removes area from the rectangle: (4 - pi) r^2 for
    // four round corners, c^2 / 2 per chamfer, and (4 - pi) r^2 for the
    // two half-circle ends of a stadium.
    let round = 2.0 - (4.0 - PI) * 0.25 * 0.25;
    let chamfered = 1.0 - 2.0 * 0.25 * 0.25 / 2.0;
    let oval = 2.0 - (4.0 - PI) * 0.5 * 0.5;
    // Areas come from chords inside the arcs, so they fall a little short.
    let close = |actual: f64, expected: f64| (0.0..0.005).contains(&(expected - actual));
    assert!(close(areas[0], round));
    assert!(close(areas[1], chamfered));
    assert!(close(areas[2], oval));
    assert!(warnings.is_empty());
}

#[test]
fn tracks_are_cut_away_under_pads() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10) (property "Reference" "R1")
  (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu") (net "A")))
(segment (start 4 10) (end 16 10) (width 0.5) (layer "F.Cu") (net "A"))
"#
    );
    let options = Options {
        pads: true,
        tracks: true,
        ..Options::default()
    };
    let (text, report) = export_text(&parse(&edges), &options);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // The pad is a solid of its own, so the track is split where it
    // passes under the pad instead of overlapping it.
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('pad ").count(), 1);
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('copper ").count(), 2);
    // Without pads the track is whole.
    let (text, _) = export_text(
        &parse(&edges),
        &Options {
            pads: false,
            ..options.clone()
        },
    );
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('copper ").count(), 1);

    // Pads of any net are cut out of a zone whose fill covers them: fills
    // go stale when pads are placed after the last refill.
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10) (property "Reference" "TP1")
  (pad "1" smd circle (at 0 0) (size 1 1) (layers "F.Cu")))
(footprint "Lib:Part" (layer "F.Cu") (at 20 10) (property "Reference" "R1")
  (pad "1" smd circle (at 0 0) (size 1 1) (layers "F.Cu") (net "B"))
  (pad "2" smd circle (at 0 8) (size 1 1) (layers "F.Cu") (net "B")))
(zone (net "A") (layers "F.Cu") (filled_polygon (layer "F.Cu") (pts (xy 5 5) (xy 25 5) (xy 25 15) (xy 5 15))))
"#
    );
    let (text, report) = export_text(
        &parse(&edges),
        &Options {
            zones: true,
            ..options
        },
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // One island with two holes (the net-less pad and R1.1); R1.2 lies
    // outside the fill and cuts nothing. Each hole is a loop on the top
    // and on the bottom face.
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('copper ").count(), 1);
    assert_eq!(text.matches("MANIFOLD_SOLID_BREP('pad ").count(), 3);
    assert_eq!(text.matches("FACE_BOUND(").count(), 4);
}

#[test]
fn polygon_point_lists_may_hold_arcs() {
    let text = parse(&format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10) (property "Reference" "U1")
  (pad "1" smd custom (at 0 0) (size 0.000001 0.000001) (layers "F.Cu") (options (clearance outline) (anchor circle))
    (primitives (gr_poly (pts (xy 0 0) (xy 1 0) (arc (start 1 0) (mid 1.5 0.5) (end 1 1)) (xy 0 1)) (width 0) (fill yes)))))
(zone (net "GND") (layers "B.Cu") (filled_polygon (layer "B.Cu") (pts (xy 0 0) (xy 1 0) (arc (start 1 0) (mid 1.5 0.5) (end 1 1)) (xy 0 1))))
"#
    ));
    let board = Board::parse(text.as_bytes()).unwrap();
    let frame = Frame { origin: Vec2::ZERO };
    let mut warnings = Vec::new();
    let edges = crate::copper::pad_outline(&board, frame, &board.pads[0], &mut warnings).unwrap();
    let half_disk = PI * 0.5 * 0.5 / 2.0;
    let area = Loop::new(edges).area().abs();
    assert!((area - (1.0 + half_disk)).abs() < 0.005, "{area}");
    assert!(warnings.is_empty(), "{warnings:?}");
    let fill = &board.fill_points
        [board.fills[0].points.start as usize..board.fills[0].points.end as usize];
    assert!(fill.len() > 6);
    assert!((crate::geom::signed_area(fill).abs() - (1.0 + half_disk)).abs() < 0.005);
}

#[test]
fn stroke_font_matches_kicad_layout() {
    use crate::font::{TextStyle, strokes};
    let style = TextStyle {
        size: Vec2::new(1.0, 1.0),
        thickness: 0.15,
        bold: false,
        italic: false,
        halign: 0,
        valign: 0,
        mirror: false,
        angle: 0.0,
    };
    // H is three strokes: two stems and a bar joining them.
    let h = strokes("H", Vec2::ZERO, &style);
    assert_eq!(h.len(), 3);
    let bar = h.iter().find(|s| (s[0].y - s[1].y).abs() < 1e-9).unwrap();
    assert!((bar[0].x - bar[1].x).abs() > 0.3);
    // Mirroring reflects every point about the anchor's x.
    let mirrored = strokes(
        "H",
        Vec2::ZERO,
        &TextStyle {
            mirror: true,
            ..style.clone()
        },
    );
    for (a, b) in h.iter().flatten().zip(mirrored.iter().flatten()) {
        assert!((a.x + b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
    }
    // A quarter turn swaps the extents.
    let turned = strokes(
        "H",
        Vec2::ZERO,
        &TextStyle {
            angle: 90.0,
            ..style.clone()
        },
    );
    let extent = |s: &[Vec<Vec2>]| {
        let xs = s.iter().flatten().map(|p| p.x);
        let ys = s.iter().flatten().map(|p| p.y);
        (
            xs.clone().fold(f64::NEG_INFINITY, f64::max) - xs.fold(f64::INFINITY, f64::min),
            ys.clone().fold(f64::NEG_INFINITY, f64::max) - ys.fold(f64::INFINITY, f64::min),
        )
    };
    let (w, hgt) = extent(&h);
    let (tw, th) = extent(&turned);
    assert!((w - th).abs() < 1e-9 && (hgt - tw).abs() < 1e-9);
    // Pen width: the stated thickness, a fifth of the width when bold
    // with no thickness, an eighth otherwise, and never over a quarter
    // of the smaller size.
    assert!((style.pen_width() - 0.15).abs() < 1e-12);
    let auto = TextStyle {
        thickness: 0.0,
        ..style.clone()
    };
    assert!((auto.pen_width() - 0.125).abs() < 1e-12);
    assert!(
        (TextStyle {
            bold: true,
            ..auto.clone()
        }
        .pen_width()
            - 0.2)
            .abs()
            < 1e-12
    );
    assert!(
        (TextStyle {
            thickness: 0.5,
            ..style.clone()
        }
        .pen_width()
            - 0.25)
            .abs()
            < 1e-12
    );
    // Markup: an overbar adds one stroke, a superscript shrinks glyphs.
    assert_eq!(strokes("~{H}", Vec2::ZERO, &style).len(), 4);
    let (sw, _) = extent(&strokes("^{H}", Vec2::ZERO, &style));
    assert!(sw < w);
    assert_eq!(
        crate::faces::expand(
            "${PCB_VERSION} of ${X}",
            &[("PCB_VERSION".into(), "v1".into())]
        ),
        "v1 of ${X}"
    );
}

#[test]
fn parses_silk_mask_and_tenting() {
    let text = parse(&format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "B.Cu") (at 10 10 90) (property "Reference" "R1" (at 0 -1.5 90) (layer "B.SilkS") (effects (font (size 1 0.8) (thickness 0.15)) (justify mirror)))
  (property "Value" "10k" (at 0 1.5 90) (layer "B.Fab") (hide yes) (effects (font (size 1 1))))
  (solder_mask_margin 0.1)
  (fp_text user "${REFERENCE}-${VALUE}" (at 0 0 90 unlocked) (layer "B.SilkS") (effects (font (size 0.5 0.5) (thickness 0.08) bold) (justify left top mirror)))
  (fp_line (start -1 -1) (end 1 -1) (stroke (width 0.12) (type solid)) (layer "B.SilkS"))
  (fp_circle (center 0 0) (end 1 0) (stroke (width 0.1) (type solid)) (fill yes) (layer "F.Mask"))
  (pad "1" smd rect (at -1 0 90) (size 1 1.5) (layers "B.Cu" "B.Mask") (solder_mask_margin 0.2))
  (pad "2" smd rect (at 1 0 90) (size 1 1.5) (layers "B.Cu" "B.Mask")))
(gr_text "Hi" (at 5 5 200) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
(gr_rect (start 3 3) (end 6 6) (stroke (width 0.2) (type solid)) (fill yes) (layer "F.SilkS"))
(via (at 5 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting front))
(via (at 6 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front no) (back yes)))
(via (at 7 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu"))
"#
    ));
    let board = Board::parse(text.as_bytes()).unwrap();
    assert!((board.mask_expansion - 0.0).abs() < 1e-12);
    assert_eq!(board.tent, [false, false]);
    // Texts: the visible reference, the user text with its variables
    // resolved, and the board text; the hidden value is skipped.
    assert_eq!(board.texts.len(), 3);
    let reference = &board.texts[0];
    assert_eq!(reference.text, "R1");
    assert_eq!(reference.layer, crate::board::Tech::BackSilk);
    assert!(reference.style.mirror);
    // Size is written height first.
    assert!(
        (reference.style.size.x - 0.8).abs() < 1e-12
            && (reference.style.size.y - 1.0).abs() < 1e-12
    );
    assert!((reference.style.angle - 90.0).abs() < 1e-12);
    let user = &board.texts[1];
    assert_eq!(user.text, "R1-10k");
    assert!(user.style.bold && user.style.halign == -1 && user.style.valign == -1);
    // The board text at 200 degrees is not kept upright; footprint text is.
    assert!((board.texts[2].style.angle - 200.0).abs() < 1e-12);
    // Shapes: the back silk line, the front mask disc, the filled rect.
    assert_eq!(board.shapes.len(), 3);
    assert!(
        board
            .shapes
            .iter()
            .any(|s| s.layer == crate::board::Tech::FrontMask && s.filled)
    );
    // Pads open the back mask; the first with its own margin, the second
    // with the footprint's.
    assert_eq!(board.pads.len(), 2);
    assert!(board.pads.iter().all(|p| p.mask == [false, true]));
    assert_eq!(board.pads[0].mask_margin, Some(0.2));
    assert_eq!(board.pads[1].mask_margin, Some(0.1));
    assert_eq!(board.vias[0].tented, [Some(true), None]);
    assert_eq!(board.vias[1].tented, [Some(false), Some(true)]);
    assert_eq!(board.vias[2].tented, [None, None]);
}

#[test]
fn silkscreen_and_mask_are_flat_faces() {
    let edges = format!(
        "{}{}",
        rect_outline(0.0, 0.0, 30.0, 20.0),
        r#"(footprint "Lib:Part" (layer "F.Cu") (at 10 10) (property "Reference" "R1" (at 0 -1.5) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (pad "1" smd rect (at -1 0) (size 1 1.5) (layers "F.Cu" "F.Mask"))
  (pad "2" smd rect (at 1 0) (size 1 1.5) (layers "F.Cu" "F.Mask")))
(gr_text "${PCB_VERSION}" (at 20 5) (layer "B.SilkS") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
(via (at 20 15) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front no) (back yes)))
"#
    );
    let options = Options {
        silkscreen: true,
        soldermask: true,
        components: false,
        text_variables: vec![("PCB_VERSION".into(), "v1".into())],
        ..Options::default()
    };
    let (text, report) = export_text(&parse(&edges), &options);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // Two silkscreen and two mask products, each a set of flat faces.
    assert_eq!(text.matches("PRODUCT('board_silkscreen'").count(), 2);
    assert_eq!(text.matches("PRODUCT('board_soldermask'").count(), 2);
    assert!(text.contains("Top Silkscreen") && text.contains("Bottom Soldermask"));
    assert!(count(&text, "SHELL_BASED_SURFACE_MODEL") > 4);
    assert_eq!(count(&text, "SURFACE_STYLE_TRANSPARENT"), 4);
    // Silk sits 0.04 mm and mask 0.015 mm above the front copper, and
    // as far below the back copper.
    let top = 1.6162 + 0.035;
    for z in [top + 0.04, top + 0.015, -0.035 - 0.04, -0.035 - 0.015] {
        assert!(
            text.contains(&format!("CARTESIAN_POINT('',(0.0,0.0,{}))", fmt9(z))),
            "{z}"
        );
    }
    // The front mask face has holes for both pads and the untented via
    // ring and drill; the back mask has none.
    let front_mask = text.find("PRODUCT('board_soldermask'").unwrap();
    let back_mask = text[front_mask + 1..]
        .find("PRODUCT('board_soldermask'")
        .unwrap()
        + front_mask
        + 1;
    let before = &text[..front_mask];
    let between = &text[front_mask..back_mask];
    assert!(
        before.matches("FACE_BOUND(").count() >= 4,
        "{}",
        before.matches("FACE_BOUND(").count()
    );
    assert_eq!(between.matches("FACE_BOUND(").count(), 0);
    // "v1" draws fewer strokes than "${PCB_VERSION}" would.
    let literal = export_text(
        &parse(&edges),
        &Options {
            text_variables: Vec::new(),
            ..options.clone()
        },
    )
    .0;
    assert!(
        count(&literal, "SHELL_BASED_SURFACE_MODEL") > count(&text, "SHELL_BASED_SURFACE_MODEL")
    );
}
