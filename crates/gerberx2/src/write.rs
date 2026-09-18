use crate::types::*;
use crate::{GerberError, Result};
use pcb_ir::geom::Polarity;
use pcb_ir::geom::region::Ring;
use std::fmt::Write as _;

/// String-backed X2 attribute used by the Gerber writer.
///
/// Attribute names should include the leading X2 dot, for example
/// `.FileFunction`, `.AperFunction`, `.N`, `.C`, or `.P`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttributeValue {
    pub name: String,
    pub fields: Vec<String>,
}

impl AttributeValue {
    pub fn new(
        name: impl Into<String>,
        fields: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            fields: fields.into_iter().map(Into::into).collect(),
        }
    }
}

/// Convert arbitrary metadata into a Gerber X2 attribute field.
///
/// Gerber attributes are comma-separated and commands are terminated by `*`
/// inside `%...%` extended commands, so those characters cannot appear
/// literally in a field. The writer keeps validation strict; source dialects
/// should normalize free-form metadata through this helper when lowering into
/// Gerber writer IR.
pub fn sanitize_attribute_field(field: &str) -> String {
    let sanitized = field
        .chars()
        .map(|ch| match ch {
            '*' | '%' | ',' => '_',
            _ => ch,
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "_".to_string()
    } else {
        sanitized
    }
}

/// One aperture definition plus X2 aperture attributes active while defining it.
#[derive(Debug, Clone, PartialEq)]
pub struct WriterAperture {
    pub code: i32,
    pub template: WriterApertureTemplate,
    pub attributes: Vec<AttributeValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriterApertureTemplate {
    Circle {
        diameter: f64,
        hole_diameter: Option<f64>,
    },
    Rectangle {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Obround {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Polygon {
        outer_diameter: f64,
        vertices: i32,
        rotation_degrees: Option<f64>,
        hole_diameter: Option<f64>,
    },
    /// Filled simple polygons about the flash origin, written as one
    /// aperture macro of additive outline primitives.
    Outline { outlines: Vec<Ring> },
}

/// One ordered graphical object plus X2 object attributes active while emitting it.
#[derive(Debug, Clone, PartialEq)]
pub struct WriterObject {
    pub kind: ObjectKind,
    pub polarity: Polarity,
    pub repeat: Option<StepRepeat>,
    /// Aperture attributes attached directly to a region object.
    pub aperture_attributes: Vec<AttributeValue>,
    pub attributes: Vec<AttributeValue>,
}

impl WriterObject {
    pub fn new(kind: ObjectKind, polarity: Polarity, attributes: Vec<AttributeValue>) -> Self {
        Self {
            kind,
            polarity,
            repeat: None,
            aperture_attributes: Vec::new(),
            attributes,
        }
    }

    pub fn dark(kind: ObjectKind) -> Self {
        Self::new(kind, Polarity::Dark, Vec::new())
    }
}

/// Format-neutral artwork/object IR for writing one Gerber X2 layer file.
///
/// This is intentionally close to Gerber's object stream: apertures remain
/// apertures, pads remain flashes, tracks remain draws/arcs, and filled copper
/// remains regions. IPC-2581 export should lower into this level before writing
/// Gerber, and use flattened geometry only for validation or unavoidable
/// fallback regions.
#[derive(Debug, Clone, PartialEq)]
pub struct GerberLayer {
    pub unit: Unit,
    pub coordinate_format: CoordinateFormat,
    pub file_attributes: Vec<AttributeValue>,
    pub apertures: Vec<WriterAperture>,
    pub objects: Vec<WriterObject>,
}

impl Default for GerberLayer {
    fn default() -> Self {
        Self {
            unit: Unit::Millimeter,
            coordinate_format: CoordinateFormat {
                x_integer_digits: 6,
                x_decimal_digits: 6,
                y_integer_digits: 6,
                y_decimal_digits: 6,
            },
            file_attributes: Vec::new(),
            apertures: Vec::new(),
            objects: Vec::new(),
        }
    }
}

/// Write one complete Gerber X2 layer.
pub fn write_layer(layer: &GerberLayer) -> Result<String> {
    let mut writer = Writer::new(layer);
    writer.write_layer()?;
    Ok(writer.output)
}

struct Writer<'a> {
    layer: &'a GerberLayer,
    output: String,
    current_aperture: Option<i32>,
    current_polarity: Polarity,
    current_plot_mode: Option<PlotMode>,
    current_repeat: Option<StepRepeat>,
    /// Modal X/Y last written.
    current_coordinates: Option<(i64, i64)>,
    /// The current point while it is known to continue a stroke.
    current_point: Option<(i64, i64)>,
    current_aperture_attributes: Vec<AttributeValue>,
    current_object_attributes: Vec<AttributeValue>,
}

impl<'a> Writer<'a> {
    fn new(layer: &'a GerberLayer) -> Self {
        Self {
            layer,
            output: String::new(),
            current_aperture: None,
            current_polarity: Polarity::Dark,
            current_plot_mode: None,
            current_repeat: None,
            current_coordinates: None,
            current_point: None,
            current_aperture_attributes: Vec::new(),
            current_object_attributes: Vec::new(),
        }
    }

    fn write_layer(&mut self) -> Result<()> {
        self.output.push_str("G04 generated by gerberx2*\n");
        self.write_format();
        self.write_unit();
        self.output.push_str("G75*\n");

        for attr in &self.layer.file_attributes {
            self.write_attribute("TF", attr)?;
        }

        for aperture in &self.layer.apertures {
            if let WriterApertureTemplate::Outline { outlines } = &aperture.template {
                self.write_outline_macro(aperture.code, outlines)?;
            }
        }

        for aperture in &self.layer.apertures {
            for attr in &aperture.attributes {
                self.write_attribute("TA", attr)?;
            }
            self.write_aperture(aperture)?;
            if !aperture.attributes.is_empty() {
                self.output.push_str("%TD*%\n");
            }
        }

        self.write_objects(&self.layer.objects)?;

        self.output.push_str("M02*\n");
        Ok(())
    }

    /// One additive code-4 outline primitive per polygon.
    fn write_outline_macro(&mut self, code: i32, outlines: &[Ring]) -> Result<()> {
        write!(self.output, "%AMOUTLINE{code}*\n").unwrap();
        for outline in outlines {
            if outline.len() < 3 {
                return Err(GerberError::InvalidStructure(
                    "cannot export a Gerber outline macro with fewer than three vertices"
                        .to_string(),
                ));
            }
            write!(self.output, "4,1,{}", outline.len()).unwrap();
            for [x, y] in outline.iter().chain(std::iter::once(&outline[0])) {
                self.output.push(',');
                self.write_decimal(*x);
                self.output.push(',');
                self.write_decimal(*y);
            }
            self.output.push_str(",0*\n");
        }
        self.output.push_str("%\n");
        Ok(())
    }

    fn write_format(&mut self) {
        let format = self.layer.coordinate_format;
        self.output.push_str(&format!(
            "%FSLAX{}{}Y{}{}*%\n",
            format.x_integer_digits,
            format.x_decimal_digits,
            format.y_integer_digits,
            format.y_decimal_digits
        ));
    }

    fn write_unit(&mut self) {
        let unit = match self.layer.unit {
            Unit::Millimeter => "MM",
            Unit::Inch => "IN",
        };
        self.output.push_str(&format!("%MO{unit}*%\n"));
    }

    fn write_attribute(&mut self, command: &str, attr: &AttributeValue) -> Result<()> {
        validate_attribute(attr)?;
        self.output.push('%');
        self.output.push_str(command);
        self.output.push_str(&attr.name);
        for field in &attr.fields {
            self.output.push(',');
            self.output.push_str(field);
        }
        self.output.push_str("*%\n");
        Ok(())
    }

    fn write_aperture(&mut self, aperture: &WriterAperture) -> Result<()> {
        if aperture.code < 10 {
            return Err(GerberError::InvalidStructure(format!(
                "aperture D{} is invalid; aperture codes must be >= 10",
                aperture.code
            )));
        }

        self.output.push_str(&format!("%ADD{}", aperture.code));
        match &aperture.template {
            WriterApertureTemplate::Circle {
                diameter,
                hole_diameter,
            } => {
                self.output.push_str("C,");
                self.write_decimal(*diameter);
                if let Some(hole_diameter) = hole_diameter {
                    self.output.push('X');
                    self.write_decimal(*hole_diameter);
                }
            }
            WriterApertureTemplate::Rectangle {
                width,
                height,
                hole_diameter,
            } => {
                self.output.push_str("R,");
                self.write_decimal(*width);
                self.output.push('X');
                self.write_decimal(*height);
                if let Some(hole_diameter) = hole_diameter {
                    self.output.push('X');
                    self.write_decimal(*hole_diameter);
                }
            }
            WriterApertureTemplate::Obround {
                width,
                height,
                hole_diameter,
            } => {
                self.output.push_str("O,");
                self.write_decimal(*width);
                self.output.push('X');
                self.write_decimal(*height);
                if let Some(hole_diameter) = hole_diameter {
                    self.output.push('X');
                    self.write_decimal(*hole_diameter);
                }
            }
            WriterApertureTemplate::Polygon {
                outer_diameter,
                vertices,
                rotation_degrees,
                hole_diameter,
            } => {
                self.output.push_str("P,");
                self.write_decimal(*outer_diameter);
                self.output.push('X');
                self.output.push_str(&vertices.to_string());
                if rotation_degrees.is_some() || hole_diameter.is_some() {
                    self.output.push('X');
                    self.write_decimal(rotation_degrees.unwrap_or(0.0));
                }
                if let Some(hole_diameter) = hole_diameter {
                    self.output.push('X');
                    self.write_decimal(*hole_diameter);
                }
            }
            WriterApertureTemplate::Outline { .. } => {
                write!(self.output, "OUTLINE{}", aperture.code).unwrap();
            }
        }
        self.output.push_str("*%\n");
        Ok(())
    }

    fn write_objects(&mut self, objects: &[WriterObject]) -> Result<()> {
        for object in objects {
            self.write_object(object)?;
        }
        self.close_step_repeat();
        Ok(())
    }

    fn write_object(&mut self, object: &WriterObject) -> Result<()> {
        if self.current_repeat != object.repeat
            || (self.current_repeat.is_some() && self.current_polarity != object.polarity)
        {
            self.close_step_repeat();
        }

        self.set_polarity(object.polarity);
        self.set_attributes(&object.aperture_attributes, &object.attributes)?;
        self.open_step_repeat(object.repeat)?;

        match &object.kind {
            ObjectKind::Draw {
                start,
                end,
                aperture,
            } => {
                self.set_aperture(*aperture);
                self.set_plot_mode(PlotMode::Linear);
                self.write_move(*start);
                self.write_plot(*end, None);
            }
            ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                aperture,
            } => {
                self.set_aperture(*aperture);
                self.write_move(*start);
                self.write_arc(*start, *end, *center_offset, *clockwise);
            }
            ObjectKind::Flash { at, aperture } => {
                self.set_aperture(*aperture);
                self.write_point(*at);
                self.output.push_str("D03*\n");
                self.current_point = None;
            }
            ObjectKind::Region { contours } => {
                self.write_region(contours)?;
            }
        }

        Ok(())
    }

    fn open_step_repeat(&mut self, repeat: Option<StepRepeat>) -> Result<()> {
        let Some(repeat) = repeat else {
            return Ok(());
        };
        if self.current_repeat == Some(repeat) {
            return Ok(());
        }
        if repeat.x_repeats <= 0
            || repeat.y_repeats <= 0
            || repeat.x_step < 0.0
            || repeat.y_step < 0.0
            || !repeat.x_step.is_finite()
            || !repeat.y_step.is_finite()
        {
            return Err(GerberError::InvalidStructure(
                "Gerber step-repeat requires positive counts and finite non-negative steps"
                    .to_string(),
            ));
        }
        self.output.push_str("%SRX");
        self.output.push_str(&repeat.x_repeats.to_string());
        self.output.push('Y');
        self.output.push_str(&repeat.y_repeats.to_string());
        self.output.push('I');
        self.write_decimal(repeat.x_step);
        self.output.push('J');
        self.write_decimal(repeat.y_step);
        self.output.push_str("*%\n");
        self.current_repeat = Some(repeat);
        self.reset_coordinates();
        Ok(())
    }

    fn close_step_repeat(&mut self) {
        if self.current_repeat.take().is_some() {
            self.output.push_str("%SR*%\n");
            self.reset_coordinates();
        }
    }

    fn set_attributes(
        &mut self,
        aperture_attributes: &[AttributeValue],
        object_attributes: &[AttributeValue],
    ) -> Result<()> {
        if self.current_aperture_attributes == aperture_attributes
            && self.current_object_attributes == object_attributes
        {
            return Ok(());
        }
        let dropped_aperture = self.current_aperture_attributes.iter().any(|current| {
            !aperture_attributes
                .iter()
                .any(|attribute| attribute.name == current.name)
        });
        let dropped_object = self.current_object_attributes.iter().any(|current| {
            !object_attributes
                .iter()
                .any(|attribute| attribute.name == current.name)
        });
        if dropped_aperture || dropped_object {
            self.output.push_str("%TD*%\n");
            self.current_aperture_attributes.clear();
            self.current_object_attributes.clear();
        }
        for attribute in aperture_attributes {
            if !self.current_aperture_attributes.contains(attribute) {
                self.write_attribute("TA", attribute)?;
            }
        }
        for attribute in object_attributes {
            if !self.current_object_attributes.contains(attribute) {
                self.write_attribute("TO", attribute)?;
            }
        }
        self.current_aperture_attributes = aperture_attributes.to_vec();
        self.current_object_attributes = object_attributes.to_vec();
        Ok(())
    }

    fn write_region(&mut self, contours: &[Contour]) -> Result<()> {
        self.output.push_str("G36*\n");
        for contour in contours {
            let Some(first) = contour.segments.first() else {
                continue;
            };
            self.set_plot_mode(PlotMode::Linear);
            // A contour always opens with its own move.
            self.current_point = None;
            self.write_move(segment_start(first));
            for segment in &contour.segments {
                match *segment {
                    ContourSegment::Line { start, end } => {
                        if self.coordinates(start) == self.coordinates(end) {
                            return Err(GerberError::InvalidStructure(format!(
                                "region segment from ({}, {}) to ({}, {}) collapses at output precision; increase precision or repair the source geometry",
                                start.x, start.y, end.x, end.y
                            )));
                        }
                        self.set_plot_mode(PlotMode::Linear);
                        self.write_plot(end, None);
                    }
                    ContourSegment::Arc {
                        start,
                        end,
                        center_offset,
                        clockwise,
                    } => {
                        self.write_arc(start, end, center_offset, clockwise);
                    }
                }
            }
        }
        self.output.push_str("G37*\n");
        self.current_point = None;
        Ok(())
    }

    fn set_aperture(&mut self, aperture: i32) {
        if self.current_aperture != Some(aperture) {
            self.output.push_str(&format!("D{aperture}*\n"));
            self.current_aperture = Some(aperture);
        }
    }

    fn set_polarity(&mut self, polarity: Polarity) {
        if self.current_polarity != polarity {
            let code = match polarity {
                Polarity::Dark => "D",
                Polarity::Clear => "C",
            };
            self.output.push_str(&format!("%LP{code}*%\n"));
            self.current_polarity = polarity;
        }
    }

    fn set_plot_mode(&mut self, mode: PlotMode) {
        if self.current_plot_mode != Some(mode) {
            let code = match mode {
                PlotMode::Linear => "G01",
                PlotMode::ClockwiseArc => "G02",
                PlotMode::CounterclockwiseArc => "G03",
            };
            self.output.push_str(code);
            self.output.push_str("*\n");
            self.current_plot_mode = Some(mode);
        }
    }

    /// Move to `point`, unless the previous plot already ended there.
    fn write_move(&mut self, point: Point) {
        if self.current_point == Some(self.coordinates(point)) {
            return;
        }
        self.write_point(point);
        self.output.push_str("D02*\n");
        self.current_point = self.current_coordinates;
    }

    /// Split circular arcs into nominal sweeps of at most 180 degrees before
    /// coordinate rounding. Serialized sweeps can differ slightly; G75 allows
    /// this. Splitting keeps long arcs away from coincident endpoints, which
    /// G75 interprets as a full circle.
    /// Equal subdivisions avoid leaving a tiny remainder for near-full circles.
    fn write_arc(&mut self, start: Point, end: Point, offset: Point, clockwise: bool) {
        use pcb_ir::geom::{Arc, Point as GeometryPoint, Segment};
        let arc = Arc::new(
            GeometryPoint::new(start.x, start.y),
            GeometryPoint::new(end.x, end.y),
            GeometryPoint::new(start.x + offset.x, start.y + offset.y),
            clockwise,
        );
        let sweep = arc.sweep_radians();
        let count = (sweep / std::f64::consts::PI).ceil().max(1.0) as usize;
        self.set_plot_mode(if clockwise {
            PlotMode::ClockwiseArc
        } else {
            PlotMode::CounterclockwiseArc
        });
        let mut center_offset = offset;
        for index in 1..=count {
            // Preserve the source endpoint exactly, including source rounding.
            let next = if index == count {
                end
            } else {
                let point = Segment::Arc(arc).point_at(index as f64 / count as f64);
                Point {
                    x: point.x,
                    y: point.y,
                }
            };
            self.write_plot(next, Some(center_offset));
            center_offset = Point {
                x: arc.center.x - next.x,
                y: arc.center.y - next.y,
            };
        }
    }

    fn write_plot(&mut self, point: Point, center_offset: Option<Point>) {
        self.write_point(point);
        if let Some(center_offset) = center_offset {
            let (i, j) = self.coordinates(center_offset);
            write!(self.output, "I{i}J{j}").unwrap();
        }
        self.output.push_str("D01*\n");
        self.current_point = self.current_coordinates;
    }

    fn write_point(&mut self, point: Point) {
        let (x, y) = self.coordinates(point);
        let (x_changed, y_changed) = self
            .current_coordinates
            .map_or((true, true), |(current_x, current_y)| {
                (current_x != x, current_y != y)
            });

        // X and Y are modal. Keep one axis explicit for same-point operations
        // to avoid relying on coordinate-free D codes in older CAM software.
        if x_changed || !y_changed {
            write!(self.output, "X{x}").unwrap();
        }
        if y_changed {
            write!(self.output, "Y{y}").unwrap();
        }
        self.current_coordinates = Some((x, y));
    }

    fn reset_coordinates(&mut self) {
        self.current_coordinates = None;
        self.current_point = None;
    }

    /// `point` in integer output units.
    fn coordinates(&self, point: Point) -> (i64, i64) {
        let format = self.layer.coordinate_format;
        let scaled = |value: f64, decimals: u8| {
            (value * 10_f64.powi(decimals as i32)).round_ties_even() as i64
        };
        (
            scaled(point.x, format.x_decimal_digits),
            scaled(point.y, format.y_decimal_digits),
        )
    }

    fn write_decimal(&mut self, value: f64) {
        self.output.push_str(&trim_decimal(value));
    }
}

fn validate_attribute(attr: &AttributeValue) -> Result<()> {
    if attr.name.is_empty() {
        return Err(GerberError::InvalidStructure(
            "attribute name must not be empty".to_string(),
        ));
    }
    if !attr.name.starts_with('.') {
        return Err(GerberError::InvalidStructure(format!(
            "X2 attribute name '{}' must start with '.'",
            attr.name
        )));
    }
    validate_no_command_delimiters(&attr.name, "attribute name")?;
    for field in &attr.fields {
        validate_no_command_delimiters(field, "attribute field")?;
    }
    Ok(())
}

fn validate_no_command_delimiters(value: &str, label: &str) -> Result<()> {
    if value.contains(['*', '%', ',']) {
        return Err(GerberError::InvalidStructure(format!(
            "{label} must not contain Gerber command delimiters or field separators"
        )));
    }
    Ok(())
}

fn segment_start(segment: &ContourSegment) -> Point {
    match *segment {
        ContourSegment::Line { start, .. } | ContourSegment::Arc { start, .. } => start,
    }
}

fn trim_decimal(value: f64) -> String {
    let mut text = format!("{value:.9}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_freeform_attribute_fields() {
        assert_eq!(sanitize_attribute_field("PWR_RST*,A%B"), "PWR_RST__A_B");
        assert_eq!(sanitize_attribute_field(""), "_");
    }

    #[test]
    fn rejects_attribute_field_separators() {
        let layer = GerberLayer {
            file_attributes: vec![AttributeValue::new(".FileFunction", ["Copper,Top"])],
            ..GerberLayer::default()
        };

        let err = write_layer(&layer).unwrap_err().to_string();
        assert!(err.contains("field separators"), "{err}");
    }

    #[test]
    fn chained_draws_share_the_current_point_but_contours_always_move() {
        let point = |x: f64, y: f64| Point { x, y };
        let draw = |start, end| {
            WriterObject::dark(ObjectKind::Draw {
                start,
                end,
                aperture: 10,
            })
        };
        let layer = GerberLayer {
            apertures: vec![WriterAperture {
                code: 10,
                template: WriterApertureTemplate::Circle {
                    diameter: 0.1,
                    hole_diameter: None,
                },
                attributes: Vec::new(),
            }],
            objects: vec![
                draw(point(0.0, 0.0), point(1.0, 0.0)),
                draw(point(1.0, 0.0), point(1.0, 1.0)),
                draw(point(2.0, 2.0), point(3.0, 2.0)),
                WriterObject::dark(ObjectKind::Region {
                    contours: vec![Contour {
                        segments: [
                            (3.0, 2.0, 4.0, 2.0),
                            (4.0, 2.0, 4.0, 3.0),
                            (4.0, 3.0, 3.0, 2.0),
                        ]
                        .map(|(x0, y0, x1, y1)| ContourSegment::Line {
                            start: point(x0, y0),
                            end: point(x1, y1),
                        })
                        .to_vec(),
                    }],
                }),
                draw(point(3.0, 2.0), point(5.0, 5.0)),
            ],
            ..GerberLayer::default()
        };

        let output = write_layer(&layer).unwrap();
        // One move per disjoint stroke start, the region contour, and the
        // stroke after the region; the chained second draw needs none.
        assert_eq!(output.matches("D02*").count(), 4, "{output}");
        assert!(output.contains("G36*\nX3000000D02*"), "{output}");
        let parsed = crate::GerberX2::parse(&output).unwrap();
        assert_eq!(parsed.objects().len(), 5);
    }

    #[test]
    fn object_attributes_persist_across_objects() {
        let flash = |x: f64, attributes: Vec<AttributeValue>| WriterObject {
            kind: ObjectKind::Flash {
                at: Point { x, y: 0.0 },
                aperture: 10,
            },
            polarity: Polarity::Dark,
            repeat: None,
            aperture_attributes: Vec::new(),
            attributes,
        };
        let net = |name: &str| AttributeValue::new(".N", [name]);
        let layer = GerberLayer {
            apertures: vec![WriterAperture {
                code: 10,
                template: WriterApertureTemplate::Circle {
                    diameter: 1.0,
                    hole_diameter: None,
                },
                attributes: Vec::new(),
            }],
            objects: vec![
                flash(0.0, vec![net("GND")]),
                flash(1.0, vec![net("GND")]),
                flash(2.0, vec![net("V3V3")]),
                flash(3.0, Vec::new()),
            ],
            ..GerberLayer::default()
        };

        let output = write_layer(&layer).unwrap();
        // The repeated net rides existing state, the changed net overrides in
        // place, and only dropping attributes resets the dictionary.
        assert_eq!(output.matches("%TO.N,GND*%").count(), 1);
        assert_eq!(output.matches("%TO.N,V3V3*%").count(), 1);
        assert_eq!(output.matches("%TD*%").count(), 1);
    }
}
