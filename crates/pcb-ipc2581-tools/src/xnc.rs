//! XNC / Excellon 2 CAD-CAM drill emitter.
//!
//! This file implements a compact CAD/CAM Exchange NC dialect for
//! Excellon-compatible drill output: the drill subset of Ucamco XNC
//! (IPC-NC-349) plus Excellon `G90` absolute mode and the `G85` canned cycle.
//! Slots are always `G85`. XNC routs slots instead, but `G85` is
//! what fabricators ask for and what ships today, so it is the one slot
//! encoding here and rout mode is not written at all. The target dialect is
//! intentionally decimal and self-describing; it does not use legacy implied
//! decimal coordinates.
//!
//! Format summary:
//! - Files are printable 7-bit ASCII plus CR/LF. One command is written per
//!   line. Commands are uppercase and case-sensitive.
//! - A file is `header`, `body`, `M30`. No data follows `M30`.
//! - Header commands are `M48`, the unit command `METRIC` (coordinates and
//!   diameters are millimeters), zero or more tool declarations, then `%`.
//! - Tool declarations are `TnnCdiameter`, where `nn` is `01..99` and diameter
//!   is a positive decimal in the file unit. Tool diameter is the finished hole
//!   or slot width.
//! - Body state consists of current point, selected tool, and drill mode.
//!   `G90` explicitly selects absolute coordinates, matching KiCad Excellon.
//!   Tools are selected with `Tnn`.
//! - Drill mode is selected with `G05`. A drill hit is `XxYy` and creates one
//!   circular hole at that coordinate with the selected tool.
//! - A straight slot is `XxYyG85XxYy`, where the first coordinate is the slot
//!   start, the second coordinate is the slot end, and the selected tool
//!   diameter is the slot width. `G05` is emitted after the slot cycle to return
//!   to drill mode.
//! - Coordinates are signed decimal numbers in file units. They must share the
//!   same origin, axes, and orientation as the companion Gerber layers.
//! - Comments start with `;` and may appear anywhere. Spaces are only allowed in
//!   comments.
//! - X2-compatible attributes are standardized comments beginning with
//!   `; #@! `. File attributes use `TF.<name>`, tool attributes use `TA.<name>`,
//!   and object attributes use `TO.<name>`. Attributes do not affect geometry.
//! - Plating is a file-level attribute, so plated and non-plated holes are
//!   emitted as separate XNC files rather than mixed in one file.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use gerberx2::{escape_attribute_field, trim_decimal};
use pcb_ir::dialects::nc;
use pcb_ir::geom::Point;

/// One X2 attribute comment. Construction escapes every field, so whatever
/// the source names contain, the file stays printable ASCII.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct XncAttribute {
    command: String,
    fields: Vec<String>,
}

impl XncAttribute {
    pub fn file(name: &str, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new("TF", name, fields)
    }

    pub fn tool(name: &str, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new("TA", name, fields)
    }

    pub fn object(name: &str, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new("TO", name, fields)
    }

    fn new(scope: &str, name: &str, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            command: format!("{scope}.{}", sanitize_attribute_name(name)),
            fields: fields
                .into_iter()
                .map(Into::into)
                .map(|field| escape_attribute_field(&field))
                .collect(),
        }
    }

    fn write_line(&self, out: &mut String) {
        out.push_str("; #@! ");
        out.push_str(&self.command);
        for field in &self.fields {
            out.push(',');
            out.push_str(field);
        }
        out.push('\n');
    }
}

#[derive(Debug, Clone)]
pub struct XncTool {
    pub number: u8,
    pub diameter: f64,
    pub attributes: Vec<XncAttribute>,
}

#[derive(Debug, Clone)]
pub enum XncObject {
    Drill {
        tool: u8,
        at: Point,
        attributes: Vec<XncAttribute>,
    },
    Slot {
        tool: u8,
        start: Point,
        end: Point,
        attributes: Vec<XncAttribute>,
    },
}

#[derive(Debug, Clone)]
pub struct XncDocument {
    pub file_attributes: Vec<XncAttribute>,
    pub tools: Vec<XncTool>,
    pub objects: Vec<XncObject>,
}

impl XncDocument {
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[derive(Debug)]
pub struct XncBuilder {
    file_attributes: Vec<XncAttribute>,
    tool_by_key: BTreeMap<XncToolKey, u8>,
    tools: Vec<XncTool>,
    objects: Vec<XncObject>,
}

impl XncBuilder {
    pub fn new(file_attributes: Vec<XncAttribute>) -> Self {
        Self {
            file_attributes,
            tool_by_key: BTreeMap::new(),
            tools: Vec::new(),
            objects: Vec::new(),
        }
    }

    pub fn add_drill(
        &mut self,
        diameter: f64,
        at: Point,
        tool_attributes: Vec<XncAttribute>,
        object_attributes: Vec<XncAttribute>,
    ) -> Result<()> {
        let tool = self.tool(diameter, tool_attributes)?;
        self.objects.push(XncObject::Drill {
            tool,
            at,
            attributes: object_attributes,
        });
        Ok(())
    }

    pub fn add_slot(
        &mut self,
        diameter: f64,
        start: Point,
        end: Point,
        tool_attributes: Vec<XncAttribute>,
        object_attributes: Vec<XncAttribute>,
    ) -> Result<()> {
        validate_slot_endpoints(start, end)?;
        let tool = self.tool(diameter, tool_attributes)?;
        self.objects.push(XncObject::Slot {
            tool,
            start,
            end,
            attributes: object_attributes,
        });
        Ok(())
    }

    pub fn finish(self) -> XncDocument {
        let mut tools = self.tools;
        let mut objects = self.objects;
        tools.sort_by(|a, b| a.diameter.total_cmp(&b.diameter));
        let renumbered: HashMap<u8, u8> = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.number, index as u8 + 1))
            .collect();
        for (index, tool) in tools.iter_mut().enumerate() {
            tool.number = index as u8 + 1;
        }
        for object in &mut objects {
            let (XncObject::Drill { tool, .. } | XncObject::Slot { tool, .. }) = object;
            *tool = renumbered[tool];
        }
        // One pass per tool, keeping source order within it.
        objects.sort_by_key(XncObject::tool);
        XncDocument {
            file_attributes: self.file_attributes,
            tools,
            objects,
        }
    }

    fn tool(&mut self, diameter: f64, attributes: Vec<XncAttribute>) -> Result<u8> {
        // Snap to 1 µm: real tools are never finer, and EDA exports carry
        // nanometer float dust (a 1.0 mm slot arriving as 0.999998 mm).
        let diameter = (diameter * 1_000.0).round() / 1_000.0;
        validate_positive("tool diameter", diameter)?;
        let key = XncToolKey {
            diameter_nm: quantize_mm(diameter),
            attributes: attributes.clone(),
        };
        if let Some(number) = self.tool_by_key.get(&key) {
            return Ok(*number);
        }
        if self.tools.len() >= 99 {
            bail!("XNC supports at most 99 tools");
        }
        let number = self.tools.len() as u8 + 1;
        self.tool_by_key.insert(key, number);
        self.tools.push(XncTool {
            number,
            diameter,
            attributes,
        });
        Ok(number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct XncToolKey {
    diameter_nm: i64,
    attributes: Vec<XncAttribute>,
}

pub fn write_xnc(doc: &XncDocument) -> Result<String> {
    validate_document(doc)?;

    let mut out = String::new();
    out.push_str("M48\n");
    for attribute in &doc.file_attributes {
        attribute.write_line(&mut out);
    }
    out.push_str("METRIC\n");
    for tool in &doc.tools {
        for attribute in &tool.attributes {
            attribute.write_line(&mut out);
        }
        out.push_str(&format!(
            "T{:02}C{}\n",
            tool.number,
            format_decimal(tool.diameter)
        ));
    }
    out.push_str("%\nG90\n");

    let mut drilling = false;
    let mut selected_tool = None;
    // Object attributes persist until deleted, so only the difference from
    // the previous object is written and a dropped attribute resets them.
    let mut current_attributes: &[XncAttribute] = &[];
    for object in &doc.objects {
        let tool = object.tool();
        if selected_tool != Some(tool) {
            out.push_str(&format!("T{tool:02}\n"));
            selected_tool = Some(tool);
        }
        let attributes = object.attributes();
        let dropped = current_attributes.iter().any(|current| {
            !attributes
                .iter()
                .any(|attribute| attribute.command == current.command)
        });
        if dropped {
            out.push_str("; #@! TD\n");
            current_attributes = &[];
        }
        for attribute in attributes {
            if !current_attributes.contains(attribute) {
                attribute.write_line(&mut out);
            }
        }
        current_attributes = attributes;
        if !drilling {
            out.push_str("G05\n");
            drilling = true;
        }
        match object {
            XncObject::Drill { at, .. } => out.push_str(&format!(
                "X{}Y{}\n",
                format_decimal(at.x),
                format_decimal(at.y)
            )),
            // The canned cycle leaves drill mode.
            XncObject::Slot { start, end, .. } => out.push_str(&format!(
                "X{}Y{}G85X{}Y{}\nG05\n",
                format_decimal(start.x),
                format_decimal(start.y),
                format_decimal(end.x),
                format_decimal(end.y)
            )),
        }
    }
    out.push_str("M30\n");
    Ok(out)
}

/// A drill file as the cuts it makes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrillImage {
    /// The `.FileFunction` attribute's fields, empty when the file has none.
    pub file_function: Vec<String>,
    pub holes: Vec<nc::Geometry>,
}

/// Whether `text` reads as a drill file: its first command opens the
/// Excellon header.
pub fn is_drill_file(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with(';'))
        == Some("M48")
}

/// How a line without a motion code moves the tool: as the last one did.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Motion {
    /// Drill a hole where it lands.
    Drill,
    /// Move the raised tool.
    Rapid,
    /// Rout a straight line, cutting while the tool is plunged.
    Line,
    /// Rout an arc, cutting while the tool is plunged.
    Arc { clockwise: bool },
}

/// How far the straight slots a routed arc reads as may stray from it.
const ARC_TOLERANCE_MM: f64 = 0.001;

/// Read a drill file with decimal coordinates: XNC, and the Excellon EDA
/// tools export in decimal format, whose `M71`/`M72` units, `G85` slots,
/// `I`/`J` arc centres and routes continued with bare coordinates XNC
/// leaves out. A routed arc reads as straight slots along it. Excellon's
/// implied-decimal formats (`LZ`/`TZ`) are rejected.
pub fn read_drill(text: &str) -> Result<DrillImage> {
    let mut image = DrillImage::default();
    let mut tools = HashMap::new();
    let mut header = false;
    let mut scale = 1.0;
    let mut tool = None;
    let mut at = Point::new(0.0, 0.0);
    let mut motion = Motion::Drill;
    let mut plunged = false;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        let context = || format!("drill file line {}: {line}", index + 1);
        if let Some(comment) = line.strip_prefix(';') {
            if let Some(fields) = comment.trim().strip_prefix("#@! TF.FileFunction,") {
                image.file_function = fields.split(',').map(str::to_owned).collect();
            }
            continue;
        }
        match line {
            "M48" => header = true,
            "%" | "M95" => header = false,
            "M15" => plunged = true,
            "M16" | "M17" => plunged = false,
            "M30" | "M00" => break,
            _ if header && !line.starts_with('T') => {
                let mut fields = line.split(',');
                match fields.next() {
                    Some("METRIC" | "M71") => scale = 1.0,
                    Some("INCH" | "M72") => scale = 25.4,
                    _ => continue,
                }
                if fields.any(|field| matches!(field, "LZ" | "TZ")) {
                    bail!(
                        "{}: implied-decimal coordinates are not supported",
                        context()
                    );
                }
            }
            _ => {
                let words = words(line).with_context(context)?;
                let value = |letter: char| {
                    let word = words.iter().find(|word| word.0 == letter)?;
                    Some(word.1 * scale)
                };
                if let Some(number) = words.iter().find(|word| word.0 == 'T') {
                    let number = number.1 as u32;
                    // `C` defines the tool's diameter; outside the header
                    // the word also selects it, `T0` none.
                    if let Some(diameter) = value('C') {
                        tools.insert(number, diameter);
                    }
                    if !header {
                        tool = (number != 0).then_some(number);
                    }
                    continue;
                }
                let diameter = || -> Result<f64> {
                    let number =
                        tool.with_context(|| format!("{}: no tool selected", context()))?;
                    tools
                        .get(&number)
                        .copied()
                        .with_context(|| format!("{}: tool T{number:02} is not defined", context()))
                };
                let point = |words: &[(char, f64)], from: Point| {
                    words
                        .iter()
                        .fold(from, |point, &(letter, value)| match letter {
                            'X' => Point::new(value * scale, point.y),
                            'Y' => Point::new(point.x, value * scale),
                            _ => point,
                        })
                };
                // A `G85` cycle cuts a slot between the points either side.
                if let Some(split) = words.iter().position(|&word| word == ('G', 85.0)) {
                    let start = point(&words[..split], at);
                    at = point(&words[split + 1..], start);
                    image.holes.push(nc::Geometry::Slot {
                        start,
                        end: at,
                        diameter: diameter()?,
                    });
                    continue;
                }
                for &(letter, code) in &words {
                    if letter == 'G' {
                        motion = match code as u32 {
                            0 => Motion::Rapid,
                            1 => Motion::Line,
                            2 => Motion::Arc { clockwise: true },
                            3 => Motion::Arc { clockwise: false },
                            5 => Motion::Drill,
                            90 => motion,
                            _ => bail!("{}: G{code} is not supported", context()),
                        };
                    }
                }
                if !words.iter().any(|word| matches!(word.0, 'X' | 'Y')) {
                    continue;
                }
                let end = point(&words, at);
                let path = match motion {
                    Motion::Drill => {
                        image.holes.push(nc::Geometry::Drill {
                            at: end,
                            diameter: diameter()?,
                        });
                        Vec::new()
                    }
                    Motion::Line if plunged => vec![end],
                    // XNC gives an arc's radius, Excellon its centre
                    // relative to the start.
                    Motion::Arc { clockwise } if plunged => {
                        let center = match value('A') {
                            Some(radius) => arc_center(at, end, radius, clockwise),
                            None => {
                                let offset = Point::new(
                                    value('I').unwrap_or_default(),
                                    value('J').unwrap_or_default(),
                                );
                                at + offset
                            }
                        };
                        let arc = pcb_ir::geom::Arc::new(at, end, center, clockwise);
                        arc.chords(ARC_TOLERANCE_MM).0
                    }
                    _ => Vec::new(),
                };
                for to in path {
                    if to.distance_to(at) > 1e-9 {
                        image.holes.push(nc::Geometry::Slot {
                            start: at,
                            end: to,
                            diameter: diameter()?,
                        });
                    }
                    at = to;
                }
                at = end;
            }
        }
    }
    Ok(image)
}

/// A line's words: each letter and the number after it, as written.
fn words(line: &str) -> Result<Vec<(char, f64)>> {
    let mut words = Vec::new();
    let mut rest = line;
    while let Some(letter) = rest.chars().next() {
        if !letter.is_ascii_uppercase() {
            bail!("unexpected '{rest}'");
        }
        let value = &rest[1..];
        let end = value
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
            .unwrap_or(value.len());
        words.push((letter, value[..end].parse()?));
        rest = &value[end..];
    }
    Ok(words)
}

/// The centre of an arc of `radius` from `start` to `end`, turning at most
/// half a circle: to the left of the chord turning counter-clockwise, to
/// the right turning clockwise.
fn arc_center(start: Point, end: Point, radius: f64, clockwise: bool) -> Point {
    let chord = end - start;
    let length = chord.x.hypot(chord.y);
    let rise = (radius * radius - length * length / 4.0).max(0.0).sqrt();
    let left = Point::new(-chord.y / length, chord.x / length);
    let side = if clockwise { -rise } else { rise };
    Point::new(
        (start.x + end.x) / 2.0 + left.x * side,
        (start.y + end.y) / 2.0 + left.y * side,
    )
}

impl XncObject {
    fn tool(&self) -> u8 {
        match self {
            Self::Drill { tool, .. } | Self::Slot { tool, .. } => *tool,
        }
    }

    fn attributes(&self) -> &[XncAttribute] {
        match self {
            Self::Drill { attributes, .. } | Self::Slot { attributes, .. } => attributes,
        }
    }
}

fn validate_document(doc: &XncDocument) -> Result<()> {
    let mut tools = BTreeSet::new();
    for tool in &doc.tools {
        if !(1..=99).contains(&tool.number) {
            bail!("XNC tool number must be in 1..=99");
        }
        if !tools.insert(tool.number) {
            bail!("XNC tool T{:02} is declared more than once", tool.number);
        }
        validate_positive("tool diameter", tool.diameter)?;
    }

    for object in &doc.objects {
        if !tools.contains(&object.tool()) {
            bail!("XNC object references undefined tool T{:02}", object.tool());
        }
        match object {
            XncObject::Drill { at, .. } => validate_point(*at)?,
            XncObject::Slot { start, end, .. } => validate_slot_endpoints(*start, *end)?,
        }
    }
    Ok(())
}

fn validate_point(point: Point) -> Result<()> {
    if !point.is_finite() {
        bail!("XNC coordinate is not finite");
    }
    Ok(())
}

fn validate_slot_endpoints(start: Point, end: Point) -> Result<()> {
    validate_point(start)?;
    validate_point(end)?;
    if start.distance_to(end) <= 1e-9 {
        bail!("XNC slot start and end must be distinct");
    }
    Ok(())
}

fn validate_positive(label: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("XNC {label} must be positive and finite");
    }
    Ok(())
}

fn sanitize_attribute_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

/// A decimal that reads as one even when integral; zero stays `0`.
fn format_decimal(value: f64) -> String {
    let text = trim_decimal(value, 6);
    if text == "0" || text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_drills_and_slots_read_back_as_the_cuts_they_make() {
        let mut builder = XncBuilder::new(vec![XncAttribute::file(
            "FileFunction",
            ["Plated", "1", "4", "PTH"],
        )]);
        builder
            .add_slot(
                0.6,
                Point::new(1.0, 2.0),
                Point::new(3.5, 2.0),
                vec![],
                vec![],
            )
            .unwrap();
        builder
            .add_drill(0.3, Point::new(-4.25, 7.0), vec![], vec![])
            .unwrap();
        let text = format!(
            "; written by a test\n{}",
            write_xnc(&builder.finish()).unwrap()
        );
        assert!(is_drill_file(&text));
        let image = read_drill(&text).unwrap();
        assert_eq!(image.file_function, ["Plated", "1", "4", "PTH"]);
        assert_eq!(
            image.holes,
            [
                nc::Geometry::Drill {
                    at: Point::new(-4.25, 7.0),
                    diameter: 0.3
                },
                nc::Geometry::Slot {
                    start: Point::new(1.0, 2.0),
                    end: Point::new(3.5, 2.0),
                    diameter: 0.6
                },
            ]
        );
    }

    #[test]
    fn a_route_cuts_every_move_while_plunged_and_none_after() {
        // Inch units set by M72; the second segment continues the modal G01 with bare
        // coordinates, and the moves after M16 cut nothing.
        let text = "M48\nM72\nT1C0.04\n%\nT1\nG00X1.0Y1.0\nM15\nG01X2.0Y1.0\nX2.0Y2.0\nM16\nX3.0Y3.0\nG00X4.0Y4.0\nM30\n";
        let slot = |start: Point, end: Point| nc::Geometry::Slot {
            start,
            end,
            diameter: 0.04 * 25.4,
        };
        let inch = |x: f64, y: f64| Point::new(x * 25.4, y * 25.4);
        assert_eq!(
            read_drill(text).unwrap().holes,
            [
                slot(inch(1.0, 1.0), inch(2.0, 1.0)),
                slot(inch(2.0, 1.0), inch(2.0, 2.0)),
            ]
        );
    }

    #[test]
    fn a_routed_arc_cuts_along_the_circle_its_radius_or_centre_gives() {
        // The rounded corner of the XNC specification's example rout: a
        // quarter circle counter-clockwise about (6, 2.6). Taken on the other
        // side of its chord, the circle would be centred on (5, 1.6).
        for arc in ["G03X6.0Y1.6A1.0", "G03X6.0Y1.6I1.0J0.0"] {
            let text = format!("M48\nMETRIC\nT01C0.8\n%\nT01\nG00X5.0Y2.6\nM15\n{arc}\nM16\nM30\n");
            let holes = read_drill(&text).unwrap().holes;
            let mut from = Point::new(5.0, 2.6);
            for hole in &holes {
                let nc::Geometry::Slot { start, end, .. } = *hole else {
                    panic!("{arc} cut {hole:?}")
                };
                assert_eq!(start, from);
                let radius = end.distance_to(Point::new(6.0, 2.6));
                assert!((radius - 1.0).abs() < 1e-9, "{arc} strays to {end:?}");
                from = end;
            }
            assert!(holes.len() > 1);
            assert_eq!(from, Point::new(6.0, 1.6));
        }
    }

    #[test]
    fn tools_snap_to_micrometers_and_sort_by_diameter() {
        let mut builder = XncBuilder::new(vec![]);
        // Float dust from EDA unit conversion snaps to the intended tool,
        // merging with an exact duplicate, and the table sorts by diameter.
        builder
            .add_drill(0.999998, Point::new(1.0, 1.0), vec![], vec![])
            .unwrap();
        builder
            .add_drill(0.3, Point::new(2.0, 2.0), vec![], vec![])
            .unwrap();
        builder
            .add_drill(1.0, Point::new(3.0, 3.0), vec![], vec![])
            .unwrap();

        let document = builder.finish();
        assert_eq!(
            document
                .tools
                .iter()
                .map(|tool| (tool.number, tool.diameter))
                .collect::<Vec<_>>(),
            vec![(1, 0.3), (2, 1.0)]
        );
        let output = write_xnc(&document).unwrap();
        assert!(output.contains("T01C0.3\n"));
        assert!(output.contains("T02C1.0\n"));
    }

    #[test]
    fn formats_coordinates_as_trimmed_decimals() {
        for (value, text) in [
            (5.0, "5.0"),
            (-2.5, "-2.5"),
            (1.234_567_8, "1.234568"),
            (0.0, "0"),
            (-0.000_000_1, "0"),
        ] {
            assert_eq!(format_decimal(value), text);
        }
    }

    #[test]
    fn hits_group_by_tool_in_source_order() {
        let mut builder = XncBuilder::new(vec![]);
        for (diameter, x) in [(1.0, 1.0), (0.3, 2.0), (1.0, 3.0), (0.3, 4.0)] {
            builder
                .add_drill(diameter, Point::new(x, 5.0), vec![], vec![])
                .unwrap();
        }

        let output = write_xnc(&builder.finish()).unwrap();
        assert!(
            output.contains("%\nG90\nT01\nG05\nX2.0Y5.0\nX4.0Y5.0\nT02\nX1.0Y5.0\nX3.0Y5.0\nM30\n")
        );
    }

    #[test]
    fn object_attributes_never_leak_into_later_holes() {
        let mut builder = XncBuilder::new(vec![]);
        let net = |name: &str| XncAttribute::object("N", [name]);
        let pin = vec![
            net("VCC"),
            XncAttribute::object("C", ["J1"]),
            XncAttribute::object("P", ["J1", "1"]),
        ];
        for (x, attributes) in [
            (1.0, pin.clone()),
            (2.0, pin),
            (3.0, vec![net("GND")]),
            (4.0, vec![net("V3V3")]),
        ] {
            builder
                .add_drill(0.3, Point::new(x, 5.0), vec![], attributes)
                .unwrap();
        }

        let output = write_xnc(&builder.finish()).unwrap();
        // The repeated pin rides existing state, the via drops the pin's
        // component attributes, and a changed net overrides in place.
        assert!(
            output.contains(
                "; #@! TO.N,VCC\n; #@! TO.C,J1\n; #@! TO.P,J1,1\nG05\nX1.0Y5.0\nX2.0Y5.0\n\
                 ; #@! TD\n; #@! TO.N,GND\nX3.0Y5.0\n; #@! TO.N,V3V3\nX4.0Y5.0\n"
            ),
            "{output}"
        );
    }

    #[test]
    fn free_form_names_stay_printable_ascii() {
        let mut builder = XncBuilder::new(vec![]);
        builder
            .add_drill(
                0.3,
                Point::new(1.0, 1.0),
                vec![],
                vec![
                    XncAttribute::object("N", ["\u{b5}C_RST;1"]),
                    XncAttribute::object("P", ["R\\1", "\u{3a9}"]),
                ],
            )
            .unwrap();

        let output = write_xnc(&builder.finish()).unwrap();
        assert!(output.is_ascii());
        assert!(output.contains("; #@! TO.N,\\u00B5C_RST\\u003B1\n"));
        assert!(output.contains("; #@! TO.P,R\\u005C1,\\u03A9\n"));
    }

    #[test]
    fn emits_decimal_metric_drill_and_slot_xnc() {
        let mut builder = XncBuilder::new(vec![XncAttribute::file(
            "FileFunction",
            ["Plated", "1", "4", "PTH"],
        )]);
        builder
            .add_drill(
                0.3,
                Point::new(1.0, -2.5),
                vec![XncAttribute::tool(
                    "AperFunction",
                    ["Plated", "PTH", "ViaDrill"],
                )],
                vec![XncAttribute::object("N", ["GND"])],
            )
            .unwrap();
        builder
            .add_slot(
                0.6,
                Point::new(3.0, 4.0),
                Point::new(3.0, 5.1),
                vec![XncAttribute::tool(
                    "AperFunction",
                    ["Plated", "PTH", "ComponentDrill"],
                )],
                vec![],
            )
            .unwrap();
        let output = write_xnc(&builder.finish()).unwrap();

        assert!(output.contains("; #@! TF.FileFunction,Plated,1,4,PTH\n"));
        assert!(output.contains("; #@! TA.AperFunction,Plated,PTH,ViaDrill\nT01C0.3\n"));
        assert!(output.contains("T01\n; #@! TO.N,GND\nG05\nX1.0Y-2.5\n"));
        assert!(output.contains("T02\n; #@! TD\nX3.0Y4.0G85X3.0Y5.1\nG05\n"));
        assert!(output.ends_with("M30\n"));
    }
}
