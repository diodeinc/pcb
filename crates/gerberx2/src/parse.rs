use crate::types::*;
use crate::{GerberError, GerberX2, Interner, Result, Symbol};
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
    aperture_macros: Vec<ApertureMacro>,
    objects: Vec<GraphicalObject>,
    region: Option<RegionBuilder>,
    state: GraphicsState,
    saw_m02: bool,
}

#[derive(Debug, Default)]
struct RegionBuilder {
    contours: Vec<Contour>,
    current: Option<Contour>,
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
            aperture_macros: Vec::new(),
            objects: Vec::new(),
            region: None,
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
                self.commands.push(Command::EndBlockAperture);
            } else {
                let code = parse_aperture_code(rest)?;
                self.commands.push(Command::BeginBlockAperture(code));
            }
            return Ok(());
        }

        if let Some(rest) = word.strip_prefix("SR") {
            if rest.is_empty() {
                self.commands.push(Command::EndStepRepeat);
            } else {
                let sr = parse_step_repeat(rest)?;
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
                self.objects.push(self.graphical_object(ObjectKind::Flash {
                    at: point,
                    aperture,
                }));
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
        let template = self.parse_template_call(template_call)?;
        let geometry = lower_standard_aperture(&template);
        Ok(ApertureDefinition {
            code,
            template,
            geometry,
            attributes: self.aperture_attributes.values().cloned().collect(),
        })
    }

    fn parse_template_call(&mut self, template_call: &str) -> Result<ApertureTemplate> {
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
                diameter: required_param(&values, 0, "circle diameter")?,
                hole_diameter: values.get(1).copied(),
            }),
            "R" => Ok(ApertureTemplate::Rectangle {
                width: required_param(&values, 0, "rectangle width")?,
                height: required_param(&values, 1, "rectangle height")?,
                hole_diameter: values.get(2).copied(),
            }),
            "O" => Ok(ApertureTemplate::Obround {
                width: required_param(&values, 0, "obround width")?,
                height: required_param(&values, 1, "obround height")?,
                hole_diameter: values.get(2).copied(),
            }),
            "P" => Ok(ApertureTemplate::Polygon {
                outer_diameter: required_param(&values, 0, "polygon outer diameter")?,
                vertices: required_param(&values, 1, "polygon vertices")? as i32,
                rotation_degrees: values.get(2).copied(),
                hole_diameter: values.get(3).copied(),
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
        Ok(ApertureMacro {
            name: self.interner.intern(name),
            body: body
                .split_terminator('*')
                .filter(|word| !word.is_empty())
                .map(|word| self.interner.intern(word))
                .collect(),
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
        ApertureTemplate::Macro { .. } => return None,
    };
    Some(ApertureGeometry { paths })
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
