//! Typed IPC-2581 element writers.
//!
//! Serialize individual typed elements (holes, fiducials, lines, polygons,
//! refs) as XML fragments for splicing into an existing document with
//! [`crate::edit`]. Coordinates are in millimeters and converted to the
//! document's units on write.

use std::io::Write;

use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};

use crate::types::ecad::{Fiducial, FiducialKind, FiducialShape, Hole, Line, PlatingStatus};
use crate::types::primitives::{
    LineEnd, LineProperty, PolyStep, PolyStepCurve, Polygon, StandardPrimitive,
};
use crate::types::{Polarity, Side, Units};
use crate::{Ipc2581Error, Result};

/// Format a millimeter value in the document's units, with up to six
/// decimals and trailing zeros trimmed.
pub fn fmt_units(value_mm: f64, units: Units) -> String {
    fmt_num(crate::units::from_mm(value_mm, units))
}

/// Format a numeric value with up to six decimals, trimming trailing zeros.
pub fn fmt_num(value: f64) -> String {
    if value.abs() < 1e-9 {
        return "0".to_string();
    }
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

pub fn side_attr(side: Side) -> &'static str {
    match side {
        Side::Top => "TOP",
        Side::Bottom => "BOTTOM",
        Side::Both => "BOTH",
        Side::Internal => "INTERNAL",
        Side::All => "ALL",
        Side::None => "NONE",
    }
}

pub fn polarity_attr(polarity: Polarity) -> &'static str {
    match polarity {
        Polarity::Positive => "POSITIVE",
        Polarity::Negative => "NEGATIVE",
    }
}

pub fn line_end_attr(line_end: LineEnd) -> &'static str {
    match line_end {
        LineEnd::Round => "ROUND",
        LineEnd::Square => "SQUARE",
        LineEnd::Flat => "FLAT",
    }
}

pub fn line_property_attr(line_property: LineProperty) -> &'static str {
    match line_property {
        LineProperty::Solid => "SOLID",
        LineProperty::Dotted => "DOTTED",
        LineProperty::Dashed => "DASHED",
        LineProperty::Center => "CENTER",
        LineProperty::Phantom => "PHANTOM",
        LineProperty::Erase => "ERASE",
    }
}

pub fn plating_status_attr(plating_status: PlatingStatus) -> &'static str {
    match plating_status {
        PlatingStatus::Plated => "PLATED",
        PlatingStatus::NonPlated => "NONPLATED",
        PlatingStatus::Via => "VIA",
    }
}

pub fn fiducial_element_name(kind: FiducialKind) -> &'static str {
    match kind {
        FiducialKind::BadBoardMark => "BadBoardMark",
        FiducialKind::Global => "GlobalFiducial",
        FiducialKind::GoodPanelMark => "GoodPanelMark",
        FiducialKind::Local => "LocalFiducial",
    }
}

pub fn step_ref<W: Write>(writer: &mut Writer<W>, name: &str) -> Result<()> {
    let mut elem = BytesStart::new("StepRef");
    elem.push_attribute(("name", name));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub fn layer_ref<W: Write>(writer: &mut Writer<W>, name: &str) -> Result<()> {
    let mut elem = BytesStart::new("LayerRef");
    elem.push_attribute(("name", name));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub fn spec_ref<W: Write>(writer: &mut Writer<W>, id: &str) -> Result<()> {
    let mut elem = BytesStart::new("SpecRef");
    elem.push_attribute(("id", id));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

/// Write an empty location-style element (`Location`, `Datum`, `PolyBegin`,
/// `PolyStepSegment`, ...) with x/y attributes.
pub fn location<W: Write>(
    writer: &mut Writer<W>,
    name: &str,
    x_mm: f64,
    y_mm: f64,
    units: Units,
) -> Result<()> {
    let x = fmt_units(x_mm, units);
    let y = fmt_units(y_mm, units);
    let mut elem = BytesStart::new(name);
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub fn circle<W: Write>(writer: &mut Writer<W>, units: Units, diameter_mm: f64) -> Result<()> {
    let diameter = fmt_units(diameter_mm, units);
    let mut elem = BytesStart::new("Circle");
    elem.push_attribute(("diameter", diameter.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

/// Write a `Line` feature with an inline `LineDesc`. Lines that reference a
/// dictionary `LineDescRef` cannot be written as standalone fragments.
pub fn line<W: Write>(writer: &mut Writer<W>, units: Units, line: &Line) -> Result<()> {
    if line.line_desc_ref.is_some() {
        return Err(Ipc2581Error::InvalidStructure(
            "Line with a LineDescRef cannot be written standalone; inline LineDesc required".into(),
        ));
    }

    let start_x = fmt_units(line.start_x, units);
    let start_y = fmt_units(line.start_y, units);
    let end_x = fmt_units(line.end_x, units);
    let end_y = fmt_units(line.end_y, units);
    let mut elem = BytesStart::new("Line");
    elem.push_attribute(("startX", start_x.as_str()));
    elem.push_attribute(("startY", start_y.as_str()));
    elem.push_attribute(("endX", end_x.as_str()));
    elem.push_attribute(("endY", end_y.as_str()));
    writer.write_event(Event::Start(elem))?;

    let line_width = fmt_units(line.line_width, units);
    let mut line_desc = BytesStart::new("LineDesc");
    line_desc.push_attribute(("lineWidth", line_width.as_str()));
    if let Some(line_end) = line.line_end {
        line_desc.push_attribute(("lineEnd", line_end_attr(line_end)));
    }
    if let Some(line_property) = line.line_property {
        line_desc.push_attribute(("lineProperty", line_property_attr(line_property)));
    }
    writer.write_event(Event::Empty(line_desc))?;
    writer.write_event(Event::End(BytesStart::new("Line").to_end()))?;
    Ok(())
}

/// Write a fiducial or panel mark with location-only round geometry.
pub fn fiducial<W: Write>(writer: &mut Writer<W>, units: Units, fiducial: &Fiducial) -> Result<()> {
    if fiducial.xform.is_some() || fiducial.pin_ref.is_some() {
        return Err(Ipc2581Error::InvalidStructure(
            "fiducial with Xform or PinRef cannot be written standalone".into(),
        ));
    }

    let elem_name = fiducial_element_name(fiducial.kind);
    writer.write_event(Event::Start(BytesStart::new(elem_name)))?;
    location(
        writer,
        "Location",
        fiducial.location.x,
        fiducial.location.y,
        units,
    )?;
    match &fiducial.shape {
        FiducialShape::Primitive(StandardPrimitive::Circle(styled)) => {
            circle(writer, units, styled.shape.diameter)?;
        }
        _ => {
            return Err(Ipc2581Error::InvalidStructure(
                "fiducial without inline Circle geometry cannot be written standalone".into(),
            ));
        }
    }
    writer.write_event(Event::End(BytesStart::new(elem_name).to_end()))?;
    Ok(())
}

/// Write a round `Hole` with the given name and zero tolerances.
pub fn hole<W: Write>(writer: &mut Writer<W>, units: Units, hole: &Hole, name: &str) -> Result<()> {
    let diameter = fmt_units(hole.diameter, units);
    let x = fmt_units(hole.x, units);
    let y = fmt_units(hole.y, units);

    let mut elem = BytesStart::new("Hole");
    elem.push_attribute(("name", name));
    elem.push_attribute(("type", "CIRCLE"));
    elem.push_attribute(("diameter", diameter.as_str()));
    elem.push_attribute(("platingStatus", plating_status_attr(hole.plating_status)));
    elem.push_attribute(("plusTol", "0"));
    elem.push_attribute(("minusTol", "0"));
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub fn profile<W: Write>(writer: &mut Writer<W>, units: Units, polygon: &Polygon) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("Profile")))?;
    self::polygon(writer, units, polygon)?;
    writer.write_event(Event::End(BytesStart::new("Profile").to_end()))?;
    Ok(())
}

pub fn polygon<W: Write>(writer: &mut Writer<W>, units: Units, polygon: &Polygon) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("Polygon")))?;
    location(writer, "PolyBegin", polygon.begin.x, polygon.begin.y, units)?;
    for step in &polygon.steps {
        match step {
            PolyStep::Segment(segment) => {
                location(
                    writer,
                    "PolyStepSegment",
                    segment.point.x,
                    segment.point.y,
                    units,
                )?;
            }
            PolyStep::Curve(curve) => poly_step_curve(writer, units, curve)?,
        }
    }
    writer.write_event(Event::End(BytesStart::new("Polygon").to_end()))?;
    Ok(())
}

pub fn poly_step_curve<W: Write>(
    writer: &mut Writer<W>,
    units: Units,
    curve: &PolyStepCurve,
) -> Result<()> {
    let x = fmt_units(curve.point.x, units);
    let y = fmt_units(curve.point.y, units);
    let center_x = fmt_units(curve.center.x, units);
    let center_y = fmt_units(curve.center.y, units);
    let mut elem = BytesStart::new("PolyStepCurve");
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    elem.push_attribute(("centerX", center_x.as_str()));
    elem.push_attribute(("centerY", center_y.as_str()));
    elem.push_attribute(("clockwise", if curve.clockwise { "true" } else { "false" }));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn render(f: impl FnOnce(&mut Writer<Cursor<Vec<u8>>>) -> Result<()>) -> String {
        let mut writer = Writer::new(Cursor::new(Vec::new()));
        f(&mut writer).unwrap();
        String::from_utf8(writer.into_inner().into_inner()).unwrap()
    }

    #[test]
    fn hole_renders_units_and_plating() {
        let hole_mm = Hole {
            name: None,
            diameter: 2.0,
            plating_status: PlatingStatus::NonPlated,
            x: 1.5,
            y: -0.25,
        };
        let xml = render(|w| hole(w, Units::Millimeter, &hole_mm, "tooling_0"));
        assert_eq!(
            xml,
            r#"<Hole name="tooling_0" type="CIRCLE" diameter="2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="1.5" y="-0.25"/>"#
        );
    }

    #[test]
    fn line_requires_inline_desc() {
        let mut writer = Writer::new(Cursor::new(Vec::new()));
        let mut interner = pcb_intern::Interner::default();
        let bad = Line {
            start_x: 0.0,
            start_y: 0.0,
            end_x: 1.0,
            end_y: 0.0,
            line_desc_ref: Some(interner.intern("ref")),
            line_width: 0.1,
            line_end: None,
            line_property: None,
        };
        assert!(line(&mut writer, Units::Millimeter, &bad).is_err());
    }

    #[test]
    fn fmt_units_converts_and_trims() {
        assert_eq!(fmt_units(25.4, Units::Inch), "1");
        assert_eq!(fmt_units(1.0, Units::Millimeter), "1");
        assert_eq!(fmt_num(-0.0000000001), "0");
    }
}
