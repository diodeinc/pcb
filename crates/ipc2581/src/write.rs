//! Typed IPC-2581 element writers.
//!
//! Serialize individual typed elements (holes, fiducials, lines, polygons,
//! refs) as XML fragments for splicing into an existing document with
//! [`crate::edit`]. Coordinates are in millimeters and converted to the
//! document's units on write.

use uppsala::XmlWriter;

use crate::types::Units;
use crate::types::ecad::{Fiducial, FiducialShape, Hole, Stroke, StrokePath};
use crate::types::primitives::{
    Contour, LineDescGroup, PolyStep, PolyStepCurve, Polygon, StandardPrimitive,
};
use crate::{Ipc2581Error, Result};

/// Format a millimeter value in the document's units on a grid of a
/// nanometre or finer, with trailing zeros trimmed. Six decimals of an inch
/// would be 25 nm.
pub fn fmt_units(value_mm: f64, units: Units) -> String {
    let decimals = if units == Units::Inch { 8 } else { 6 };
    fmt_decimals(crate::units::from_mm(value_mm, units), decimals)
}

/// Format a numeric value with up to six decimals, trimming trailing zeros.
pub fn fmt_num(value: f64) -> String {
    fmt_decimals(value, 6)
}

fn fmt_decimals(value: f64, decimals: usize) -> String {
    if value.abs() < 1e-9 {
        return "0".to_string();
    }
    let mut text = format!("{value:.decimals$}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
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

/// Write a stroked feature with its inline `LineDesc`. A stroke that names a
/// dictionary `LineDescRef`, or no description, cannot be written standalone.
pub fn stroke(writer: &mut XmlWriter, units: Units, stroke: &Stroke) -> Result<()> {
    let Some(LineDescGroup::Inline(line_desc)) = stroke.line_desc else {
        return Err(Ipc2581Error::InvalidStructure(
            "stroke without an inline LineDesc cannot be written standalone".into(),
        ));
    };
    let line_width = fmt_units(line_desc.line_width, units);
    let mut line_desc_attrs = vec![
        ("lineWidth", line_width.as_str()),
        ("lineEnd", line_desc.line_end.as_str()),
    ];
    if let Some(line_property) = line_desc.line_property {
        line_desc_attrs.push(("lineProperty", line_property.as_str()));
    }

    let mm = |value| fmt_units(value, units);
    let (name, attrs) = match &stroke.path {
        StrokePath::Line(line) => (
            "Line",
            vec![
                ("startX", mm(line.start.x)),
                ("startY", mm(line.start.y)),
                ("endX", mm(line.end.x)),
                ("endY", mm(line.end.y)),
            ],
        ),
        StrokePath::Arc(arc) => (
            "Arc",
            vec![
                ("startX", mm(arc.start.x)),
                ("startY", mm(arc.start.y)),
                ("endX", mm(arc.end.x)),
                ("endY", mm(arc.end.y)),
                ("centerX", mm(arc.center.x)),
                ("centerY", mm(arc.center.y)),
                ("clockwise", arc.clockwise.to_string()),
            ],
        ),
        StrokePath::Polyline(_) => ("Polyline", Vec::new()),
    };
    let attrs = attrs
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect::<Vec<_>>();
    writer.start_element(name, &attrs);
    if let StrokePath::Polyline(polyline) = &stroke.path {
        poly_steps(writer, units, polyline);
    }
    writer.empty_element("LineDesc", &line_desc_attrs);
    writer.end_element(name);
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

    let elem_name = fiducial.kind.as_str();
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

/// Write a `Hole` under the given name with zero tolerances. A hole with an
/// `Xform` or spec refs cannot be written standalone.
pub fn hole(writer: &mut XmlWriter, units: Units, hole: &Hole, name: &str) -> Result<()> {
    if hole.xform.is_some() || !hole.spec_refs.is_empty() {
        return Err(Ipc2581Error::InvalidStructure(
            "hole with Xform or SpecRef cannot be written standalone".into(),
        ));
    }
    writer.empty_element(
        "Hole",
        &[
            ("name", name),
            ("type", hole.shape.as_str()),
            ("diameter", fmt_units(hole.diameter, units).as_str()),
            ("platingStatus", hole.plating_status.as_str()),
            ("plusTol", "0"),
            ("minusTol", "0"),
            ("x", fmt_units(hole.x, units).as_str()),
            ("y", fmt_units(hole.y, units).as_str()),
        ],
    );
    Ok(())
}

pub fn profile(writer: &mut XmlWriter, units: Units, polygon: &Polygon) {
    profile_with_cutouts(writer, units, polygon, &[]);
}

/// Write a step profile whose material has `cutouts` removed from it.
pub fn profile_with_cutouts(
    writer: &mut XmlWriter,
    units: Units,
    polygon: &Polygon,
    cutouts: &[Polygon],
) {
    writer.start_element("Profile", &[]);
    self::polygon(writer, units, polygon);
    for cutout in cutouts {
        polygon_element(writer, "Cutout", units, cutout);
    }
    writer.end_element("Profile");
}

pub fn polygon(writer: &mut XmlWriter, units: Units, polygon: &Polygon) {
    polygon_element(writer, "Polygon", units, polygon);
}

/// Write a schema-native filled contour with optional cutout polygons.
pub fn contour(writer: &mut XmlWriter, units: Units, contour: &Contour) {
    writer.start_element("Contour", &[]);
    polygon(writer, units, &contour.polygon);
    for cutout in &contour.cutouts {
        polygon_element(writer, "Cutout", units, cutout);
    }
    writer.end_element("Contour");
}

fn polygon_element(writer: &mut XmlWriter, name: &str, units: Units, polygon: &Polygon) {
    writer.start_element(name, &[]);
    poly_steps(writer, units, polygon);
    writer.end_element(name);
}

/// Write the `PolyBegin` and steps of a polygon or polyline.
fn poly_steps(writer: &mut XmlWriter, units: Units, polygon: &Polygon) {
    let begin = polygon.begin();
    location(writer, "PolyBegin", begin.x, begin.y, units);
    for step in polygon.steps() {
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
            PolyStep::Curve(curve) => poly_step_curve(writer, units, &curve),
        }
    }
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
    use crate::types::PlatingStatus;

    #[test]
    fn hole_renders_units_and_plating() {
        let hole_mm = Hole {
            name: None,
            shape: crate::types::HoleShape::Circle,
            diameter: 2.0,
            plating_status: PlatingStatus::NonPlated,
            xform: None,
            spec_refs: Vec::new(),
            x: 1.5,
            y: -0.25,
        };
        let mut writer = XmlWriter::new();
        hole(&mut writer, Units::Millimeter, &hole_mm, "tooling_0").unwrap();
        let square = Hole {
            shape: crate::types::HoleShape::Square,
            ..hole_mm.clone()
        };
        hole(&mut writer, Units::Millimeter, &square, "tooling_1").unwrap();
        assert_eq!(
            writer.into_string(),
            r#"<Hole name="tooling_0" type="CIRCLE" diameter="2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="1.5" y="-0.25"/><Hole name="tooling_1" type="SQUARE" diameter="2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="1.5" y="-0.25"/>"#
        );

        let placed = Hole {
            xform: Some(crate::types::Xform::default()),
            ..hole_mm
        };
        assert!(
            hole(
                &mut XmlWriter::new(),
                Units::Millimeter,
                &placed,
                "tooling_2"
            )
            .is_err()
        );
    }

    #[test]
    fn contour_writes_polygon_and_cutout() {
        let point = |x| crate::types::Point { x, y: 0.0 };
        let closed = |from, to| {
            let segment = |x| PolyStep::Segment(crate::types::PolyStepSegment { point: point(x) });
            Polygon::new(point(from), [segment(to), segment(from)])
        };
        let contour = Contour {
            polygon: closed(0.0, 2.0),
            cutouts: vec![closed(0.5, 1.0)],
        };
        let mut writer = XmlWriter::new();

        self::contour(&mut writer, Units::Millimeter, &contour);

        assert_eq!(
            writer.into_string(),
            "<Contour><Polygon><PolyBegin x=\"0\" y=\"0\"/><PolyStepSegment x=\"2\" y=\"0\"/><PolyStepSegment x=\"0\" y=\"0\"/></Polygon><Cutout><PolyBegin x=\"0.5\" y=\"0\"/><PolyStepSegment x=\"1\" y=\"0\"/><PolyStepSegment x=\"0.5\" y=\"0\"/></Cutout></Contour>"
        );
    }

    #[test]
    fn stroke_writes_its_path_and_requires_an_inline_desc() {
        use crate::types::{Arc, Line, LineDesc, LineEnd, LineProperty, Point};

        let line_desc = LineDesc {
            line_width: 0.1,
            line_end: LineEnd::Round,
            line_property: Some(LineProperty::Solid),
        };
        let point = |x, y| Point { x, y };
        let paths = [
            StrokePath::Line(Line {
                start: point(0.0, 0.0),
                end: point(1.0, 0.0),
            }),
            StrokePath::Arc(Arc {
                start: point(1.0, 0.0),
                end: point(0.0, 1.0),
                center: point(0.0, 0.0),
                clockwise: false,
            }),
            StrokePath::Polyline(Polygon::new(
                point(0.0, 0.0),
                [PolyStep::Segment(crate::types::PolyStepSegment {
                    point: point(0.0, 2.0),
                })],
            )),
        ];
        let mut writer = XmlWriter::new();
        for path in &paths {
            let stroke = Stroke {
                path: path.clone(),
                line_desc: Some(LineDescGroup::Inline(line_desc)),
            };
            self::stroke(&mut writer, Units::Millimeter, &stroke).unwrap();
        }
        let desc = r#"<LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="SOLID"/>"#;
        assert_eq!(
            writer.into_string(),
            format!(
                r#"<Line startX="0" startY="0" endX="1" endY="0">{desc}</Line><Arc startX="1" startY="0" endX="0" endY="1" centerX="0" centerY="0" clockwise="false">{desc}</Arc><Polyline><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="2"/>{desc}</Polyline>"#
            )
        );

        let mut interner = pcb_intern::Interner::default();
        for line_desc in [None, Some(LineDescGroup::Ref(interner.intern("ref")))] {
            let stroke = Stroke {
                path: paths[0].clone(),
                line_desc,
            };
            assert!(self::stroke(&mut XmlWriter::new(), Units::Millimeter, &stroke).is_err());
        }
    }

    #[test]
    fn fmt_units_converts_and_trims() {
        assert_eq!(fmt_units(25.4, Units::Inch), "1");
        assert_eq!(fmt_units(1.0, Units::Millimeter), "1");
        // A nanometre survives in either unit.
        assert_eq!(fmt_units(10.000001, Units::Millimeter), "10.000001");
        assert_eq!(fmt_units(10.000001, Units::Inch), "0.39370083");
        assert_eq!(fmt_num(-0.0000000001), "0");
    }
}
