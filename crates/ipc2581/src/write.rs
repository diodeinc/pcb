//! Typed IPC-2581 element writers.
//!
//! Serialize individual typed elements (holes, fiducials, lines, polygons,
//! refs) as XML fragments for splicing into an existing document with
//! [`crate::edit`]. Coordinates are in millimeters and converted to the
//! document's units on write.

use uppsala::XmlWriter;

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

pub fn step_ref(writer: &mut XmlWriter, name: &str) {
    writer.empty_element("StepRef", &[("name", name)]);
}

pub fn layer_ref(writer: &mut XmlWriter, name: &str) {
    writer.empty_element("LayerRef", &[("name", name)]);
}

pub fn spec_ref(writer: &mut XmlWriter, id: &str) {
    writer.empty_element("SpecRef", &[("id", id)]);
}

/// Write an empty location-style element (`Location`, `Datum`, `PolyBegin`,
/// `PolyStepSegment`, ...) with x/y attributes.
pub fn location(writer: &mut XmlWriter, name: &str, x_mm: f64, y_mm: f64, units: Units) {
    writer.empty_element(
        name,
        &[
            ("x", fmt_units(x_mm, units).as_str()),
            ("y", fmt_units(y_mm, units).as_str()),
        ],
    );
}

pub fn circle(writer: &mut XmlWriter, units: Units, diameter_mm: f64) {
    writer.empty_element(
        "Circle",
        &[("diameter", fmt_units(diameter_mm, units).as_str())],
    );
}

/// Write a `Line` feature with an inline `LineDesc`. Lines that reference a
/// dictionary `LineDescRef` cannot be written as standalone fragments.
pub fn line(writer: &mut XmlWriter, units: Units, line: &Line) -> Result<()> {
    if line.line_desc_ref.is_some() {
        return Err(Ipc2581Error::InvalidStructure(
            "Line with a LineDescRef cannot be written standalone; inline LineDesc required".into(),
        ));
    }

    writer.start_element(
        "Line",
        &[
            ("startX", fmt_units(line.start_x, units).as_str()),
            ("startY", fmt_units(line.start_y, units).as_str()),
            ("endX", fmt_units(line.end_x, units).as_str()),
            ("endY", fmt_units(line.end_y, units).as_str()),
        ],
    );

    let line_width = fmt_units(line.line_width, units);
    let mut attrs = vec![("lineWidth", line_width.as_str())];
    if let Some(line_end) = line.line_end {
        attrs.push(("lineEnd", line_end_attr(line_end)));
    }
    if let Some(line_property) = line.line_property {
        attrs.push(("lineProperty", line_property_attr(line_property)));
    }
    writer.empty_element("LineDesc", &attrs);
    writer.end_element("Line");
    Ok(())
}

/// Write a fiducial or panel mark with location-only round geometry.
pub fn fiducial(writer: &mut XmlWriter, units: Units, fiducial: &Fiducial) -> Result<()> {
    if fiducial.xform.is_some() || fiducial.pin_ref.is_some() {
        return Err(Ipc2581Error::InvalidStructure(
            "fiducial with Xform or PinRef cannot be written standalone".into(),
        ));
    }
    let FiducialShape::Primitive(StandardPrimitive::Circle(styled)) = &fiducial.shape else {
        return Err(Ipc2581Error::InvalidStructure(
            "fiducial without inline Circle geometry cannot be written standalone".into(),
        ));
    };

    let elem_name = fiducial_element_name(fiducial.kind);
    writer.start_element(elem_name, &[]);
    location(
        writer,
        "Location",
        fiducial.location.x,
        fiducial.location.y,
        units,
    );
    circle(writer, units, styled.shape.diameter);
    writer.end_element(elem_name);
    Ok(())
}

/// Write a round `Hole` with the given name and zero tolerances.
pub fn hole(writer: &mut XmlWriter, units: Units, hole: &Hole, name: &str) {
    writer.empty_element(
        "Hole",
        &[
            ("name", name),
            ("type", "CIRCLE"),
            ("diameter", fmt_units(hole.diameter, units).as_str()),
            ("platingStatus", plating_status_attr(hole.plating_status)),
            ("plusTol", "0"),
            ("minusTol", "0"),
            ("x", fmt_units(hole.x, units).as_str()),
            ("y", fmt_units(hole.y, units).as_str()),
        ],
    );
}

pub fn profile(writer: &mut XmlWriter, units: Units, polygon: &Polygon) {
    writer.start_element("Profile", &[]);
    self::polygon(writer, units, polygon);
    writer.end_element("Profile");
}

pub fn polygon(writer: &mut XmlWriter, units: Units, polygon: &Polygon) {
    writer.start_element("Polygon", &[]);
    location(writer, "PolyBegin", polygon.begin.x, polygon.begin.y, units);
    for step in &polygon.steps {
        match step {
            PolyStep::Segment(segment) => {
                location(
                    writer,
                    "PolyStepSegment",
                    segment.point.x,
                    segment.point.y,
                    units,
                );
            }
            PolyStep::Curve(curve) => poly_step_curve(writer, units, curve),
        }
    }
    writer.end_element("Polygon");
}

pub fn poly_step_curve(writer: &mut XmlWriter, units: Units, curve: &PolyStepCurve) {
    writer.empty_element(
        "PolyStepCurve",
        &[
            ("x", fmt_units(curve.point.x, units).as_str()),
            ("y", fmt_units(curve.point.y, units).as_str()),
            ("centerX", fmt_units(curve.center.x, units).as_str()),
            ("centerY", fmt_units(curve.center.y, units).as_str()),
            ("clockwise", if curve.clockwise { "true" } else { "false" }),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hole_renders_units_and_plating() {
        let hole_mm = Hole {
            name: None,
            diameter: 2.0,
            plating_status: PlatingStatus::NonPlated,
            x: 1.5,
            y: -0.25,
        };
        let mut writer = XmlWriter::new();
        hole(&mut writer, Units::Millimeter, &hole_mm, "tooling_0");
        assert_eq!(
            writer.into_string(),
            r#"<Hole name="tooling_0" type="CIRCLE" diameter="2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="1.5" y="-0.25"/>"#
        );
    }

    #[test]
    fn line_requires_inline_desc() {
        let mut writer = XmlWriter::new();
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
