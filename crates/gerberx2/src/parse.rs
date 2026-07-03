use crate::types::Mirroring;
use crate::types::*;
use crate::{GerberError, GerberX2, Interner, Result, Symbol};
use pcb_ir::geom::Polarity;
use std::collections::HashMap;

pub struct Parser<'a> {
    source: &'a str,
    pos: usize,
    interner: Interner,
    commands: Vec<Command>,
    file_attributes: Vec<Attribute>,
    aperture_attributes: HashMap<Symbol, Attribute>,
    object_attributes: HashMap<Symbol, Attribute>,
    aperture_definitions: Vec<ApertureDefinition>,
    aperture_lookup: HashMap<i32, ApertureDefinition>,
    macro_lookup: HashMap<Symbol, ApertureMacro>,
    block_lookup: HashMap<i32, Vec<GraphicalObject>>,
    aperture_macros: Vec<ApertureMacro>,
    objects: Vec<GraphicalObject>,
    region: Option<RegionBuilder>,
    block: Option<BlockBuilder>,
    step_repeat: Option<StepRepeatBuilder>,
    state: GraphicsState,
    saw_m02: bool,
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
            commands: Vec::new(),
            file_attributes: Vec::new(),
            aperture_attributes: HashMap::new(),
            object_attributes: HashMap::new(),
            aperture_definitions: Vec::new(),
            aperture_lookup: HashMap::new(),
            macro_lookup: HashMap::new(),
            block_lookup: HashMap::new(),
            aperture_macros: Vec::new(),
            objects: Vec::new(),
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

        let aperture_attributes = self.aperture_attributes.values().cloned().collect();
        let object_attributes = self.object_attributes.values().cloned().collect();
        Ok(GerberX2 {
            interner: std::mem::take(&mut self.interner),
            commands: std::mem::take(&mut self.commands),
            file_attributes: std::mem::take(&mut self.file_attributes),
            aperture_attributes,
            object_attributes,
            aperture_definitions: std::mem::take(&mut self.aperture_definitions),
            aperture_macros: std::mem::take(&mut self.aperture_macros),
            objects: std::mem::take(&mut self.objects),
            final_state: self.state.clone(),
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
            self.commands.push(Command::Unit(unit));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("FS") {
            let format = parse_format(rest).ok_or_else(|| self.syntax("invalid FS command"))?;
            self.state.coordinate_format = Some(format);
            self.commands.push(Command::Format(format));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AD") {
            let aperture = self.parse_aperture_definition(rest)?;
            self.commands
                .push(Command::ApertureDefinition(aperture.clone()));
            self.aperture_lookup.insert(aperture.code, aperture.clone());
            self.aperture_definitions.push(aperture);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AM") {
            let macro_def = self.parse_aperture_macro(rest)?;
            self.commands
                .push(Command::ApertureMacro(macro_def.clone()));
            self.macro_lookup.insert(macro_def.name, macro_def.clone());
            self.aperture_macros.push(macro_def);
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LP") {
            let polarity = match rest {
                "D" => Polarity::Dark,
                "C" => Polarity::Clear,
                _ => return Err(self.syntax(format!("invalid LP polarity '{rest}'"))),
            };
            self.state.polarity = polarity;
            self.commands.push(Command::LoadPolarity(polarity));
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
            self.commands.push(Command::LoadMirroring(mirroring));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LR") {
            let rotation = parse_f64(rest)?;
            self.state.rotation_degrees = rotation;
            self.commands.push(Command::LoadRotation(rotation));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("LS") {
            let scaling = parse_f64(rest)?;
            self.state.scaling = scaling;
            self.commands.push(Command::LoadScaling(scaling));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("AB") {
            if rest.is_empty() {
                let block = self
                    .block
                    .take()
                    .ok_or_else(|| self.syntax("AB close without matching AB open"))?;
                let objects = self.objects.split_off(block.object_start);
                self.block_lookup
                    .insert(block.aperture_code, objects.clone());
                let aperture = ApertureDefinition {
                    code: block.aperture_code,
                    template: ApertureTemplate::Block { objects },
                    geometry: None,
                    attributes: self.aperture_attributes.values().cloned().collect(),
                };
                self.aperture_lookup.insert(aperture.code, aperture.clone());
                self.aperture_definitions.push(aperture);
                self.commands.push(Command::EndBlockAperture);
            } else {
                let code = parse_aperture_code(rest)?;
                if self.block.is_some() {
                    return Err(self.syntax("nested AB block apertures are not supported"));
                }
                self.block = Some(BlockBuilder {
                    aperture_code: code,
                    object_start: self.objects.len(),
                });
                self.commands.push(Command::BeginBlockAperture(code));
            }
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("SR") {
            if rest.is_empty() {
                let step = self
                    .step_repeat
                    .take()
                    .ok_or_else(|| self.syntax("SR close without matching SR open"))?;
                let seed = self.objects.split_off(step.object_start);
                let mut expanded = Vec::new();
                for ix in 0..step.repeat.x_repeats {
                    for iy in 0..step.repeat.y_repeats {
                        let dx = ix as f64 * step.repeat.x_step;
                        let dy = iy as f64 * step.repeat.y_step;
                        expanded.extend(
                            seed.iter()
                                .cloned()
                                .map(|object| translate_object(object, dx, dy)),
                        );
                    }
                }
                self.objects.extend(expanded);
                self.commands.push(Command::EndStepRepeat);
            } else {
                let sr = parse_step_repeat(rest)?;
                if self.step_repeat.is_some() {
                    return Err(self.syntax("nested SR statements are not supported"));
                }
                self.step_repeat = Some(StepRepeatBuilder {
                    repeat: sr,
                    object_start: self.objects.len(),
                });
                self.commands.push(Command::BeginStepRepeat(sr));
            }
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TF") {
            let attr = self.parse_attribute(rest)?;
            self.file_attributes.push(attr.clone());
            self.commands.push(Command::FileAttribute(attr));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TA") {
            let attr = self.parse_attribute(rest)?;
            self.aperture_attributes.insert(attr.name, attr.clone());
            self.commands.push(Command::ApertureAttribute(attr));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TO") {
            let attr = self.parse_attribute(rest)?;
            self.object_attributes.insert(attr.name, attr.clone());
            self.commands.push(Command::ObjectAttribute(attr));
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("TD") {
            let name = if rest.is_empty() {
                self.aperture_attributes.clear();
                self.object_attributes.clear();
                None
            } else {
                let name = self.interner.intern(rest);
                self.aperture_attributes.remove(&name);
                self.object_attributes.remove(&name);
                Some(name)
            };
            self.commands.push(Command::DeleteAttribute(name));
            return Ok(());
        }

        Err(self.syntax(format!("unsupported extended command '{word}'")))
    }

    fn parse_word_command(&mut self, command: &'a str) -> Result<()> {
        let word = command.strip_suffix('*').unwrap_or(command);
        if let Some(comment) = word.strip_prefix("G04") {
            let comment = self.interner.intern(comment);
            self.commands.push(Command::Comment(comment));
            return Ok(());
        }

        match word {
            "G01" => {
                self.state.plot_mode = Some(PlotMode::Linear);
                self.commands.push(Command::PlotMode(PlotMode::Linear));
                return Ok(());
            }
            "G02" => {
                self.state.plot_mode = Some(PlotMode::ClockwiseArc);
                self.commands
                    .push(Command::PlotMode(PlotMode::ClockwiseArc));
                return Ok(());
            }
            "G03" => {
                self.state.plot_mode = Some(PlotMode::CounterclockwiseArc);
                self.commands
                    .push(Command::PlotMode(PlotMode::CounterclockwiseArc));
                return Ok(());
            }
            "G75" => {
                self.commands.push(Command::QuadrantModeMulti);
                return Ok(());
            }
            "G36" => {
                if self.region.is_some() {
                    return Err(self.syntax("nested region statements are not allowed"));
                }
                self.region = Some(RegionBuilder::default());
                self.commands.push(Command::BeginRegion);
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
                self.objects.push(self.graphical_object(ObjectKind::Region {
                    contours: region.contours,
                }));
                self.commands.push(Command::EndRegion);
                return Ok(());
            }
            "M02" => {
                self.saw_m02 = true;
                self.commands.push(Command::EndOfFile);
                return Ok(());
            }
            _ => {}
        }

        if let Some(code) = parse_set_aperture(word) {
            self.state.current_aperture = Some(code);
            self.commands.push(Command::SetCurrentAperture(code));
            return Ok(());
        }

        let (fields, code) = parse_operation(word)?;
        self.interpret_operation(fields, code)?;
        self.commands.push(Command::Operation { fields, code });
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
                if let Some(block_objects) = self.block_lookup.get(&aperture) {
                    let objects = block_objects
                        .iter()
                        .cloned()
                        .map(|object| transform_block_object(object, point, &self.state));
                    self.objects.extend(objects);
                } else {
                    self.objects.push(self.graphical_object(ObjectKind::Flash {
                        at: point,
                        aperture,
                    }));
                }
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
                    self.objects.push(self.graphical_object(kind));
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

    fn graphical_object(&self, kind: ObjectKind) -> GraphicalObject {
        let aperture_attributes = match &kind {
            ObjectKind::Draw { aperture, .. }
            | ObjectKind::Arc { aperture, .. }
            | ObjectKind::Flash { aperture, .. } => self
                .aperture_lookup
                .get(aperture)
                .map(|aperture| aperture.attributes.clone())
                .unwrap_or_default(),
            ObjectKind::Region { .. } => self.aperture_attributes.values().cloned().collect(),
        };
        GraphicalObject {
            kind,
            polarity: self.state.polarity,
            mirroring: self.state.mirroring,
            rotation_degrees: self.state.rotation_degrees,
            scaling: self.state.scaling,
            aperture_attributes,
            object_attributes: self.object_attributes.values().cloned().collect(),
        }
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
            attributes: self.aperture_attributes.values().cloned().collect(),
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
                vertices: required_param(&values, 1, "polygon vertices")? as i32,
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
        for word in body
            .split_terminator('*')
            .map(str::trim)
            .filter(|word| !word.is_empty())
        {
            if let Some(text) = word.strip_prefix('0') {
                primitives.push(MacroPrimitive::Comment(
                    self.interner.intern(text.trim_start()),
                ));
            } else if let Some((variable, expression)) = word.split_once('=') {
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
            MacroPrimitive::Comment(_) => {}
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

fn lower_macro_shape(code: i32, values: &[f64], unit: Unit) -> Result<Vec<GeometryPath>> {
    match code {
        1 => {
            let exposure = macro_bool(values, 0)?;
            let diameter = macro_length(values, 1, "macro circle diameter", unit)?;
            let center = Point {
                x: macro_length(values, 2, "macro circle center x", unit)?,
                y: macro_length(values, 3, "macro circle center y", unit)?,
            };
            let rotation = values.get(4).copied().unwrap_or(0.0);
            Ok(vec![transform_path(
                circle_path(diameter / 2.0, exposure),
                center,
                rotation,
            )])
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
            Ok(vec![vector_line_path(
                start, end, width, exposure, rotation,
            )])
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
            Ok(vec![transform_path(
                rect_path(width, height, exposure),
                center,
                rotation,
            )])
        }
        4 => {
            let exposure = macro_bool(values, 0)?;
            let vertices = macro_value(values, 1, "macro outline vertices")? as usize;
            let expected = 2 + (vertices + 1) * 2 + 1;
            if values.len() != expected {
                return Err(GerberError::InvalidStructure(
                    "macro outline has the wrong number of parameters".to_string(),
                ));
            }
            if vertices < 3 {
                return Err(GerberError::InvalidStructure(
                    "macro outline requires at least 3 vertices".to_string(),
                ));
            }
            let rotation = values[expected - 1];
            let first = Point {
                x: values[2],
                y: values[3],
            };
            let last = Point {
                x: values[2 + vertices * 2],
                y: values[3 + vertices * 2],
            };
            if !points_close(first, last) {
                return Err(GerberError::InvalidStructure(
                    "macro outline last vertex must equal first vertex".to_string(),
                ));
            }
            let mut commands = Vec::new();
            for index in 0..=vertices {
                let point = rotate_point(
                    Point {
                        x: scale_length(values[2 + index * 2], unit),
                        y: scale_length(values[3 + index * 2], unit),
                    },
                    rotation,
                );
                if index == 0 {
                    commands.push(PathCommand::MoveTo(point));
                } else {
                    commands.push(PathCommand::LineTo(point));
                }
            }
            commands.push(PathCommand::Close);
            Ok(vec![GeometryPath {
                contours: vec![GeometryContour { commands }],
                polarity: exposure,
            }])
        }
        5 => {
            let exposure = macro_bool(values, 0)?;
            let vertices = macro_value(values, 1, "macro polygon vertices")? as i32;
            let center = Point {
                x: macro_length(values, 2, "macro polygon center x", unit)?,
                y: macro_length(values, 3, "macro polygon center y", unit)?,
            };
            let diameter = macro_length(values, 4, "macro polygon diameter", unit)?;
            let rotation = macro_value(values, 5, "macro polygon rotation")?;
            Ok(polygon_paths(diameter, vertices, rotation, None)
                .into_iter()
                .map(|path| transform_path(repolarity(path, exposure), center, 0.0))
                .collect())
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
            Ok(paths
                .into_iter()
                .map(|path| transform_path(path, center, rotation))
                .collect())
        }
        _ => Err(GerberError::InvalidStructure(format!(
            "unsupported aperture macro primitive {code}"
        ))),
    }
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
    let mut paths = Vec::new();
    if vertices >= 3 {
        let radius = outer_diameter / 2.0;
        let rotation = rotation_degrees.to_radians();
        let mut commands = Vec::new();
        for i in 0..vertices {
            let angle = rotation + i as f64 * std::f64::consts::TAU / vertices as f64;
            let point = Point {
                x: radius * angle.cos(),
                y: radius * angle.sin(),
            };
            if i == 0 {
                commands.push(PathCommand::MoveTo(point));
            } else {
                commands.push(PathCommand::LineTo(point));
            }
        }
        commands.push(PathCommand::Close);
        paths.push(GeometryPath {
            contours: vec![GeometryContour { commands }],
            polarity: Polarity::Dark,
        });
    }
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

fn vector_line_path(
    start: Point,
    end: Point,
    width: f64,
    polarity: Polarity,
    rotation: f64,
) -> GeometryPath {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return transform_path(rect_path(0.0, width, polarity), start, rotation);
    }
    let nx = -dy / len * width / 2.0;
    let ny = dx / len * width / 2.0;
    let mut path = GeometryPath {
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
    };
    if rotation != 0.0 {
        path = transform_path(path, Point { x: 0.0, y: 0.0 }, rotation);
    }
    path
}

fn transform_path(mut path: GeometryPath, offset: Point, rotation: f64) -> GeometryPath {
    for contour in &mut path.contours {
        for command in &mut contour.commands {
            match command {
                PathCommand::MoveTo(point) | PathCommand::LineTo(point) => {
                    *point = translate_point(rotate_point(*point, rotation), offset.x, offset.y);
                }
                PathCommand::ArcTo { end, center, .. } => {
                    *end = translate_point(rotate_point(*end, rotation), offset.x, offset.y);
                    *center = translate_point(rotate_point(*center, rotation), offset.x, offset.y);
                }
                PathCommand::Close => {}
            }
        }
    }
    path
}

fn translate_object(object: GraphicalObject, dx: f64, dy: f64) -> GraphicalObject {
    transform_object(object, LinearTransform::identity(), Point { x: dx, y: dy })
}

fn transform_block_object(
    object: GraphicalObject,
    origin: Point,
    state: &GraphicsState,
) -> GraphicalObject {
    transform_object(object, LinearTransform::from_state(state), origin)
}

fn transform_object(
    mut object: GraphicalObject,
    outer: LinearTransform,
    origin: Point,
) -> GraphicalObject {
    let flips_orientation = outer.determinant() < 0.0;
    match &mut object.kind {
        ObjectKind::Draw { start, end, .. } => {
            *start = outer.transform_point(*start, origin);
            *end = outer.transform_point(*end, origin);
        }
        ObjectKind::Arc {
            start,
            end,
            center_offset,
            clockwise,
            ..
        } => {
            *start = outer.transform_point(*start, origin);
            *end = outer.transform_point(*end, origin);
            *center_offset = outer.transform_vector(*center_offset);
            *clockwise ^= flips_orientation;
        }
        ObjectKind::Flash { at, .. } => *at = outer.transform_point(*at, origin),
        ObjectKind::Region { contours } => {
            for contour in contours {
                for segment in &mut contour.segments {
                    match segment {
                        ContourSegment::Line { start, end } => {
                            *start = outer.transform_point(*start, origin);
                            *end = outer.transform_point(*end, origin);
                        }
                        ContourSegment::Arc {
                            start,
                            end,
                            center_offset,
                            clockwise,
                        } => {
                            *start = outer.transform_point(*start, origin);
                            *end = outer.transform_point(*end, origin);
                            *center_offset = outer.transform_vector(*center_offset);
                            *clockwise ^= flips_orientation;
                        }
                    }
                }
            }
        }
    }
    let modifiers = outer.compose(LinearTransform::from_parts(
        object.mirroring,
        object.rotation_degrees,
        object.scaling,
    ));
    let (mirroring, rotation_degrees, scaling) = modifiers.decompose();
    object.mirroring = mirroring;
    object.rotation_degrees = rotation_degrees;
    object.scaling = scaling;
    object
}

#[derive(Debug, Clone, Copy)]
struct LinearTransform {
    m00: f64,
    m01: f64,
    m10: f64,
    m11: f64,
}

impl LinearTransform {
    fn identity() -> Self {
        Self {
            m00: 1.0,
            m01: 0.0,
            m10: 0.0,
            m11: 1.0,
        }
    }

    fn from_state(state: &GraphicsState) -> Self {
        Self::from_parts(state.mirroring, state.rotation_degrees, state.scaling)
    }

    fn from_parts(mirroring: Mirroring, rotation_degrees: f64, scale: f64) -> Self {
        let sx = match mirroring {
            Mirroring::X | Mirroring::XY => -scale,
            _ => scale,
        };
        let sy = match mirroring {
            Mirroring::Y | Mirroring::XY => -scale,
            _ => scale,
        };
        let (sin, cos) = rotation_degrees.to_radians().sin_cos();
        Self {
            m00: cos * sx,
            m01: -sin * sy,
            m10: sin * sx,
            m11: cos * sy,
        }
    }

    fn transform_point(self, point: Point, origin: Point) -> Point {
        let vector = self.transform_vector(point);
        Point {
            x: origin.x + vector.x,
            y: origin.y + vector.y,
        }
    }

    fn transform_vector(self, point: Point) -> Point {
        Point {
            x: self.m00 * point.x + self.m01 * point.y,
            y: self.m10 * point.x + self.m11 * point.y,
        }
    }

    fn compose(self, rhs: Self) -> Self {
        Self {
            m00: self.m00 * rhs.m00 + self.m01 * rhs.m10,
            m01: self.m00 * rhs.m01 + self.m01 * rhs.m11,
            m10: self.m10 * rhs.m00 + self.m11 * rhs.m10,
            m11: self.m10 * rhs.m01 + self.m11 * rhs.m11,
        }
    }

    fn determinant(self) -> f64 {
        self.m00 * self.m11 - self.m01 * self.m10
    }

    fn decompose(self) -> (Mirroring, f64, f64) {
        let scale = self.m00.hypot(self.m10);
        if scale == 0.0 {
            return (Mirroring::None, 0.0, 0.0);
        }
        let mirroring = if self.determinant() < 0.0 {
            Mirroring::X
        } else {
            Mirroring::None
        };
        let sx = if mirroring == Mirroring::X {
            -scale
        } else {
            scale
        };
        let cos = self.m00 / sx;
        let sin = self.m10 / sx;
        (mirroring, sin.atan2(cos).to_degrees(), scale)
    }
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
