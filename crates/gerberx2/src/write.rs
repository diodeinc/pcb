use crate::types::*;
use crate::{GerberError, Result};
use pcb_ir::geom::Polarity;

/// String-backed X2 attribute used by the Gerber writer.
///
/// Attribute names should include the leading X2 dot, for example
/// `.FileFunction`, `.AperFunction`, `.N`, `.C`, or `.P`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    Macro {
        name: String,
        parameters: Vec<f64>,
    },
    Block {
        objects: Vec<WriterObject>,
    },
}

impl TryFrom<ApertureTemplate> for WriterApertureTemplate {
    type Error = GerberError;

    fn try_from(template: ApertureTemplate) -> Result<Self> {
        match template {
            ApertureTemplate::Circle {
                diameter,
                hole_diameter,
            } => Ok(Self::Circle {
                diameter,
                hole_diameter,
            }),
            ApertureTemplate::Rectangle {
                width,
                height,
                hole_diameter,
            } => Ok(Self::Rectangle {
                width,
                height,
                hole_diameter,
            }),
            ApertureTemplate::Obround {
                width,
                height,
                hole_diameter,
            } => Ok(Self::Obround {
                width,
                height,
                hole_diameter,
            }),
            ApertureTemplate::Polygon {
                outer_diameter,
                vertices,
                rotation_degrees,
                hole_diameter,
            } => Ok(Self::Polygon {
                outer_diameter,
                vertices,
                rotation_degrees,
                hole_diameter,
            }),
            ApertureTemplate::Macro { .. } | ApertureTemplate::Block { .. } => Err(
                GerberError::InvalidStructure(
                    "parsed macro/block aperture templates contain interned data; construct WriterApertureTemplate directly"
                        .to_string(),
                ),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriterApertureMacro {
    pub name: String,
    pub primitives: Vec<WriterMacroPrimitive>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriterMacroPrimitive {
    Comment(String),
    VariableDefinition {
        variable: usize,
        expression: WriterMacroExpression,
    },
    Shape {
        code: i32,
        parameters: Vec<WriterMacroExpression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriterMacroExpression {
    Number(f64),
    Variable(usize),
    UnaryMinus(Box<WriterMacroExpression>),
    Add(Box<WriterMacroExpression>, Box<WriterMacroExpression>),
    Subtract(Box<WriterMacroExpression>, Box<WriterMacroExpression>),
    Multiply(Box<WriterMacroExpression>, Box<WriterMacroExpression>),
    Divide(Box<WriterMacroExpression>, Box<WriterMacroExpression>),
}

/// One ordered graphical object plus X2 object attributes active while emitting it.
#[derive(Debug, Clone, PartialEq)]
pub struct WriterObject {
    pub kind: ObjectKind,
    pub polarity: Polarity,
    pub attributes: Vec<AttributeValue>,
}

impl WriterObject {
    pub fn dark(kind: ObjectKind) -> Self {
        Self {
            kind,
            polarity: Polarity::Dark,
            attributes: Vec::new(),
        }
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
    pub aperture_macros: Vec<WriterApertureMacro>,
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
            aperture_macros: Vec::new(),
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
}

impl<'a> Writer<'a> {
    fn new(layer: &'a GerberLayer) -> Self {
        Self {
            layer,
            output: String::new(),
            current_aperture: None,
            current_polarity: Polarity::Dark,
            current_plot_mode: None,
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

        for macro_def in &self.layer.aperture_macros {
            self.write_macro(macro_def)?;
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

        for object in &self.layer.objects {
            self.write_object(object)?;
        }

        self.output.push_str("M02*\n");
        Ok(())
    }

    fn write_macro(&mut self, macro_def: &WriterApertureMacro) -> Result<()> {
        validate_identifier(&macro_def.name, "aperture macro name")?;
        self.output.push_str("%AM");
        self.output.push_str(&macro_def.name);
        self.output.push_str("*\n");
        for primitive in &macro_def.primitives {
            match primitive {
                WriterMacroPrimitive::Comment(comment) => {
                    validate_no_command_delimiters(comment, "aperture macro comment")?;
                    self.output.push_str("0 ");
                    self.output.push_str(comment);
                    self.output.push_str("*\n");
                }
                WriterMacroPrimitive::VariableDefinition {
                    variable,
                    expression,
                } => {
                    self.output.push('$');
                    self.output.push_str(&variable.to_string());
                    self.output.push('=');
                    self.output.push_str(&format_macro_expression(expression));
                    self.output.push_str("*\n");
                }
                WriterMacroPrimitive::Shape { code, parameters } => {
                    self.output.push_str(&code.to_string());
                    for parameter in parameters {
                        self.output.push(',');
                        self.output.push_str(&format_macro_expression(parameter));
                    }
                    self.output.push_str("*\n");
                }
            }
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

        if let WriterApertureTemplate::Block { objects } = &aperture.template {
            self.write_block_aperture(aperture.code, objects)?;
            return Ok(());
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
            WriterApertureTemplate::Macro { name, parameters } => {
                validate_identifier(name, "aperture macro name")?;
                self.output.push_str(name);
                if !parameters.is_empty() {
                    self.output.push(',');
                    for (index, parameter) in parameters.iter().enumerate() {
                        if index > 0 {
                            self.output.push('X');
                        }
                        self.write_decimal(*parameter);
                    }
                }
            }
            WriterApertureTemplate::Block { .. } => {
                unreachable!("block apertures are handled above")
            }
        }
        self.output.push_str("*%\n");
        Ok(())
    }

    fn write_block_aperture(&mut self, code: i32, objects: &[WriterObject]) -> Result<()> {
        self.output.push_str(&format!("%ABD{code}*%\n"));
        let saved_aperture = self.current_aperture;
        let saved_polarity = self.current_polarity;
        let saved_plot_mode = self.current_plot_mode;
        self.current_aperture = None;
        self.current_polarity = Polarity::Dark;
        self.current_plot_mode = None;
        for object in objects {
            self.write_object(object)?;
        }
        self.output.push_str("%AB*%\n");
        self.current_aperture = saved_aperture;
        self.current_polarity = saved_polarity;
        self.current_plot_mode = saved_plot_mode;
        Ok(())
    }

    fn write_object(&mut self, object: &WriterObject) -> Result<()> {
        self.set_polarity(object.polarity);
        for attr in &object.attributes {
            self.write_attribute("TO", attr)?;
        }

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
                self.set_plot_mode(if *clockwise {
                    PlotMode::ClockwiseArc
                } else {
                    PlotMode::CounterclockwiseArc
                });
                self.write_move(*start);
                self.write_plot(*end, Some(*center_offset));
            }
            ObjectKind::Flash { at, aperture } => {
                self.set_aperture(*aperture);
                self.write_point(*at);
                self.output.push_str("D03*\n");
            }
            ObjectKind::Region { contours } => {
                self.write_region(contours)?;
            }
        }

        if !object.attributes.is_empty() {
            self.output.push_str("%TD*%\n");
        }
        Ok(())
    }

    fn write_region(&mut self, contours: &[Contour]) -> Result<()> {
        self.output.push_str("G36*\n");
        for contour in contours {
            let Some(first) = contour.segments.first() else {
                continue;
            };
            self.set_plot_mode(PlotMode::Linear);
            self.write_move(segment_start(first));
            for segment in &contour.segments {
                match *segment {
                    ContourSegment::Line { end, .. } => {
                        self.set_plot_mode(PlotMode::Linear);
                        self.write_plot(end, None);
                    }
                    ContourSegment::Arc {
                        end,
                        center_offset,
                        clockwise,
                        ..
                    } => {
                        self.set_plot_mode(if clockwise {
                            PlotMode::ClockwiseArc
                        } else {
                            PlotMode::CounterclockwiseArc
                        });
                        self.write_plot(end, Some(center_offset));
                    }
                }
            }
        }
        self.output.push_str("G37*\n");
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

    fn write_move(&mut self, point: Point) {
        self.write_point(point);
        self.output.push_str("D02*\n");
    }

    fn write_plot(&mut self, point: Point, center_offset: Option<Point>) {
        self.write_point(point);
        if let Some(center_offset) = center_offset {
            self.output.push('I');
            self.output
                .push_str(&self.coordinate(center_offset.x, true));
            self.output.push('J');
            self.output
                .push_str(&self.coordinate(center_offset.y, false));
        }
        self.output.push_str("D01*\n");
    }

    fn write_point(&mut self, point: Point) {
        self.output.push('X');
        self.output.push_str(&self.coordinate(point.x, true));
        self.output.push('Y');
        self.output.push_str(&self.coordinate(point.y, false));
    }

    fn coordinate(&self, value: f64, x_axis: bool) -> String {
        let decimals = if x_axis {
            self.layer.coordinate_format.x_decimal_digits
        } else {
            self.layer.coordinate_format.y_decimal_digits
        };
        let scale = 10_f64.powi(decimals as i32);
        format!("{:.0}", value * scale)
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

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty() {
        return Err(GerberError::InvalidStructure(format!(
            "{label} must not be empty"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
    {
        return Err(GerberError::InvalidStructure(format!(
            "{label} '{value}' contains characters that are not safe in Gerber identifiers"
        )));
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

fn format_macro_expression(expression: &WriterMacroExpression) -> String {
    match expression {
        WriterMacroExpression::Number(value) => trim_decimal(*value),
        WriterMacroExpression::Variable(index) => format!("${index}"),
        WriterMacroExpression::UnaryMinus(inner) => format!("-{}", format_macro_factor(inner)),
        WriterMacroExpression::Add(left, right) => {
            format!(
                "{}+{}",
                format_macro_expression(left),
                format_macro_term(right)
            )
        }
        WriterMacroExpression::Subtract(left, right) => {
            format!(
                "{}-{}",
                format_macro_expression(left),
                format_macro_term(right)
            )
        }
        WriterMacroExpression::Multiply(left, right) => {
            format!("{}x{}", format_macro_term(left), format_macro_factor(right))
        }
        WriterMacroExpression::Divide(left, right) => {
            format!("{}/{}", format_macro_term(left), format_macro_factor(right))
        }
    }
}

fn format_macro_term(expression: &WriterMacroExpression) -> String {
    match expression {
        WriterMacroExpression::Add(..) | WriterMacroExpression::Subtract(..) => {
            format!("({})", format_macro_expression(expression))
        }
        _ => format_macro_expression(expression),
    }
}

fn format_macro_factor(expression: &WriterMacroExpression) -> String {
    match expression {
        WriterMacroExpression::Number(_) | WriterMacroExpression::Variable(_) => {
            format_macro_expression(expression)
        }
        _ => format!("({})", format_macro_expression(expression)),
    }
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
}
