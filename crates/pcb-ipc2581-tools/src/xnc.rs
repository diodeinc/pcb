//! XNC / Excellon 2 CAD-CAM drill/rout emitter.
//!
//! This file implements a compact CAD/CAM Exchange NC dialect for
//! Excellon-compatible drill/rout output. The core is the Ucamco XNC subset of
//! IPC-NC-349, with the common Excellon `G85` canned cycle for simple straight
//! slots. The target dialect is intentionally decimal and self-describing; it
//! does not use legacy implied decimal coordinates.
//!
//! Format summary:
//! - Files are printable 7-bit ASCII plus CR/LF. One command is written per
//!   line. Commands are uppercase and case-sensitive.
//! - A file is `header`, `body`, `M30`. No data follows `M30`.
//! - Header commands are `M48`, exactly one unit command (`METRIC` for mm or
//!   `INCH`), zero or more tool declarations, then `%`.
//! - Tool declarations are `TnnCdiameter`, where `nn` is `01..99` and diameter
//!   is a positive decimal in the file unit. Tool diameter is the finished hole
//!   or route width.
//! - Body state consists of current unit, current point, selected tool, and
//!   drill/rout mode. Tools are selected with `Tnn`.
//! - Drill mode is selected with `G05`. A drill hit is `XxYy` and creates one
//!   circular hole at that coordinate with the selected tool.
//! - A straight slot is `XxYyG85XxYy`, where the first coordinate is the slot
//!   start, the second coordinate is the slot end, and the selected tool
//!   diameter is the slot width. `G05` is emitted after the slot cycle to return
//!   to drill mode.
//! - Rout mode is entered with `G00XxYy`, which moves to the route start point.
//!   `M15` lowers the tool and starts a route path; `M16` raises it and ends the
//!   route path.
//! - Linear route segments are `G01XxYy`. Clockwise and counter-clockwise arc
//!   route segments are `G02XxYyAr` and `G03XxYyAr`; the `A` value is a positive
//!   radius and the represented arc is at most 180 degrees.
//! - Coordinates are signed decimal numbers in file units. They must share the
//!   same origin, axes, and orientation as the companion Gerber layers.
//! - Comments start with `;` and may appear anywhere. Spaces are only allowed in
//!   comments.
//! - X2-compatible attributes are standardized comments beginning with
//!   `; #@! `. File attributes use `TF.<name>`, tool attributes use `TA.<name>`,
//!   and object attributes use `TO.<name>`. Attributes do not affect geometry.
//! - Plating is a file-level attribute, so plated and non-plated holes are
//!   emitted as separate XNC files rather than mixed in one file.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use gerberx2::sanitize_attribute_field;
use pcb_ir::geom::Point;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XncUnit {
    Metric,
    Inch,
}

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
                .map(|field| sanitize_attribute_field(&field))
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
    Route {
        tool: u8,
        start: Point,
        segments: Vec<XncRouteSegment>,
        attributes: Vec<XncAttribute>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum XncRouteSegment {
    Line { to: Point },
    ClockwiseArc { to: Point, radius: f64 },
    CounterClockwiseArc { to: Point, radius: f64 },
}

#[derive(Debug, Clone)]
pub struct XncDocument {
    pub unit: XncUnit,
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
    unit: XncUnit,
    file_attributes: Vec<XncAttribute>,
    tool_by_key: BTreeMap<XncToolKey, u8>,
    tools: Vec<XncTool>,
    objects: Vec<XncObject>,
}

impl XncBuilder {
    pub fn new(unit: XncUnit, file_attributes: Vec<XncAttribute>) -> Self {
        Self {
            unit,
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

    pub fn add_route(
        &mut self,
        diameter: f64,
        start: Point,
        segments: Vec<XncRouteSegment>,
        tool_attributes: Vec<XncAttribute>,
        object_attributes: Vec<XncAttribute>,
    ) -> Result<()> {
        if segments.is_empty() {
            bail!("XNC route object has no segments");
        }
        let tool = self.tool(diameter, tool_attributes)?;
        self.objects.push(XncObject::Route {
            tool,
            start,
            segments,
            attributes: object_attributes,
        });
        Ok(())
    }

    pub fn finish(self) -> XncDocument {
        XncDocument {
            unit: self.unit,
            file_attributes: self.file_attributes,
            tools: self.tools,
            objects: self.objects,
        }
    }

    fn tool(&mut self, diameter: f64, attributes: Vec<XncAttribute>) -> Result<u8> {
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
    out.push_str(match doc.unit {
        XncUnit::Metric => "METRIC\n",
        XncUnit::Inch => "INCH\n",
    });
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
    out.push_str("%\n");

    let mut mode = XncMode::Unknown;
    let mut selected_tool = None;
    for object in &doc.objects {
        let tool = object.tool();
        if selected_tool != Some(tool) {
            out.push_str(&format!("T{tool:02}\n"));
            selected_tool = Some(tool);
        }
        for attribute in object.attributes() {
            attribute.write_line(&mut out);
        }
        match object {
            XncObject::Drill { at, .. } => {
                if mode != XncMode::Drill {
                    out.push_str("G05\n");
                    mode = XncMode::Drill;
                }
                out.push_str(&format!(
                    "X{}Y{}\n",
                    format_decimal(at.x),
                    format_decimal(at.y)
                ));
            }
            XncObject::Slot { start, end, .. } => {
                if mode != XncMode::Drill {
                    out.push_str("G05\n");
                }
                out.push_str(&format!(
                    "X{}Y{}G85X{}Y{}\n",
                    format_decimal(start.x),
                    format_decimal(start.y),
                    format_decimal(end.x),
                    format_decimal(end.y)
                ));
                out.push_str("G05\n");
                mode = XncMode::Drill;
            }
            XncObject::Route {
                start, segments, ..
            } => {
                out.push_str(&format!(
                    "G00X{}Y{}\n",
                    format_decimal(start.x),
                    format_decimal(start.y)
                ));
                mode = XncMode::Route;
                out.push_str("M15\n");
                for segment in segments {
                    match segment {
                        XncRouteSegment::Line { to } => out.push_str(&format!(
                            "G01X{}Y{}\n",
                            format_decimal(to.x),
                            format_decimal(to.y)
                        )),
                        XncRouteSegment::ClockwiseArc { to, radius } => out.push_str(&format!(
                            "G02X{}Y{}A{}\n",
                            format_decimal(to.x),
                            format_decimal(to.y),
                            format_decimal(*radius)
                        )),
                        XncRouteSegment::CounterClockwiseArc { to, radius } => {
                            out.push_str(&format!(
                                "G03X{}Y{}A{}\n",
                                format_decimal(to.x),
                                format_decimal(to.y),
                                format_decimal(*radius)
                            ))
                        }
                    }
                }
                out.push_str("M16\n");
            }
        }
    }
    out.push_str("M30\n");

    validate_ascii(&out)?;
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XncMode {
    Unknown,
    Drill,
    Route,
}

impl XncObject {
    fn tool(&self) -> u8 {
        match self {
            Self::Drill { tool, .. } | Self::Slot { tool, .. } | Self::Route { tool, .. } => *tool,
        }
    }

    fn attributes(&self) -> &[XncAttribute] {
        match self {
            Self::Drill { attributes, .. }
            | Self::Slot { attributes, .. }
            | Self::Route { attributes, .. } => attributes,
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
        validate_attributes(&tool.attributes)?;
    }
    validate_attributes(&doc.file_attributes)?;

    for object in &doc.objects {
        if !tools.contains(&object.tool()) {
            bail!("XNC object references undefined tool T{:02}", object.tool());
        }
        validate_attributes(object.attributes())?;
        match object {
            XncObject::Drill { at, .. } => validate_point(*at)?,
            XncObject::Slot { start, end, .. } => {
                validate_point(*start)?;
                validate_point(*end)?;
                validate_slot_endpoints(*start, *end)?;
            }
            XncObject::Route {
                start, segments, ..
            } => {
                validate_point(*start)?;
                if segments.is_empty() {
                    bail!("XNC route object has no segments");
                }
                let mut current = *start;
                for segment in segments {
                    match *segment {
                        XncRouteSegment::Line { to } => {
                            validate_point(to)?;
                            current = to;
                        }
                        XncRouteSegment::ClockwiseArc { to, radius }
                        | XncRouteSegment::CounterClockwiseArc { to, radius } => {
                            validate_point(to)?;
                            validate_positive("arc radius", radius)?;
                            if current.distance_to(to) > radius * 2.0 + 1e-9 {
                                bail!("XNC arc chord is larger than its diameter");
                            }
                            current = to;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_attributes(attributes: &[XncAttribute]) -> Result<()> {
    for attribute in attributes {
        validate_token("XNC attribute command", &attribute.command)?;
        for field in &attribute.fields {
            validate_token("XNC attribute field", field)?;
        }
    }
    Ok(())
}

fn validate_token(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} is empty");
    }
    if value.contains([',', ';', '\n', '\r']) {
        bail!("{label} contains an invalid separator");
    }
    validate_ascii(value)
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

fn validate_ascii(text: &str) -> Result<()> {
    if text
        .bytes()
        .all(|byte| byte == b'\n' || byte == b'\r' || (32..=126).contains(&byte))
    {
        Ok(())
    } else {
        bail!("XNC output contains non-ASCII characters")
    }
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

fn format_decimal(value: f64) -> String {
    if value.abs() < 0.0000000005 {
        return "0".to_string();
    }
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" {
        "0".to_string()
    } else if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_decimal_metric_drill_slot_and_route_xnc() {
        let mut builder = XncBuilder::new(
            XncUnit::Metric,
            vec![XncAttribute::file(
                "FileFunction",
                ["Plated", "1", "4", "PTH"],
            )],
        );
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
        builder
            .add_route(
                0.7,
                Point::new(4.0, 4.0),
                vec![XncRouteSegment::Line {
                    to: Point::new(5.0, 4.0),
                }],
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
        assert!(output.contains("T02\nX3.0Y4.0G85X3.0Y5.1\nG05\n"));
        assert!(output.contains("T03\nG00X4.0Y4.0\nM15\nG01X5.0Y4.0\nM16\n"));
        assert!(output.ends_with("M30\n"));
    }
}
