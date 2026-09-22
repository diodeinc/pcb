use crate::types::*;
use crate::{GerberError, Result};
use pcb_ir::geom::Polarity;
use pcb_ir::geom::region::Ring;
use std::collections::HashMap;
use std::fmt::Write as _;

/// String-backed X2 attribute used by the Gerber writer.
///
/// Attribute names should include the leading X2 dot, for example
/// `.FileFunction`, `.AperFunction`, `.N`, `.C`, or `.P`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Encode free-form metadata as one X2 attribute field, for Gerber and XNC
/// alike.
///
/// A field is printable ASCII. The command delimiters `*` and `%`, the field
/// separator `,`, the XNC comment mark `;`, the escape character `\` itself,
/// control characters and everything beyond ASCII are written as `\uXXXX`
/// UTF-16 escapes, which is what the format specifies and what KiCad writes.
/// The writers keep validation strict; source dialects pass free-form
/// metadata through this encoding when lowering into writer IR.
pub fn escape_attribute_field(field: &str) -> String {
    if field.is_empty() {
        return "_".to_string();
    }
    let mut escaped = String::with_capacity(field.len());
    for ch in field.chars() {
        if matches!(ch, ' '..='~') && !matches!(ch, '\\' | '*' | '%' | ',' | ';') {
            escaped.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(escaped, "\\u{unit:04X}").unwrap();
            }
        }
    }
    escaped
}

/// Decode the `\uXXXX` escapes of an attribute field read from a file.
pub fn unescape_attribute_field(field: &str) -> String {
    let mut units = Vec::with_capacity(field.len());
    let mut rest = field;
    while let Some(ch) = rest.chars().next() {
        let escape = rest
            .strip_prefix("\\u")
            .and_then(|hex| hex.get(..4))
            .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if let Some(hex) = escape {
            units.push(u16::from_str_radix(hex, 16).expect("four hex digits"));
            rest = &rest[6..];
        } else {
            units.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
            rest = &rest[ch.len_utf8()..];
        }
    }
    String::from_utf16_lossy(&units)
}

/// The distinct X2 attribute sets of one layer. Apertures and objects name
/// their set by id, so the handful of sets a layer uses is stored once and
/// equal sets compare as one integer.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeSets {
    sets: Vec<Vec<AttributeValue>>,
    ids: HashMap<Vec<AttributeValue>, u32>,
}

impl AttributeSets {
    /// The set without attributes, present in every layer.
    pub const EMPTY: u32 = 0;

    pub fn intern(&mut self, set: Vec<AttributeValue>) -> u32 {
        if let Some(&id) = self.ids.get(&set) {
            return id;
        }
        let id = self.sets.len() as u32;
        self.sets.push(set.clone());
        self.ids.insert(set, id);
        id
    }

    pub fn get(&self, id: u32) -> Option<&[AttributeValue]> {
        self.sets.get(id as usize).map(Vec::as_slice)
    }

    /// Every set, indexed by id.
    pub fn sets(&self) -> &[Vec<AttributeValue>] {
        &self.sets
    }
}

impl Default for AttributeSets {
    fn default() -> Self {
        Self {
            sets: vec![Vec::new()],
            ids: HashMap::from([(Vec::new(), Self::EMPTY)]),
        }
    }
}

/// One aperture definition plus the X2 aperture attributes active while
/// defining it, as a set of [`GerberLayer::attribute_sets`].
#[derive(Debug, Clone, PartialEq)]
pub struct WriterAperture {
    pub code: i32,
    pub template: WriterApertureTemplate,
    pub attributes: u32,
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

/// One ordered graphical object plus the X2 object attributes active while
/// emitting it, as a set of [`GerberLayer::attribute_sets`].
#[derive(Debug, Clone, PartialEq)]
pub struct WriterObject {
    pub kind: ObjectKind,
    pub polarity: Polarity,
    pub repeat: Option<StepRepeat>,
    /// Aperture attributes attached directly to a region object.
    pub aperture_attributes: u32,
    pub attributes: u32,
}

impl WriterObject {
    pub fn new(kind: ObjectKind, polarity: Polarity, attributes: u32) -> Self {
        Self {
            kind,
            polarity,
            repeat: None,
            aperture_attributes: AttributeSets::EMPTY,
            attributes,
        }
    }

    pub fn dark(kind: ObjectKind) -> Self {
        Self::new(kind, Polarity::Dark, AttributeSets::EMPTY)
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
    pub attribute_sets: AttributeSets,
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
            attribute_sets: AttributeSets::default(),
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
    /// Where the previous object's draw ended, while no other operation has
    /// intervened.
    stroke_end: Option<(i64, i64)>,
    /// The aperture and object attribute sets of the last object, whose
    /// attributes are the file's dictionary at this point.
    current_attribute_sets: (u32, u32),
    current_aperture_attributes: &'a [AttributeValue],
    current_object_attributes: &'a [AttributeValue],
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
            stroke_end: None,
            current_attribute_sets: (AttributeSets::EMPTY, AttributeSets::EMPTY),
            current_aperture_attributes: &[],
            current_object_attributes: &[],
        }
    }

    fn attribute_set(&self, id: u32) -> Result<&'a [AttributeValue]> {
        self.layer.attribute_sets.get(id).ok_or_else(|| {
            GerberError::InvalidStructure(format!("attribute set {id} is not in the layer"))
        })
    }

    fn write_layer(&mut self) -> Result<()> {
        let format = self.layer.coordinate_format;
        let unit = match self.layer.unit {
            Unit::Millimeter => "MM",
            Unit::Inch => "IN",
        };
        writeln!(
            self.output,
            "G04 generated by gerberx2*\n%FSLAX{}{}Y{}{}*%\n%MO{unit}*%\nG75*",
            format.x_integer_digits,
            format.x_decimal_digits,
            format.y_integer_digits,
            format.y_decimal_digits
        )
        .unwrap();

        for attr in &self.layer.file_attributes {
            self.write_attribute("TF", attr)?;
        }

        for aperture in &self.layer.apertures {
            if let WriterApertureTemplate::Outline { outlines } = &aperture.template {
                self.write_outline_macro(aperture.code, outlines)?;
            }
        }

        for aperture in &self.layer.apertures {
            let attributes = self.attribute_set(aperture.attributes)?;
            for attr in attributes {
                self.write_attribute("TA", attr)?;
            }
            self.write_aperture(aperture)?;
            if !attributes.is_empty() {
                self.output.push_str("%TD*%\n");
            }
        }

        self.write_objects(&self.layer.objects)?;

        self.output.push_str("M02*\n");
        Ok(())
    }

    /// One additive code-4 outline primitive per polygon.
    fn write_outline_macro(&mut self, code: i32, outlines: &[Ring]) -> Result<()> {
        writeln!(self.output, "%AMOUTLINE{code}*").unwrap();
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

        // A template is its letter and `X`-separated parameters, the hole last.
        let (letter, parameters, hole_diameter) = match aperture.template {
            WriterApertureTemplate::Circle {
                diameter,
                hole_diameter,
            } => ('C', vec![diameter], hole_diameter),
            WriterApertureTemplate::Rectangle {
                width,
                height,
                hole_diameter,
            } => ('R', vec![width, height], hole_diameter),
            WriterApertureTemplate::Obround {
                width,
                height,
                hole_diameter,
            } => ('O', vec![width, height], hole_diameter),
            WriterApertureTemplate::Polygon {
                outer_diameter,
                vertices,
                rotation_degrees,
                hole_diameter,
            } => {
                // A hole is positional after the rotation.
                let rotation = rotation_degrees.or(hole_diameter.map(|_| 0.0));
                let mut parameters = vec![outer_diameter, f64::from(vertices)];
                parameters.extend(rotation);
                ('P', parameters, hole_diameter)
            }
            WriterApertureTemplate::Outline { .. } => {
                writeln!(self.output, "%ADD{0}OUTLINE{0}*%", aperture.code).unwrap();
                return Ok(());
            }
        };
        write!(self.output, "%ADD{}{letter}", aperture.code).unwrap();
        for (index, value) in parameters.into_iter().chain(hole_diameter).enumerate() {
            self.output.push(if index == 0 { ',' } else { 'X' });
            self.write_decimal(value);
        }
        self.output.push_str("*%\n");
        Ok(())
    }

    fn write_objects(&mut self, objects: &[WriterObject]) -> Result<()> {
        for (index, object) in objects.iter().enumerate() {
            if !self.is_covered_dot(object, objects.get(index + 1)) {
                self.write_object(object)?;
            }
        }
        self.close_step_repeat();
        Ok(())
    }

    /// Whether a segment's endpoints coincide at output precision. A full
    /// circle is exempt; every other arc must not reach the file this way,
    /// because G75 reads coincident arc endpoints as 360 degrees.
    fn collapses(&self, start: Point, end: Point, arc: Option<(Point, bool)>) -> bool {
        self.coordinates(start) == self.coordinates(end)
            && arc.is_none_or(|(offset, clockwise)| {
                geometry_arc(start, end, offset, clockwise).sweep_radians() <= std::f64::consts::PI
            })
    }

    /// The point and aperture of a draw that images as a single dot.
    fn dot(&self, kind: &ObjectKind) -> Option<((i64, i64), i32)> {
        let (start, end, arc, aperture) = match *kind {
            ObjectKind::Draw {
                start,
                end,
                aperture,
            } => (start, end, None, aperture),
            ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                aperture,
            } => (start, end, Some((center_offset, clockwise)), aperture),
            ObjectKind::Flash { .. } | ObjectKind::Region { .. } => return None,
        };
        self.collapses(start, end, arc)
            .then(|| (self.coordinates(end), aperture))
    }

    /// A dot is redundant when the neighbouring draw of the same stroke
    /// already images its disc; a dot on its own is the whole image.
    fn is_covered_dot(&self, object: &WriterObject, next: Option<&WriterObject>) -> bool {
        let Some((at, aperture)) = self.dot(&object.kind) else {
            return false;
        };
        let before = self.stroke_end == Some(at)
            && self.current_aperture == Some(aperture)
            && self.current_polarity == object.polarity
            && self.current_repeat == object.repeat
            && self.current_attribute_sets.1 == object.attributes;
        let after = next.is_some_and(|next| {
            let continues = match next.kind {
                ObjectKind::Draw {
                    start,
                    aperture: next_aperture,
                    ..
                }
                | ObjectKind::Arc {
                    start,
                    aperture: next_aperture,
                    ..
                } => next_aperture == aperture && self.coordinates(start) == at,
                ObjectKind::Flash { .. } | ObjectKind::Region { .. } => false,
            };
            continues
                && next.polarity == object.polarity
                && next.repeat == object.repeat
                && next.attributes == object.attributes
        });
        before || after
    }

    fn write_object(&mut self, object: &WriterObject) -> Result<()> {
        if self.current_repeat != object.repeat
            || (self.current_repeat.is_some() && self.current_polarity != object.polarity)
        {
            self.close_step_repeat();
        }

        self.set_polarity(object.polarity);
        self.set_attributes(object.aperture_attributes, object.attributes)?;
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
        self.stroke_end = match object.kind {
            ObjectKind::Draw { .. } | ObjectKind::Arc { .. } => self.current_point,
            ObjectKind::Flash { .. } | ObjectKind::Region { .. } => None,
        };

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
        write!(
            self.output,
            "%SRX{}Y{}I",
            repeat.x_repeats, repeat.y_repeats
        )
        .unwrap();
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

    fn set_attributes(&mut self, aperture_set: u32, object_set: u32) -> Result<()> {
        if self.current_attribute_sets == (aperture_set, object_set) {
            return Ok(());
        }
        let aperture_attributes = self.attribute_set(aperture_set)?;
        let object_attributes = self.attribute_set(object_set)?;
        let drops = |current: &[AttributeValue], next: &[AttributeValue]| {
            current
                .iter()
                .any(|current| !next.iter().any(|next| next.name == current.name))
        };
        if drops(self.current_aperture_attributes, aperture_attributes)
            || drops(self.current_object_attributes, object_attributes)
        {
            self.output.push_str("%TD*%\n");
            self.current_aperture_attributes = &[];
            self.current_object_attributes = &[];
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
        self.current_attribute_sets = (aperture_set, object_set);
        self.current_aperture_attributes = aperture_attributes;
        self.current_object_attributes = object_attributes;
        Ok(())
    }

    fn write_region(&mut self, contours: &[Contour]) -> Result<()> {
        self.output.push_str("G36*\n");
        for contour in contours {
            let Some(ContourSegment::Line { start, .. } | ContourSegment::Arc { start, .. }) =
                contour.segments.first()
            else {
                continue;
            };
            self.set_plot_mode(PlotMode::Linear);
            // A contour always opens with its own move.
            self.current_point = None;
            self.write_move(*start);
            for segment in &contour.segments {
                let (start, end, arc) = match *segment {
                    ContourSegment::Line { start, end } => (start, end, None),
                    ContourSegment::Arc {
                        start,
                        end,
                        center_offset,
                        clockwise,
                    } => (start, end, Some((center_offset, clockwise))),
                };
                if self.collapses(start, end, arc) {
                    return Err(GerberError::InvalidStructure(format!(
                        "region segment from ({}, {}) to ({}, {}) collapses at output precision; increase precision or repair the source geometry",
                        start.x, start.y, end.x, end.y
                    )));
                }
                match arc {
                    None => {
                        self.set_plot_mode(PlotMode::Linear);
                        self.write_plot(end, None);
                    }
                    Some((center_offset, clockwise)) => {
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
            writeln!(self.output, "D{aperture}*").unwrap();
            self.current_aperture = Some(aperture);
        }
    }

    fn set_polarity(&mut self, polarity: Polarity) {
        if self.current_polarity != polarity {
            let code = match polarity {
                Polarity::Dark => "D",
                Polarity::Clear => "C",
            };
            writeln!(self.output, "%LP{code}*%").unwrap();
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
        // A collapsed arc images as its dot.
        if self.collapses(start, end, Some((offset, clockwise))) {
            self.set_plot_mode(PlotMode::Linear);
            return self.write_plot(end, None);
        }
        let arc = geometry_arc(start, end, offset, clockwise);
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
                let point = pcb_ir::geom::Segment::Arc(arc).point_at(index as f64 / count as f64);
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
        self.output.push_str(&trim_decimal(value, 9));
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

fn geometry_arc(start: Point, end: Point, offset: Point, clockwise: bool) -> pcb_ir::geom::Arc {
    use pcb_ir::geom::Point as GeometryPoint;
    pcb_ir::geom::Arc::new(
        GeometryPoint::new(start.x, start.y),
        GeometryPoint::new(end.x, end.y),
        GeometryPoint::new(start.x + offset.x, start.y + offset.y),
        clockwise,
    )
}

/// `value` in fixed-point notation with at most `decimals` decimals and no
/// trailing zeros.
pub fn trim_decimal(value: f64, decimals: usize) -> String {
    let mut text = format!("{value:.decimals$}");
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
    fn escapes_freeform_attribute_fields() {
        for (field, escaped) in [
            ("PWR RST-1", "PWR RST-1"),
            ("PWR_RST*,A%B;", "PWR_RST\\u002A\\u002CA\\u0025B\\u003B"),
            ("R\\E\\S", "R\\u005CE\\u005CS"),
            ("\u{b5}C_RST\t", "\\u00B5C_RST\\u0009"),
            ("\u{1f600}", "\\uD83D\\uDE00"),
        ] {
            assert_eq!(escape_attribute_field(field), escaped);
            assert_eq!(unescape_attribute_field(escaped), field);
        }
        assert_eq!(escape_attribute_field(""), "_");
        // Anything short of a full escape is literal text.
        assert_eq!(unescape_attribute_field("\\u00B"), "\\u00B");
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
        let layer = stroke_layer(vec![
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
        ]);

        let output = write_layer(&layer).unwrap();
        // One move per disjoint stroke start, the region contour, and the
        // stroke after the region; the chained second draw needs none.
        assert_eq!(output.matches("D02*").count(), 4, "{output}");
        assert!(output.contains("G36*\nX3000000D02*"), "{output}");
        let parsed = crate::GerberX2::parse(&output).unwrap();
        assert_eq!(parsed.objects().len(), 5);
    }

    fn stroke_layer(objects: Vec<WriterObject>) -> GerberLayer {
        GerberLayer {
            apertures: [10, 11]
                .map(|code| WriterAperture {
                    code,
                    template: WriterApertureTemplate::Circle {
                        diameter: 0.1,
                        hole_diameter: None,
                    },
                    attributes: AttributeSets::EMPTY,
                })
                .to_vec(),
            objects,
            ..GerberLayer::default()
        }
    }

    #[test]
    fn sub_grid_arc_serializes_as_a_dot_not_a_full_circle() {
        let output = write_layer(&stroke_layer(vec![WriterObject::dark(ObjectKind::Arc {
            start: Point { x: 10.0, y: 0.0 },
            end: Point {
                x: 10.000_000_3,
                y: 0.000_000_2,
            },
            center_offset: Point { x: -5.0, y: 0.0 },
            clockwise: false,
            aperture: 10,
        })]))
        .unwrap();
        assert!(
            output.contains("X10000000Y0D02*\nG01*\nX10000000D01*\n"),
            "{output}"
        );
        let parsed = crate::GerberX2::parse(&output).unwrap();
        assert!(matches!(parsed.objects()[0].kind, ObjectKind::Draw { .. }));
    }

    #[test]
    fn zero_length_draws_survive_only_as_a_whole_stroke() {
        let point = |x: f64, y: f64| Point { x, y };
        let draw = |start, end, aperture| {
            WriterObject::dark(ObjectKind::Draw {
                start,
                end,
                aperture,
            })
        };
        let nudge = 0.000_000_3;
        let plots = |objects| {
            let output = write_layer(&stroke_layer(objects)).unwrap();
            (
                output.matches("D01*").count(),
                output.matches("D02*").count(),
            )
        };
        // Inside a polyline the neighbouring draws already image the point,
        // whether the collapsed draw leads, sits inside, or trails.
        assert_eq!(
            plots(vec![
                draw(point(0.0, 0.0), point(nudge, 0.0), 10),
                draw(point(nudge, 0.0), point(1.0, 0.0), 10),
                draw(point(1.0, 0.0), point(1.0, nudge), 10),
                draw(point(1.0, nudge), point(1.0, 1.0), 10),
                draw(point(1.0, 1.0), point(1.0, 1.0), 10),
            ]),
            (2, 1)
        );
        // A dot on its own is the whole image, as is one whose neighbour
        // draws through a different aperture.
        assert_eq!(
            plots(vec![draw(point(2.0, 2.0), point(2.0, 2.0), 10)]),
            (1, 1)
        );
        assert_eq!(
            plots(vec![
                draw(point(0.0, 0.0), point(1.0, 0.0), 10),
                draw(point(1.0, 0.0), point(1.0, 0.0), 11),
            ]),
            (2, 1)
        );
    }

    #[test]
    fn object_attributes_persist_across_objects() {
        let mut attribute_sets = AttributeSets::default();
        let mut flash = |x: f64, net: Option<&str>| {
            WriterObject::new(
                ObjectKind::Flash {
                    at: Point { x, y: 0.0 },
                    aperture: 10,
                },
                Polarity::Dark,
                attribute_sets.intern(
                    net.map(|net| AttributeValue::new(".N", [net]))
                        .into_iter()
                        .collect(),
                ),
            )
        };
        let objects = vec![
            flash(0.0, Some("GND")),
            flash(1.0, Some("GND")),
            flash(2.0, Some("V3V3")),
            flash(3.0, None),
        ];
        assert_eq!(objects[0].attributes, objects[1].attributes);
        assert_eq!(objects[3].attributes, AttributeSets::EMPTY);
        let layer = GerberLayer {
            attribute_sets,
            ..stroke_layer(objects)
        };

        let output = write_layer(&layer).unwrap();
        // The repeated net rides existing state, the changed net overrides in
        // place, and only dropping attributes resets the dictionary.
        assert_eq!(output.matches("%TO.N,GND*%").count(), 1);
        assert_eq!(output.matches("%TO.N,V3V3*%").count(), 1);
        assert_eq!(output.matches("%TD*%").count(), 1);
    }
}
