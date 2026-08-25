//! Emit the interposer as a KiCad board via `pcb_ir::dialects::kicad`.
//!
//! The deterministic board: the panel's outline, tooling holes, and
//! fiducials, the S13 mate lands on the bottom copper, and full-sheet
//! GND pours on both faces — GND rides the pours, unrouted; the
//! post-route stitching pass (`crate::stitch`) adds the vias that tie
//! the faces together around whatever copper the router drew. With a
//! fixture plan, the board is also populated — a pogo pad on the top
//! face at every tested contact, and the plan's nets bound on both the
//! pogo and its mate land — so the unrouted airwires are exactly the
//! routing pass's work list. Without a plan (no ICT contacts), GND
//! lands join the pour and everything else stays un-netted.

use pcb_ir::dialects::kicad::{
    At, Document, Footprint, FootprintAttrs, Graphic, Mount, Pad, PadKind, PadShape, Property,
    Stroke, UuidGen, Zone, ZoneConnect, ZoneFill,
};
use pcb_ir::geom::Point;

use crate::panel::{Outline, Panel};
use crate::pattern::{Land, Role};
use crate::plan::Plan;
use crate::pogo::PogoTemplate;

/// Mate land pad diameter: the target for the fixture bed's 2.54 mm
/// pitch pogo blocks — tip plus alignment margin, with ~1 mm of copper
/// gap left between neighbors.
const LAND_DIA_MM: f64 = 1.5;

/// Build the `.kicad_pcb` source.
pub fn board(panel: &Panel, lands: &[Land], plan: Option<&Plan>) -> String {
    let mut doc = Document::two_layer();
    doc.generator = "pcb-interposer".into();
    let mut uuids = UuidGen::new();
    let gnd = doc.net("GND");

    // The plan's nets, keyed both ways: per land index and per contact.
    let mut land_nets: std::collections::BTreeMap<usize, (u32, String)> = Default::default();
    let mut contact_nets: Vec<(usize, (u32, String))> = Vec::new();
    if let Some(plan) = plan {
        for binding in &plan.bindings {
            let net = (doc.net(&binding.net), binding.net.clone());
            if let Some(land) = binding.land {
                land_nets.insert(land, net.clone());
            }
            contact_nets.push((binding.contact, net));
        }
    }

    for outline in &panel.outline {
        doc.graphics.push(match *outline {
            Outline::Line { start, end } => Graphic::Line {
                start: Point::new(start[0], start[1]),
                end: Point::new(end[0], end[1]),
                stroke: Stroke::solid(0.1),
                layer: "Edge.Cuts".into(),
                uuid: uuids.next_uuid(),
            },
            Outline::Arc { start, mid, end } => Graphic::Arc {
                start: Point::new(start[0], start[1]),
                mid: Point::new(mid[0], mid[1]),
                end: Point::new(end[0], end[1]),
                stroke: Stroke::solid(0.1),
                layer: "Edge.Cuts".into(),
                uuid: uuids.next_uuid(),
            },
        });
    }

    for (index, (at, dia)) in panel.holes.iter().enumerate() {
        doc.footprints
            .push(hole_footprint(&mut uuids, index, *at, *dia));
    }
    for (index, at) in panel.fids_top.iter().enumerate() {
        doc.footprints
            .push(fid_footprint(&mut uuids, index, *at, true));
    }
    for (index, at) in panel.fids_bottom.iter().enumerate() {
        let ordinal = panel.fids_top.len() + index;
        doc.footprints
            .push(fid_footprint(&mut uuids, ordinal, *at, false));
    }
    for (index, land) in lands.iter().enumerate() {
        let net = land_nets
            .get(&index)
            .cloned()
            .or_else(|| (land.role == Role::Gnd).then(|| (gnd, "GND".to_string())));
        doc.footprints
            .push(land_footprint(&mut uuids, index, land, net));
    }
    if let Some(plan) = plan {
        let template = PogoTemplate::load().expect("vendored pogo template is valid");
        for (ordinal, (contact_index, net)) in contact_nets.iter().enumerate() {
            let contact = &plan.contacts[*contact_index];
            doc.raw_footprints.push(template.stamp(
                contact.xy,
                &format!("P{}", ordinal + 1),
                net.clone(),
                &mut uuids,
            ));
        }
    }

    for layer in ["F.Cu", "B.Cu"] {
        doc.zones.push(Zone {
            net: gnd,
            net_name: "GND".into(),
            layers: vec![layer.into()],
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
                Point::new(panel.width, 0.0),
                Point::new(panel.width, panel.height),
                Point::new(0.0, panel.height),
            ],
        });
    }

    pcb_ir::dialects::kicad::write(&doc)
}

fn hidden_properties(uuids: &mut UuidGen, reference: &str, top: bool) -> Vec<Property> {
    let (silk, fab) = if top {
        ("F.SilkS", "F.Fab")
    } else {
        ("B.SilkS", "B.Fab")
    };
    vec![
        Property {
            key: "Reference".into(),
            value: reference.into(),
            at: At::xy(0.0, -1.8),
            layer: silk.into(),
            hide: true,
            uuid: uuids.next_uuid(),
        },
        Property {
            key: "Value".into(),
            value: String::new(),
            at: At::xy(0.0, 1.8),
            layer: fab.into(),
            hide: true,
            uuid: uuids.next_uuid(),
        },
    ]
}

fn hole_footprint(uuids: &mut UuidGen, index: usize, at: [f64; 2], dia: f64) -> Footprint {
    Footprint {
        lib_id: "Interposer:ToolingHole".into(),
        layer: "F.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(at[0], at[1]),
        properties: hidden_properties(uuids, &format!("H{}", index + 1), true),
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
            size: (dia, dia),
            drill: Some(dia),
            layers: vec!["*.Cu".into(), "*.Mask".into()],
            net: None,
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    }
}

/// A global fiducial: Ø1 copper dot with a Ø2 mask opening. The pad-level
/// clearance keeps pour copper outside the mask aperture so the aperture
/// never bridges the dot with poured GND.
fn fid_footprint(uuids: &mut UuidGen, index: usize, at: [f64; 2], top: bool) -> Footprint {
    let (copper, mask) = if top {
        ("F.Cu", "F.Mask")
    } else {
        ("B.Cu", "B.Mask")
    };
    Footprint {
        lib_id: "Interposer:Fiducial_1.0_2.0".into(),
        layer: copper.into(),
        uuid: uuids.next_uuid(),
        at: At::xy(at[0], at[1]),
        properties: hidden_properties(uuids, &format!("FID{}", index + 1), top),
        attrs: FootprintAttrs {
            mount: Some(Mount::Smd),
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: String::new(),
            kind: PadKind::Smd,
            shape: PadShape::Circle,
            at: At::default(),
            size: (1.0, 1.0),
            drill: None,
            layers: vec![copper.into(), mask.into()],
            net: None,
            solder_mask_margin: Some(0.5),
            clearance: Some(0.6),
            uuid: uuids.next_uuid(),
        }],
    }
}

fn land_footprint(
    uuids: &mut UuidGen,
    index: usize,
    land: &Land,
    net: Option<(u32, String)>,
) -> Footprint {
    let mut properties = hidden_properties(uuids, &format!("L{}", index + 1), false);
    properties.push(Property {
        key: "Ict".into(),
        value: land.role.name().into(),
        at: At::xy(0.0, 3.0),
        layer: "B.Fab".into(),
        hide: true,
        uuid: uuids.next_uuid(),
    });
    Footprint {
        lib_id: "Interposer:Mate_Pad_D1.5mm".into(),
        layer: "B.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(land.xy[0], land.xy[1]),
        properties,
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
            size: (LAND_DIA_MM, LAND_DIA_MM),
            drill: None,
            layers: vec!["B.Cu".into(), "B.Mask".into()],
            net,
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    }
}

/// The sibling `.kicad_pro`: design rules the later routing passes are
/// built for, so every interposer artifact carries one consistent rule
/// set from the start.
pub fn project() -> String {
    r##"{
  "board": {
    "design_settings": {
      "rules": {
        "max_error": 0.005,
        "min_clearance": 0.1,
        "min_connection": 0.0,
        "min_copper_edge_clearance": 0.3,
        "min_hole_clearance": 0.25,
        "min_hole_to_hole": 0.25,
        "min_microvia_diameter": 0.2,
        "min_microvia_drill": 0.1,
        "min_resolved_spokes": 1,
        "min_silk_clearance": 0.0,
        "min_text_height": 0.8,
        "min_text_thickness": 0.08,
        "min_through_hole_diameter": 0.3,
        "min_track_width": 0.15,
        "min_via_annular_width": 0.1,
        "min_via_diameter": 0.5
      }
    }
  },
  "net_settings": {
    "classes": [
      {
        "name": "Default",
        "clearance": 0.2,
        "track_width": 0.25,
        "via_diameter": 0.6,
        "via_drill": 0.3
      },
      {
        "name": "USB",
        "clearance": 0.1,
        "track_width": 0.2,
        "via_diameter": 0.6,
        "via_drill": 0.3
      }
    ],
    "netclass_patterns": [
      {
        "netclass": "USB",
        "pattern": "*USB_DP*"
      },
      {
        "netclass": "USB",
        "pattern": "*USB_DM*"
      }
    ]
  },
  "meta": {
    "version": 3
  }
}
"##
    .to_string()
}
