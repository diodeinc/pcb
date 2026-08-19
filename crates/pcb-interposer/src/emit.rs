//! Emit the routed interposer as a real KiCad board (`.kicad_pcb` +
//! `.kicad_pro`), ready for `kicad-cli pcb drc --refill-zones`.
//!
//! The board carries: the sheet outline, SMT pogo pads on the top copper,
//! SMT mate lands on the bottom, every routed trace at its real width, the
//! terminal vias-in-pad, GND stitch vias (each GND pogo into the bottom
//! pour, each GND land into the top pour), tooling holes, and full-sheet
//! GND zones on both faces. The project file sets the design rules the
//! router promised: 0.2 mm minimum clearance overall and a USB netclass
//! at 0.1 mm so the 0.15 mm intra-pair gap is legal.

use std::collections::BTreeMap;

use crate::route::RouteResult;
use crate::types::{Assign, ContactId, Kind, Problem};

/// Deterministic well-formed UUIDs so re-emits diff cleanly.
struct Uuids(u64);

impl Uuids {
    fn next(&mut self) -> String {
        self.0 += 1;
        format!("00000000-0000-4000-8000-{:012x}", self.0)
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The same tooling holes the router blocks out.
pub fn tooling_holes(w: f64, h: f64) -> Vec<[f64; 2]> {
    let mut holes = vec![[2.5, 2.5], [71.5, 2.5], [2.5, 102.5], [71.5, 102.5]];
    if w > 80.0 || h > 110.0 {
        holes.extend([
            [3.0, 3.0],
            [w - 3.0, 3.0],
            [3.0, h - 3.0],
            [w - 3.0, h - 3.0],
        ]);
    }
    holes
}

fn pad_footprint(
    u: &mut Uuids,
    reference: &str,
    xy: [f64; 2],
    top: bool,
    net: Option<(usize, &str)>,
) -> String {
    let (fp_layer, pad_layers) = if top {
        ("F.Cu", "\"F.Cu\" \"F.Mask\"")
    } else {
        ("B.Cu", "\"B.Cu\" \"B.Mask\"")
    };
    let net_s = net
        .map(|(n, name)| format!("\n\t\t\t(net {n} \"{name}\")"))
        .unwrap_or_default();
    format!(
        "\t(footprint \"Interposer:Pad_D1.0mm\"\n\
         \t\t(layer \"{fp_layer}\")\n\
         \t\t(uuid \"{u0}\")\n\
         \t\t(at {x} {y})\n\
         \t\t(property \"Reference\" \"{reference}\"\n\
         \t\t\t(at 0 -1.4 0)\n\
         \t\t\t(layer \"{silk}\")\n\
         \t\t\t(hide yes)\n\
         \t\t\t(uuid \"{u1}\")\n\
         \t\t\t(effects (font (size 1 1) (thickness 0.15)))\n\
         \t\t)\n\
         \t\t(property \"Value\" \"\"\n\
         \t\t\t(at 0 1.4 0)\n\
         \t\t\t(layer \"{fab}\")\n\
         \t\t\t(hide yes)\n\
         \t\t\t(uuid \"{u2}\")\n\
         \t\t\t(effects (font (size 1 1) (thickness 0.15)))\n\
         \t\t)\n\
         \t\t(attr smd exclude_from_pos_files exclude_from_bom)\n\
         \t\t(pad \"1\" smd circle\n\
         \t\t\t(at 0 0)\n\
         \t\t\t(size 1 1)\n\
         \t\t\t(layers {pad_layers}){net_s}\n\
         \t\t\t(uuid \"{u3}\")\n\
         \t\t)\n\
         \t)\n",
        x = fmt(xy[0]),
        y = fmt(xy[1]),
        silk = if top { "F.SilkS" } else { "B.SilkS" },
        fab = if top { "F.Fab" } else { "B.Fab" },
        u0 = u.next(),
        u1 = u.next(),
        u2 = u.next(),
        u3 = u.next(),
    )
}

fn hole_footprint(u: &mut Uuids, reference: &str, xy: [f64; 2], drill: f64) -> String {
    format!(
        "\t(footprint \"Interposer:ToolingHole\"\n\
         \t\t(layer \"F.Cu\")\n\
         \t\t(uuid \"{u0}\")\n\
         \t\t(at {x} {y})\n\
         \t\t(property \"Reference\" \"{reference}\"\n\
         \t\t\t(at 0 -2.2 0)\n\
         \t\t\t(layer \"F.SilkS\")\n\
         \t\t\t(hide yes)\n\
         \t\t\t(uuid \"{u1}\")\n\
         \t\t\t(effects (font (size 1 1) (thickness 0.15)))\n\
         \t\t)\n\
         \t\t(property \"Value\" \"\"\n\
         \t\t\t(at 0 2.2 0)\n\
         \t\t\t(layer \"F.Fab\")\n\
         \t\t\t(hide yes)\n\
         \t\t\t(uuid \"{u2}\")\n\
         \t\t\t(effects (font (size 1 1) (thickness 0.15)))\n\
         \t\t)\n\
         \t\t(attr exclude_from_pos_files exclude_from_bom)\n\
         \t\t(pad \"\" np_thru_hole circle\n\
         \t\t\t(at 0 0)\n\
         \t\t\t(size {d} {d})\n\
         \t\t\t(drill {d})\n\
         \t\t\t(layers \"*.Cu\" \"*.Mask\")\n\
         \t\t\t(uuid \"{u3}\")\n\
         \t\t)\n\
         \t)\n",
        x = fmt(xy[0]),
        y = fmt(xy[1]),
        d = fmt(drill),
        u0 = u.next(),
        u1 = u.next(),
        u2 = u.next(),
        u3 = u.next(),
    )
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

/// Build the `.kicad_pcb` source for one routed panel.
pub fn emit_kicad(
    sheet_w: f64,
    sheet_h: f64,
    problem: &Problem,
    assign: &Assign,
    route: &RouteResult,
) -> String {
    let mut u = Uuids(0);

    // ---- nets: 0 = none, 1 = GND, then one per routed non-GND contact ----
    let mut net_of: BTreeMap<ContactId, usize> = BTreeMap::new();
    let mut net_names: Vec<String> = vec![String::new(), "GND".into()];
    for c in problem.contacts.values() {
        if c.ict.kind() == Some(Kind::Gnd) {
            net_of.insert(c.id, 1);
        }
    }
    for t in &route.traces {
        if let std::collections::btree_map::Entry::Vacant(e) = net_of.entry(t.contact) {
            let c = &problem.contacts[&t.contact];
            net_names.push(sanitize(&c.path));
            e.insert(net_names.len() - 1);
        }
    }

    let mut s = String::new();
    s.push_str(
        "(kicad_pcb\n\
         \t(version 20260206)\n\
         \t(generator \"pcb-interposer\")\n\
         \t(generator_version \"10.0\")\n\
         \t(general\n\t\t(thickness 1.6)\n\t\t(legacy_teardrops no)\n\t)\n\
         \t(paper \"A4\")\n\
         \t(layers\n\
         \t\t(0 \"F.Cu\" signal)\n\
         \t\t(2 \"B.Cu\" signal)\n\
         \t\t(13 \"F.Paste\" user)\n\
         \t\t(15 \"B.Paste\" user)\n\
         \t\t(5 \"F.SilkS\" user \"F.Silkscreen\")\n\
         \t\t(7 \"B.SilkS\" user \"B.Silkscreen\")\n\
         \t\t(1 \"F.Mask\" user)\n\
         \t\t(3 \"B.Mask\" user)\n\
         \t\t(25 \"Edge.Cuts\" user)\n\
         \t\t(29 \"B.CrtYd\" user \"B.Courtyard\")\n\
         \t\t(31 \"F.CrtYd\" user \"F.Courtyard\")\n\
         \t\t(33 \"B.Fab\" user)\n\
         \t\t(35 \"F.Fab\" user)\n\
         \t)\n\
         \t(setup\n\
         \t\t(pad_to_mask_clearance 0)\n\
         \t\t(allow_soldermask_bridges_in_footprints no)\n\
         \t)\n",
    );

    for (i, name) in net_names.iter().enumerate() {
        s.push_str(&format!("\t(net {i} \"{name}\")\n"));
    }

    // ---- outline ----
    s.push_str(&format!(
        "\t(gr_rect\n\t\t(start 0 0)\n\t\t(end {w} {h})\n\t\t(stroke (width 0.1) (type solid))\n\t\t(fill no)\n\t\t(layer \"Edge.Cuts\")\n\t\t(uuid \"{u0}\")\n\t)\n",
        w = fmt(sheet_w),
        h = fmt(sheet_h),
        u0 = u.next(),
    ));

    // ---- tooling holes ----
    for (i, hxy) in tooling_holes(sheet_w, sheet_h).iter().enumerate() {
        s.push_str(&hole_footprint(&mut u, &format!("H{}", i + 1), *hxy, 3.2));
    }

    // ---- pogo pads (top) ----
    for (i, c) in problem.contacts.values().enumerate() {
        let net = net_of.get(&c.id).map(|n| (*n, net_names[*n].as_str()));
        s.push_str(&pad_footprint(
            &mut u,
            &format!("P{}", i + 1),
            c.xy,
            true,
            net,
        ));
    }

    // ---- mate lands (bottom) ----
    let mut pin_net: BTreeMap<crate::types::MatePinId, usize> = BTreeMap::new();
    for slot in problem.slots.values() {
        if slot.kind == Kind::Gnd {
            for p in &slot.pins {
                pin_net.insert(*p, 1);
            }
        }
    }
    // Routed contacts bind the land their trace actually terminates on —
    // the router may have re-slotted a net away from the matcher's choice,
    // and binding the stale assigned land too would leave a floating pad on
    // the net (KiCad flags it unconnected). Only unrouted contacts fall
    // back to the matcher's assignment.
    let routed: std::collections::BTreeSet<ContactId> =
        route.traces.iter().map(|t| t.contact).collect();
    for (cid, pid) in &assign.contact_to_pin {
        if routed.contains(cid) {
            continue;
        }
        if let Some(n) = net_of.get(cid) {
            pin_net.entry(*pid).or_insert(*n);
        }
    }
    for t in &route.traces {
        let Some(n) = net_of.get(&t.contact) else {
            continue;
        };
        // Traces run pogo → land, so only the far end and the terminal via
        // may bind a land. Endpoints are grid-snapped (≤0.15 mm off the pad
        // center) while a pogo can hover within ~0.5 mm of a foreign land,
        // so the match radius must stay well under that.
        let ends = [t.points.last(), t.term_via.as_ref()];
        for end in ends.into_iter().flatten() {
            for p in problem.pins.values() {
                if crate::types::dist(p.xy, *end) < 0.3 {
                    pin_net.insert(p.id, *n);
                }
            }
        }
    }
    for (i, p) in problem.pins.values().enumerate() {
        let net = pin_net.get(&p.id).map(|n| (*n, net_names[*n].as_str()));
        s.push_str(&pad_footprint(
            &mut u,
            &format!("L{}", i + 1),
            p.xy,
            false,
            net,
        ));
    }

    // ---- traces + terminal vias ----
    for t in &route.traces {
        let n = net_of.get(&t.contact).copied().unwrap_or(0);
        for (k, w) in t.points.windows(2).enumerate() {
            let layer = if t.layer_of.get(k).copied().unwrap_or(0) == 1 {
                "F.Cu"
            } else {
                "B.Cu"
            };
            if crate::types::dist(w[0], w[1]) < 1e-6 {
                continue;
            }
            s.push_str(&format!(
                "\t(segment\n\t\t(start {x1} {y1})\n\t\t(end {x2} {y2})\n\t\t(width {w})\n\t\t(layer \"{layer}\")\n\t\t(net {n})\n\t\t(uuid \"{u0}\")\n\t)\n",
                x1 = fmt(w[0][0]),
                y1 = fmt(w[0][1]),
                x2 = fmt(w[1][0]),
                y2 = fmt(w[1][1]),
                w = fmt(t.width),
                u0 = u.next(),
            ));
        }
        if let Some(v) = t.term_via {
            let (size, drill) = if t.kind == Kind::Vtarget || t.kind == Kind::Vusb {
                (0.8, 0.4)
            } else {
                (0.6, 0.3)
            };
            s.push_str(&format!(
                "\t(via\n\t\t(at {x} {y})\n\t\t(size {size})\n\t\t(drill {drill})\n\t\t(layers \"F.Cu\" \"B.Cu\")\n\t\t(net {n})\n\t\t(uuid \"{u0}\")\n\t)\n",
                x = fmt(v[0]),
                y = fmt(v[1]),
                size = fmt(size),
                drill = fmt(drill),
                u0 = u.next(),
            ));
        }
    }

    // ---- GND stitching: via into the bottom pour per poured pogo, with a
    // short top stub when the pad spot is occupied underneath ----
    for (_, stub, xy) in &route.gnd_stitches {
        if stub.len() == 2 {
            s.push_str(&format!(
                "\t(segment\n\t\t(start {x1} {y1})\n\t\t(end {x2} {y2})\n\t\t(width 0.25)\n\t\t(layer \"F.Cu\")\n\t\t(net 1)\n\t\t(uuid \"{u0}\")\n\t)\n",
                x1 = fmt(stub[0][0]),
                y1 = fmt(stub[0][1]),
                x2 = fmt(stub[1][0]),
                y2 = fmt(stub[1][1]),
                u0 = u.next(),
            ));
        }
        s.push_str(&format!(
            "\t(via\n\t\t(at {x} {y})\n\t\t(size 0.6)\n\t\t(drill 0.3)\n\t\t(layers \"F.Cu\" \"B.Cu\")\n\t\t(net 1)\n\t\t(uuid \"{u0}\")\n\t)\n",
            x = fmt(xy[0]),
            y = fmt(xy[1]),
            u0 = u.next(),
        ));
    }

    // ---- GND land stitching: via joining each GND land's bottom fill
    // fragment to the top pour, with a short bottom stub when displaced ----
    for (_, stub, xy) in &route.gnd_land_stitches {
        if stub.len() == 2 {
            s.push_str(&format!(
                "\t(segment\n\t\t(start {x1} {y1})\n\t\t(end {x2} {y2})\n\t\t(width 0.25)\n\t\t(layer \"B.Cu\")\n\t\t(net 1)\n\t\t(uuid \"{u0}\")\n\t)\n",
                x1 = fmt(stub[0][0]),
                y1 = fmt(stub[0][1]),
                x2 = fmt(stub[1][0]),
                y2 = fmt(stub[1][1]),
                u0 = u.next(),
            ));
        }
        s.push_str(&format!(
            "\t(via\n\t\t(at {x} {y})\n\t\t(size 0.6)\n\t\t(drill 0.3)\n\t\t(layers \"F.Cu\" \"B.Cu\")\n\t\t(net 1)\n\t\t(uuid \"{u0}\")\n\t)\n",
            x = fmt(xy[0]),
            y = fmt(xy[1]),
            u0 = u.next(),
        ));
    }

    // ---- GND pour on both faces (the top pour joins the bottom fill's
    // fragments through the stitch vias) ----
    for layer in ["B.Cu", "F.Cu"] {
        s.push_str(&format!(
            "\t(zone\n\
             \t\t(net 1)\n\
             \t\t(net_name \"GND\")\n\
             \t\t(layer \"{layer}\")\n\
             \t\t(uuid \"{u0}\")\n\
             \t\t(hatch edge 0.5)\n\
             \t\t(connect_pads\n\t\t\t(clearance 0.25)\n\t\t)\n\
             \t\t(min_thickness 0.25)\n\
             \t\t(filled_areas_thickness no)\n\
             \t\t(fill yes\n\t\t\t(thermal_gap 0.3)\n\t\t\t(thermal_bridge_width 0.4)\n\t\t)\n\
             \t\t(polygon\n\t\t\t(pts\n\t\t\t\t(xy 0 0) (xy {w} 0) (xy {w} {h}) (xy 0 {h})\n\t\t\t)\n\t\t)\n\
             \t)\n",
            u0 = u.next(),
            w = fmt(sheet_w),
            h = fmt(sheet_h),
        ));
    }

    s.push_str("\t(embedded_fonts no)\n)\n");
    s
}

/// The sibling `.kicad_pro`: design rules matching what the router
/// guaranteed, plus a USB netclass legalizing the 0.15 mm intra-pair gap.
pub fn emit_project() -> String {
    r##"{
  "board": {
    "design_settings": {
      "rules": {
        "max_error": 0.005,
        "min_clearance": 0.1,
        "min_connection": 0.0,
        "min_copper_edge_clearance": 0.3,
        "min_groove_width": 0.0,
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
      },
      "rule_severities": {
        "lib_footprint_issues": "ignore",
        "lib_footprint_mismatch": "ignore"
      }
    }
  },
  "meta": {
    "filename": "interposer.kicad_pro",
    "version": 3
  },
  "net_settings": {
    "classes": [
      {
        "clearance": 0.2,
        "line_style": 0,
        "name": "Default",
        "priority": 2147483647,
        "track_width": 0.25,
        "via_diameter": 0.6,
        "via_drill": 0.3
      },
      {
        "clearance": 0.1,
        "line_style": 0,
        "name": "USB",
        "priority": 0,
        "track_width": 0.2,
        "via_diameter": 0.6,
        "via_drill": 0.3
      }
    ],
    "meta": { "version": 4 },
    "netclass_assignments": {},
    "netclass_patterns": [
      { "netclass": "USB", "pattern": "*USB_DP*" },
      { "netclass": "USB", "pattern": "*USB_DM*" }
    ]
  }
}
"##
    .to_string()
}
