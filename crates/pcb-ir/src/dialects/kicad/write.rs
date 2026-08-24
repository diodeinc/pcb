//! Serialize a [`Document`] into `.kicad_pcb` s-expression text.
//!
//! Output follows KiCad's own file conventions — tab indentation, one node
//! per line, quoted strings, numbers trimmed to at most six decimals — so
//! generated boards diff cleanly against pcbnew re-saves.

use super::{At, Document, Graphic, LayerKind, Mount, Pad, PadKind, PadShape, ZoneConnect};
use crate::geom::Point;

/// Render the document as complete `.kicad_pcb` text.
pub fn write(document: &Document) -> String {
    let mut w = Writer::default();
    w.open("kicad_pcb");
    w.line(&format!("(version {})", document.version));
    w.line(&format!("(generator {})", quote(&document.generator)));
    w.line(&format!(
        "(generator_version {})",
        quote(&document.generator_version)
    ));
    w.open("general");
    w.line(&format!("(thickness {})", num(document.thickness_mm)));
    w.line("(legacy_teardrops no)");
    w.close();
    w.line(&format!("(paper {})", quote(&document.paper)));

    w.open("layers");
    for layer in &document.layers {
        let kind = match layer.kind {
            LayerKind::Signal => "signal",
            LayerKind::Power => "power",
            LayerKind::Mixed => "mixed",
            LayerKind::Jumper => "jumper",
            LayerKind::User => "user",
        };
        let mut entry = format!("({} {} {kind}", layer.ordinal, quote(&layer.canonical));
        if let Some(user_name) = &layer.user_name {
            entry.push(' ');
            entry.push_str(&quote(user_name));
        }
        entry.push(')');
        w.line(&entry);
    }
    w.close();

    w.open("setup");
    w.line(&format!(
        "(pad_to_mask_clearance {})",
        num(document.setup.pad_to_mask_clearance)
    ));
    w.line(&format!(
        "(allow_soldermask_bridges_in_footprints {})",
        yes_no(document.setup.allow_soldermask_bridges_in_footprints)
    ));
    w.close();

    for (index, name) in document.nets.iter().enumerate() {
        w.line(&format!("(net {index} {})", quote(name)));
    }

    for footprint in &document.footprints {
        write_footprint(&mut w, footprint);
    }
    for raw in &document.raw_footprints {
        w.raw_block(raw);
    }
    for graphic in &document.graphics {
        write_graphic(&mut w, graphic);
    }
    for segment in &document.segments {
        w.open("segment");
        w.line(&format!("(start {})", xy(segment.start)));
        w.line(&format!("(end {})", xy(segment.end)));
        w.line(&format!("(width {})", num(segment.width)));
        w.line(&format!("(layer {})", quote(&segment.layer)));
        w.line(&format!("(net {})", segment.net));
        w.line(&format!("(uuid {})", quote(&segment.uuid)));
        w.close();
    }
    for arc in &document.arcs {
        w.open("arc");
        w.line(&format!("(start {})", xy(arc.start)));
        w.line(&format!("(mid {})", xy(arc.mid)));
        w.line(&format!("(end {})", xy(arc.end)));
        w.line(&format!("(width {})", num(arc.width)));
        w.line(&format!("(layer {})", quote(&arc.layer)));
        w.line(&format!("(net {})", arc.net));
        w.line(&format!("(uuid {})", quote(&arc.uuid)));
        w.close();
    }
    for via in &document.vias {
        w.open("via");
        w.line(&format!("(at {})", xy(via.at)));
        w.line(&format!("(size {})", num(via.size)));
        w.line(&format!("(drill {})", num(via.drill)));
        w.line(&format!(
            "(layers {} {})",
            quote(&via.layers.0),
            quote(&via.layers.1)
        ));
        w.line(&format!("(net {})", via.net));
        w.line(&format!("(uuid {})", quote(&via.uuid)));
        w.close();
    }
    for zone in &document.zones {
        write_zone(&mut w, zone);
    }

    w.line("(embedded_fonts no)");
    w.close();
    w.finish()
}

fn write_footprint(w: &mut Writer, footprint: &super::Footprint) {
    w.open_with("footprint", &quote(&footprint.lib_id));
    w.line(&format!("(layer {})", quote(&footprint.layer)));
    w.line(&format!("(uuid {})", quote(&footprint.uuid)));
    w.line(&format!("(at {})", at(footprint.at)));
    for property in &footprint.properties {
        w.open_with(
            "property",
            &format!("{} {}", quote(&property.key), quote(&property.value)),
        );
        // Properties always carry the angle, even when zero — pcbnew's shape.
        w.line(&format!(
            "(at {} {} {})",
            num(property.at.x),
            num(property.at.y),
            num(property.at.rot)
        ));
        w.line(&format!("(layer {})", quote(&property.layer)));
        if property.hide {
            w.line("(hide yes)");
        }
        w.line(&format!("(uuid {})", quote(&property.uuid)));
        w.line("(effects (font (size 1 1) (thickness 0.15)))");
        w.close();
    }
    let attrs = &footprint.attrs;
    if attrs.mount.is_some() || attrs.exclude_from_pos_files || attrs.exclude_from_bom {
        let mut entry = String::from("(attr");
        match attrs.mount {
            Some(Mount::Smd) => entry.push_str(" smd"),
            Some(Mount::ThroughHole) => entry.push_str(" through_hole"),
            None => {}
        }
        if attrs.exclude_from_pos_files {
            entry.push_str(" exclude_from_pos_files");
        }
        if attrs.exclude_from_bom {
            entry.push_str(" exclude_from_bom");
        }
        entry.push(')');
        w.line(&entry);
    }
    for pad in &footprint.pads {
        write_pad(w, pad);
    }
    w.close();
}

fn write_pad(w: &mut Writer, pad: &Pad) {
    let kind = match pad.kind {
        PadKind::Smd => "smd",
        PadKind::ThruHole => "thru_hole",
        PadKind::NpThruHole => "np_thru_hole",
    };
    let shape = match pad.shape {
        PadShape::Circle => "circle",
        PadShape::Rect => "rect",
        PadShape::Oval => "oval",
        PadShape::RoundRect { .. } => "roundrect",
    };
    w.open_with("pad", &format!("{} {kind} {shape}", quote(&pad.number)));
    w.line(&format!("(at {})", at(pad.at)));
    w.line(&format!("(size {} {})", num(pad.size.0), num(pad.size.1)));
    if let Some(drill) = pad.drill {
        w.line(&format!("(drill {})", num(drill)));
    }
    if let PadShape::RoundRect { ratio } = pad.shape {
        w.line(&format!("(roundrect_rratio {})", num(ratio)));
    }
    let layers = pad
        .layers
        .iter()
        .map(|layer| quote(layer))
        .collect::<Vec<_>>()
        .join(" ");
    w.line(&format!("(layers {layers})"));
    if let Some((net, name)) = &pad.net {
        w.line(&format!("(net {net} {})", quote(name)));
    }
    if let Some(margin) = pad.solder_mask_margin {
        w.line(&format!("(solder_mask_margin {})", num(margin)));
    }
    if let Some(clearance) = pad.clearance {
        w.line(&format!("(clearance {})", num(clearance)));
    }
    w.line(&format!("(uuid {})", quote(&pad.uuid)));
    w.close();
}

fn write_zone(w: &mut Writer, zone: &super::Zone) {
    w.open("zone");
    w.line(&format!("(net {})", zone.net));
    w.line(&format!("(net_name {})", quote(&zone.net_name)));
    if let [layer] = zone.layers.as_slice() {
        w.line(&format!("(layer {})", quote(layer)));
    } else {
        let layers = zone
            .layers
            .iter()
            .map(|layer| quote(layer))
            .collect::<Vec<_>>()
            .join(" ");
        w.line(&format!("(layers {layers})"));
    }
    w.line(&format!("(uuid {})", quote(&zone.uuid)));
    if let Some(name) = &zone.name {
        w.line(&format!("(name {})", quote(name)));
    }
    if let Some(priority) = zone.priority {
        w.line(&format!("(priority {priority})"));
    }
    w.line(&format!("(hatch edge {})", num(zone.hatch_pitch)));
    let mode = match zone.connect_pads {
        ZoneConnect::Thermal => None,
        ZoneConnect::Solid => Some("yes"),
        ZoneConnect::ThruHoleOnly => Some("thru_hole_only"),
        ZoneConnect::None => Some("no"),
    };
    match mode {
        Some(mode) => w.open_with("connect_pads", mode),
        None => w.open("connect_pads"),
    }
    w.line(&format!("(clearance {})", num(zone.connect_clearance)));
    w.close();
    w.line(&format!("(min_thickness {})", num(zone.min_thickness)));
    w.line("(filled_areas_thickness no)");
    w.open_with("fill", yes_no(zone.fill.enabled));
    w.line(&format!("(thermal_gap {})", num(zone.fill.thermal_gap)));
    w.line(&format!(
        "(thermal_bridge_width {})",
        num(zone.fill.thermal_bridge_width)
    ));
    w.close();
    w.open("polygon");
    w.open("pts");
    for point in &zone.polygon {
        w.line(&format!("(xy {})", xy(*point)));
    }
    w.close();
    w.close();
    w.close();
}

fn write_graphic(w: &mut Writer, graphic: &Graphic) {
    match graphic {
        Graphic::Line {
            start,
            end,
            stroke,
            layer,
            uuid,
        } => {
            w.open("gr_line");
            w.line(&format!("(start {})", xy(*start)));
            w.line(&format!("(end {})", xy(*end)));
            w.line(&format!(
                "(stroke (width {}) (type solid))",
                num(stroke.width)
            ));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.close();
        }
        Graphic::Rect {
            start,
            end,
            stroke,
            fill,
            layer,
            uuid,
        } => {
            w.open("gr_rect");
            w.line(&format!("(start {})", xy(*start)));
            w.line(&format!("(end {})", xy(*end)));
            w.line(&format!(
                "(stroke (width {}) (type solid))",
                num(stroke.width)
            ));
            w.line(&format!("(fill {})", yes_no(*fill)));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.close();
        }
        Graphic::Circle {
            center,
            end,
            stroke,
            fill,
            layer,
            uuid,
        } => {
            w.open("gr_circle");
            w.line(&format!("(center {})", xy(*center)));
            w.line(&format!("(end {})", xy(*end)));
            w.line(&format!(
                "(stroke (width {}) (type solid))",
                num(stroke.width)
            ));
            w.line(&format!("(fill {})", yes_no(*fill)));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.close();
        }
        Graphic::Arc {
            start,
            mid,
            end,
            stroke,
            layer,
            uuid,
        } => {
            w.open("gr_arc");
            w.line(&format!("(start {})", xy(*start)));
            w.line(&format!("(mid {})", xy(*mid)));
            w.line(&format!("(end {})", xy(*end)));
            w.line(&format!(
                "(stroke (width {}) (type solid))",
                num(stroke.width)
            ));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.close();
        }
        Graphic::Poly {
            pts,
            stroke,
            fill,
            layer,
            uuid,
        } => {
            w.open("gr_poly");
            w.open("pts");
            for point in pts {
                w.line(&format!("(xy {})", xy(*point)));
            }
            w.close();
            w.line(&format!(
                "(stroke (width {}) (type solid))",
                num(stroke.width)
            ));
            w.line(&format!("(fill {})", yes_no(*fill)));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.close();
        }
        Graphic::Text {
            text,
            at: position,
            layer,
            size,
            thickness,
            uuid,
        } => {
            w.open_with("gr_text", &quote(text));
            w.line(&format!("(at {})", at(*position)));
            w.line(&format!("(layer {})", quote(layer)));
            w.line(&format!("(uuid {})", quote(uuid)));
            w.line(&format!(
                "(effects (font (size {} {}) (thickness {})))",
                num(size.0),
                num(size.1),
                num(*thickness)
            ));
            w.close();
        }
    }
}

/// Format a number the way KiCad does: at most six decimals, trailing
/// zeros trimmed, negative zero normalized.
fn num(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    let formatted = format!("{value:.6}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".into()
    } else {
        trimmed.into()
    }
}

fn xy(point: Point) -> String {
    format!("{} {}", num(point.x), num(point.y))
}

fn at(at: At) -> String {
    if at.rot == 0.0 {
        format!("{} {}", num(at.x), num(at.y))
    } else {
        format!("{} {} {}", num(at.x), num(at.y), num(at.rot))
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Quote a string as a KiCad s-expression atom, escaping as pcbnew does.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Tab-indented node writer matching pcbnew's output shape.
#[derive(Default)]
struct Writer {
    out: String,
    depth: usize,
}

impl Writer {
    fn open(&mut self, name: &str) {
        self.indent();
        self.out.push('(');
        self.out.push_str(name);
        self.out.push('\n');
        self.depth += 1;
    }

    /// Open a node that carries inline arguments after its name.
    fn open_with(&mut self, name: &str, args: &str) {
        self.indent();
        self.out.push('(');
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(args);
        self.out.push('\n');
        self.depth += 1;
    }

    fn line(&mut self, content: &str) {
        self.indent();
        self.out.push_str(content);
        self.out.push('\n');
    }

    /// Splice an already-serialized block, re-indented to this depth.
    fn raw_block(&mut self, block: &str) {
        for line in block.lines() {
            if line.trim().is_empty() {
                continue;
            }
            self.indent();
            self.out.push_str(line);
            self.out.push('\n');
        }
    }

    fn close(&mut self) {
        self.depth -= 1;
        self.indent();
        self.out.push_str(")\n");
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push('\t');
        }
    }

    fn finish(self) -> String {
        debug_assert_eq!(self.depth, 0);
        self.out
    }
}
