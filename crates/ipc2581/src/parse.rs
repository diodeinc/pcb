use crate::dom::{Dom, Node};
use crate::types::*;
use crate::{Interner, Ipc2581Error, Result, Symbol};

/// The `LineDescGroup` and `FillDescGroup` children of a shape.
#[derive(Default)]
struct ShapeStyle {
    line_desc: Option<LineDesc>,
    line_desc_ref: Option<Symbol>,
    fill_desc: Option<FillDesc>,
    fill_desc_ref: Option<Symbol>,
}

/// A `Polygon`, `Polyline` or `Cutout` read in one pass over its children.
struct Poly {
    polygon: Polygon,
    xform: Option<Xform>,
    style: ShapeStyle,
}

fn single_feature_offset(locations: &[Point], xform: Option<Xform>) -> Point {
    if locations.len() <= 1 && xform.is_none() {
        locations
            .first()
            .copied()
            .unwrap_or(Point { x: 0.0, y: 0.0 })
    } else {
        Point { x: 0.0, y: 0.0 }
    }
}

/// Parser context holding the string interner and unit context
pub struct Parser<'a> {
    pub interner: Interner,
    /// Current ECAD units for converting dimensions (set when parsing CadHeader)
    ecad_units: Option<Units>,
    /// Specs from CadHeader (set when parsing CadHeader, used by StackupLayer parsing)
    specs: std::collections::HashMap<Symbol, ecad::Spec>,
    doc: Option<&'a Dom<'a>>,
}

impl<'a> Parser<'a> {
    pub fn new() -> Self {
        Self {
            interner: Interner::new(),
            ecad_units: None,
            specs: std::collections::HashMap::new(),
            doc: None,
        }
    }

    fn doc(&self) -> &'a Dom<'a> {
        self.doc.expect("parser document is set while parsing")
    }

    fn name<'n>(&self, node: &'n Node) -> &'a str {
        self.doc().name(*node)
    }

    fn attr<'n>(&self, node: &'n Node, attr: &str) -> Option<&'a str> {
        self.doc().attr(*node, attr)
    }

    /// Child elements in document order. The iterator borrows the document,
    /// not the parser, so its items can be parsed as they come.
    fn element_children(&self, node: &Node) -> impl Iterator<Item = Node> + use<'a> {
        self.doc().children(*node)
    }

    fn children_named(
        &self,
        node: &Node,
        name: &'static str,
    ) -> impl Iterator<Item = Node> + use<'a> {
        let doc = self.doc();
        doc.children(*node)
            .filter(move |child| doc.name(*child) == name)
    }

    pub fn parse_document(&mut self, doc: &'a Dom<'a>) -> Result<ParsedIpc2581> {
        self.doc = Some(doc);
        let root = doc.root();

        // Verify root element
        if self.name(&root) != "IPC-2581" {
            return Err(Ipc2581Error::InvalidStructure(format!(
                "Expected root element 'IPC-2581', found '{}'",
                self.name(&root)
            )));
        }

        // Parse revision
        let revision = self
            .attr(&root, "revision")
            .ok_or(Ipc2581Error::MissingAttribute {
                element: "IPC-2581",
                attr: "revision",
            })?;
        let revision = self.interner.intern(revision);

        // Single pass through children
        let mut content_node = None;
        let mut logistic_header = None;
        let mut history_record = None;
        let mut ecad = None;
        let mut boms = Vec::new();
        let mut avl = None;

        for child in self.element_children(&root) {
            match self.name(&child) {
                "Content" => content_node = Some(child),
                "LogisticHeader" => logistic_header = Some(self.parse_logistic_header(&child)?),
                "HistoryRecord" => history_record = Some(self.parse_history_record(&child)?),
                "Ecad" => ecad = Some(self.parse_ecad(&child)?),
                "Bom" => boms.push(self.parse_bom(&child)?),
                "Avl" => avl = Some(self.parse_avl(&child)?),
                _ => {}
            }
        }

        let content =
            self.parse_content(&content_node.ok_or(Ipc2581Error::MissingElement("Content"))?)?;

        Ok(ParsedIpc2581 {
            revision,
            content,
            logistic_header,
            history_record,
            ecad,
            boms,
            avl,
        })
    }

    fn parse_content(&mut self, node: &Node) -> Result<Content> {
        let role_ref = self.required_attr(node, "roleRef", "Content")?;

        // Single pass through children
        let mut function_mode_node = None;
        let mut step_refs = Vec::new();
        let mut layer_refs = Vec::new();
        let mut bom_refs = Vec::new();
        let mut avl_refs = Vec::new();
        let mut dictionary_color = None;
        let mut dictionary_line_desc = None;
        let mut dictionary_fill_desc = None;
        let mut dictionary_font = None;
        let mut dictionary_firmware = None;
        let mut dictionary_standard = None;
        let mut dictionary_user = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "FunctionMode" => function_mode_node = Some(child),
                "StepRef" => step_refs.push(self.required_attr(&child, "name", "StepRef")?),
                "LayerRef" => layer_refs.push(self.required_attr(&child, "name", "LayerRef")?),
                "BomRef" => bom_refs.push(self.required_attr(&child, "name", "BomRef")?),
                "AvlRef" => avl_refs.push(self.required_attr(&child, "name", "AvlRef")?),
                "DictionaryColor" => dictionary_color = Some(self.parse_dictionary_color(&child)?),
                "DictionaryLineDesc" => {
                    dictionary_line_desc = Some(self.parse_dictionary_line_desc(&child)?)
                }
                "DictionaryFillDesc" => {
                    dictionary_fill_desc = Some(self.parse_dictionary_fill_desc(&child)?)
                }
                "DictionaryFont" => dictionary_font = Some(self.parse_dictionary_font(&child)?),
                "DictionaryFirmware" => {
                    dictionary_firmware = Some(self.parse_dictionary_firmware(&child)?)
                }
                "DictionaryStandard" => {
                    dictionary_standard = Some(self.parse_dictionary_standard(&child)?)
                }
                "DictionaryUser" => dictionary_user = Some(self.parse_dictionary_user(&child)?),
                _ => {}
            }
        }

        let function_mode = self.parse_function_mode(
            &function_mode_node.ok_or(Ipc2581Error::MissingElement("FunctionMode"))?,
        )?;

        Ok(Content {
            role_ref,
            function_mode,
            step_refs,
            layer_refs,
            bom_refs,
            avl_refs,
            dictionary_color: dictionary_color.unwrap_or_default(),
            dictionary_line_desc: dictionary_line_desc.unwrap_or_default(),
            dictionary_fill_desc: dictionary_fill_desc.unwrap_or_default(),
            dictionary_font: dictionary_font.unwrap_or_default(),
            dictionary_firmware: dictionary_firmware.unwrap_or_default(),
            dictionary_standard: dictionary_standard.unwrap_or_default(),
            dictionary_user: dictionary_user.unwrap_or_default(),
        })
    }

    fn parse_function_mode(&mut self, node: &Node) -> Result<FunctionMode> {
        let mode = Mode::from_ipc(self.required_str(node, "mode", "FunctionMode")?)?;

        let level = self
            .attr(node, "level")
            .map(|s| self.parse_level(s))
            .transpose()?;

        Ok(FunctionMode { mode, level })
    }

    fn parse_level(&self, s: &str) -> Result<Level> {
        let level: u8 = s.parse().map_err(|_| {
            Ipc2581Error::InvalidAttribute(format!(
                "Invalid level (expected positive integer): {}",
                s
            ))
        })?;

        if level == 0 {
            return Err(Ipc2581Error::InvalidAttribute(
                "Invalid level (expected positive integer): 0".to_string(),
            ));
        }

        Ok(Level(level))
    }

    fn parse_dictionary_color(&mut self, node: &Node) -> Result<DictionaryColor> {
        let entries = self
            .children_named(node, "EntryColor")
            .map(|n| self.parse_entry_color(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryColor { entries })
    }

    fn parse_entry_color(&mut self, node: &Node) -> Result<EntryColor> {
        let id = self.required_attr(node, "id", "EntryColor")?;

        let color_node = self
            .children_named(node, "Color")
            .next()
            .ok_or(Ipc2581Error::MissingElement("Color"))?;

        let r = self.parse_u8_attr(&color_node, "r", "Color")?;
        let g = self.parse_u8_attr(&color_node, "g", "Color")?;
        let b = self.parse_u8_attr(&color_node, "b", "Color")?;

        Ok(EntryColor {
            id,
            color: Color { r, g, b },
        })
    }

    fn parse_dictionary_line_desc(&mut self, node: &Node) -> Result<DictionaryLineDesc> {
        let units = self.attr(node, "units").map(Units::from_ipc).transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = self
            .children_named(node, "EntryLineDesc")
            .map(|n| self.parse_entry_line_desc(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryLineDesc { units, entries })
    }

    fn parse_entry_line_desc(&mut self, node: &Node, units: Units) -> Result<EntryLineDesc> {
        let id = self.required_attr(node, "id", "EntryLineDesc")?;

        let line_desc_node = self
            .children_named(node, "LineDesc")
            .next()
            .ok_or(Ipc2581Error::MissingElement("LineDesc"))?;

        let line_desc = self.parse_line_desc(&line_desc_node, units)?;

        Ok(EntryLineDesc { id, line_desc })
    }

    fn parse_line_desc(&mut self, node: &Node, units: Units) -> Result<LineDesc> {
        let line_width = self
            .number(node, "lineWidth", Sign::NonNegative, Some(units))?
            .ok_or(Ipc2581Error::MissingAttribute {
                element: "LineDesc",
                attr: "lineWidth",
            })?;
        let line_end = LineEnd::from_ipc(self.required_str(node, "lineEnd", "LineDesc")?)?;

        let line_property = self
            .attr(node, "lineProperty")
            .map(LineProperty::from_ipc)
            .transpose()?;

        Ok(LineDesc {
            line_width,
            line_end,
            line_property,
        })
    }

    fn parse_line_desc_group(
        &mut self,
        node: &Node,
        units: Units,
        context: &'static str,
    ) -> Result<LineDescGroup> {
        let style = self.parse_fill_and_line_desc(node, units)?;
        match (style.line_desc, style.line_desc_ref) {
            (Some(line_desc), None) => Ok(LineDescGroup::Inline(line_desc)),
            (None, Some(line_desc_ref)) => Ok(LineDescGroup::Ref(line_desc_ref)),
            (None, None) => Err(Ipc2581Error::MissingElement(context)),
            (Some(_), Some(_)) => Err(Ipc2581Error::InvalidStructure(format!(
                "{context} contains multiple LineDescGroup children"
            ))),
        }
    }

    fn parse_dictionary_fill_desc(&mut self, node: &Node) -> Result<DictionaryFillDesc> {
        let units = self.attr(node, "units").map(Units::from_ipc).transpose()?;
        let fill_units = units.unwrap_or(Units::Millimeter);
        let mut entries = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) != "EntryFillDesc" {
                continue;
            }
            let id = self.required_attr(&child, "id", "EntryFillDesc")?;
            let fill = self
                .children_named(&child, "FillDesc")
                .next()
                .ok_or(Ipc2581Error::MissingElement("FillDesc in EntryFillDesc"))?;
            entries.push(EntryFillDesc {
                id,
                fill_desc: self.parse_fill_desc(&fill, fill_units)?,
            });
        }
        Ok(DictionaryFillDesc { units, entries })
    }

    fn parse_fill_desc(&mut self, node: &Node, units: Units) -> Result<FillDesc> {
        let fill_property =
            FillProperty::from_ipc(self.required_str(node, "fillProperty", "FillDesc")?)?;
        let color = self
            .element_children(node)
            .find_map(|child| self.parse_color_group(&child).transpose())
            .transpose()?;

        Ok(FillDesc {
            fill_property,
            line_width: self.number(node, "lineWidth", Sign::NonNegative, Some(units))?,
            pitch1: self.number(node, "pitch1", Sign::NonNegative, Some(units))?,
            pitch2: self.number(node, "pitch2", Sign::NonNegative, Some(units))?,
            angle1: self.opt_num(node, "angle1")?,
            angle2: self.opt_num(node, "angle2")?,
            color,
        })
    }

    fn parse_color_group(&mut self, node: &Node) -> Result<Option<ColorGroup>> {
        let color = match self.name(node) {
            "Color" => Some(ColorGroup::Color(Color {
                r: self.parse_u8_attr(node, "r", "Color")?,
                g: self.parse_u8_attr(node, "g", "Color")?,
                b: self.parse_u8_attr(node, "b", "Color")?,
            })),
            "ColorRef" => Some(ColorGroup::Ref(self.required_attr(node, "id", "ColorRef")?)),
            "ColorTerm" => Some(ColorGroup::Term {
                name: self.required_attr(node, "name", "ColorTerm")?,
                comment: self.optional_attr(node, "comment"),
            }),
            _ => None,
        };
        Ok(color)
    }

    fn parse_dictionary_firmware(&mut self, node: &Node) -> Result<DictionaryFirmware> {
        let mut entries = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) != "EntryFirmware" {
                continue;
            }
            let id = self.required_attr(&child, "id", "EntryFirmware")?;
            let cached = self.children_named(&child, "CachedFirmware").next().ok_or(
                Ipc2581Error::MissingElement("CachedFirmware in EntryFirmware"),
            )?;
            entries.push(EntryFirmware {
                id,
                hex_encoded_binary: self.required_attr(
                    &cached,
                    "hexEncodedBinary",
                    "CachedFirmware",
                )?,
            });
        }
        Ok(DictionaryFirmware { entries })
    }

    fn parse_dictionary_font(&mut self, node: &Node) -> Result<DictionaryFont> {
        let units = self.attr(node, "units").map(Units::from_ipc).transpose()?;
        let font_units = units.unwrap_or(Units::Millimeter);
        let mut entries = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) != "EntryFont" {
                continue;
            }
            let id = self.required_attr(&child, "id", "EntryFont")?;
            let definition_node = self
                .element_children(&child)
                .find(|entry| matches!(self.name(entry), "FontDefEmbedded" | "FontDefExternal"))
                .ok_or(Ipc2581Error::MissingElement("FontDef in EntryFont"))?;
            let definition = match self.name(&definition_node) {
                "FontDefEmbedded" => FontDefinition::Embedded(
                    self.parse_embedded_font(&definition_node, font_units)?,
                ),
                "FontDefExternal" => FontDefinition::External(ExternalFont {
                    name: self.required_attr(&definition_node, "name", "FontDefExternal")?,
                    urn: self.required_attr(&definition_node, "urn", "FontDefExternal")?,
                }),
                _ => unreachable!("font definition was filtered above"),
            };
            entries.push(EntryFont { id, definition });
        }
        Ok(DictionaryFont { units, entries })
    }

    fn parse_embedded_font(&mut self, node: &Node, units: Units) -> Result<EmbeddedFont> {
        let name = self.required_attr(node, "name", "FontDefEmbedded")?;
        let line_desc =
            self.parse_line_desc_group(node, units, "LineDescGroup in FontDefEmbedded")?;
        let glyphs = self
            .children_named(node, "Glyph")
            .map(|glyph| self.parse_font_glyph(&glyph, units))
            .collect::<Result<_>>()?;
        Ok(EmbeddedFont {
            name,
            line_desc,
            glyphs,
        })
    }

    fn parse_font_glyph(&mut self, node: &Node, units: Units) -> Result<FontGlyph> {
        let char_code = self.required_attr(node, "charCode", "Glyph")?;
        let bounding_box = BoundingBox {
            lower_left: self.point(node, "lowerLeftX", "lowerLeftY", "Glyph", units)?,
            upper_right: self.point(node, "upperRightX", "upperRightY", "Glyph", units)?,
        };
        let mut shapes = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Arc" | "Line" | "Polyline" => {
                    let shape = self.parse_user_shape(&child, units)?.ok_or_else(|| {
                        Ipc2581Error::InvalidStructure(format!(
                            "Unsupported {} in Glyph",
                            self.name(&child)
                        ))
                    })?;
                    shapes.push(FontShape::Shape(shape));
                }
                "Outline" => shapes.push(FontShape::Outline(
                    self.parse_package_outline(&child, units)?,
                )),
                _ => {}
            }
        }
        Ok(FontGlyph {
            char_code,
            bounding_box,
            shapes,
        })
    }

    /// Parse optional FillDesc and LineDesc children from a primitive node
    fn parse_fill_and_line_desc(&mut self, node: &Node, units: Units) -> Result<ShapeStyle> {
        let mut style = ShapeStyle::default();
        for child in self.element_children(node) {
            self.parse_style_child(&child, units, &mut style)?;
        }
        Ok(style)
    }

    /// Reads `child` into `style` if it is a LineDescGroup or FillDescGroup member.
    fn parse_style_child(
        &mut self,
        child: &Node,
        units: Units,
        style: &mut ShapeStyle,
    ) -> Result<()> {
        match self.name(child) {
            "LineDesc" => style.line_desc = Some(self.parse_line_desc(child, units)?),
            "LineDescRef" => {
                style.line_desc_ref = Some(self.required_attr(child, "id", "LineDescRef")?)
            }
            "FillDesc" => style.fill_desc = Some(self.parse_fill_desc(child, units)?),
            "FillDescRef" => {
                style.fill_desc_ref = Some(self.required_attr(child, "id", "FillDescRef")?)
            }
            _ => {}
        }
        Ok(())
    }

    /// Wrap a shape with styling (fill_property and line_desc_ref)
    fn styled<T>(&mut self, node: &Node, shape: T, units: Units) -> Result<Styled<T>> {
        let style = self.parse_fill_and_line_desc(node, units)?;
        Ok(Styled {
            shape,
            fill_property: style.fill_desc.map(|desc| desc.fill_property),
            line_desc: style.line_desc,
            line_desc_ref: style.line_desc_ref,
            fill_desc: style.fill_desc,
            fill_desc_ref: style.fill_desc_ref,
        })
    }

    fn parse_dictionary_standard(&mut self, node: &Node) -> Result<DictionaryStandard> {
        let units = self.attr(node, "units").map(Units::from_ipc).transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = self
            .children_named(node, "EntryStandard")
            .map(|n| self.parse_entry_standard(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryStandard { units, entries })
    }

    fn parse_entry_standard(&mut self, node: &Node, units: Units) -> Result<EntryStandard> {
        let id = self.required_attr(node, "id", "EntryStandard")?;

        // Find the primitive child element
        let primitive_node = self
            .element_children(node)
            .next()
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive"))?;

        let primitive = self.parse_standard_primitive(&primitive_node, units)?;

        Ok(EntryStandard { id, primitive })
    }

    fn parse_standard_primitive(&mut self, node: &Node, units: Units) -> Result<StandardPrimitive> {
        match self.name(node) {
            "Circle" => Ok(StandardPrimitive::Circle(self.styled(
                node,
                Circle {
                    diameter: self.mm(node, "diameter", "Circle", units)?,
                },
                units,
            )?)),
            "RectCenter" => Ok(StandardPrimitive::RectCenter(self.styled(
                node,
                RectCenter {
                    size: Size {
                        width: self.mm(node, "width", "RectCenter", units)?,
                        height: self.mm(node, "height", "RectCenter", units)?,
                    },
                },
                units,
            )?)),
            "RectRound" => Ok(StandardPrimitive::RectRound(self.styled(
                node,
                RectRound {
                    size: Size {
                        width: self.mm(node, "width", "RectRound", units)?,
                        height: self.mm(node, "height", "RectRound", units)?,
                    },
                    radius: self.mm(node, "radius", "RectRound", units)?,
                    upper_right: self.parse_flag_attr(node, "upperRight")?,
                    upper_left: self.parse_flag_attr(node, "upperLeft")?,
                    lower_right: self.parse_flag_attr(node, "lowerRight")?,
                    lower_left: self.parse_flag_attr(node, "lowerLeft")?,
                },
                units,
            )?)),
            "RectCham" => Ok(StandardPrimitive::RectCham(self.styled(
                node,
                RectCham {
                    size: Size {
                        width: self.mm(node, "width", "RectCham", units)?,
                        height: self.mm(node, "height", "RectCham", units)?,
                    },
                    chamfer: self.mm(node, "chamfer", "RectCham", units)?,
                    upper_right: self.parse_flag_attr(node, "upperRight")?,
                    upper_left: self.parse_flag_attr(node, "upperLeft")?,
                    lower_right: self.parse_flag_attr(node, "lowerRight")?,
                    lower_left: self.parse_flag_attr(node, "lowerLeft")?,
                },
                units,
            )?)),
            "RectCorner" => Ok(StandardPrimitive::RectCorner(self.styled(
                node,
                RectCorner {
                    lower_left: self.point(
                        node,
                        "lowerLeftX",
                        "lowerLeftY",
                        "RectCorner",
                        units,
                    )?,
                    upper_right: self.point(
                        node,
                        "upperRightX",
                        "upperRightY",
                        "RectCorner",
                        units,
                    )?,
                },
                units,
            )?)),
            "Butterfly" => {
                let shape =
                    ButterflyShape::from_ipc(self.required_str(node, "shape", "Butterfly")?)?;
                let attr_name = if matches!(shape, ButterflyShape::Round) {
                    "diameter"
                } else {
                    "side"
                };
                Ok(StandardPrimitive::Butterfly(self.styled(
                    node,
                    Butterfly {
                        shape,
                        size: self.mm(node, attr_name, "Butterfly", units)?,
                    },
                    units,
                )?))
            }
            "Diamond" => Ok(StandardPrimitive::Diamond(self.styled(
                node,
                Diamond {
                    size: Size {
                        width: self.mm(node, "width", "Diamond", units)?,
                        height: self.mm(node, "height", "Diamond", units)?,
                    },
                },
                units,
            )?)),
            "Donut" => {
                let shape = ConcentricShape::from_ipc(self.required_str(node, "shape", "Donut")?)?;
                Ok(StandardPrimitive::Donut(self.styled(
                    node,
                    Donut {
                        shape,
                        outer_diameter: self.mm(node, "outerDiameter", "Donut", units)?,
                        inner_diameter: self.mm(node, "innerDiameter", "Donut", units)?,
                    },
                    units,
                )?))
            }
            "Ellipse" => Ok(StandardPrimitive::Ellipse(self.styled(
                node,
                Ellipse {
                    size: Size {
                        width: self.mm(node, "width", "Ellipse", units)?,
                        height: self.mm(node, "height", "Ellipse", units)?,
                    },
                },
                units,
            )?)),
            "Hexagon" => Ok(StandardPrimitive::Hexagon(self.styled(
                node,
                Hexagon {
                    point_to_point: self.mm(node, "length", "Hexagon", units)?,
                },
                units,
            )?)),
            "Moire" => Ok(StandardPrimitive::Moire(Moire {
                diameter: self.mm(node, "diameter", "Moire", units)?,
                ring_width: self.mm(node, "ringWidth", "Moire", units)?,
                ring_gap: self.mm(node, "ringGap", "Moire", units)?,
                ring_number: self
                    .parse_optional_count_attr(node, "ringNumber", MAX_MOIRE_RINGS)?
                    .ok_or(Ipc2581Error::MissingAttribute {
                        element: "Moire",
                        attr: "ringNumber",
                    })?,
                line_width: self.opt_mm(node, "lineWidth", units)?,
                line_length: self.opt_mm(node, "lineLength", units)?,
                line_angle: self.opt_num(node, "lineAngle")?,
            })),
            "Octagon" => Ok(StandardPrimitive::Octagon(self.styled(
                node,
                Octagon {
                    point_to_point: self.mm(node, "length", "Octagon", units)?,
                },
                units,
            )?)),
            "Thermal" => {
                let shape =
                    ConcentricShape::from_ipc(self.required_str(node, "shape", "Thermal")?)?;
                Ok(StandardPrimitive::Thermal(
                    self.styled(
                        node,
                        Thermal {
                            shape,
                            outer_diameter: self.mm(node, "outerDiameter", "Thermal", units)?,
                            inner_diameter: self.mm(node, "innerDiameter", "Thermal", units)?,
                            // IPC-2581C spokeCountType.
                            spoke_count: self
                                .parse_optional_count_attr(node, "spokeCount", 4)?
                                .unwrap_or(4),
                            spoke_width: self.opt_mm(node, "spokeWidth", units)?,
                            spoke_start_angle: self.opt_num(node, "spokeStartAngle")?,
                        },
                        units,
                    )?,
                ))
            }
            "Triangle" => Ok(StandardPrimitive::Triangle(self.styled(
                node,
                Triangle {
                    base: self.mm(node, "base", "Triangle", units)?,
                    height: self.mm(node, "height", "Triangle", units)?,
                },
                units,
            )?)),
            "Oval" => Ok(StandardPrimitive::Oval(self.styled(
                node,
                Oval {
                    size: Size {
                        width: self.mm(node, "width", "Oval", units)?,
                        height: self.mm(node, "height", "Oval", units)?,
                    },
                },
                units,
            )?)),
            "Contour" => Ok(StandardPrimitive::Contour(self.parse_contour(node, units)?)),
            name => Err(Ipc2581Error::InvalidStructure(format!(
                "Unknown standard primitive: {}",
                name
            ))),
        }
    }

    fn parse_contour(&mut self, node: &Node, units: Units) -> Result<Contour> {
        let (polygon, cutouts) = self.parse_polygon_and_cutouts(node, units, "Polygon")?;
        Ok(Contour { polygon, cutouts })
    }

    /// The `Polygon` and `Cutout` children shared by `Contour` and `Profile`.
    fn parse_polygon_and_cutouts(
        &mut self,
        node: &Node,
        units: Units,
        missing: &'static str,
    ) -> Result<(Polygon, Vec<Polygon>)> {
        let mut polygon = None;
        let mut cutouts = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Polygon" if polygon.is_none() => {
                    polygon = Some(self.parse_polygon(&child, units)?)
                }
                "Cutout" => cutouts.push(self.parse_polygon_container(&child, units)?),
                _ => {}
            }
        }
        Ok((
            polygon.ok_or(Ipc2581Error::MissingElement(missing))?,
            cutouts,
        ))
    }

    fn parse_polygon_container(&mut self, node: &Node, units: Units) -> Result<Polygon> {
        match self.children_named(node, "Polygon").next() {
            Some(polygon) => self.parse_polygon(&polygon, units),
            None => self.parse_polygon(node, units),
        }
    }

    fn parse_polygon(&mut self, node: &Node, units: Units) -> Result<Polygon> {
        Ok(self.parse_poly(node, units, "PolyBegin")?.polygon)
    }

    fn parse_poly(&mut self, node: &Node, units: Units, missing: &'static str) -> Result<Poly> {
        // Nearly every child is a point, so their count sizes the table once.
        let mut points = Vec::with_capacity(self.element_children(node).count());
        let mut curves = Vec::new();
        let mut begin = None;
        let mut xform = None;
        let mut style = ShapeStyle::default();
        // `PolyBegin` comes first in every file seen but is accepted anywhere.
        points.push(Point { x: 0.0, y: 0.0 });
        for child in self.element_children(node) {
            match self.name(&child) {
                "PolyBegin" => begin = Some(self.point(&child, "x", "y", "PolyBegin", units)?),
                "PolyStepSegment" => {
                    points.push(self.point(&child, "x", "y", "PolyStepSegment", units)?)
                }
                "PolyStepCurve" => {
                    curves.push(PolyCurve {
                        point: points.len() as u32,
                        clockwise: self.parse_bool_attr(&child, "clockwise")?,
                        center: self.point(&child, "centerX", "centerY", "PolyStepCurve", units)?,
                    });
                    points.push(self.point(&child, "x", "y", "PolyStepCurve", units)?);
                }
                "Xform" => xform = Some(self.parse_xform(&child, units)?),
                _ => self.parse_style_child(&child, units, &mut style)?,
            }
        }
        points[0] = begin.ok_or(Ipc2581Error::MissingElement(missing))?;
        Ok(Poly {
            polygon: Polygon { points, curves },
            xform,
            style,
        })
    }

    fn point(
        &self,
        node: &Node,
        x: &'static str,
        y: &'static str,
        element: &'static str,
        units: Units,
    ) -> Result<Point> {
        Ok(Point {
            x: self.mm(node, x, element, units)?,
            y: self.mm(node, y, element, units)?,
        })
    }

    fn parse_dictionary_user(&mut self, node: &Node) -> Result<DictionaryUser> {
        let units = self.attr(node, "units").map(Units::from_ipc).transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = self
            .children_named(node, "EntryUser")
            .map(|n| self.parse_entry_user(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryUser { units, entries })
    }

    fn parse_entry_user(&mut self, node: &Node, units: Units) -> Result<EntryUser> {
        let id = self.required_attr(node, "id", "EntryUser")?;
        let (child, shape) = self
            .element_children(node)
            .find_map(|child| {
                self.parse_feature_shape(&child, units)
                    .map(|shape| shape.map(|shape| (child, shape)))
                    .transpose()
            })
            .transpose()?
            .ok_or(Ipc2581Error::MissingElement("UserPrimitive"))?;
        let primitive = self.user_primitive(&child, shape, units)?;

        Ok(EntryUser { id, primitive })
    }

    fn parse_user_special(&mut self, node: &Node, units: Units) -> Result<UserPrimitive> {
        let mut shapes = Vec::new();
        for child in self.element_children(node) {
            let shape = self.parse_feature_shape(&child, units)?.ok_or_else(|| {
                Ipc2581Error::InvalidStructure(format!(
                    "Unexpected {} in UserSpecial",
                    self.name(&child)
                ))
            })?;
            shapes.push(self.user_shape(&child, shape, units)?);
        }

        Ok(UserPrimitive::UserSpecial(UserSpecial { shapes }))
    }

    /// A `Feature` as a user primitive: a `UserSpecial` as itself, anything
    /// else as the only shape of one.
    fn user_primitive(
        &mut self,
        node: &Node,
        shape: FeatureShape,
        units: Units,
    ) -> Result<UserPrimitive> {
        Ok(match shape {
            FeatureShape::UserPrimitive(primitive) => *primitive,
            shape => UserPrimitive::UserSpecial(UserSpecial {
                shapes: vec![self.user_shape(node, shape, units)?],
            }),
        })
    }

    /// A `Feature` parsed from `node` as a member of a `UserSpecial`.
    fn user_shape(&mut self, node: &Node, shape: FeatureShape, units: Units) -> Result<UserShape> {
        let mut style_node = *node;
        let shape = match shape {
            FeatureShape::UserShape(shape) => return Ok(*shape),
            FeatureShape::StandardPrimitive(primitive) => match *primitive {
                StandardPrimitive::Circle(circle) => UserShapeType::Circle(circle.shape),
                StandardPrimitive::RectCenter(rect) => UserShapeType::RectCenter(rect.shape),
                StandardPrimitive::Oval(oval) => UserShapeType::Oval(oval.shape),
                StandardPrimitive::RectRound(rect) => UserShapeType::RectRound(rect.shape),
                StandardPrimitive::Contour(contour) => {
                    // A Contour is styled through its Polygon.
                    style_node = self.children_named(node, "Polygon").next().unwrap_or(*node);
                    UserShapeType::Contour(contour)
                }
                primitive => UserShapeType::StandardPrimitive(primitive),
            },
            FeatureShape::StandardPrimitiveRef(id) => UserShapeType::StandardPrimitiveRef(id),
            FeatureShape::UserPrimitive(primitive) => UserShapeType::UserPrimitive(*primitive),
            FeatureShape::UserPrimitiveRef(id) => UserShapeType::UserPrimitiveRef(id),
            FeatureShape::Text(text) => UserShapeType::Text(*text),
            FeatureShape::Outline(outline) => UserShapeType::Outline(*outline),
        };
        let style = self.parse_fill_and_line_desc(&style_node, units)?;
        Ok(user_shape(shape, style))
    }

    /// The stroked `Simple` members of the `Feature` group, plus the bare
    /// `Polygon` that KiCad writes where a `Feature` belongs.
    fn parse_user_shape(&mut self, node: &Node, units: Units) -> Result<Option<UserShape>> {
        let (shape, style) = match self.name(node) {
            "Polygon" => {
                let Poly { polygon, style, .. } = self.parse_poly(node, units, "PolyBegin")?;
                (UserShapeType::Polygon(polygon), style)
            }
            "Polyline" => {
                let Poly { polygon, style, .. } =
                    self.parse_poly(node, units, "PolyBegin in Polyline")?;
                (UserShapeType::Polyline(polygon), style)
            }
            "Line" => (
                UserShapeType::Line(Line {
                    start: self.point(node, "startX", "startY", "Line", units)?,
                    end: self.point(node, "endX", "endY", "Line", units)?,
                }),
                self.parse_fill_and_line_desc(node, units)?,
            ),
            "Arc" => (
                UserShapeType::Arc(self.parse_user_arc(node, units)?),
                self.parse_fill_and_line_desc(node, units)?,
            ),
            _ => return Ok(None),
        };
        Ok(Some(user_shape(shape, style)))
    }

    fn parse_user_arc(&mut self, node: &Node, units: Units) -> Result<Arc> {
        Ok(Arc {
            start: self.point(node, "startX", "startY", "Arc", units)?,
            end: self.point(node, "endX", "endY", "Arc", units)?,
            center: self.point(node, "centerX", "centerY", "Arc", units)?,
            clockwise: self.parse_bool_attr(node, "clockwise")?,
        })
    }

    fn parse_logistic_header(&mut self, node: &Node) -> Result<LogisticHeader> {
        let mut roles = Vec::new();
        let mut enterprises = Vec::new();
        let mut persons = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "Role" => {
                    let id = self.required_attr(&child, "id", "Role")?;
                    let role_function = self.required_attr(&child, "roleFunction", "Role")?;
                    roles.push(Role { id, role_function });
                }
                "Enterprise" => {
                    let id = self.required_attr(&child, "id", "Enterprise")?;
                    let code = self.required_attr(&child, "code", "Enterprise")?;
                    let name = self.optional_attr(&child, "name");
                    enterprises.push(Enterprise { id, code, name });
                }
                "Person" => {
                    let name = self.required_attr(&child, "name", "Person")?;
                    let email = self.optional_attr(&child, "email");
                    persons.push(Person { name, email });
                }
                _ => {}
            }
        }

        Ok(LogisticHeader {
            roles,
            enterprises,
            persons,
        })
    }

    fn parse_history_record(&mut self, node: &Node) -> Result<HistoryRecord> {
        // historyNumberType is a dotted revision such as "2" or "1.2.3"; the
        // leading component counts the saves.
        let number = self
            .attr(node, "number")
            .ok_or(Ipc2581Error::MissingAttribute {
                element: "HistoryRecord",
                attr: "number",
            })?;
        let number = number
            .split('.')
            .next()
            .and_then(|major| major.trim().parse().ok())
            .ok_or_else(|| {
                Ipc2581Error::InvalidAttribute(format!("Invalid number value: {number}"))
            })?;

        let origination = self.required_attr(node, "origination", "HistoryRecord")?;
        let software = self.optional_attr(node, "software");
        let last_change = self.required_attr(node, "lastChange", "HistoryRecord")?;

        // The schema allows one FileRevision, but pcb up to 0.4.11 appended
        // one per save. The first is the file's own; saving again turns the
        // rest into ChangeRec entries.
        let file_revision = self
            .children_named(node, "FileRevision")
            .next()
            .map(|child| self.parse_file_revision(&child))
            .transpose()?;

        Ok(HistoryRecord {
            number,
            origination,
            software,
            last_change,
            file_revision,
        })
    }

    fn parse_file_revision(&mut self, node: &Node) -> Result<metadata::FileRevision> {
        let file_revision = self.required_attr(node, "fileRevisionId", "FileRevision")?;
        let comment = self.optional_attr(node, "comment");

        // Parse SoftwarePackage child element
        let mut software_package = None;
        for child in self.element_children(node) {
            if self.name(&child) == "SoftwarePackage" {
                software_package = Some(self.parse_software_package(&child)?);
                break;
            }
        }

        Ok(metadata::FileRevision {
            file_revision,
            comment,
            software_package,
        })
    }

    fn parse_software_package(&mut self, node: &Node) -> Result<metadata::SoftwarePackage> {
        let name = self.required_attr(node, "name", "SoftwarePackage")?;
        let revision = self.optional_attr(node, "revision");
        let vendor = self.optional_attr(node, "vendor");

        Ok(metadata::SoftwarePackage {
            name,
            revision,
            vendor,
        })
    }

    // Helper methods
    fn required_attr(
        &mut self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
    ) -> Result<Symbol> {
        self.attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })
            .map(|s| self.interner.intern(s))
    }

    /// A required attribute that is parsed rather than kept, so not interned.
    fn required_str(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
    ) -> Result<&'a str> {
        self.attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })
    }

    fn optional_attr(&mut self, node: &Node, attr: &str) -> Option<Symbol> {
        self.attr(node, attr).map(|s| self.interner.intern(s))
    }

    fn parse_ipc_integer(&self, value: Symbol, attr: &str, positive: bool) -> Result<u32> {
        let source = self.interner.resolve(value).trim();
        let parsed = source.parse::<u32>().map_err(|_| {
            Ipc2581Error::InvalidAttribute(format!("Invalid integer value for {attr}: {source}"))
        })?;
        if parsed > i32::MAX as u32 || (positive && parsed == 0) {
            return Err(Ipc2581Error::InvalidAttribute(format!(
                "Integer value for {attr} is outside the IPC-2581C range: {source}"
            )));
        }
        Ok(parsed)
    }

    /// A required length attribute, in millimeters.
    fn mm(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
        units: Units,
    ) -> Result<f64> {
        self.opt_mm(node, attr, units)?
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })
    }

    /// An optional length attribute, in millimeters.
    fn opt_mm(&self, node: &Node, attr: &'static str, units: Units) -> Result<Option<f64>> {
        self.number(node, attr, Sign::Any, Some(units))
    }

    /// An optional dimensionless attribute.
    fn opt_num(&self, node: &Node, attr: &'static str) -> Result<Option<f64>> {
        self.number(node, attr, Sign::Any, None)
    }

    /// An optional numeric attribute, scaled to millimeters when it has `units`.
    fn number(
        &self,
        node: &Node,
        attr: &'static str,
        sign: Sign,
        units: Option<Units>,
    ) -> Result<Option<f64>> {
        self.attr(node, attr)
            .map(|value| parse_f64(value, attr, sign, units))
            .transpose()
    }

    fn parse_u8_attr(&self, node: &Node, attr: &'static str, element: &'static str) -> Result<u8> {
        let attr_val = self
            .attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })?;
        attr_val.parse().map_err(|_| {
            Ipc2581Error::InvalidAttribute(format!("Invalid u8 value for {} in {}", attr, element))
        })
    }

    /// Parse an optional count, which the importer loops over, within the
    /// bounds the schema gives it.
    fn parse_optional_count_attr(
        &self,
        node: &Node,
        attr: &'static str,
        max: u32,
    ) -> Result<Option<u32>> {
        self.attr(node, attr)
            .map(|value| {
                value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|count| *count <= max)
                    .ok_or_else(|| {
                        Ipc2581Error::InvalidAttribute(format!(
                            "Value for {attr} is not an integer in 0..={max}: {value}"
                        ))
                    })
            })
            .transpose()
    }

    /// An optional boolean attribute: absent is `false`, malformed an error.
    fn parse_flag_attr(&self, node: &Node, attr: &'static str) -> Result<bool> {
        Ok(self
            .attr(node, attr)
            .map(|value| parse_xsd_bool(value, attr))
            .transpose()?
            .unwrap_or(false))
    }

    fn parse_bool_attr(&self, node: &Node, attr: &'static str) -> Result<bool> {
        match self.attr(node, attr) {
            Some(value) => parse_xsd_bool(value, attr),
            None => Err(Ipc2581Error::MissingAttribute {
                element: "unknown",
                attr,
            }),
        }
    }

    fn parse_ecad(&mut self, node: &Node) -> Result<Ecad> {
        // Parse CadHeader first to establish units for the ECAD section
        let cad_header_node = self
            .children_named(node, "CadHeader")
            .next()
            .ok_or(Ipc2581Error::MissingElement("CadHeader"))?;
        let mut cad_header = self.parse_cad_header(&cad_header_node)?;

        // Store ECAD units for use when parsing dimensions
        self.ecad_units = Some(cad_header.units);

        // Move specs into parser context to avoid cloning
        // We'll move them back after parsing CadData
        self.specs = std::mem::take(&mut cad_header.specs);

        let cad_data_node = self
            .children_named(node, "CadData")
            .next()
            .ok_or(Ipc2581Error::MissingElement("CadData"))?;
        let cad_data = self.parse_cad_data(&cad_data_node)?;

        // Move specs back into cad_header
        cad_header.specs = std::mem::take(&mut self.specs);

        Ok(Ecad {
            cad_header,
            cad_data,
        })
    }

    fn parse_cad_header(&mut self, node: &Node) -> Result<CadHeader> {
        let units = self
            .attr(node, "units")
            .ok_or(Ipc2581Error::MissingAttribute {
                element: "CadHeader",
                attr: "units",
            })?;
        let units = Units::from_ipc(units)?;

        // Parse Spec elements
        let mut specs = std::collections::HashMap::new();
        for child in self.element_children(node) {
            if self.name(&child) == "Spec" {
                let spec = self.parse_spec(&child)?;
                specs.insert(spec.name, spec);
            }
        }

        Ok(CadHeader { units, specs })
    }

    fn parse_spec(&mut self, node: &Node) -> Result<ecad::Spec> {
        let name = self.required_attr(node, "name", "Spec")?;

        let mut material = None;
        let mut dielectric_constant = None;
        let mut loss_tangent = None;
        let mut properties = Vec::new();
        let mut surface_finish = None;
        let mut copper_weight_oz = None;
        let mut color_term = None;
        let mut color_rgb = None;
        let mut items = Vec::new();

        // Parse child elements for material and dielectric properties
        for child in self.element_children(node) {
            let item = self.parse_spec_item(&child)?;
            let values = item
                .properties
                .iter()
                .filter_map(|property| Some((property.value?, property.unit)));
            match self.name(&child) {
                "General" if self.attr(&child, "type") == Some("MATERIAL") => {
                    // Look for Property, ColorTerm, and Color elements
                    for prop in self.element_children(&child) {
                        match self.name(&prop) {
                            "Property" => {
                                if let Some(text) = self.attr(&prop, "text")
                                    && !text.is_empty()
                                {
                                    let text_sym = self.interner.intern(text);
                                    // Store all property texts
                                    properties.push(text_sym);
                                    // Take the first non-empty material text we find
                                    if material.is_none() {
                                        material = Some(text_sym);
                                    }
                                }
                            }
                            "ColorTerm" => {
                                // Parse ColorTerm name attribute (e.g., "GREEN", "WHITE", "BLACK")
                                if let Some(color_name) = self.attr(&prop, "name") {
                                    color_term = Some(self.interner.intern(color_name));
                                }
                            }
                            "Color" => {
                                // Parse Color r, g, b attributes (0-255)
                                if let (Some(r_str), Some(g_str), Some(b_str)) = (
                                    self.attr(&prop, "r"),
                                    self.attr(&prop, "g"),
                                    self.attr(&prop, "b"),
                                ) && let (Ok(r), Ok(g), Ok(b)) = (
                                    r_str.parse::<u8>(),
                                    g_str.parse::<u8>(),
                                    b_str.parse::<u8>(),
                                ) {
                                    color_rgb = Some((r, g, b));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "Dielectric" => {
                    let value = values.map(|(value, _)| value).next_back();
                    match self.attr(&child, "type") {
                        Some("DIELECTRIC_CONSTANT") => {
                            dielectric_constant = value.or(dielectric_constant)
                        }
                        Some("LOSS_TANGENT") => loss_tangent = value.or(loss_tangent),
                        _ => {}
                    }
                }
                "Conductor" if self.attr(&child, "type") == Some("WEIGHT") => {
                    // The weight is in ounces unless a unit says otherwise.
                    copper_weight_oz = values
                        .filter(|(_, unit)| {
                            unit.is_none_or(|unit| {
                                self.interner.resolve(unit).eq_ignore_ascii_case("OZ")
                            })
                        })
                        .map(|(value, _)| value)
                        .next_back()
                        .or(copper_weight_oz);
                }
                "SurfaceFinish" => surface_finish = Some(self.parse_surface_finish(&child)?),
                _ => {}
            }
            items.push(item);
        }

        Ok(ecad::Spec {
            name,
            items,
            material,
            dielectric_constant,
            loss_tangent,
            properties,
            surface_finish,
            copper_weight_oz,
            color_term,
            color_rgb,
        })
    }

    fn parse_spec_item(&mut self, node: &Node) -> Result<ecad::SpecItem> {
        let element_name = self.name(node).to_string();
        let element = self.interner.intern(&element_name);
        let item_type = self.attr(node, "type").map(|s| self.interner.intern(s));
        let comment = self.attr(node, "comment").map(|s| self.interner.intern(s));
        let properties = self
            .children_named(node, "Property")
            .map(|child| self.parse_spec_property(&child))
            .collect::<Result<_>>()?;

        Ok(ecad::SpecItem {
            element,
            kind: spec_item_kind(&element_name),
            item_type,
            comment,
            properties,
        })
    }

    fn parse_spec_property(&mut self, node: &Node) -> Result<ecad::SpecProperty> {
        Ok(ecad::SpecProperty {
            value: self.opt_num(node, "value")?,
            text: self.attr(node, "text").map(|s| self.interner.intern(s)),
            unit: self.attr(node, "unit").map(|s| self.interner.intern(s)),
            plus_tol: self.opt_num(node, "plusTol")?,
            minus_tol: self.opt_num(node, "minusTol")?,
            tol_percent: self
                .attr(node, "tolPercent")
                .map(|value| parse_xsd_bool(value, "tolPercent"))
                .transpose()?,
        })
    }

    fn parse_surface_finish(&mut self, node: &Node) -> Result<ecad::SurfaceFinish> {
        // IPC-2581C puts `type`, `comment` and the Products on SurfaceFinish;
        // older KiCad nested them in a Finish child.
        let finish = if self.attr(node, "type").is_some() {
            *node
        } else {
            self.children_named(node, "Finish")
                .next()
                .ok_or(Ipc2581Error::MissingAttribute {
                    element: "SurfaceFinish",
                    attr: "type",
                })?
        };
        // A finish outside the schema's list is still a finish.
        let finish_type = self
            .attr(&finish, "type")
            .and_then(|finish_type| ecad::FinishType::from_ipc(finish_type).ok())
            .unwrap_or(ecad::FinishType::Other);
        let comment = self.optional_attr(&finish, "comment");
        let products = self
            .children_named(&finish, "Product")
            .map(|product| {
                Ok(ecad::FinishProduct {
                    name: self.required_attr(&product, "name", "Product")?,
                    criteria: self
                        .attr(&product, "criteria")
                        .map(ecad::ProductCriteria::from_ipc)
                        .transpose()?,
                })
            })
            .collect::<Result<_>>()?;

        Ok(ecad::SurfaceFinish {
            finish_type,
            comment,
            products,
        })
    }

    fn parse_cad_data(&mut self, node: &Node) -> Result<CadData> {
        let mut steps = Vec::new();
        let mut layers = Vec::new();
        let mut stackups = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "Step" => steps.push(self.parse_step(&child)?),
                "Layer" => layers.push(self.parse_layer(&child)?),
                "Stackup" => stackups.push(self.parse_stackup(&child)?),
                _ => {}
            }
        }

        Ok(CadData {
            steps,
            layers,
            stackups,
        })
    }

    fn parse_stackup(&mut self, node: &Node) -> Result<Stackup> {
        // Stackup is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.required_attr(node, "name", "Stackup")?;

        let overall_thickness = self.opt_mm(node, "overallThickness", units)?;
        let where_measured = self
            .attr(node, "whereMeasured")
            .map(WhereMeasured::from_ipc)
            .transpose()?;
        // A percentage is not a length.
        let tol_percent = self.parse_flag_attr(node, "tolPercent")?;
        let tol_units = (!tol_percent).then_some(units);
        let tol_plus = self.number(node, "tolPlus", Sign::Any, tol_units)?;
        let tol_minus = self.number(node, "tolMinus", Sign::Any, tol_units)?;

        let mut layers = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) == "StackupGroup" {
                // StackupGroup contains StackupLayer elements
                for layer_node in self.element_children(&child) {
                    if self.name(&layer_node) == "StackupLayer" {
                        layers.push(self.parse_stackup_layer(&layer_node)?);
                    }
                }
            }
        }

        Ok(Stackup {
            name,
            overall_thickness,
            where_measured,
            tol_plus,
            tol_minus,
            tol_percent,
            layers,
        })
    }

    fn parse_stackup_layer(&mut self, node: &Node) -> Result<StackupLayer> {
        // StackupLayer is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let layer_ref = self.required_attr(node, "layerOrGroupRef", "StackupLayer")?;

        let thickness = self.opt_mm(node, "thickness", units)?;

        // A percentage is not a length.
        let tol_percent = self.parse_flag_attr(node, "tolPercent")?;
        let tol_units = (!tol_percent).then_some(units);
        let tol_plus = self.number(node, "tolPlus", Sign::Any, tol_units)?;
        let tol_minus = self.number(node, "tolMinus", Sign::Any, tol_units)?;

        // `sequence` is a double in the schema; layers are numbered with whole ones.
        let layer_number = self
            .number(node, "sequence", Sign::NonNegative, None)?
            .map(|sequence| {
                (sequence.fract() == 0.0 && sequence <= f64::from(u32::MAX))
                    .then_some(sequence as u32)
                    .ok_or_else(|| {
                        Ipc2581Error::InvalidAttribute(format!(
                            "StackupLayer sequence is not a whole number: {sequence}"
                        ))
                    })
            })
            .transpose()?;

        // Look up material and dielectric properties from Spec via SpecRef
        let mut material = None;
        let mut spec_ref = None;
        let mut dielectric_constant = None;
        let mut loss_tangent = None;

        // Parse SpecRef child element
        for child in self.element_children(node) {
            if self.name(&child) == "SpecRef"
                && let Some(spec_id) = self.attr(&child, "id")
            {
                // Exact match - pure IPC-2581 spec
                let spec_symbol = self.interner.intern(spec_id);
                if let Some(spec) = self.specs.get(&spec_symbol) {
                    spec_ref = Some(spec_symbol);
                    material = spec.material;
                    dielectric_constant = spec.dielectric_constant;
                    loss_tangent = spec.loss_tangent;
                }
                // If spec not found, silently continue - this is valid per spec
                // (SpecRef may reference specs not in this document)
            }
        }

        Ok(StackupLayer {
            layer_ref,
            thickness,
            tol_plus,
            tol_minus,
            tol_percent,
            material,
            spec_ref,
            dielectric_constant,
            loss_tangent,
            layer_number,
        })
    }

    fn parse_step(&mut self, node: &Node) -> Result<Step> {
        let name = self.required_attr(node, "name", "Step")?;
        let step_type = self
            .attr(node, "type")
            .map(ecad::StepType::from_ipc)
            .transpose()?;

        // Single pass through children
        let mut datum = None;
        let mut profile = None;
        let mut step_repeats = Vec::new();
        let mut padstack_defs = Vec::new();
        let mut packages = Vec::new();
        let mut components = Vec::new();
        let mut logical_nets = Vec::new();
        let mut phy_net_groups = Vec::new();
        let mut layer_features = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "Datum" => datum = Some(self.parse_datum(&child)?),
                "Profile" => profile = Some(self.parse_profile(&child)?),
                "StepRepeat" => step_repeats.push(self.parse_step_repeat(&child)?),
                "PadStackDef" => padstack_defs.push(self.parse_padstack_def(&child)?),
                "Package" => packages.push(self.parse_package(&child)?),
                "Component" => components.push(self.parse_component(&child)?),
                "LogicalNet" => logical_nets.push(self.parse_logical_net(&child)?),
                "PhyNetGroup" => phy_net_groups.push(self.parse_phy_net_group(&child)?),
                "LayerFeature" => layer_features.push(self.parse_layer_feature(&child)?),
                _ => {}
            }
        }

        Ok(Step {
            name,
            step_type,
            datum,
            profile,
            step_repeats,
            padstack_defs,
            packages,
            components,
            logical_nets,
            phy_net_groups,
            layer_features,
        })
    }

    fn parse_step_repeat(&mut self, node: &Node) -> Result<StepRepeat> {
        // StepRepeat is in ECAD section, use ECAD units.
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let step_ref = self.required_attr(node, "stepRef", "StepRepeat")?;
        let x = self.opt_mm(node, "x", units)?.unwrap_or(0.0);
        let y = self.opt_mm(node, "y", units)?.unwrap_or(0.0);
        // The importer bounds the expanded instance count.
        let nx = self
            .parse_optional_count_attr(node, "nx", u32::MAX)?
            .unwrap_or(1);
        let ny = self
            .parse_optional_count_attr(node, "ny", u32::MAX)?
            .unwrap_or(1);
        let dx = self.opt_mm(node, "dx", units)?.unwrap_or(0.0);
        let dy = self.opt_mm(node, "dy", units)?.unwrap_or(0.0);
        let angle = self.opt_num(node, "angle")?.unwrap_or(0.0);
        let mirror = self
            .attr(node, "mirror")
            .map(|value| parse_xsd_bool(value, "mirror"))
            .transpose()?
            .unwrap_or(false);

        Ok(StepRepeat {
            step_ref,
            x,
            y,
            nx,
            ny,
            dx,
            dy,
            angle,
            mirror,
        })
    }

    fn parse_datum(&mut self, node: &Node) -> Result<Datum> {
        // Datum is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let x = self.mm(node, "x", "Datum", units)?;
        let y = self.mm(node, "y", "Datum", units)?;
        Ok(Datum { x, y })
    }

    fn parse_profile(&mut self, node: &Node) -> Result<Profile> {
        // Profile is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let (polygon, cutouts) =
            self.parse_polygon_and_cutouts(node, units, "Polygon in Profile")?;
        Ok(Profile { polygon, cutouts })
    }

    fn parse_package(&mut self, node: &Node) -> Result<Package> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let name = self.required_attr(node, "name", "Package")?;
        let package_type = self.required_attr(node, "type", "Package")?;
        let pin_one = self.optional_attr(node, "pinOne");
        let pin_one_orientation = self.optional_attr(node, "pinOneOrientation");
        let height = self.number(node, "height", Sign::NonNegative, Some(units))?;
        let negative_body_extension = self.number(
            node,
            "negativeBodyExtension",
            Sign::NonNegative,
            Some(units),
        )?;
        let comment = self.optional_attr(node, "comment");

        let mut outline = None;
        let mut pickup_point = None;
        let mut land_pattern = None;
        let mut silkscreen = None;
        let mut assembly_drawing = None;
        let mut pins = Vec::new();
        let mut topside = None;
        let mut other_side_view = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "Outline" => outline = Some(self.parse_package_outline(&child, units)?),
                "PickupPoint" => pickup_point = Some(self.parse_location(&child, units)?),
                "LandPattern" => {
                    land_pattern = Some(self.parse_package_land_pattern(&child, units)?)
                }
                "SilkScreen" => silkscreen = Some(self.parse_package_silkscreen(&child, units)?),
                "AssemblyDrawing" => {
                    assembly_drawing = Some(self.parse_package_assembly_drawing(&child, units)?)
                }
                "Pin" => pins.push(self.parse_package_pin(&child, units)?),
                "Topside" => topside = Some(self.parse_package_side_view(&child, units)?),
                "OtherSideView" => {
                    other_side_view = Some(self.parse_package_other_side_view(&child, units)?)
                }
                _ => {}
            }
        }

        Ok(Package {
            name,
            package_type,
            pin_one,
            pin_one_orientation,
            height,
            negative_body_extension,
            comment,
            outline,
            pickup_point,
            land_pattern,
            silkscreen,
            assembly_drawing,
            pins,
            topside,
            other_side_view,
        })
    }

    fn parse_package_outline(&mut self, node: &Node, units: Units) -> Result<PackageOutline> {
        let polygon_node = self
            .children_named(node, "Polygon")
            .next()
            .ok_or(Ipc2581Error::MissingElement("Polygon in Package Outline"))?;
        let Poly {
            polygon,
            xform,
            style,
        } = self.parse_poly(&polygon_node, units, "PolyBegin")?;
        let line_desc =
            self.parse_line_desc_group(node, units, "LineDescGroup in Package Outline")?;
        Ok(PackageOutline {
            polygon,
            polygon_xform: xform,
            polygon_line_desc: style.line_desc,
            polygon_line_desc_ref: style.line_desc_ref,
            polygon_fill_desc: style.fill_desc,
            polygon_fill_desc_ref: style.fill_desc_ref,
            line_desc,
        })
    }

    fn parse_package_land_pattern(
        &mut self,
        node: &Node,
        units: Units,
    ) -> Result<PackageLandPattern> {
        let mut pads = Vec::new();
        let mut targets = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Pad" => pads.push(self.parse_pad(&child)?),
                "Target" => targets.push(self.parse_package_target(&child, units)?),
                _ => {}
            }
        }
        Ok(PackageLandPattern { pads, targets })
    }

    fn parse_package_target(&mut self, node: &Node, units: Units) -> Result<PackageTarget> {
        let xform = self.parse_xform_child(node, units)?;
        let location_node = self
            .children_named(node, "Location")
            .next()
            .ok_or(Ipc2581Error::MissingElement("Location in Package Target"))?;
        let location = self.parse_location(&location_node, units)?;
        let shape = self
            .element_children(node)
            .find_map(|child| self.parse_standard_shape(&child, units).transpose())
            .transpose()?
            .ok_or(Ipc2581Error::MissingElement(
                "StandardShape in Package Target",
            ))?;
        Ok(PackageTarget {
            xform,
            location,
            shape,
        })
    }

    fn parse_package_silkscreen(&mut self, node: &Node, units: Units) -> Result<PackageSilkscreen> {
        let mut outlines = Vec::new();
        let mut markings = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Outline" => outlines.push(self.parse_package_outline(&child, units)?),
                "Marking" => markings.push(self.parse_package_marking(&child, units)?),
                _ => {}
            }
        }
        Ok(PackageSilkscreen { outlines, markings })
    }

    fn parse_package_assembly_drawing(
        &mut self,
        node: &Node,
        units: Units,
    ) -> Result<PackageAssemblyDrawing> {
        let mut outline = None;
        let mut markings = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Outline" => outline = Some(self.parse_package_outline(&child, units)?),
                "Marking" => markings.push(self.parse_package_marking(&child, units)?),
                _ => {}
            }
        }
        Ok(PackageAssemblyDrawing { outline, markings })
    }

    fn parse_package_marking(&mut self, node: &Node, units: Units) -> Result<PackageMarking> {
        let usage = self.optional_attr(node, "markingUsage");
        let xform = self.parse_xform_child(node, units)?;
        let location = self
            .children_named(node, "Location")
            .next()
            .map(|child| self.parse_location(&child, units))
            .transpose()?;
        let feature = self
            .element_children(node)
            .find_map(|child| self.parse_feature_shape(&child, units).transpose())
            .transpose()?
            .ok_or(Ipc2581Error::MissingElement("Feature in Package Marking"))?;
        Ok(PackageMarking {
            usage,
            xform,
            location,
            feature,
        })
    }

    fn parse_package_side_view(&mut self, node: &Node, units: Units) -> Result<PackageSideView> {
        let mut outline = None;
        let mut land_pattern = None;
        let mut silkscreen = None;
        let mut assembly_drawing = None;
        let mut pins = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Outline" => outline = Some(self.parse_package_outline(&child, units)?),
                "LandPattern" => {
                    land_pattern = Some(self.parse_package_land_pattern(&child, units)?)
                }
                "SilkScreen" => silkscreen = Some(self.parse_package_silkscreen(&child, units)?),
                "AssemblyDrawing" => {
                    assembly_drawing = Some(self.parse_package_assembly_drawing(&child, units)?)
                }
                "Pin" => pins.push(self.parse_package_pin(&child, units)?),
                _ => {}
            }
        }
        Ok(PackageSideView {
            outline,
            land_pattern,
            silkscreen,
            assembly_drawing,
            pins,
        })
    }

    fn parse_package_other_side_view(
        &mut self,
        node: &Node,
        units: Units,
    ) -> Result<PackageOtherSideView> {
        let mut outline = None;
        let mut silkscreen = None;
        let mut assembly_drawing = None;
        for child in self.element_children(node) {
            match self.name(&child) {
                "Outline" => outline = Some(self.parse_package_outline(&child, units)?),
                "SilkScreen" => silkscreen = Some(self.parse_package_silkscreen(&child, units)?),
                "AssemblyDrawing" => {
                    assembly_drawing = Some(self.parse_package_assembly_drawing(&child, units)?)
                }
                _ => {}
            }
        }
        Ok(PackageOtherSideView {
            outline,
            silkscreen,
            assembly_drawing,
        })
    }

    fn parse_package_pin(&mut self, node: &Node, units: Units) -> Result<PackagePin> {
        let number = self.required_attr(node, "number", "Pin")?;
        let name = self.optional_attr(node, "name");
        let pin_type = PackagePinType::from_ipc(self.required_str(node, "type", "Pin")?)?;
        let electrical_type = self
            .attr(node, "electricalType")
            .map(PackagePinElectricalType::from_ipc)
            .transpose()?;
        let mount_type = self
            .attr(node, "mountType")
            .map(PackagePinMountType::from_ipc)
            .transpose()?;
        let polarity = self
            .attr(node, "pinPolarity")
            .map(PackagePinPolarity::from_ipc)
            .transpose()?;
        let xform = self.parse_xform_child(node, units)?;
        let location = self
            .children_named(node, "Location")
            .next()
            .map(|child| self.parse_location(&child, units))
            .transpose()?;
        let shape = self
            .element_children(node)
            .find_map(|child| self.parse_standard_shape(&child, units).transpose())
            .transpose()?
            .ok_or(Ipc2581Error::MissingElement("StandardShape in Package Pin"))?;
        Ok(PackagePin {
            number,
            name,
            pin_type,
            electrical_type,
            mount_type,
            polarity,
            xform,
            location,
            shape,
        })
    }

    /// The `StandardShape` half of the `Feature` group.
    fn parse_standard_shape(&mut self, node: &Node, units: Units) -> Result<Option<StandardShape>> {
        Ok(match self.parse_feature_shape(node, units)? {
            Some(FeatureShape::StandardPrimitive(primitive)) => {
                Some(StandardShape::Primitive(primitive))
            }
            Some(FeatureShape::StandardPrimitiveRef(id)) => Some(StandardShape::PrimitiveRef(id)),
            _ => None,
        })
    }

    /// The IPC-2581C `Feature` substitution group; `None` for any other
    /// element. Every element that holds a `Feature` parses it through here.
    fn parse_feature_shape(&mut self, node: &Node, units: Units) -> Result<Option<FeatureShape>> {
        let shape = match self.name(node) {
            "StandardPrimitiveRef" => FeatureShape::StandardPrimitiveRef(self.required_attr(
                node,
                "id",
                "StandardPrimitiveRef",
            )?),
            "UserPrimitiveRef" => FeatureShape::UserPrimitiveRef(self.required_attr(
                node,
                "id",
                "UserPrimitiveRef",
            )?),
            "UserSpecial" => {
                FeatureShape::UserPrimitive(Box::new(self.parse_user_special(node, units)?))
            }
            "Text" => FeatureShape::Text(Box::new(self.parse_text(node, units)?)),
            "Outline" => FeatureShape::Outline(Box::new(self.parse_package_outline(node, units)?)),
            name if is_standard_primitive_name(name) => FeatureShape::StandardPrimitive(Box::new(
                self.parse_standard_primitive(node, units)?,
            )),
            _ => {
                return Ok(self
                    .parse_user_shape(node, units)?
                    .map(|shape| FeatureShape::UserShape(Box::new(shape))));
            }
        };
        Ok(Some(shape))
    }

    fn parse_text(&mut self, node: &Node, units: Units) -> Result<Text> {
        let text_string = self.required_attr(node, "textString", "Text")?;
        let font_size_raw = self.required_attr(node, "fontSize", "Text")?;
        let font_size = self.parse_ipc_integer(font_size_raw, "fontSize", true)?;
        let xform = self.parse_xform_child(node, units)?;
        let bounding_box_node = self
            .children_named(node, "BoundingBox")
            .next()
            .ok_or(Ipc2581Error::MissingElement("BoundingBox in Text"))?;
        let bounding_box = BoundingBox {
            lower_left: self.point(
                &bounding_box_node,
                "lowerLeftX",
                "lowerLeftY",
                "BoundingBox",
                units,
            )?,
            upper_right: self.point(
                &bounding_box_node,
                "upperRightX",
                "upperRightY",
                "BoundingBox",
                units,
            )?,
        };
        let font_ref = self
            .children_named(node, "FontRef")
            .next()
            .map(|child| self.required_attr(&child, "id", "FontRef"))
            .transpose()?;
        let color = self
            .element_children(node)
            .find_map(|child| self.parse_color_group(&child).transpose())
            .transpose()?;
        Ok(Text {
            text_string,
            font_size,
            font_size_raw,
            xform,
            bounding_box,
            font_ref,
            color,
        })
    }

    fn parse_location(&self, node: &Node, units: Units) -> Result<Location> {
        let Point { x, y } = self.point(node, "x", "y", "Location", units)?;
        Ok(Location { x, y })
    }

    fn parse_component(&mut self, node: &Node) -> Result<Component> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let ref_des = self.optional_attr(node, "refDes");
        let package_ref = self.optional_attr(node, "packageRef");
        let mat_des = self.optional_attr(node, "matDes");
        let layer_ref = self.required_attr(node, "layerRef", "Component")?;
        let layer_ref_topside = self.optional_attr(node, "layerRefTopside");
        let mount_type = MountType::from_ipc(self.attr(node, "mountType").ok_or(
            Ipc2581Error::MissingAttribute {
                element: "Component",
                attr: "mountType",
            },
        )?)?;
        let part = self.required_attr(node, "part", "Component")?;
        let model_ref = self.optional_attr(node, "modelRef");
        let weight = self.number(node, "weight", Sign::NonNegative, None)?;
        let height = self.number(node, "height", Sign::NonNegative, Some(units))?;
        let standoff = self.number(node, "standoff", Sign::NonNegative, Some(units))?;

        let mut nonstandard_attributes = Vec::new();
        let mut xform = None;
        let mut location = None;
        let mut slot_cavity_ref = None;
        let mut spec_refs = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "NonstandardAttribute" => {
                    nonstandard_attributes.push(self.parse_nonstandard_attribute(&child)?);
                }
                "Xform" => {
                    xform = Some(self.parse_xform(&child, units)?);
                }
                "Location" => {
                    location = Some(self.parse_location(&child, units)?);
                }
                "SlotCavityRef" => {
                    slot_cavity_ref = self.optional_attr(&child, "id");
                }
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                _ => {}
            }
        }

        Ok(Component {
            ref_des,
            package_ref,
            mat_des,
            layer_ref,
            mount_type,
            part,
            layer_ref_topside,
            model_ref,
            weight,
            height,
            standoff,
            location: location.ok_or(Ipc2581Error::MissingElement("Location"))?,
            xform,
            nonstandard_attributes,
            slot_cavity_ref,
            spec_refs,
        })
    }

    fn parse_logical_net(&mut self, node: &Node) -> Result<LogicalNet> {
        let name = self.required_attr(node, "name", "LogicalNet")?;

        let pin_refs = self
            .children_named(node, "PinRef")
            .map(|n| self.parse_pin_ref(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(LogicalNet { name, pin_refs })
    }

    fn parse_pin_ref(&mut self, node: &Node) -> Result<PinRef> {
        let component_ref = self
            .attr(node, "componentRef")
            .map(|s| self.interner.intern(s));
        let pin = self.required_attr(node, "pin", "PinRef")?;
        let title = self.attr(node, "title").map(|s| self.interner.intern(s));
        Ok(PinRef {
            component_ref,
            pin,
            title,
        })
    }

    fn parse_phy_net_group(&mut self, node: &Node) -> Result<PhyNetGroup> {
        let name = self.required_attr(node, "name", "PhyNetGroup")?;
        Ok(PhyNetGroup { name })
    }

    fn parse_layer(&mut self, node: &Node) -> Result<Layer> {
        let name = self.required_attr(node, "name", "Layer")?;
        let layer_function =
            LayerFunction::from_ipc(self.required_str(node, "layerFunction", "Layer")?)?;

        let side = self.attr(node, "side").map(Side::from_ipc).transpose()?;
        let polarity = self
            .attr(node, "polarity")
            .map(Polarity::from_ipc)
            .transpose()?;

        let mut span = None;
        let mut profiles = Vec::new();
        let mut spec_refs = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                "Span" => {
                    span = Some(ecad::LayerSpan {
                        from_layer: self
                            .attr(&child, "fromLayer")
                            .map(|s| self.interner.intern(s)),
                        to_layer: self
                            .attr(&child, "toLayer")
                            .map(|s| self.interner.intern(s)),
                    });
                }
                "Profile" => profiles.push(self.parse_profile(&child)?),
                _ => {}
            }
        }

        Ok(Layer {
            name,
            layer_function,
            side,
            polarity,
            span,
            spec_refs,
            profiles,
        })
    }

    fn parse_layer_feature(&mut self, node: &Node) -> Result<LayerFeature> {
        let mut layer = LayerFeature {
            layer_ref: self.required_attr(node, "layerRef", "LayerFeature")?,
            sets: Vec::new(),
            features: Vec::new(),
            spec_refs: Vec::new(),
            nonstandard_attributes: Vec::new(),
        };
        for set in self.children_named(node, "Set") {
            self.parse_feature_set(&set, &mut layer)?;
        }
        layer.sets.shrink_to_fit();
        layer.features.shrink_to_fit();
        layer.spec_refs.shrink_to_fit();
        layer.nonstandard_attributes.shrink_to_fit();
        Ok(layer)
    }

    /// Appends the `Set` at `node`, and what it spans, to `layer`.
    fn parse_feature_set(&mut self, node: &Node, layer: &mut LayerFeature) -> Result<()> {
        let net = self.attr(node, "net").map(|s| self.interner.intern(s));
        let geometry = self.attr(node, "geometry").map(|s| self.interner.intern(s));
        let component_ref = self
            .attr(node, "componentRef")
            .map(|s| self.interner.intern(s));
        let geometry_usage = self
            .attr(node, "geometryUsage")
            .map(GeometryUsage::from_ipc)
            .transpose()?;

        let polarity = self
            .attr(node, "polarity")
            .map(Polarity::from_ipc)
            .transpose()?;

        let LayerFeature {
            features,
            spec_refs,
            nonstandard_attributes,
            ..
        } = layer;
        let starts = (
            features.len(),
            spec_refs.len(),
            nonstandard_attributes.len(),
        );

        for child in self.element_children(node) {
            match self.name(&child) {
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                "Hole" => features.push(ecad::SetFeature::Hole(self.parse_hole(&child)?)),
                "SlotCavity" => features.push(ecad::SetFeature::Slot(Box::new(
                    self.parse_slot_cavity(&child)?,
                ))),
                "Pad" => features.push(ecad::SetFeature::Pad(self.parse_pad(&child)?)),
                name if ecad::FiducialKind::from_ipc(name).is_ok() => features.push(
                    ecad::SetFeature::Fiducial(Box::new(self.parse_fiducial(&child)?)),
                ),
                "Polyline" => {
                    features.push(ecad::SetFeature::Stroke(self.parse_set_polyline(&child)?))
                }
                "Features" => self.parse_features(&child, features)?,
                "NonstandardAttribute" => {
                    nonstandard_attributes.push(self.parse_nonstandard_attribute(&child)?);
                }
                _ => {}
            }
        }

        layer.sets.push(FeatureSet {
            net,
            geometry,
            component_ref,
            geometry_usage,
            polarity,
            spec_refs: span(starts.1, layer.spec_refs.len()),
            features: span(starts.0, layer.features.len()),
            nonstandard_attributes: span(starts.2, layer.nonstandard_attributes.len()),
        });
        Ok(())
    }

    fn parse_nonstandard_attribute(&mut self, node: &Node) -> Result<ecad::NonstandardAttribute> {
        let name = self.required_attr(node, "name", "NonstandardAttribute")?;
        let value = self.attr(node, "value").map(|s| self.interner.intern(s));
        let attr_type = self.attr(node, "type").map(|s| self.interner.intern(s));

        Ok(ecad::NonstandardAttribute {
            name,
            value,
            attr_type,
        })
    }

    fn parse_fiducial(&mut self, node: &Node) -> Result<ecad::Fiducial> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let kind = ecad::FiducialKind::from_ipc(self.name(node))?;

        let mut location = None;
        let xform = self.parse_xform_child(node, units)?;
        let mut shape = None;
        let mut pin_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "Location" => {
                    location = Some(self.parse_location(&child, units)?);
                }
                "PinRef" => pin_ref = Some(self.parse_pin_ref(&child)?),
                _ => match self.parse_standard_shape(&child, units)? {
                    Some(StandardShape::Primitive(primitive)) => {
                        shape = Some(ecad::FiducialShape::Primitive(*primitive));
                    }
                    Some(StandardShape::PrimitiveRef(id)) => {
                        shape = Some(ecad::FiducialShape::StandardPrimitiveRef(id));
                    }
                    None => {}
                },
            }
        }

        Ok(ecad::Fiducial {
            kind,
            location: location.ok_or(Ipc2581Error::MissingElement("Location"))?,
            xform,
            shape: shape.ok_or(Ipc2581Error::MissingElement("StandardShape"))?,
            pin_ref,
        })
    }

    /// Appends the members of the `Features` at `features_node` to `out`.
    fn parse_features(
        &mut self,
        features_node: &Node,
        out: &mut Vec<ecad::SetFeature>,
    ) -> Result<()> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let mut locations = Vec::new();
        let mut xform = None;
        // IPC-2581C specifies one Feature child, but KiCad emits multiple
        // substitution-group children in a single Features container. Accept
        // that de facto shape without relaxing the container's child ordering.
        let start = out.len();

        for child in self.element_children(features_node) {
            let child_name = self.name(&child);
            let offset = single_feature_offset(&locations, xform);
            match child_name {
                "Xform" => {
                    if xform.is_some() || !locations.is_empty() || out.len() > start {
                        return Err(Ipc2581Error::InvalidStructure(
                            "Xform must be the first child of Features".to_string(),
                        ));
                    }
                    xform = Some(self.parse_xform(&child, units)?);
                }
                "Location" => {
                    if out.len() > start {
                        return Err(Ipc2581Error::InvalidStructure(
                            "Location must precede the Feature in Features".to_string(),
                        ));
                    }
                    locations.push(self.point(&child, "x", "y", "Location", units)?);
                }
                name => {
                    let shape = self.parse_feature_shape(&child, units)?.ok_or_else(|| {
                        Ipc2581Error::InvalidStructure(format!("Unexpected {name} in Features"))
                    })?;
                    out.push(self.set_feature(&child, shape, units, offset)?);
                }
            }
        }

        if out.len() == start {
            return Err(Ipc2581Error::MissingElement("Feature in Features"));
        }

        if locations.len() > 1 || xform.is_some() {
            if locations.is_empty() {
                locations.push(Point { x: 0.0, y: 0.0 });
            }
            let features = out.split_off(start);
            out.push(ecad::SetFeature::PlacementGroup(Box::new(
                ecad::FeaturePlacementGroup {
                    xform,
                    locations,
                    features,
                },
            )));
        }
        Ok(())
    }

    /// A `Feature` parsed from `node` as a `Features` member placed at `at`.
    fn set_feature(
        &mut self,
        node: &Node,
        shape: FeatureShape,
        units: Units,
        at: Point,
    ) -> Result<ecad::SetFeature> {
        let (x, y) = (at.x, at.y);
        let stroked = match shape {
            FeatureShape::StandardPrimitiveRef(id) => {
                return Ok(ecad::SetFeature::StandardPrimitiveRef(
                    ecad::FeaturePrimitiveRef { id, x, y },
                ));
            }
            FeatureShape::UserPrimitiveRef(id) => {
                return Ok(ecad::SetFeature::UserPrimitiveRef(
                    ecad::FeaturePrimitiveRef { id, x, y },
                ));
            }
            FeatureShape::UserShape(shape) => *shape,
            shape => {
                let primitive = self.user_primitive(node, shape, units)?;
                return Ok(ecad::SetFeature::UserPrimitive(
                    ecad::FeatureUserPrimitive { primitive, x, y },
                ));
            }
        };

        let line_desc = line_desc_group(stroked.line_desc_ref, stroked.line_desc);
        let moved = |point: Point| Point {
            x: point.x + x,
            y: point.y + y,
        };
        let path = match stroked.shape {
            UserShapeType::Polygon(mut polygon) => {
                polygon.translate(at);
                return Ok(ecad::SetFeature::Polygon(polygon));
            }
            UserShapeType::Line(line) => ecad::StrokePath::Line(Line {
                start: moved(line.start),
                end: moved(line.end),
            }),
            UserShapeType::Arc(arc) => ecad::StrokePath::Arc(Arc {
                start: moved(arc.start),
                end: moved(arc.end),
                center: moved(arc.center),
                clockwise: arc.clockwise,
            }),
            UserShapeType::Polyline(mut polyline) => {
                polyline.translate(at);
                ecad::StrokePath::Polyline(polyline)
            }
            _ => unreachable!("parse_user_shape yields only stroked shapes and polygons"),
        };
        Ok(ecad::SetFeature::Stroke(ecad::Stroke { path, line_desc }))
    }

    fn parse_hole(&mut self, node: &Node) -> Result<Hole> {
        // Hole is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.attr(node, "name").map(|s| self.interner.intern(s));
        let shape = ecad::HoleShape::from_ipc(self.attr(node, "type").unwrap_or("CIRCLE"))?;
        let diameter = self.mm(node, "diameter", "Hole", units)?;
        let plating_status =
            PlatingStatus::from_ipc(self.required_str(node, "platingStatus", "Hole")?)?;
        let x = self.mm(node, "x", "Hole", units)?;
        let y = self.mm(node, "y", "Hole", units)?;
        let mut xform = None;
        let mut spec_refs = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                "Xform" => xform = Some(self.parse_xform(&child, units)?),
                _ => {}
            }
        }

        Ok(Hole {
            name,
            shape,
            diameter,
            plating_status,
            xform,
            spec_refs,
            x,
            y,
        })
    }

    fn parse_slot_cavity(&mut self, node: &Node) -> Result<Slot> {
        // SlotCavity is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.attr(node, "name").map(|s| self.interner.intern(s));
        let plating_status =
            PlatingStatus::from_ipc(self.required_str(node, "platingStatus", "SlotCavity")?)?;

        // Allegro and KiCad revision B omit the Location; the shape is then
        // in step coordinates.
        let (mut x, mut y) = (0.0, 0.0);
        let mut xform = None;
        let mut shape = None;
        for child in self.element_children(node) {
            match self.name(&child) {
                "Location" => {
                    x = self.mm(&child, "x", "Location", units)?;
                    y = self.mm(&child, "y", "Location", units)?;
                }
                "Xform" => xform = Some(self.parse_xform(&child, units)?),
                name if shape.is_none() => {
                    shape = match self.parse_feature_shape(&child, units)? {
                        Some(FeatureShape::Outline(outline)) => {
                            Some(SlotShape::Outline(outline.polygon))
                        }
                        Some(FeatureShape::StandardPrimitive(primitive)) => {
                            Some(SlotShape::Primitive(*primitive))
                        }
                        Some(_) => {
                            return Err(Ipc2581Error::InvalidStructure(format!(
                                "Unsupported {name} shape in SlotCavity"
                            )));
                        }
                        None => None,
                    };
                }
                _ => {}
            }
        }
        let shape = shape.ok_or(Ipc2581Error::MissingElement("Feature in SlotCavity"))?;

        let z_axis_dim = has_z_axis_dim(self.doc(), node);

        Ok(Slot {
            name,
            shape,
            plating_status,
            z_axis_dim,
            xform,
            x,
            y,
        })
    }

    fn parse_pad(&mut self, node: &Node) -> Result<Pad> {
        // Pad is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let padstack_def_ref = self
            .attr(node, "padstackDefRef")
            .map(|s| self.interner.intern(s));

        // x and y attributes are a legacy form of the Location child.
        let mut x = self.opt_mm(node, "x", units)?;
        let mut y = self.opt_mm(node, "y", units)?;

        let mut xform = None;
        let mut feature = None;
        let mut pin_ref = None;
        for child in self.element_children(node) {
            match self.name(&child) {
                "Location" => {
                    let location = self.parse_location(&child, units)?;
                    (x, y) = (Some(location.x), Some(location.y));
                }
                "Xform" => xform = Some(self.parse_xform(&child, units)?),
                "PinRef" => pin_ref = Some(self.parse_pin_ref(&child)?),
                _ if feature.is_none() => feature = self.parse_feature_shape(&child, units)?,
                _ => {}
            }
        }

        Ok(Pad {
            padstack_def_ref,
            x,
            y,
            xform,
            feature,
            pin_ref,
        })
    }

    /// A `Polyline` directly inside a `Set`, which may name its `LineDescRef`
    /// in an attribute.
    fn parse_set_polyline(&mut self, node: &Node) -> Result<ecad::Stroke> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let Poly { polygon, style, .. } = self.parse_poly(node, units, "PolyBegin in Polyline")?;
        let reference = style
            .line_desc_ref
            .or_else(|| self.optional_attr(node, "lineDescRef"));
        let line_desc = line_desc_group(reference, style.line_desc);
        Ok(ecad::Stroke {
            path: ecad::StrokePath::Polyline(polygon),
            line_desc,
        })
    }

    fn parse_padstack_def(&mut self, node: &Node) -> Result<PadStackDef> {
        let name = self.required_attr(node, "name", "PadStackDef")?;

        let mut hole_def = None;
        let mut pad_defs = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "PadstackHoleDef" => hole_def = Some(self.parse_padstack_hole_def(&child)?),
                "PadstackPadDef" => pad_defs.push(self.parse_padstack_pad_def(&child)?),
                _ => {}
            }
        }

        Ok(PadStackDef {
            name,
            hole_def,
            pad_defs,
        })
    }

    fn parse_padstack_hole_def(&mut self, node: &Node) -> Result<PadstackHoleDef> {
        // PadstackHoleDef is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.required_attr(node, "name", "PadstackHoleDef")?;
        let diameter = self.mm(node, "diameter", "PadstackHoleDef", units)?;
        let plating_status = PlatingStatus::from_ipc(self.required_str(
            node,
            "platingStatus",
            "PadstackHoleDef",
        )?)?;
        let plus_tol = self.mm(node, "plusTol", "PadstackHoleDef", units)?;
        let minus_tol = self.mm(node, "minusTol", "PadstackHoleDef", units)?;
        let x = self.mm(node, "x", "PadstackHoleDef", units)?;
        let y = self.mm(node, "y", "PadstackHoleDef", units)?;

        Ok(PadstackHoleDef {
            name,
            diameter,
            plating_status,
            plus_tol,
            minus_tol,
            x,
            y,
        })
    }

    fn parse_padstack_pad_def(&mut self, node: &Node) -> Result<PadstackPadDef> {
        let layer_ref = self.required_attr(node, "layerRef", "PadstackPadDef")?;
        let pad_use = PadUse::from_ipc(self.required_str(node, "padUse", "PadstackPadDef")?)?;

        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let (mut x, mut y) = (0.0, 0.0);
        let mut xform = None;
        let mut feature = None;
        for child in self.element_children(node) {
            match self.name(&child) {
                "Xform" => xform = Some(self.parse_xform(&child, units)?),
                "Location" => {
                    let location = self.parse_location(&child, units)?;
                    (x, y) = (location.x, location.y);
                }
                _ if feature.is_none() => feature = self.parse_feature_shape(&child, units)?,
                _ => {}
            }
        }

        Ok(PadstackPadDef {
            layer_ref,
            pad_use,
            xform,
            x,
            y,
            feature,
        })
    }

    fn parse_bom(&mut self, node: &Node) -> Result<Bom> {
        let name = self.required_attr(node, "name", "Bom")?;
        let mut header = None;
        let mut items = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "BomHeader" => header = Some(self.parse_bom_header(&child)?),
                "BomItem" => items.push(self.parse_bom_item(&child)?),
                _ => {}
            }
        }
        Ok(Bom {
            name,
            header,
            items,
        })
    }

    fn parse_bom_header(&mut self, node: &Node) -> Result<BomHeader> {
        let assembly = self.required_attr(node, "assembly", "BomHeader")?;
        let revision = self.required_attr(node, "revision", "BomHeader")?;
        let affecting = self
            .attr(node, "affecting")
            .map(|value| parse_xsd_bool(value, "affecting"))
            .transpose()?;
        let mut step_refs = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) == "StepRef" {
                step_refs.push(self.required_attr(&child, "name", "StepRef")?);
            }
        }
        Ok(BomHeader {
            assembly,
            revision,
            affecting,
            step_refs,
        })
    }

    fn parse_bom_item(&mut self, node: &Node) -> Result<BomItem> {
        let oem_design_number_ref = self.required_attr(node, "OEMDesignNumberRef", "BomItem")?;
        let quantity_raw = self.required_attr(node, "quantity", "BomItem")?;
        let quantity = self.interner.resolve(quantity_raw).parse().ok();
        let pin_count_raw = self.optional_attr(node, "pinCount");
        let pin_count = pin_count_raw
            .map(|value| self.parse_ipc_integer(value, "pinCount", false))
            .transpose()?;
        let category = self
            .attr(node, "category")
            .map(BomCategory::from_ipc)
            .transpose()?;
        let internal_part_number = self.optional_attr(node, "internalPartNumber");
        let description = self.optional_attr(node, "description");

        let mut designators = Vec::new();
        let mut characteristics = None;
        let mut spec_refs = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "RefDes" => {
                    designators.push(BomDesignator::Reference(self.parse_bom_ref_des(&child)?))
                }
                "MatDes" => designators.push(BomDesignator::Material(
                    self.parse_bom_named_designator(&child, "MatDes")?,
                )),
                "DocDes" => designators.push(BomDesignator::Document(
                    self.parse_bom_named_designator(&child, "DocDes")?,
                )),
                "ToolDes" => designators.push(BomDesignator::Tool(
                    self.parse_bom_named_designator(&child, "ToolDes")?,
                )),
                "FindDes" => {
                    designators.push(BomDesignator::Find(self.parse_bom_find_designator(&child)?))
                }
                "Characteristics" => characteristics = Some(self.parse_characteristics(&child)?),
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                _ => {}
            }
        }

        Ok(BomItem {
            oem_design_number_ref,
            quantity,
            quantity_raw,
            pin_count,
            pin_count_raw,
            category,
            internal_part_number,
            description,
            designators,
            characteristics,
            spec_refs,
        })
    }

    fn parse_bom_ref_des(&mut self, node: &Node) -> Result<BomRefDes> {
        let name = self.required_attr(node, "name", "RefDes")?;
        let package_ref = self.optional_attr(node, "packageRef");
        let layer_ref = self.optional_attr(node, "layerRef");
        let model_ref = self.optional_attr(node, "modelRef");
        let populate = self
            .attr(node, "populate")
            .map(|value| parse_xsd_bool(value, "populate"))
            .transpose()?;
        let mut tunings = Vec::new();
        let mut firmwares = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Tuning" => tunings.push(BomTuning {
                    value: self.required_attr(&child, "value", "Tuning")?,
                    comments: self.optional_attr(&child, "comments"),
                }),
                "Firmware" => firmwares.push(self.parse_bom_firmware(&child)?),
                _ => {}
            }
        }

        Ok(BomRefDes {
            name,
            package_ref,
            populate,
            layer_ref,
            model_ref,
            tunings,
            firmwares,
        })
    }

    fn parse_bom_named_designator(
        &mut self,
        node: &Node,
        element: &'static str,
    ) -> Result<BomNamedDesignator> {
        Ok(BomNamedDesignator {
            name: self.required_attr(node, "name", element)?,
            layer_ref: self.optional_attr(node, "layerRef"),
        })
    }

    fn parse_bom_find_designator(&mut self, node: &Node) -> Result<BomFindDesignator> {
        let number_raw = self.required_attr(node, "number", "FindDes")?;
        let number = self.parse_ipc_integer(number_raw, "number", true)?;
        Ok(BomFindDesignator {
            number,
            number_raw,
            layer_ref: self.optional_attr(node, "layerRef"),
            model_ref: self.optional_attr(node, "modelRef"),
        })
    }

    fn parse_bom_firmware(&mut self, node: &Node) -> Result<BomFirmware> {
        let program_name = self.required_attr(node, "progName", "Firmware")?;
        let program_version = self.required_attr(node, "progVersion", "Firmware")?;
        let mut file = None;
        let mut payload = None;
        for child in self.element_children(node) {
            match self.name(&child) {
                "File" => {
                    file = Some(BomFirmwareFile {
                        name: self.required_attr(&child, "name", "File")?,
                        crc: self.required_attr(&child, "crc", "File")?,
                    })
                }
                "FirmwareRef" => {
                    payload = Some(BomFirmwarePayload::Reference(self.required_attr(
                        &child,
                        "id",
                        "FirmwareRef",
                    )?))
                }
                "CachedFirmware" => {
                    payload = Some(BomFirmwarePayload::Cached(self.required_attr(
                        &child,
                        "hexEncodedBinary",
                        "CachedFirmware",
                    )?))
                }
                _ => {}
            }
        }
        Ok(BomFirmware {
            program_name,
            program_version,
            file: file.ok_or(Ipc2581Error::MissingElement("File in Firmware"))?,
            payload: payload.ok_or(Ipc2581Error::MissingElement("FirmwareGroup in Firmware"))?,
        })
    }

    fn parse_characteristics(&mut self, node: &Node) -> Result<Characteristics> {
        let category = self
            .attr(node, "category")
            .map(BomCategory::from_ipc)
            .transpose()?;
        let mut measured = Vec::new();
        let mut ranged = Vec::new();
        let mut enumerated = Vec::new();
        let mut textuals = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "Measured" => measured.push(self.parse_measured_characteristic(&child)?),
                "Ranged" => ranged.push(self.parse_ranged_characteristic(&child)?),
                "Enumerated" => enumerated.push(self.parse_enumerated_characteristic(&child)),
                "Textual" => textuals.push(self.parse_textual_characteristic(&child)?),
                _ => {}
            }
        }
        Ok(Characteristics {
            category,
            measured,
            ranged,
            enumerated,
            textuals,
        })
    }

    fn parse_measured_characteristic(&mut self, node: &Node) -> Result<MeasuredCharacteristic> {
        Ok(MeasuredCharacteristic {
            definition_source: self.optional_attr(node, "definitionSource"),
            name: self.optional_attr(node, "measuredCharacteristicName"),
            value: self.opt_num(node, "measuredCharacteristicValue")?,
            engineering_unit: self.optional_attr(node, "engineeringUnitOfMeasure"),
            negative_tolerance: self.opt_num(node, "engineeringNegativeTolerance")?,
            positive_tolerance: self.opt_num(node, "engineeringPositiveTolerance")?,
        })
    }

    fn parse_ranged_characteristic(&mut self, node: &Node) -> Result<RangedCharacteristic> {
        Ok(RangedCharacteristic {
            definition_source: self.optional_attr(node, "definitionSource"),
            name: self.optional_attr(node, "rangedCharacteristicName"),
            lower_value: self.opt_num(node, "rangedCharacteristicLowerValue")?,
            upper_value: self.opt_num(node, "rangedCharacteristicUpperValue")?,
            engineering_unit: self.optional_attr(node, "engineeringUnitOfMeasure"),
            negative_tolerance: self.opt_num(node, "engineeringNegativeTolerance")?,
            positive_tolerance: self.opt_num(node, "engineeringPositiveTolerance")?,
        })
    }

    fn parse_enumerated_characteristic(&mut self, node: &Node) -> EnumeratedCharacteristic {
        EnumeratedCharacteristic {
            definition_source: self.optional_attr(node, "definitionSource"),
            name: self.optional_attr(node, "enumeratedCharacteristicName"),
            value: self.optional_attr(node, "enumeratedCharacteristicValue"),
        }
    }

    fn parse_textual_characteristic(&mut self, node: &Node) -> Result<TextualCharacteristic> {
        let definition_source = self
            .attr(node, "definitionSource")
            .map(|s| self.interner.intern(s));
        let name = self
            .attr(node, "textualCharacteristicName")
            .map(|s| self.interner.intern(s));
        let value = self
            .attr(node, "textualCharacteristicValue")
            .map(|s| self.interner.intern(s));

        Ok(TextualCharacteristic {
            definition_source,
            name,
            value,
        })
    }

    fn parse_avl(&mut self, node: &Node) -> Result<Avl> {
        let name = self.required_attr(node, "name", "Avl")?;

        let mut header = None;
        let mut items = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "AvlHeader" => header = Some(self.parse_avl_header(&child)?),
                "AvlItem" => items.push(self.parse_avl_item(&child)?),
                _ => {}
            }
        }

        Ok(Avl {
            name,
            header,
            items,
        })
    }

    fn parse_avl_header(&mut self, node: &Node) -> Result<AvlHeader> {
        let title = self.required_attr(node, "title", "AvlHeader")?;
        let source = self.required_attr(node, "source", "AvlHeader")?;
        let author = self.required_attr(node, "author", "AvlHeader")?;
        let datetime = self.required_attr(node, "datetime", "AvlHeader")?;

        let version = self
            .parse_optional_count_attr(node, "version", u32::MAX)?
            .unwrap_or(1);

        let comment = self.optional_attr(node, "comment");
        let mod_ref = self.optional_attr(node, "modRef");

        Ok(AvlHeader {
            title,
            source,
            author,
            datetime,
            version,
            comment,
            mod_ref,
        })
    }

    fn parse_avl_item(&mut self, node: &Node) -> Result<AvlItem> {
        let oem_design_number = self.required_attr(node, "OEMDesignNumber", "AvlItem")?;

        let mut vmpn_list = Vec::new();
        let mut spec_refs = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "AvlVmpn" => vmpn_list.push(self.parse_avl_vmpn(&child)?),
                "SpecRef" => spec_refs.push(self.required_attr(&child, "id", "SpecRef")?),
                _ => {}
            }
        }

        Ok(AvlItem {
            oem_design_number,
            vmpn_list,
            spec_refs,
        })
    }

    fn parse_avl_vmpn(&mut self, node: &Node) -> Result<AvlVmpn> {
        let evpl_vendor = self.optional_attr(node, "evplVendor");
        let evpl_mpn = self.optional_attr(node, "evplMpn");

        let qualified = self
            .attr(node, "qualified")
            .map(|value| parse_xsd_bool(value, "qualified"))
            .transpose()?;

        let chosen = self
            .attr(node, "chosen")
            .map(|value| parse_xsd_bool(value, "chosen"))
            .transpose()?;

        let mut mpns = Vec::new();
        let mut vendors = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "AvlMpn" => mpns.push(self.parse_avl_mpn(&child)?),
                "AvlVendor" => vendors.push(self.parse_avl_vendor(&child)?),
                _ => {}
            }
        }

        Ok(AvlVmpn {
            evpl_vendor,
            evpl_mpn,
            qualified,
            chosen,
            mpns,
            vendors,
        })
    }

    fn parse_avl_mpn(&mut self, node: &Node) -> Result<AvlMpn> {
        let name = self.required_attr(node, "name", "AvlMpn")?;

        let rank = self.parse_optional_count_attr(node, "rank", u32::MAX)?;
        let cost = self.opt_num(node, "cost")?;
        let moisture_sensitivity = self
            .attr(node, "moistureSensitivity")
            .map(MoistureSensitivity::from_ipc)
            .transpose()?;

        let availability = self
            .attr(node, "availability")
            .map(|value| parse_xsd_bool(value, "availability"))
            .transpose()?;

        let other = self.optional_attr(node, "other");

        Ok(AvlMpn {
            name,
            rank,
            cost,
            moisture_sensitivity,
            availability,
            other,
        })
    }

    fn parse_avl_vendor(&mut self, node: &Node) -> Result<AvlVendor> {
        let enterprise_ref = self.required_attr(node, "enterpriseRef", "AvlVendor")?;

        Ok(AvlVendor { enterprise_ref })
    }

    fn parse_xform(&self, node: &Node, units: Units) -> Result<Xform> {
        let identity = Xform::default();
        Ok(Xform {
            x_offset: self
                .opt_mm(node, "xOffset", units)?
                .unwrap_or(identity.x_offset),
            y_offset: self
                .opt_mm(node, "yOffset", units)?
                .unwrap_or(identity.y_offset),
            rotation: self.opt_num(node, "rotation")?.unwrap_or(identity.rotation),
            mirror: self.parse_flag_attr(node, "mirror")?,
            face_up: self.parse_flag_attr(node, "faceUp")?,
            scale: self.opt_num(node, "scale")?.unwrap_or(identity.scale),
        })
    }

    fn parse_xform_child(&self, node: &Node, units: Units) -> Result<Option<Xform>> {
        self.children_named(node, "Xform")
            .next()
            .map(|n| self.parse_xform(&n, units))
            .transpose()
    }
}

/// The schema leaves `ringNumber` unbounded; a target has a handful of rings
/// and the importer images each one.
const MAX_MOIRE_RINGS: u32 = 256;

/// The span of a table that grew from `start` to `end` items. Every item
/// comes from an element of its own, and `Dom` numbers those in a `u32`.
fn span(start: usize, end: usize) -> Span {
    Span {
        start: start as u32,
        count: (end - start) as u32,
    }
}

/// A shape's one line description: its reference if it names one.
fn line_desc_group(reference: Option<Symbol>, inline: Option<LineDesc>) -> Option<LineDescGroup> {
    reference
        .map(LineDescGroup::Ref)
        .or(inline.map(LineDescGroup::Inline))
}

fn user_shape(shape: UserShapeType, style: ShapeStyle) -> UserShape {
    UserShape {
        shape,
        line_desc: style.line_desc,
        line_desc_ref: style.line_desc_ref,
        fill_desc: style.fill_desc,
        fill_desc_ref: style.fill_desc_ref,
    }
}

#[derive(Clone, Copy)]
enum Sign {
    Any,
    /// IPC-2581C `nonNegativeDoubleType`.
    NonNegative,
}

/// The one place attribute text becomes an `f64`. `NaN` and `INF` are valid
/// `xsd:double`s that no geometry can use, and a finite source value can still
/// overflow once scaled to millimeters.
fn parse_f64(value: &str, attr: &str, sign: Sign, units: Option<Units>) -> Result<f64> {
    let invalid =
        |why: &str| Ipc2581Error::InvalidAttribute(format!("Value for {attr} {why}: {value}"));
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|_| invalid("is not a number"))?;
    if matches!(sign, Sign::NonNegative) && !(0.0..=3.4e38).contains(&parsed) {
        return Err(invalid("is outside the IPC-2581C non-negative range"));
    }
    let scaled = units.map_or(parsed, |units| crate::units::to_mm(parsed, units));
    scaled
        .is_finite()
        .then_some(scaled)
        .ok_or_else(|| invalid("is not finite"))
}

fn has_z_axis_dim(doc: &Dom, node: &Node) -> bool {
    let cuts = |node: Node| matches!(doc.name(node), "MaterialCut" | "MaterialLeft");
    doc.children(*node).any(|child| {
        cuts(child)
            || (matches!(doc.name(child), "Z_AxisDim" | "ZAxisDim")
                && doc.children(child).any(cuts))
    })
}

fn parse_optional_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_xsd_bool(value: &str, attr: &str) -> Result<bool> {
    parse_optional_bool(value)
        .ok_or_else(|| Ipc2581Error::InvalidAttribute(format!("Invalid bool value for {attr}")))
}

fn spec_item_kind(element: &str) -> ecad::SpecItemKind {
    match element {
        "General" => ecad::SpecItemKind::General,
        "Dielectric" => ecad::SpecItemKind::Dielectric,
        "Conductor" => ecad::SpecItemKind::Conductor,
        "SurfaceFinish" => ecad::SpecItemKind::SurfaceFinish,
        "V_Cut" => ecad::SpecItemKind::VCut,
        _ => ecad::SpecItemKind::Other,
    }
}

fn is_standard_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "Butterfly"
            | "Circle"
            | "Contour"
            | "Diamond"
            | "Donut"
            | "Ellipse"
            | "Hexagon"
            | "Moire"
            | "Octagon"
            | "Oval"
            | "RectCenter"
            | "RectCham"
            | "RectCorner"
            | "RectRound"
            | "Thermal"
            | "Triangle"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_slot_cavity_z_axis_substitution_children() {
        let doc = Dom::parse(
            r#"<SlotCavity><Location x="0" y="0"/><MaterialCut depth="0.1"/></SlotCavity>"#,
        )
        .unwrap();

        assert!(has_z_axis_dim(&doc, &doc.root()));
    }

    #[test]
    fn detects_wrapped_slot_cavity_z_axis_dimensions() {
        let doc = Dom::parse(
            r#"<SlotCavity><Location x="0" y="0"/><ZAxisDim><MaterialLeft thickness="0.1"/></ZAxisDim></SlotCavity>"#,
        )
        .unwrap();

        assert!(has_z_axis_dim(&doc, &doc.root()));
    }

    #[test]
    fn parses_slot_cavity_xform() {
        let ipc = crate::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Cu_B.Cu_1"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu_B.Cu_1" layerFunction="ROUT" side="ALL"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="F.Cu_B.Cu_1">
          <Set>
            <SlotCavity name="SLOT1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="1" y="2"/>
              <Xform rotation="90" mirror="true" scale="2" xOffset="0.5" yOffset="0.25"/>
              <Oval width="1.7" height="0.6"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let slot = ipc
            .ecad()
            .unwrap()
            .cad_data
            .steps
            .first()
            .unwrap()
            .layer_features
            .first()
            .unwrap()
            .slots()
            .next()
            .unwrap();

        let xform = slot.xform.unwrap();
        assert_eq!(xform.rotation, 90.0);
        assert!(xform.mirror);
        assert_eq!(xform.scale, 2.0);
        assert_eq!(xform.x_offset, 0.5);
        assert_eq!(xform.y_offset, 0.25);
    }
}

/// Parsed IPC-2581 document (before transferring to user arena)
#[derive(Debug)]
pub struct ParsedIpc2581 {
    pub revision: Symbol,
    pub content: Content,
    pub logistic_header: Option<LogisticHeader>,
    pub history_record: Option<HistoryRecord>,
    pub ecad: Option<Ecad>,
    pub boms: Vec<Bom>,
    pub avl: Option<Avl>,
}
