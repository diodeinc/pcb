use crate::types::Mirroring;
use crate::types::*;
use crate::{GerberError, GerberX2, Interner, Result, Symbol};
use pcb_ir::geom::{Polarity, Span};
use std::collections::HashMap;

pub struct Parser<'a> {
    source: &'a str,
    pos: usize,
    interner: Interner,
    file_attributes: Vec<Attribute>,
    aperture_attributes: AttributeDictionary,
    object_attributes: AttributeDictionary,
    /// Arena of the attribute sets objects and apertures refer to.
    attributes: Vec<Attribute>,
    aperture_definitions: Vec<ApertureDefinition>,
    aperture_lookup: HashMap<i32, usize>,
    macro_lookup: HashMap<Symbol, ApertureMacro>,
    objects: Vec<GraphicalObject>,
    step_repeats: Vec<StepRepeatBlock>,
    region: Option<RegionBuilder>,
    block: Option<BlockBuilder>,
    step_repeat: Option<StepRepeatBuilder>,
    state: GraphicsState,
    saw_m02: bool,
}

/// One X2 attribute dictionary. A handful of entries at most, so a vector in
/// insertion order is both faster than hashing and deterministic.
#[derive(Debug, Default)]
struct AttributeDictionary {
    entries: Vec<Attribute>,
    /// The arena copy of `entries`, until the dictionary next changes.
    set: Option<Span>,
}

impl AttributeDictionary {
    fn insert(&mut self, attribute: Attribute) {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.name == attribute.name)
        {
            Some(entry) => *entry = attribute,
            None => self.entries.push(attribute),
        }
        self.set = None;
    }

    /// Delete one attribute, or all of them.
    fn remove(&mut self, name: Option<Symbol>) {
        self.entries
            .retain(|entry| name.is_some_and(|name| entry.name != name));
        self.set = None;
    }

    /// The current entries as a set in `arena`, copied once per change.
    fn set(&mut self, arena: &mut Vec<Attribute>) -> Span {
        *self.set.get_or_insert_with(|| {
            let set = Span::new(arena.len() as u32, self.entries.len() as u32);
            arena.extend_from_slice(&self.entries);
            set
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ApertureMacro {
    name: Symbol,
    primitives: Vec<MacroPrimitive>,
}

#[derive(Debug, Clone, PartialEq)]
enum MacroPrimitive {
    VariableDefinition {
        variable: usize,
        expression: MacroExpression,
    },
    Shape {
        code: i32,
        parameters: Vec<MacroExpression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum MacroExpression {
    Number(f64),
    Variable(usize),
    UnaryMinus(Box<MacroExpression>),
    Add(Box<MacroExpression>, Box<MacroExpression>),
    Subtract(Box<MacroExpression>, Box<MacroExpression>),
    Multiply(Box<MacroExpression>, Box<MacroExpression>),
    Divide(Box<MacroExpression>, Box<MacroExpression>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationCode {
    Plot,
    Move,
    Flash,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CoordinateFields {
    x: Option<i64>,
    y: Option<i64>,
    i: Option<i64>,
    j: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
struct GraphicsState {
    unit: Option<Unit>,
    coordinate_format: Option<CoordinateFormat>,
    current_point: Option<Point>,
    current_aperture: Option<i32>,
    plot_mode: Option<PlotMode>,
    polarity: Polarity,
    mirroring: Mirroring,
    rotation_degrees: f64,
    scaling: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            unit: None,
            coordinate_format: None,
            current_point: None,
            current_aperture: None,
            plot_mode: None,
            polarity: Polarity::Dark,
            mirroring: Mirroring::None,
            rotation_degrees: 0.0,
            scaling: 1.0,
        }
    }
}

#[derive(Debug, Default)]
struct RegionBuilder {
    contours: Vec<Contour>,
    current: Option<Contour>,
}

#[derive(Debug)]
struct BlockBuilder {
    aperture_code: i32,
    object_start: usize,
}

#[derive(Debug)]
struct StepRepeatBuilder {
    repeat: StepRepeat,
    object_start: usize,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            interner: Interner::new(),
            file_attributes: Vec::new(),
            aperture_attributes: AttributeDictionary::default(),
            object_attributes: AttributeDictionary::default(),
            attributes: Vec::new(),
            aperture_definitions: Vec::new(),
            aperture_lookup: HashMap::new(),
            macro_lookup: HashMap::new(),
            objects: Vec::new(),
            step_repeats: Vec::new(),
            region: None,
            block: None,
            step_repeat: None,
            state: GraphicsState::default(),
            saw_m02: false,
        }
    }

    pub fn parse(&mut self) -> Result<GerberX2> {
        while self.skip_line_breaks() {
            if self.saw_m02 {
                return Err(self.syntax("data after M02 end-of-file command"));
            }

            if self.current_byte() == Some(b'%') {
                let command = self.read_extended_command()?;
                self.parse_extended_command(command)?;
            } else {
                let command = self.read_word_command()?;
                self.parse_word_command(command)?;
            }
        }

        if !self.saw_m02 {
            return Err(GerberError::InvalidStructure(
                "missing required M02 end-of-file command".to_string(),
            ));
        }
        if self.region.is_some() {
            return Err(GerberError::InvalidStructure(
                "G36 region was not closed before M02".to_string(),
            ));
        }
        if self.block.is_some() {
            return Err(GerberError::InvalidStructure(
                "AB block aperture was not closed before M02".to_string(),
            ));
        }
        if self.step_repeat.is_some() {
            return Err(GerberError::InvalidStructure(
                "SR step-repeat was not closed before M02".to_string(),
            ));
        }

        Ok(GerberX2 {
            interner: std::mem::take(&mut self.interner),
            file_attributes: std::mem::take(&mut self.file_attributes),
            attributes: std::mem::take(&mut self.attributes),
            aperture_definitions: std::mem::take(&mut self.aperture_definitions),
            objects: std::mem::take(&mut self.objects),
            step_repeats: std::mem::take(&mut self.step_repeats),
        })
    }

    fn skip_line_breaks(&mut self) -> bool {
        while matches!(self.current_byte(), Some(b'\n' | b'\r' | b'\t' | b' ')) {
            self.pos += 1;
        }
        self.pos < self.source.len()
    }

    fn current_byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.pos).copied()
    }

    fn read_extended_command(&mut self) -> Result<&'a str> {
        let start = self.pos;
        self.pos += 1;
        while self.pos < self.source.len() && self.current_byte() != Some(b'%') {
            self.pos += 1;
        }
        if self.current_byte() != Some(b'%') {
            return Err(self.syntax("unterminated extended command"));
        }
        self.pos += 1;
        Ok(&self.source[start + 1..self.pos - 1])
    }

    fn read_word_command(&mut self) -> Result<&'a str> {
        let start = self.pos;
        while self.pos < self.source.len() && self.current_byte() != Some(b'*') {
            if self.current_byte() == Some(b'%') {
                return Err(self.syntax("unexpected '%' in word command"));
            }
            self.pos += 1;
        }
        if self.current_byte() != Some(b'*') {
            return Err(self.syntax("unterminated word command"));
        }
        self.pos += 1;
        Ok(&self.source[start..self.pos])
    }

    fn parse_extended_command(&mut self, command: &'a str) -> Result<()> {
        if command.starts_with("AM") {
            return self.parse_extended_word(command.trim_end_matches('*'));
        }
        for word in command.split_terminator('*') {
            if word.is_empty() {
                continue;
            }
            self.parse_extended_word(word)?;
        }
        Ok(())
    }

    fn parse_extended_word(&mut self, word: &'a str) -> Result<()> {
        if let Some(rest) = word.strip_prefix("MO") {
            let unit = match rest {
                "MM" => Unit::Millimeter,
                "IN" => Unit::Inch,
                _ => return Err(self.syntax(format!("invalid MO unit '{rest}'"))),
            };
            self.state.unit = Some(unit);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("FS") {
            let format = parse_format(rest).ok_or_else(|| self.syntax("invalid FS command"))?;
            self.state.coordinate_format = Some(format);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AD") {
            let aperture = self.parse_aperture_definition(rest)?;
            self.aperture_lookup
                .insert(aperture.code, self.aperture_definitions.len());
            self.aperture_definitions.push(aperture);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AM") {
            let macro_def = self.parse_aperture_macro(rest)?;
            self.macro_lookup.insert(macro_def.name, macro_def);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LP") {
            let polarity = match rest {
                "D" => Polarity::Dark,
                "C" => Polarity::Clear,
                _ => return Err(self.syntax(format!("invalid LP polarity '{rest}'"))),
            };
            self.state.polarity = polarity;
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LM") {
            let mirroring = match rest {
                "N" => Mirroring::None,
                "X" => Mirroring::X,
                "Y" => Mirroring::Y,
                "XY" => Mirroring::XY,
                _ => return Err(self.syntax(format!("invalid LM mirroring '{rest}'"))),
            };
            self.state.mirroring = mirroring;
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LR") {
            let rotation = parse_f64(rest)?;
            self.state.rotation_degrees = rotation;
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LS") {
            let scaling = parse_f64(rest)?;
            self.state.scaling = scaling;
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AB") {
            if rest.is_empty() {
                let block = self
                    .block
                    .take()
                    .ok_or_else(|| self.syntax("AB close without matching AB open"))?;
                let objects = self.objects.split_off(block.object_start);
                let aperture = ApertureDefinition {
                    code: block.aperture_code,
                    template: ApertureTemplate::Block { objects },
                    geometry: None,
                    attributes: self.aperture_attributes.set(&mut self.attributes),
                };
                self.aperture_lookup
                    .insert(aperture.code, self.aperture_definitions.len());
                self.aperture_definitions.push(aperture);
            } else {
                let code = parse_aperture_code(rest)?;
                if self.block.is_some() {
                    return Err(self.syntax("nested AB block apertures are not supported"));
                }
                self.block = Some(BlockBuilder {
                    aperture_code: code,
                    object_start: self.objects.len(),
                });
            }
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("SR") {
            if self.block.is_some() {
                return Err(self.syntax("SR is not allowed inside an AB block aperture"));
            }
            if rest.is_empty() {
                let step = self
                    .step_repeat
                    .take()
                    .ok_or_else(|| self.syntax("SR close without matching SR open"))?;
                let objects = Span::new(
                    step.object_start as u32,
                    (self.objects.len() - step.object_start) as u32,
                );
                // A single occurrence is the run itself.
                if !objects.is_empty() && (step.repeat.x_repeats > 1 || step.repeat.y_repeats > 1) {
                    self.step_repeats.push(StepRepeatBlock {
                        repeat: step.repeat,
                        objects,
                    });
                }
            } else {
                let sr = parse_step_repeat(rest)?;
                if self.step_repeat.is_some() {
                    return Err(self.syntax("nested SR statements are not supported"));
                }
                self.step_repeat = Some(StepRepeatBuilder {
                    repeat: sr,
                    object_start: self.objects.len(),
                });
            }
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TF") {
            let attr = self.parse_attribute(rest)?;
            self.file_attributes.push(attr.clone());
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TA") {
            let attr = self.parse_attribute(rest)?;
            self.aperture_attributes.insert(attr.clone());
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TO") {
            let attr = self.parse_attribute(rest)?;
            self.object_attributes.insert(attr.clone());
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TD") {
            let name = (!rest.is_empty()).then(|| self.interner.intern(rest));
            self.aperture_attributes.remove(name);
            self.object_attributes.remove(name);
            return Ok(());
        }

        Err(self.syntax(format!("unsupported extended command '{word}'")))
    }

    fn parse_word_command(&mut self, command: &'a str) -> Result<()> {
        let word = command.strip_suffix('*').unwrap_or(command);
        if word.starts_with("G04") {
            return Ok(());
        }

        match word {
            "G01" => {
                self.state.plot_mode = Some(PlotMode::Linear);
                return Ok(());
            }
            "G02" => {
                self.state.plot_mode = Some(PlotMode::ClockwiseArc);
                return Ok(());
            }
            "G03" => {
                self.state.plot_mode = Some(PlotMode::CounterclockwiseArc);
                return Ok(());
            }
            "G75" => {
                return Ok(());
            }
            "G36" => {
                if self.region.is_some() {
                    return Err(self.syntax("nested region statements are not allowed"));
                }
                self.region = Some(RegionBuilder::default());
                return Ok(());
            }
            "G37" => {
                let mut region = self
                    .region
                    .take()
                    .ok_or_else(|| self.syntax("G37 without matching G36"))?;
                if let Some(contour) = region.current.take() {
                    region.contours.push(contour);
                }
                if region.contours.is_empty() {
                    return Err(self.syntax("empty region statement"));
                }
                validate_region_contours(&region.contours)?;
                self.push_object(ObjectKind::Region {
                    contours: region.contours,
                });
                return Ok(());
            }
            "M02" => {
                self.saw_m02 = true;
                return Ok(());
            }
            _ => {}
        }

        if let Some(code) = parse_set_aperture(word) {
            self.state.current_aperture = Some(code);
            return Ok(());
        }

        let (fields, code) = parse_operation(word)?;
        self.interpret_operation(fields, code)?;
        Ok(())
    }

    fn interpret_operation(&mut self, fields: CoordinateFields, code: OperationCode) -> Result<()> {
        let point = self.operation_point(fields)?;
        match code {
            OperationCode::Move => {
                if let Some(region) = &mut self.region {
                    if let Some(contour) = region.current.take() {
                        region.contours.push(contour);
                    }
                    region.current = Some(Contour {
                        segments: Vec::new(),
                    });
                }
                self.state.current_point = Some(point);
            }
            OperationCode::Flash => {
                if self.region.is_some() {
                    return Err(self.syntax("D03 flash is not allowed inside a region"));
                }
                let aperture = self.current_aperture()?;
                self.push_object(ObjectKind::Flash {
                    at: point,
                    aperture,
                });
                self.state.current_point = Some(point);
            }
            OperationCode::Plot => {
                let start = self
                    .state
                    .current_point
                    .ok_or_else(|| self.syntax("D01 plot requires a current point"))?;
                let plot_mode = self
                    .state
                    .plot_mode
                    .ok_or_else(|| self.syntax("D01 plot requires G01/G02/G03 plot mode"))?;
                let segment = match plot_mode {
                    PlotMode::Linear => ContourSegment::Line { start, end: point },
                    PlotMode::ClockwiseArc | PlotMode::CounterclockwiseArc => {
                        let center_offset = self.coordinate_offset(fields)?;
                        ContourSegment::Arc {
                            start,
                            end: point,
                            center_offset,
                            clockwise: plot_mode == PlotMode::ClockwiseArc,
                        }
                    }
                };
                if let Some(region) = &mut self.region {
                    let Some(contour) = region.current.as_mut() else {
                        return Err(GerberError::Syntax {
                            offset: self.pos,
                            message: "region D01 must follow D02 contour start".to_string(),
                        });
                    };
                    contour.segments.push(segment);
                } else {
                    let aperture = self.current_aperture()?;
                    let kind = match segment {
                        ContourSegment::Line { start, end } => ObjectKind::Draw {
                            start,
                            end,
                            aperture,
                        },
                        ContourSegment::Arc {
                            start,
                            end,
                            center_offset,
                            clockwise,
                        } => ObjectKind::Arc {
                            start,
                            end,
                            center_offset,
                            clockwise,
                            aperture,
                        },
                    };
                    self.push_object(kind);
                }
                self.state.current_point = Some(point);
            }
        }
        Ok(())
    }

    fn operation_point(&self, fields: CoordinateFields) -> Result<Point> {
        let current = self.state.current_point;
        let x = match fields.x {
            Some(x) => self.decode_x(x)?,
            None => current
                .map(|point| point.x)
                .ok_or_else(|| self.syntax("modal X coordinate requires a current point"))?,
        };
        let y = match fields.y {
            Some(y) => self.decode_y(y)?,
            None => current
                .map(|point| point.y)
                .ok_or_else(|| self.syntax("modal Y coordinate requires a current point"))?,
        };
        Ok(Point { x, y })
    }

    fn coordinate_offset(&self, fields: CoordinateFields) -> Result<Point> {
        let i = fields
            .i
            .ok_or_else(|| self.syntax("arc D01 requires I offset"))?;
        let j = fields
            .j
            .ok_or_else(|| self.syntax("arc D01 requires J offset"))?;
        Ok(Point {
            x: self.decode_x(i)?,
            y: self.decode_y(j)?,
        })
    }

    fn decode_x(&self, value: i64) -> Result<f64> {
        let format = self.coordinate_format()?;
        Ok(scale_coordinate(
            value,
            format.x_decimal_digits,
            self.unit()?,
        ))
    }

    fn decode_y(&self, value: i64) -> Result<f64> {
        let format = self.coordinate_format()?;
        Ok(scale_coordinate(
            value,
            format.y_decimal_digits,
            self.unit()?,
        ))
    }

    fn unit(&self) -> Result<Unit> {
        self.state
            .unit
            .ok_or_else(|| self.syntax("operation requires MO unit command first"))
    }

    fn coordinate_format(&self) -> Result<CoordinateFormat> {
        self.state
            .coordinate_format
            .ok_or_else(|| self.syntax("operation requires FS coordinate format first"))
    }

    fn current_aperture(&self) -> Result<i32> {
        self.state
            .current_aperture
            .ok_or_else(|| self.syntax("operation requires current aperture"))
    }

    /// Append an object imaged under the current graphics state.
    fn push_object(&mut self, kind: ObjectKind) {
        let aperture_attributes = match &kind {
            ObjectKind::Draw { aperture, .. }
            | ObjectKind::Arc { aperture, .. }
            | ObjectKind::Flash { aperture, .. } => self
                .aperture_lookup
                .get(aperture)
                .and_then(|&index| self.aperture_definitions.get(index))
                .map_or(Span::EMPTY, |definition| definition.attributes),
            ObjectKind::Region { .. } => self.aperture_attributes.set(&mut self.attributes),
        };
        let object_attributes = self.object_attributes.set(&mut self.attributes);
        self.objects.push(GraphicalObject {
            kind,
            polarity: self.state.polarity,
            mirroring: self.state.mirroring,
            rotation_degrees: self.state.rotation_degrees,
            scaling: self.state.scaling,
            aperture_attributes,
            object_attributes,
        });
    }

    fn parse_attribute(&mut self, rest: &str) -> Result<Attribute> {
        let mut fields = rest.split(',');
        let Some(name) = fields.next().filter(|name| !name.is_empty()) else {
            return Err(self.syntax("attribute missing name"));
        };
        Ok(Attribute {
            name: self.interner.intern(name),
            fields: fields.map(|field| self.interner.intern(field)).collect(),
        })
    }

    fn parse_aperture_definition(&mut self, rest: &str) -> Result<ApertureDefinition> {
        let rest = rest.strip_prefix('D').unwrap_or(rest);
        let d_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if d_len == 0 {
            return Err(self.syntax("AD missing aperture code"));
        }
        let code = parse_aperture_code(&rest[..d_len])?;
        let template_call = &rest[d_len..];
        let unit = self.unit()?;
        let template = self.parse_template_call(template_call, unit)?;
        let geometry = self.lower_aperture(&template, unit)?;
        Ok(ApertureDefinition {
            code,
            template,
            geometry,
            attributes: self.aperture_attributes.set(&mut self.attributes),
        })
    }

    fn lower_aperture(
        &self,
        template: &ApertureTemplate,
        unit: Unit,
    ) -> Result<Option<ApertureGeometry>> {
        if let ApertureTemplate::Macro { name, parameters } = template {
            let Some(macro_def) = self.macro_lookup.get(name) else {
                return Err(GerberError::InvalidStructure(format!(
                    "aperture macro '{}' was not defined before use",
                    self.interner.resolve(*name)
                )));
            };
            return lower_macro_aperture(macro_def, parameters, unit);
        }
        Ok(lower_standard_aperture(template))
    }

    fn parse_template_call(&mut self, template_call: &str, unit: Unit) -> Result<ApertureTemplate> {
        let (name, params) = template_call
            .split_once(',')
            .map(|(name, params)| (name, params.split('X').collect::<Vec<_>>()))
            .unwrap_or((template_call, Vec::new()));
        let values = params
            .into_iter()
            .map(parse_f64)
            .collect::<Result<Vec<_>>>()?;

        match name {
            "C" => Ok(ApertureTemplate::Circle {
                diameter: scale_length(required_param(&values, 0, "circle diameter")?, unit),
                hole_diameter: values
                    .get(1)
                    .copied()
                    .map(|value| scale_length(value, unit)),
            }),
            "R" => Ok(ApertureTemplate::Rectangle {
                width: scale_length(required_param(&values, 0, "rectangle width")?, unit),
                height: scale_length(required_param(&values, 1, "rectangle height")?, unit),
                hole_diameter: values
                    .get(2)
                    .copied()
                    .map(|value| scale_length(value, unit)),
            }),
            "O" => Ok(ApertureTemplate::Obround {
                width: scale_length(required_param(&values, 0, "obround width")?, unit),
                height: scale_length(required_param(&values, 1, "obround height")?, unit),
                hole_diameter: values
                    .get(2)
                    .copied()
                    .map(|value| scale_length(value, unit)),
            }),
            "P" => Ok(ApertureTemplate::Polygon {
                outer_diameter: scale_length(
                    required_param(&values, 0, "polygon outer diameter")?,
                    unit,
                ),
                vertices: bounded_count(
                    required_param(&values, 1, "polygon vertices")?,
                    "polygon vertices",
                    POLYGON_VERTICES,
                )? as i32,
                rotation_degrees: values.get(2).copied(),
                hole_diameter: values
                    .get(3)
                    .copied()
                    .map(|value| scale_length(value, unit)),
            }),
            _ => Ok(ApertureTemplate::Macro {
                name: self.interner.intern(name),
                parameters: values,
            }),
        }
    }

    fn parse_aperture_macro(&mut self, rest: &str) -> Result<ApertureMacro> {
        let Some((name, body)) = rest.split_once('*') else {
            return Err(self.syntax("AM missing body"));
        };
        let mut primitives = Vec::new();
        // Primitive code 0 is a comment.
        for word in body
            .split_terminator('*')
            .map(str::trim)
            .filter(|word| !word.is_empty() && !word.starts_with('0'))
        {
            if let Some((variable, expression)) = word.split_once('=') {
                let variable = variable
                    .strip_prefix('$')
                    .ok_or_else(|| self.syntax("macro variable definition missing $ prefix"))?
                    .parse::<usize>()
                    .map_err(|_| GerberError::InvalidNumber(variable.to_string()))?;
                let mut parser = MacroExpressionParser::new(expression);
                primitives.push(MacroPrimitive::VariableDefinition {
                    variable,
                    expression: parser.parse()?,
                });
            } else {
                let mut fields = word.split(',');
                let code = fields
                    .next()
                    .ok_or_else(|| self.syntax("macro primitive missing code"))?
                    .parse::<i32>()
                    .map_err(|_| GerberError::InvalidNumber(word.to_string()))?;
                let parameters = fields
                    .map(|field| MacroExpressionParser::new(field).parse())
                    .collect::<Result<Vec<_>>>()?;
                primitives.push(MacroPrimitive::Shape { code, parameters });
            }
        }
        Ok(ApertureMacro {
            name: self.interner.intern(name),
            primitives,
        })
    }

    fn syntax(&self, message: impl Into<String>) -> GerberError {
        GerberError::Syntax {
            offset: self.pos,
            message: message.into(),
        }
    }
}

fn parse_format(rest: &str) -> Option<CoordinateFormat> {
    let rest = rest.strip_prefix("LA")?;
    let rest = rest.strip_prefix('X')?;
    let mut chars = rest.chars();
    let x_integer_digits = chars.next()?.to_digit(10)? as u8;
    let x_decimal_digits = chars.next()?.to_digit(10)? as u8;
    let rest = chars.as_str().strip_prefix('Y')?;
    let mut chars = rest.chars();
    let y_integer_digits = chars.next()?.to_digit(10)? as u8;
    let y_decimal_digits = chars.next()?.to_digit(10)? as u8;
    if !chars.as_str().is_empty() {
        return None;
    }
    Some(CoordinateFormat {
        x_integer_digits,
        x_decimal_digits,
        y_integer_digits,
        y_decimal_digits,
    })
}

fn scale_coordinate(value: i64, decimal_digits: u8, unit: Unit) -> f64 {
    let value = value as f64 / 10_f64.powi(decimal_digits as i32);
    scale_length(value, unit)
}

fn scale_length(value: f64, unit: Unit) -> f64 {
    match unit {
        Unit::Millimeter => value,
        Unit::Inch => value * 25.4,
    }
}

fn lower_standard_aperture(template: &ApertureTemplate) -> Option<ApertureGeometry> {
    let paths = match *template {
        ApertureTemplate::Circle {
            diameter,
            hole_diameter,
        } => circle_paths(diameter, hole_diameter),
        ApertureTemplate::Rectangle {
            width,
            height,
            hole_diameter,
        } => rect_paths(width, height, hole_diameter),
        ApertureTemplate::Obround {
            width,
            height,
            hole_diameter,
        } => obround_paths(width, height, hole_diameter),
        ApertureTemplate::Polygon {
            outer_diameter,
            vertices,
            rotation_degrees,
            hole_diameter,
        } => polygon_paths(
            outer_diameter,
            vertices,
            rotation_degrees.unwrap_or(0.0),
            hole_diameter,
        ),
        ApertureTemplate::Macro { .. } | ApertureTemplate::Block { .. } => return None,
    };
    Some(ApertureGeometry { paths })
}

fn lower_macro_aperture(
    macro_def: &ApertureMacro,
    parameters: &[f64],
    unit: Unit,
) -> Result<Option<ApertureGeometry>> {
    let mut vars: HashMap<usize, f64> = parameters
        .iter()
        .enumerate()
        .map(|(index, value)| (index + 1, *value))
        .collect();
    let mut paths = Vec::new();
    for primitive in &macro_def.primitives {
        match primitive {
            MacroPrimitive::VariableDefinition {
                variable,
                expression,
            } => {
                vars.insert(*variable, eval_macro_expr(expression, &vars)?);
            }
            MacroPrimitive::Shape { code, parameters } => {
                let values = parameters
                    .iter()
                    .map(|expr| eval_macro_expr(expr, &vars))
                    .collect::<Result<Vec<_>>>()?;
                paths.extend(lower_macro_shape(*code, &values, unit)?);
            }
        }
    }
    Ok(Some(ApertureGeometry { paths }))
}

/// Lower one macro primitive. Every primitive is built unrotated at its own
/// position and then rotated about the macro origin, as the format specifies.
fn lower_macro_shape(code: i32, values: &[f64], unit: Unit) -> Result<Vec<GeometryPath>> {
    let (paths, rotation) = match code {
        1 => {
            let exposure = macro_bool(values, 0)?;
            let diameter = macro_length(values, 1, "macro circle diameter", unit)?;
            let center = Point {
                x: macro_length(values, 2, "macro circle center x", unit)?,
                y: macro_length(values, 3, "macro circle center y", unit)?,
            };
            let rotation = values.get(4).copied().unwrap_or(0.0);
            (
                vec![translate_path(
                    circle_path(diameter / 2.0, exposure),
                    center,
                )],
                rotation,
            )
        }
        20 => {
            let exposure = macro_bool(values, 0)?;
            let width = macro_length(values, 1, "macro vector line width", unit)?;
            let start = Point {
                x: macro_length(values, 2, "macro vector line start x", unit)?,
                y: macro_length(values, 3, "macro vector line start y", unit)?,
            };
            let end = Point {
                x: macro_length(values, 4, "macro vector line end x", unit)?,
                y: macro_length(values, 5, "macro vector line end y", unit)?,
            };
            let rotation = macro_value(values, 6, "macro vector line rotation")?;
            (
                vec![vector_line_path(start, end, width, exposure)],
                rotation,
            )
        }
        21 => {
            let exposure = macro_bool(values, 0)?;
            let width = macro_length(values, 1, "macro center line width", unit)?;
            let height = macro_length(values, 2, "macro center line height", unit)?;
            let center = Point {
                x: macro_length(values, 3, "macro center line x", unit)?,
                y: macro_length(values, 4, "macro center line y", unit)?,
            };
            let rotation = macro_value(values, 5, "macro center line rotation")?;
            (
                vec![translate_path(rect_path(width, height, exposure), center)],
                rotation,
            )
        }
        4 => {
            let exposure = macro_bool(values, 0)?;
            let vertices = bounded_count(
                macro_value(values, 1, "macro outline vertices")?,
                "macro outline vertices",
                3..=OUTLINE_MAX_VERTICES,
            )?;
            let expected = 2 + (vertices + 1) * 2 + 1;
            if values.len() != expected {
                return Err(GerberError::InvalidStructure(
                    "macro outline has the wrong number of parameters".to_string(),
                ));
            }
            let point = |index: usize| Point {
                x: scale_length(values[2 + index * 2], unit),
                y: scale_length(values[3 + index * 2], unit),
            };
            if !points_close(point(0), point(vertices)) {
                return Err(GerberError::InvalidStructure(
                    "macro outline last vertex must equal first vertex".to_string(),
                ));
            }
            let commands = std::iter::once(PathCommand::MoveTo(point(0)))
                .chain((1..=vertices).map(|index| PathCommand::LineTo(point(index))))
                .chain(std::iter::once(PathCommand::Close))
                .collect();
            (
                vec![GeometryPath {
                    contours: vec![GeometryContour { commands }],
                    polarity: exposure,
                }],
                values[expected - 1],
            )
        }
        5 => {
            let exposure = macro_bool(values, 0)?;
            let vertices = bounded_count(
                macro_value(values, 1, "macro polygon vertices")?,
                "macro polygon vertices",
                POLYGON_VERTICES,
            )? as i32;
            let center = Point {
                x: macro_length(values, 2, "macro polygon center x", unit)?,
                y: macro_length(values, 3, "macro polygon center y", unit)?,
            };
            let diameter = macro_length(values, 4, "macro polygon diameter", unit)?;
            let rotation = macro_value(values, 5, "macro polygon rotation")?;
            (
                polygon_paths(diameter, vertices, 0.0, None)
                    .into_iter()
                    .map(|path| translate_path(repolarity(path, exposure), center))
                    .collect(),
                rotation,
            )
        }
        7 => {
            let center = Point {
                x: macro_length(values, 0, "macro thermal center x", unit)?,
                y: macro_length(values, 1, "macro thermal center y", unit)?,
            };
            let outer = macro_length(values, 2, "macro thermal outer diameter", unit)?;
            let inner = macro_length(values, 3, "macro thermal inner diameter", unit)?;
            let gap = macro_length(values, 4, "macro thermal gap", unit)?;
            let rotation = macro_value(values, 5, "macro thermal rotation")?;
            let mut paths = circle_paths(outer, Some(inner));
            paths.push(rect_path(outer, gap, Polarity::Clear));
            paths.push(rect_path(gap, outer, Polarity::Clear));
            (
                paths
                    .into_iter()
                    .map(|path| translate_path(path, center))
                    .collect(),
                rotation,
            )
        }
        _ => {
            return Err(GerberError::InvalidStructure(format!(
                "unsupported aperture macro primitive {code}"
            )));
        }
    };
    Ok(paths
        .into_iter()
        .map(|path| map_path(path, |point| rotate_point(point, rotation)))
        .collect())
}

fn validate_region_contours(contours: &[Contour]) -> Result<()> {
    for contour in contours {
        if contour.segments.is_empty() {
            return Err(GerberError::InvalidStructure(
                "region contour has no segments".to_string(),
            ));
        }
        let mut first = None;
        let mut previous = None;
        for segment in &contour.segments {
            let (start, end) = match *segment {
                ContourSegment::Line { start, end } | ContourSegment::Arc { start, end, .. } => {
                    (start, end)
                }
            };
            if let Some(previous) = previous
                && !points_close(previous, start)
            {
                return Err(GerberError::InvalidStructure(
                    "region contour segments must be connected".to_string(),
                ));
            }
            first.get_or_insert(start);
            previous = Some(end);
        }
        if !points_close(first.unwrap(), previous.unwrap()) {
            return Err(GerberError::InvalidStructure(
                "region contour must be closed".to_string(),
            ));
        }
    }
    Ok(())
}

fn points_close(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= 1e-9 && (a.y - b.y).abs() <= 1e-9
}

fn circle_paths(diameter: f64, hole_diameter: Option<f64>) -> Vec<GeometryPath> {
    let mut paths = Vec::new();
    if diameter > 0.0 {
        paths.push(circle_path(diameter / 2.0, Polarity::Dark));
    }
    if let Some(hole_diameter) = hole_diameter
        && hole_diameter > 0.0
    {
        paths.push(circle_path(hole_diameter / 2.0, Polarity::Clear));
    }
    paths
}

fn rect_paths(width: f64, height: f64, hole_diameter: Option<f64>) -> Vec<GeometryPath> {
    let mut paths = vec![rect_path(width, height, Polarity::Dark)];
    if let Some(hole_diameter) = hole_diameter
        && hole_diameter > 0.0
    {
        paths.push(circle_path(hole_diameter / 2.0, Polarity::Clear));
    }
    paths
}

fn obround_paths(width: f64, height: f64, hole_diameter: Option<f64>) -> Vec<GeometryPath> {
    let mut paths = Vec::new();
    let rx = width / 2.0;
    let ry = height / 2.0;
    let commands = if width >= height {
        let r = ry;
        let cx = rx - r;
        vec![
            PathCommand::MoveTo(Point { x: -cx, y: -r }),
            PathCommand::LineTo(Point { x: cx, y: -r }),
            PathCommand::ArcTo {
                end: Point { x: cx, y: r },
                center: Point { x: cx, y: 0.0 },
                clockwise: false,
            },
            PathCommand::LineTo(Point { x: -cx, y: r }),
            PathCommand::ArcTo {
                end: Point { x: -cx, y: -r },
                center: Point { x: -cx, y: 0.0 },
                clockwise: false,
            },
            PathCommand::Close,
        ]
    } else {
        let r = rx;
        let cy = ry - r;
        vec![
            PathCommand::MoveTo(Point { x: r, y: -cy }),
            PathCommand::LineTo(Point { x: r, y: cy }),
            PathCommand::ArcTo {
                end: Point { x: -r, y: cy },
                center: Point { x: 0.0, y: cy },
                clockwise: false,
            },
            PathCommand::LineTo(Point { x: -r, y: -cy }),
            PathCommand::ArcTo {
                end: Point { x: r, y: -cy },
                center: Point { x: 0.0, y: -cy },
                clockwise: false,
            },
            PathCommand::Close,
        ]
    };
    paths.push(GeometryPath {
        contours: vec![GeometryContour { commands }],
        polarity: Polarity::Dark,
    });
    if let Some(hole_diameter) = hole_diameter
        && hole_diameter > 0.0
    {
        paths.push(circle_path(hole_diameter / 2.0, Polarity::Clear));
    }
    paths
}

fn polygon_paths(
    outer_diameter: f64,
    vertices: i32,
    rotation_degrees: f64,
    hole_diameter: Option<f64>,
) -> Vec<GeometryPath> {
    let radius = outer_diameter / 2.0;
    let rotation = rotation_degrees.to_radians();
    let vertex = |index: i32| {
        let angle = rotation + index as f64 * std::f64::consts::TAU / vertices as f64;
        Point {
            x: radius * angle.cos(),
            y: radius * angle.sin(),
        }
    };
    let commands = std::iter::once(PathCommand::MoveTo(vertex(0)))
        .chain((1..vertices).map(|index| PathCommand::LineTo(vertex(index))))
        .chain(std::iter::once(PathCommand::Close))
        .collect();
    let mut paths = vec![GeometryPath {
        contours: vec![GeometryContour { commands }],
        polarity: Polarity::Dark,
    }];
    if let Some(hole_diameter) = hole_diameter
        && hole_diameter > 0.0
    {
        paths.push(circle_path(hole_diameter / 2.0, Polarity::Clear));
    }
    paths
}

fn circle_path(radius: f64, polarity: Polarity) -> GeometryPath {
    GeometryPath {
        contours: vec![GeometryContour {
            commands: vec![
                PathCommand::MoveTo(Point { x: radius, y: 0.0 }),
                PathCommand::ArcTo {
                    end: Point { x: -radius, y: 0.0 },
                    center: Point { x: 0.0, y: 0.0 },
                    clockwise: false,
                },
                PathCommand::ArcTo {
                    end: Point { x: radius, y: 0.0 },
                    center: Point { x: 0.0, y: 0.0 },
                    clockwise: false,
                },
                PathCommand::Close,
            ],
        }],
        polarity,
    }
}

fn rect_path(width: f64, height: f64, polarity: Polarity) -> GeometryPath {
    let hw = width / 2.0;
    let hh = height / 2.0;
    GeometryPath {
        contours: vec![GeometryContour {
            commands: vec![
                PathCommand::MoveTo(Point { x: -hw, y: -hh }),
                PathCommand::LineTo(Point { x: hw, y: -hh }),
                PathCommand::LineTo(Point { x: hw, y: hh }),
                PathCommand::LineTo(Point { x: -hw, y: hh }),
                PathCommand::Close,
            ],
        }],
        polarity,
    }
}

/// Vertex counts the format allows for regular polygons.
const POLYGON_VERTICES: std::ops::RangeInclusive<usize> = 3..=12;
/// Most vertices the format allows in one outline primitive.
const OUTLINE_MAX_VERTICES: usize = 5000;
/// The format sets no limit; this one only stops malformed input, far above
/// any real panel.
const STEP_REPEAT_MAX_OCCURRENCES: i64 = 1_000_000;

/// A count read from the file, bounded before it sizes any loop or allocation.
fn bounded_count(value: f64, name: &str, range: std::ops::RangeInclusive<usize>) -> Result<usize> {
    if value.fract() == 0.0 && (*range.start() as f64..=*range.end() as f64).contains(&value) {
        Ok(value as usize)
    } else {
        Err(GerberError::InvalidStructure(format!(
            "{name} must be an integer in {}..={}, got {value}",
            range.start(),
            range.end()
        )))
    }
}

fn macro_value(values: &[f64], index: usize, name: &str) -> Result<f64> {
    values
        .get(index)
        .copied()
        .ok_or_else(|| GerberError::InvalidStructure(format!("missing {name}")))
}

fn macro_length(values: &[f64], index: usize, name: &str, unit: Unit) -> Result<f64> {
    Ok(scale_length(macro_value(values, index, name)?, unit))
}

fn macro_bool(values: &[f64], index: usize) -> Result<Polarity> {
    Ok(if macro_value(values, index, "macro exposure")? == 0.0 {
        Polarity::Clear
    } else {
        Polarity::Dark
    })
}

fn eval_macro_expr(expr: &MacroExpression, vars: &HashMap<usize, f64>) -> Result<f64> {
    Ok(match expr {
        MacroExpression::Number(value) => *value,
        MacroExpression::Variable(index) => *vars.get(index).ok_or_else(|| {
            GerberError::InvalidStructure(format!("macro variable ${index} used before definition"))
        })?,
        MacroExpression::UnaryMinus(inner) => -eval_macro_expr(inner, vars)?,
        MacroExpression::Add(left, right) => {
            eval_macro_expr(left, vars)? + eval_macro_expr(right, vars)?
        }
        MacroExpression::Subtract(left, right) => {
            eval_macro_expr(left, vars)? - eval_macro_expr(right, vars)?
        }
        MacroExpression::Multiply(left, right) => {
            eval_macro_expr(left, vars)? * eval_macro_expr(right, vars)?
        }
        MacroExpression::Divide(left, right) => {
            eval_macro_expr(left, vars)? / eval_macro_expr(right, vars)?
        }
    })
}

fn repolarity(mut path: GeometryPath, polarity: Polarity) -> GeometryPath {
    path.polarity = polarity;
    path
}

fn vector_line_path(start: Point, end: Point, width: f64, polarity: Polarity) -> GeometryPath {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return translate_path(rect_path(0.0, width, polarity), start);
    }
    let nx = -dy / len * width / 2.0;
    let ny = dx / len * width / 2.0;
    GeometryPath {
        contours: vec![GeometryContour {
            commands: vec![
                PathCommand::MoveTo(Point {
                    x: start.x + nx,
                    y: start.y + ny,
                }),
                PathCommand::LineTo(Point {
                    x: start.x - nx,
                    y: start.y - ny,
                }),
                PathCommand::LineTo(Point {
                    x: end.x - nx,
                    y: end.y - ny,
                }),
                PathCommand::LineTo(Point {
                    x: end.x + nx,
                    y: end.y + ny,
                }),
                PathCommand::Close,
            ],
        }],
        polarity,
    }
}

fn translate_path(path: GeometryPath, offset: Point) -> GeometryPath {
    map_path(path, |point| translate_point(point, offset.x, offset.y))
}

fn map_path(mut path: GeometryPath, map: impl Fn(Point) -> Point) -> GeometryPath {
    for command in path
        .contours
        .iter_mut()
        .flat_map(|contour| &mut contour.commands)
    {
        match command {
            PathCommand::MoveTo(point) | PathCommand::LineTo(point) => *point = map(*point),
            PathCommand::ArcTo { end, center, .. } => {
                *end = map(*end);
                *center = map(*center);
            }
            PathCommand::Close => {}
        }
    }
    path
}

fn translate_point(point: Point, dx: f64, dy: f64) -> Point {
    Point {
        x: point.x + dx,
        y: point.y + dy,
    }
}

fn rotate_point(point: Point, degrees: f64) -> Point {
    if degrees == 0.0 {
        return point;
    }
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    Point {
        x: point.x * cos - point.y * sin,
        y: point.x * sin + point.y * cos,
    }
}

struct MacroExpressionParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> MacroExpressionParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn parse(&mut self) -> Result<MacroExpression> {
        let expr = self.parse_add_sub()?;
        self.skip_ws();
        if self.pos != self.input.len() {
            return Err(GerberError::InvalidStructure(format!(
                "invalid macro expression '{}'",
                self.input
            )));
        }
        Ok(expr)
    }

    fn parse_add_sub(&mut self) -> Result<MacroExpression> {
        let mut expr = self.parse_mul_div()?;
        loop {
            self.skip_ws();
            if self.eat('+') {
                expr = MacroExpression::Add(Box::new(expr), Box::new(self.parse_mul_div()?));
            } else if self.eat('-') {
                expr = MacroExpression::Subtract(Box::new(expr), Box::new(self.parse_mul_div()?));
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_mul_div(&mut self) -> Result<MacroExpression> {
        let mut expr = self.parse_factor()?;
        loop {
            self.skip_ws();
            if self.eat('x') || self.eat('X') {
                expr = MacroExpression::Multiply(Box::new(expr), Box::new(self.parse_factor()?));
            } else if self.eat('/') {
                expr = MacroExpression::Divide(Box::new(expr), Box::new(self.parse_factor()?));
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_factor(&mut self) -> Result<MacroExpression> {
        self.skip_ws();
        if self.eat('-') {
            return Ok(MacroExpression::UnaryMinus(Box::new(self.parse_factor()?)));
        }
        if self.eat('+') {
            return self.parse_factor();
        }
        if self.eat('(') {
            let expr = self.parse_add_sub()?;
            self.skip_ws();
            if !self.eat(')') {
                return Err(GerberError::InvalidStructure(format!(
                    "unclosed macro expression '{}'",
                    self.input
                )));
            }
            return Ok(expr);
        }
        if self.eat('$') {
            let start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
            return Ok(MacroExpression::Variable(
                self.input[start..self.pos]
                    .parse()
                    .map_err(|_| GerberError::InvalidNumber(self.input.to_string()))?,
            ));
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(GerberError::InvalidNumber(self.input.to_string()));
        }
        Ok(MacroExpression::Number(
            self.input[start..self.pos]
                .parse()
                .map_err(|_| GerberError::InvalidNumber(self.input[start..self.pos].to_string()))?,
        ))
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }
}

fn parse_aperture_code(value: &str) -> Result<i32> {
    let code = value
        .strip_prefix('D')
        .unwrap_or(value)
        .parse::<i32>()
        .map_err(|_| GerberError::InvalidNumber(value.to_string()))?;
    if code < 10 {
        return Err(GerberError::InvalidStructure(format!(
            "aperture code must be >= 10, got {code}"
        )));
    }
    Ok(code)
}

fn parse_set_aperture(word: &str) -> Option<i32> {
    let code = word.strip_prefix('D')?.parse::<i32>().ok()?;
    (code >= 10).then_some(code)
}

fn parse_operation(word: &str) -> Result<(CoordinateFields, OperationCode)> {
    let (body, code) = if let Some(body) = word.strip_suffix("D01") {
        (body, OperationCode::Plot)
    } else if let Some(body) = word.strip_suffix("D02") {
        (body, OperationCode::Move)
    } else if let Some(body) = word.strip_suffix("D03") {
        (body, OperationCode::Flash)
    } else {
        return Err(GerberError::InvalidStructure(format!(
            "unsupported word command '{word}'"
        )));
    };

    Ok((parse_coordinate_fields(body)?, code))
}

fn parse_coordinate_fields(mut body: &str) -> Result<CoordinateFields> {
    let mut fields = CoordinateFields::default();
    while !body.is_empty() {
        let axis = body.as_bytes()[0] as char;
        if !matches!(axis, 'X' | 'Y' | 'I' | 'J') {
            return Err(GerberError::InvalidStructure(format!(
                "invalid coordinate field '{body}'"
            )));
        }
        body = &body[1..];
        let len = body
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'+' || *b == b'-')
            .count();
        if len == 0 {
            return Err(GerberError::InvalidStructure(format!(
                "missing value for coordinate field {axis}"
            )));
        }
        let value_text = &body[..len];
        let value = value_text
            .parse::<i64>()
            .map_err(|_| GerberError::InvalidNumber(value_text.to_string()))?;
        match axis {
            'X' => fields.x = Some(value),
            'Y' => fields.y = Some(value),
            'I' => fields.i = Some(value),
            'J' => fields.j = Some(value),
            _ => unreachable!(),
        }
        body = &body[len..];
    }
    Ok(fields)
}

fn parse_step_repeat(rest: &str) -> Result<StepRepeat> {
    let Some(rest) = rest.strip_prefix('X') else {
        return Err(GerberError::InvalidStructure(
            "SR missing X repeats".to_string(),
        ));
    };
    let (x_repeats, rest) = parse_i32_prefix(rest)?;
    let Some(rest) = rest.strip_prefix('Y') else {
        return Err(GerberError::InvalidStructure(
            "SR missing Y repeats".to_string(),
        ));
    };
    let (y_repeats, rest) = parse_i32_prefix(rest)?;
    let Some(rest) = rest.strip_prefix('I') else {
        return Err(GerberError::InvalidStructure(
            "SR missing I step".to_string(),
        ));
    };
    let (x_step, rest) = parse_f64_prefix(rest)?;
    let Some(rest) = rest.strip_prefix('J') else {
        return Err(GerberError::InvalidStructure(
            "SR missing J step".to_string(),
        ));
    };
    let (y_step, rest) = parse_f64_prefix(rest)?;
    if !rest.is_empty() {
        return Err(GerberError::InvalidStructure(format!(
            "unexpected SR suffix '{rest}'"
        )));
    }
    if x_repeats < 1
        || y_repeats < 1
        || i64::from(x_repeats) * i64::from(y_repeats) > STEP_REPEAT_MAX_OCCURRENCES
        || !(x_step.is_finite() && y_step.is_finite())
    {
        return Err(GerberError::InvalidStructure(format!(
            "SR repeats {x_repeats} x {y_repeats} must be positive, finite, and at most {STEP_REPEAT_MAX_OCCURRENCES} occurrences"
        )));
    }
    Ok(StepRepeat {
        x_repeats,
        y_repeats,
        x_step,
        y_step,
    })
}

fn parse_i32_prefix(value: &str) -> Result<(i32, &str)> {
    let len = value.bytes().take_while(|b| b.is_ascii_digit()).count();
    if len == 0 {
        return Err(GerberError::InvalidNumber(value.to_string()));
    }
    Ok((
        value[..len]
            .parse()
            .map_err(|_| GerberError::InvalidNumber(value[..len].to_string()))?,
        &value[len..],
    ))
}

fn parse_f64_prefix(value: &str) -> Result<(f64, &str)> {
    let len = value
        .bytes()
        .take_while(|b| b.is_ascii_digit() || matches!(*b, b'+' | b'-' | b'.'))
        .count();
    if len == 0 {
        return Err(GerberError::InvalidNumber(value.to_string()));
    }
    Ok((parse_f64(&value[..len])?, &value[len..]))
}

fn parse_f64(value: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .map_err(|_| GerberError::InvalidNumber(value.to_string()))
}

fn required_param(values: &[f64], index: usize, name: &str) -> Result<f64> {
    values
        .get(index)
        .copied()
        .ok_or_else(|| GerberError::InvalidStructure(format!("missing {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_coordinate_fields() {
        let fields = parse_coordinate_fields("X+100Y-200I0J30").unwrap();
        assert_eq!(fields.x, Some(100));
        assert_eq!(fields.y, Some(-200));
        assert_eq!(fields.i, Some(0));
        assert_eq!(fields.j, Some(30));
    }

    #[test]
    fn parses_step_repeat() {
        let sr = parse_step_repeat("X2Y3I4.5J0").unwrap();
        assert_eq!(sr.x_repeats, 2);
        assert_eq!(sr.y_repeats, 3);
        assert_eq!(sr.x_step, 4.5);
        assert_eq!(sr.y_step, 0.0);
    }
}
