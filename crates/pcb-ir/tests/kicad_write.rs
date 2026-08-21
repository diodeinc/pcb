//! The kicad dialect writer produces well-formed, deterministic board text.

use pcb_ir::dialects::kicad::{
    self, At, Document, Footprint, FootprintAttrs, Graphic, Mount, Pad, PadKind, PadShape,
    Property, Segment, Stroke, UuidGen, Via, Zone, ZoneConnect, ZoneFill,
};
use pcb_ir::geom::Point;
use pcb_sexpr::SexprKind;

/// A small but representative board: outline, an SMT pad footprint with a
/// bound net, an NPTH tooling hole, one track, a terminal via, and a GND
/// zone.
fn sample() -> Document {
    let mut doc = Document::two_layer();
    let mut uuids = UuidGen::new();
    let gnd = doc.net("GND");
    let sig = doc.net("B0.TP1");

    doc.graphics.push(Graphic::Rect {
        start: Point::new(0.0, 0.0),
        end: Point::new(74.0, 105.0),
        stroke: Stroke::solid(0.1),
        fill: false,
        layer: "Edge.Cuts".into(),
        uuid: uuids.next_uuid(),
    });

    doc.footprints.push(Footprint {
        lib_id: "Interposer:Pad_D1.0mm".into(),
        layer: "F.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(10.0, 20.5),
        properties: vec![
            Property {
                key: "Reference".into(),
                value: "P1".into(),
                at: At {
                    x: 0.0,
                    y: -1.4,
                    rot: 90.0,
                },
                layer: "F.SilkS".into(),
                hide: true,
                uuid: uuids.next_uuid(),
            },
            Property {
                key: "Value".into(),
                value: String::new(),
                at: At::xy(0.0, 1.4),
                layer: "F.Fab".into(),
                hide: true,
                uuid: uuids.next_uuid(),
            },
        ],
        attrs: FootprintAttrs {
            mount: Some(Mount::Smd),
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Circle,
            at: At::default(),
            size: (1.0, 1.0),
            drill: None,
            layers: vec!["F.Cu".into(), "F.Mask".into()],
            net: Some((sig, "B0.TP1".into())),
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    });

    doc.footprints.push(Footprint {
        lib_id: "Interposer:ToolingHole".into(),
        layer: "F.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(3.0, 3.0),
        properties: Vec::new(),
        attrs: FootprintAttrs {
            mount: None,
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: String::new(),
            kind: PadKind::NpThruHole,
            shape: PadShape::Circle,
            at: At::default(),
            size: (2.1, 2.1),
            drill: Some(2.1),
            layers: vec!["*.Cu".into(), "*.Mask".into()],
            net: None,
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    });

    doc.segments.push(Segment {
        start: Point::new(10.0, 20.5),
        end: Point::new(14.25, 24.75),
        width: 0.25,
        layer: "F.Cu".into(),
        net: sig,
        uuid: uuids.next_uuid(),
    });
    doc.vias.push(Via {
        at: Point::new(14.25, 24.75),
        size: 0.6,
        drill: 0.3,
        layers: ("F.Cu".into(), "B.Cu".into()),
        net: sig,
        uuid: uuids.next_uuid(),
    });
    doc.zones.push(Zone {
        net: gnd,
        net_name: "GND".into(),
        layers: vec!["B.Cu".into()],
        uuid: uuids.next_uuid(),
        name: None,
        priority: None,
        hatch_pitch: 0.5,
        connect_pads: ZoneConnect::Thermal,
        connect_clearance: 0.25,
        min_thickness: 0.25,
        fill: ZoneFill {
            enabled: true,
            thermal_gap: 0.3,
            thermal_bridge_width: 0.4,
        },
        polygon: vec![
            Point::new(0.0, 0.0),
            Point::new(74.0, 0.0),
            Point::new(74.0, 105.0),
            Point::new(0.0, 105.0),
        ],
    });
    doc
}

#[test]
fn writes_a_parseable_board() {
    let text = kicad::write(&sample());
    // For validating against real KiCad tooling out of band.
    if let Some(path) = std::env::var_os("KICAD_WRITE_DUMP") {
        std::fs::write(path, &text).unwrap();
    }
    let root = pcb_sexpr::parse(&text).expect("writer output parses as s-expressions");

    let SexprKind::List(items) = &root.kind else {
        panic!("root is a list");
    };
    assert_eq!(items[0].as_atom(), Some("kicad_pcb"));

    let count = |name: &str| {
        items
            .iter()
            .filter(|item| {
                matches!(&item.kind, SexprKind::List(children)
                    if children.first().and_then(|c| c.as_atom()) == Some(name))
            })
            .count()
    };
    assert_eq!(count("net"), 3);
    assert_eq!(count("footprint"), 2);
    assert_eq!(count("segment"), 1);
    assert_eq!(count("via"), 1);
    assert_eq!(count("zone"), 1);
    assert_eq!(count("gr_rect"), 1);
    assert_eq!(count("layers"), 1);
}

#[test]
fn output_is_deterministic_and_kicad_shaped() {
    let text = kicad::write(&sample());
    assert_eq!(text, kicad::write(&sample()));

    // Spot-check the exact shapes pcbnew expects.
    assert!(text.starts_with("(kicad_pcb\n\t(version 20260206)\n"));
    assert!(text.contains("\t(net 0 \"\")\n"));
    assert!(text.contains("\t(net 1 \"GND\")\n"));
    assert!(text.contains("(pad \"1\" smd circle\n"));
    assert!(text.contains("(pad \"\" np_thru_hole circle\n"));
    assert!(text.contains("(attr smd exclude_from_pos_files exclude_from_bom)"));
    assert!(text.contains("(hatch edge 0.5)"));
    assert!(text.contains("(fill yes\n"));
    assert!(text.trim_end().ends_with(')'));
    // Numbers trim like pcbnew's: no trailing zeros, no float noise.
    assert!(text.contains("(at 10 20.5)"));
    // Property rotation survives, and zero angles are still written out.
    assert!(text.contains("(at 0 -1.4 90)"));
    assert!(text.contains("(at 0 1.4 0)"));
    assert!(text.contains("(size 2.1 2.1)"));
}

#[test]
fn raw_footprints_splice_verbatim() {
    let mut doc = Document::two_layer();
    doc.raw_footprints.push(
        "(footprint \"Lib:Part\"\n\t(layer \"F.Cu\")\n\t(at 1 2)\n\t(pad \"1\" smd circle\n\t\t(at 0 0)\n\t\t(size 1 1)\n\t\t(layers \"F.Cu\")\n\t)\n)"
            .into(),
    );
    let text = kicad::write(&doc);
    // Re-indented one level under the document root, and still parseable.
    assert!(text.contains("\t(footprint \"Lib:Part\"\n\t\t(layer \"F.Cu\")"));
    pcb_sexpr::parse(&text).expect("spliced output parses");
}

#[test]
fn number_formatting_matches_kicad() {
    // Exercised through a segment width, which passes straight through num().
    let mut doc = Document::two_layer();
    let mut uuids = UuidGen::new();
    for width in [0.1234567, 1.0, 0.25, -0.0] {
        doc.segments.push(Segment {
            start: Point::new(0.0, 0.0),
            end: Point::new(1.0, 1.0),
            width,
            layer: "F.Cu".into(),
            net: 0,
            uuid: uuids.next_uuid(),
        });
    }
    let text = kicad::write(&doc);
    assert!(text.contains("(width 0.123457)"));
    assert!(text.contains("(width 1)"));
    assert!(text.contains("(width 0.25)"));
    assert!(text.contains("(width 0)"));
}
