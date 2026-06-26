use crate::types::*;
use crate::{Interner, Ipc2581Error, Result, Symbol};
use uppsala::{Document, NodeId as Node};

/// Parser context holding the string interner and unit context
pub struct Parser<'a> {
    pub interner: Interner,
    /// Current ECAD units for converting dimensions (set when parsing CadHeader)
    ecad_units: Option<Units>,
    /// Specs from CadHeader (set when parsing CadHeader, used by StackupLayer parsing)
    specs: std::collections::HashMap<Symbol, ecad::Spec>,
    doc: Option<&'a Document<'a>>,
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

    fn doc(&self) -> &'a Document<'a> {
        self.doc.expect("parser document is set while parsing")
    }

    fn name<'n>(&self, node: &'n Node) -> &'a str {
        self.doc()
            .element(*node)
            .expect("expected XML element")
            .name
            .local_name
            .as_ref()
    }

    fn attr<'n>(&self, node: &'n Node, attr: &str) -> Option<&'a str> {
        self.doc().get_attribute(*node, attr)
    }

    fn element_children(&self, node: &Node) -> std::vec::IntoIter<Node> {
        self.doc()
            .children_iter(*node)
            .filter(|child| self.doc().element(*child).is_some())
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn parse_document(&mut self, doc: &'a Document<'a>) -> Result<ParsedIpc2581> {
        self.doc = Some(doc);
        let root = doc
            .document_element()
            .ok_or(Ipc2581Error::MissingElement("IPC-2581"))?;

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
        let mut bom = None;
        let mut avl = None;

        for child in self.element_children(&root) {
            match self.name(&child) {
                "Content" => content_node = Some(child),
                "LogisticHeader" => logistic_header = Some(self.parse_logistic_header(&child)?),
                "HistoryRecord" => history_record = Some(self.parse_history_record(&child)?),
                "Ecad" => ecad = Some(self.parse_ecad(&child)?),
                "Bom" => bom = Some(self.parse_bom(&child)?),
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
            bom,
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
            dictionary_standard: dictionary_standard.unwrap_or_default(),
            dictionary_user: dictionary_user.unwrap_or_default(),
        })
    }

    fn parse_function_mode(&mut self, node: &Node) -> Result<FunctionMode> {
        let mode_str = self.required_attr(node, "mode", "FunctionMode")?;
        let mode = self.parse_mode(self.interner.resolve(mode_str))?;

        let level = self
            .attr(node, "level")
            .map(|s| self.parse_level(s))
            .transpose()?;

        Ok(FunctionMode { mode, level })
    }

    fn parse_mode(&self, s: &str) -> Result<Mode> {
        match s {
            "USERDEF" => Ok(Mode::UserDef),
            "BOM" => Ok(Mode::Bom),
            "STACKUP" => Ok(Mode::Stackup),
            "FABRICATION" => Ok(Mode::Fabrication),
            "ASSEMBLY" => Ok(Mode::Assembly),
            "TEST" => Ok(Mode::Test),
            "STENCIL" => Ok(Mode::Stencil),
            "DFX" => Ok(Mode::Dfx),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown mode: {}",
                s
            ))),
        }
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
        let entry_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "EntryColor")
            .collect::<Vec<_>>();
        let entries = entry_nodes
            .into_iter()
            .map(|n| self.parse_entry_color(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryColor { entries })
    }

    fn parse_entry_color(&mut self, node: &Node) -> Result<EntryColor> {
        let id = self.required_attr(node, "id", "EntryColor")?;

        let color_node = self
            .element_children(node)
            .find(|n| self.name(n) == "Color")
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
        let units = self
            .attr(node, "units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entry_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "EntryLineDesc")
            .collect::<Vec<_>>();
        let entries = entry_nodes
            .into_iter()
            .map(|n| self.parse_entry_line_desc(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryLineDesc { units, entries })
    }

    fn parse_entry_line_desc(&mut self, node: &Node, units: Units) -> Result<EntryLineDesc> {
        let id = self.required_attr(node, "id", "EntryLineDesc")?;

        let line_desc_node = self
            .element_children(node)
            .find(|n| self.name(n) == "LineDesc")
            .ok_or(Ipc2581Error::MissingElement("LineDesc"))?;

        let line_desc = self.parse_line_desc(&line_desc_node, units)?;

        Ok(EntryLineDesc { id, line_desc })
    }

    fn parse_line_desc(&mut self, node: &Node, units: Units) -> Result<LineDesc> {
        let line_width = self.parse_f64_attr_with_units(node, "lineWidth", "LineDesc", units)?;
        let line_end_str = self.required_attr(node, "lineEnd", "LineDesc")?;
        let line_end = self.parse_line_end(self.interner.resolve(line_end_str))?;

        let line_property = self
            .attr(node, "lineProperty")
            .map(|s| self.parse_line_property(s))
            .transpose()?;

        Ok(LineDesc {
            line_width,
            line_end,
            line_property,
        })
    }

    fn parse_line_end(&self, s: &str) -> Result<LineEnd> {
        match s {
            "ROUND" => Ok(LineEnd::Round),
            "SQUARE" => Ok(LineEnd::Square),
            "FLAT" => Ok(LineEnd::Flat),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown lineEnd: {}",
                s
            ))),
        }
    }

    fn parse_line_property(&self, s: &str) -> Result<LineProperty> {
        match s {
            "SOLID" => Ok(LineProperty::Solid),
            "DOTTED" => Ok(LineProperty::Dotted),
            "DASHED" => Ok(LineProperty::Dashed),
            "CENTER" => Ok(LineProperty::Center),
            "PHANTOM" => Ok(LineProperty::Phantom),
            "ERASE" => Ok(LineProperty::Erase),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown lineProperty: {}",
                s
            ))),
        }
    }

    fn parse_feature_line_desc(
        &self,
        node: &Node,
        units: Units,
    ) -> Result<(f64, Option<LineEnd>, Option<LineProperty>)> {
        let line_width = self
            .attr(node, "lineWidth")
            .map(|s| self.parse_f64_str_with_units(s, units))
            .transpose()?
            .unwrap_or(0.25);
        let line_end = self
            .attr(node, "lineEnd")
            .map(|s| self.parse_line_end(s))
            .transpose()?;
        let line_property = self
            .attr(node, "lineProperty")
            .map(|s| self.parse_line_property(s))
            .transpose()?;

        Ok((line_width, line_end, line_property))
    }

    fn parse_dictionary_fill_desc(&mut self, _node: &Node) -> Result<DictionaryFillDesc> {
        // Simplified for now
        Ok(DictionaryFillDesc::default())
    }

    fn parse_fill_desc(&mut self, node: &Node) -> Result<FillDesc> {
        let fill_property_str = self.required_attr(node, "fillProperty", "FillDesc")?;
        let fill_property = self.parse_fill_property(self.interner.resolve(fill_property_str))?;

        let angle1 = self
            .attr(node, "angle1")
            .map(|s| s.parse::<f64>())
            .transpose()
            .map_err(|_| Ipc2581Error::InvalidAttribute("angle1".to_string()))?;

        let angle2 = self
            .attr(node, "angle2")
            .map(|s| s.parse::<f64>())
            .transpose()
            .map_err(|_| Ipc2581Error::InvalidAttribute("angle2".to_string()))?;

        Ok(FillDesc {
            fill_property,
            angle1,
            angle2,
        })
    }

    fn parse_fill_property(&self, s: &str) -> Result<FillProperty> {
        match s {
            "FILL" => Ok(FillProperty::Fill),
            "HOLLOW" => Ok(FillProperty::Hollow),
            "VOID" => Ok(FillProperty::Void),
            "HATCH" => Ok(FillProperty::Hatch),
            "MESH" => Ok(FillProperty::Mesh),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown fillProperty: {}",
                s
            ))),
        }
    }

    /// Generic enum parser using FromStr trait
    fn parse_enum_attr<T: std::str::FromStr<Err = String>>(&self, s: &str) -> Result<T> {
        s.parse().map_err(Ipc2581Error::InvalidAttribute)
    }

    /// Parse optional FillDesc and LineDesc children from a primitive node
    fn parse_fill_and_line_desc(
        &mut self,
        node: &Node,
    ) -> Result<(Option<FillProperty>, Option<Symbol>)> {
        let mut fill_property = None;
        let mut line_desc_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "FillDesc" => {
                    let fill_desc = self.parse_fill_desc(&child)?;
                    fill_property = Some(fill_desc.fill_property);
                }
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok((fill_property, line_desc_ref))
    }

    /// Wrap a shape with styling (fill_property and line_desc_ref)
    fn styled<T>(&mut self, node: &Node, shape: T) -> Result<Styled<T>> {
        let (fill_property, line_desc_ref) = self.parse_fill_and_line_desc(node)?;
        Ok(Styled {
            shape,
            fill_property,
            line_desc_ref,
        })
    }

    fn parse_dictionary_standard(&mut self, node: &Node) -> Result<DictionaryStandard> {
        let units = self
            .attr(node, "units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entry_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "EntryStandard")
            .collect::<Vec<_>>();
        let entries = entry_nodes
            .into_iter()
            .map(|n| self.parse_entry_standard(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryStandard { units, entries })
    }

    fn parse_entry_standard(&mut self, node: &Node, units: Units) -> Result<EntryStandard> {
        let id = self.required_attr(node, "id", "EntryStandard")?;

        // Find the primitive child element
        let primitive_node = self
            .element_children(node)
            .find(|n| self.doc().element(*n).is_some())
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive"))?;

        let primitive = self.parse_standard_primitive(&primitive_node, units)?;

        Ok(EntryStandard { id, primitive })
    }

    fn parse_standard_primitive(&mut self, node: &Node, units: Units) -> Result<StandardPrimitive> {
        match self.name(node) {
            "Circle" => Ok(StandardPrimitive::Circle(self.styled(
                node,
                Circle {
                    diameter: self.parse_f64_attr_with_units(node, "diameter", "Circle", units)?,
                },
            )?)),
            "RectCenter" => Ok(StandardPrimitive::RectCenter(self.styled(
                node,
                RectCenter {
                    size: Size {
                        width: self.parse_f64_attr_with_units(
                            node,
                            "width",
                            "RectCenter",
                            units,
                        )?,
                        height: self.parse_f64_attr_with_units(
                            node,
                            "height",
                            "RectCenter",
                            units,
                        )?,
                    },
                },
            )?)),
            "RectRound" => Ok(StandardPrimitive::RectRound(self.styled(
                node,
                RectRound {
                    size: Size {
                        width: self.parse_f64_attr_with_units(node, "width", "RectRound", units)?,
                        height: self.parse_f64_attr_with_units(
                            node,
                            "height",
                            "RectRound",
                            units,
                        )?,
                    },
                    radius: self.parse_f64_attr_with_units(node, "radius", "RectRound", units)?,
                    upper_right: self.parse_bool_attr(node, "upperRight").unwrap_or(false),
                    upper_left: self.parse_bool_attr(node, "upperLeft").unwrap_or(false),
                    lower_right: self.parse_bool_attr(node, "lowerRight").unwrap_or(false),
                    lower_left: self.parse_bool_attr(node, "lowerLeft").unwrap_or(false),
                },
            )?)),
            "RectCham" => Ok(StandardPrimitive::RectCham(self.styled(
                node,
                RectCham {
                    size: Size {
                        width: self.parse_f64_attr_with_units(node, "width", "RectCham", units)?,
                        height:
                            self.parse_f64_attr_with_units(node, "height", "RectCham", units)?,
                    },
                    chamfer: self.parse_f64_attr_with_units(node, "chamfer", "RectCham", units)?,
                    upper_right: self.parse_bool_attr(node, "upperRight").unwrap_or(false),
                    upper_left: self.parse_bool_attr(node, "upperLeft").unwrap_or(false),
                    lower_right: self.parse_bool_attr(node, "lowerRight").unwrap_or(false),
                    lower_left: self.parse_bool_attr(node, "lowerLeft").unwrap_or(false),
                },
            )?)),
            "RectCorner" => Ok(StandardPrimitive::RectCorner(self.styled(
                node,
                RectCorner {
                    lower_left: Point {
                        x: self.parse_f64_attr_with_units(
                            node,
                            "lowerLeftX",
                            "RectCorner",
                            units,
                        )?,
                        y: self.parse_f64_attr_with_units(
                            node,
                            "lowerLeftY",
                            "RectCorner",
                            units,
                        )?,
                    },
                    upper_right: Point {
                        x: self.parse_f64_attr_with_units(
                            node,
                            "upperRightX",
                            "RectCorner",
                            units,
                        )?,
                        y: self.parse_f64_attr_with_units(
                            node,
                            "upperRightY",
                            "RectCorner",
                            units,
                        )?,
                    },
                },
            )?)),
            "Butterfly" => {
                let shape_attr = self.required_attr(node, "shape", "Butterfly")?;
                let shape =
                    self.parse_enum_attr::<ButterflyShape>(self.interner.resolve(shape_attr))?;
                let attr_name = if matches!(shape, ButterflyShape::Round) {
                    "diameter"
                } else {
                    "side"
                };
                Ok(StandardPrimitive::Butterfly(self.styled(
                    node,
                    Butterfly {
                        shape,
                        size: self.parse_f64_attr_with_units(
                            node,
                            attr_name,
                            "Butterfly",
                            units,
                        )?,
                    },
                )?))
            }
            "Diamond" => Ok(StandardPrimitive::Diamond(self.styled(
                node,
                Diamond {
                    size: Size {
                        width: self.parse_f64_attr_with_units(node, "width", "Diamond", units)?,
                        height: self.parse_f64_attr_with_units(node, "height", "Diamond", units)?,
                    },
                },
            )?)),
            "Donut" => {
                let shape_attr = self.required_attr(node, "shape", "Donut")?;
                let shape =
                    self.parse_enum_attr::<ConcentricShape>(self.interner.resolve(shape_attr))?;
                Ok(StandardPrimitive::Donut(self.styled(
                    node,
                    Donut {
                        shape,
                        outer_diameter: self.parse_f64_attr_with_units(
                            node,
                            "outerDiameter",
                            "Donut",
                            units,
                        )?,
                        inner_diameter: self.parse_f64_attr_with_units(
                            node,
                            "innerDiameter",
                            "Donut",
                            units,
                        )?,
                    },
                )?))
            }
            "Ellipse" => Ok(StandardPrimitive::Ellipse(self.styled(
                node,
                Ellipse {
                    size: Size {
                        width: self.parse_f64_attr_with_units(node, "width", "Ellipse", units)?,
                        height: self.parse_f64_attr_with_units(node, "height", "Ellipse", units)?,
                    },
                },
            )?)),
            "Hexagon" => Ok(StandardPrimitive::Hexagon(self.styled(
                node,
                Hexagon {
                    point_to_point:
                        self.parse_f64_attr_with_units(node, "length", "Hexagon", units)?,
                },
            )?)),
            "Moire" => Ok(StandardPrimitive::Moire(Moire {
                diameter: self.parse_f64_attr_with_units(node, "diameter", "Moire", units)?,
                ring_width: self.parse_f64_attr_with_units(node, "ringWidth", "Moire", units)?,
                ring_gap: self.parse_f64_attr_with_units(node, "ringGap", "Moire", units)?,
                ring_number: self.parse_u32_attr(node, "ringNumber", "Moire")?,
                line_width: self.parse_optional_f64_attr_with_units(node, "lineWidth", units)?,
                line_length: self.parse_optional_f64_attr_with_units(node, "lineLength", units)?,
                line_angle: self.parse_optional_f64_attr(node, "lineAngle")?,
            })),
            "Octagon" => Ok(StandardPrimitive::Octagon(self.styled(
                node,
                Octagon {
                    point_to_point:
                        self.parse_f64_attr_with_units(node, "length", "Octagon", units)?,
                },
            )?)),
            "Thermal" => {
                let shape_attr = self.required_attr(node, "shape", "Thermal")?;
                let shape =
                    self.parse_enum_attr::<ConcentricShape>(self.interner.resolve(shape_attr))?;
                Ok(StandardPrimitive::Thermal(
                    self.styled(
                        node,
                        Thermal {
                            shape,
                            outer_diameter: self.parse_f64_attr_with_units(
                                node,
                                "outerDiameter",
                                "Thermal",
                                units,
                            )?,
                            inner_diameter: self.parse_f64_attr_with_units(
                                node,
                                "innerDiameter",
                                "Thermal",
                                units,
                            )?,
                            spoke_count: self
                                .parse_optional_u32_attr(node, "spokeCount")?
                                .unwrap_or(4),
                            spoke_width: self.parse_optional_f64_attr_with_units(
                                node,
                                "spokeWidth",
                                units,
                            )?,
                            spoke_start_angle: self
                                .parse_optional_f64_attr(node, "spokeStartAngle")?,
                        },
                    )?,
                ))
            }
            "Triangle" => Ok(StandardPrimitive::Triangle(self.styled(
                node,
                Triangle {
                    base: self.parse_f64_attr_with_units(node, "base", "Triangle", units)?,
                    height: self.parse_f64_attr_with_units(node, "height", "Triangle", units)?,
                },
            )?)),
            "Oval" => Ok(StandardPrimitive::Oval(self.styled(
                node,
                Oval {
                    size: Size {
                        width: self.parse_f64_attr_with_units(node, "width", "Oval", units)?,
                        height: self.parse_f64_attr_with_units(node, "height", "Oval", units)?,
                    },
                },
            )?)),
            "Contour" => Ok(StandardPrimitive::Contour(self.parse_contour(node, units)?)),
            name => Err(Ipc2581Error::InvalidStructure(format!(
                "Unknown standard primitive: {}",
                name
            ))),
        }
    }

    fn parse_contour(&mut self, node: &Node, units: Units) -> Result<Contour> {
        let polygon_node = self
            .element_children(node)
            .find(|n| self.name(n) == "Polygon")
            .ok_or(Ipc2581Error::MissingElement("Polygon"))?;

        let polygon = self.parse_polygon(&polygon_node, units)?;
        let cutout_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "Cutout")
            .collect::<Vec<_>>();
        let cutouts = cutout_nodes
            .into_iter()
            .map(|n| self.parse_polygon_container(&n, units))
            .collect::<Result<Vec<_>>>()?;

        Ok(Contour { polygon, cutouts })
    }

    fn parse_polygon_container(&mut self, node: &Node, units: Units) -> Result<Polygon> {
        match self
            .element_children(node)
            .find(|child| self.name(child) == "Polygon")
        {
            Some(polygon) => self.parse_polygon(&polygon, units),
            None => self.parse_polygon(node, units),
        }
    }

    fn parse_polygon(&mut self, node: &Node, units: Units) -> Result<Polygon> {
        let mut begin: Option<Point> = None;
        let mut steps = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "PolyBegin" => {
                    begin = Some(Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyBegin", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyBegin", units)?,
                    })
                }
                "PolyStepSegment" => steps.push(PolyStep::Segment(PolyStepSegment {
                    point: Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyStepSegment", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyStepSegment", units)?,
                    },
                })),
                "PolyStepCurve" => steps.push(PolyStep::Curve(PolyStepCurve {
                    point: Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyStepCurve", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyStepCurve", units)?,
                    },
                    center: Point {
                        x: self.parse_f64_attr_with_units(
                            &child,
                            "centerX",
                            "PolyStepCurve",
                            units,
                        )?,
                        y: self.parse_f64_attr_with_units(
                            &child,
                            "centerY",
                            "PolyStepCurve",
                            units,
                        )?,
                    },
                    clockwise: self.parse_bool_attr(&child, "clockwise")?,
                })),
                _ => {}
            }
        }

        Ok(Polygon {
            begin: begin.ok_or(Ipc2581Error::MissingElement("PolyBegin"))?,
            steps,
        })
    }

    fn parse_dictionary_user(&mut self, node: &Node) -> Result<DictionaryUser> {
        let units = self
            .attr(node, "units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entry_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "EntryUser")
            .collect::<Vec<_>>();
        let entries = entry_nodes
            .into_iter()
            .map(|n| self.parse_entry_user(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryUser { units, entries })
    }

    fn parse_entry_user(&mut self, node: &Node, units: Units) -> Result<EntryUser> {
        let id = self.required_attr(node, "id", "EntryUser")?;

        // Find the primitive child element (currently only supporting UserSpecial)
        let primitive_node = self
            .element_children(node)
            .find(|n| self.name(n) == "UserSpecial")
            .ok_or(Ipc2581Error::MissingElement("UserPrimitive"))?;

        let primitive = self.parse_user_special(&primitive_node, units)?;

        Ok(EntryUser { id, primitive })
    }

    fn parse_user_special(&mut self, node: &Node, units: Units) -> Result<UserPrimitive> {
        let mut shapes = Vec::new();

        for child in self.element_children(node) {
            let tag_name = self.name(&child);

            if tag_name == "UserSpecial" {
                let UserPrimitive::UserSpecial(nested) = self.parse_user_special(&child, units)?;
                shapes.extend(nested.shapes);
                continue;
            }

            let shape_type = match tag_name {
                "Contour" => Some(UserShapeType::Contour(self.parse_contour(&child, units)?)),
                "Circle" => Some(UserShapeType::Circle(Circle {
                    diameter: self
                        .parse_f64_attr_with_units(&child, "diameter", "Circle", units)?,
                })),
                "RectCenter" => Some(UserShapeType::RectCenter(RectCenter {
                    size: Size {
                        width: self.parse_f64_attr_with_units(
                            &child,
                            "width",
                            "RectCenter",
                            units,
                        )?,
                        height: self.parse_f64_attr_with_units(
                            &child,
                            "height",
                            "RectCenter",
                            units,
                        )?,
                    },
                })),
                "Oval" => Some(UserShapeType::Oval(Oval {
                    size: Size {
                        width: self.parse_f64_attr_with_units(&child, "width", "Oval", units)?,
                        height: self.parse_f64_attr_with_units(&child, "height", "Oval", units)?,
                    },
                })),
                "RectRound" => Some(UserShapeType::RectRound(RectRound {
                    size: Size {
                        width: self.parse_f64_attr_with_units(
                            &child,
                            "width",
                            "RectRound",
                            units,
                        )?,
                        height: self.parse_f64_attr_with_units(
                            &child,
                            "height",
                            "RectRound",
                            units,
                        )?,
                    },
                    radius: self.parse_f64_attr_with_units(&child, "radius", "RectRound", units)?,
                    upper_right: self.parse_bool_attr(&child, "upperRight").unwrap_or(false),
                    upper_left: self.parse_bool_attr(&child, "upperLeft").unwrap_or(false),
                    lower_right: self.parse_bool_attr(&child, "lowerRight").unwrap_or(false),
                    lower_left: self.parse_bool_attr(&child, "lowerLeft").unwrap_or(false),
                })),
                "Polygon" => Some(UserShapeType::Polygon(self.parse_polygon(&child, units)?)),
                "Line" => Some(UserShapeType::Line(crate::types::primitives::Line {
                    start: Point {
                        x: self.parse_f64_attr_with_units(&child, "startX", "Line", units)?,
                        y: self.parse_f64_attr_with_units(&child, "startY", "Line", units)?,
                    },
                    end: Point {
                        x: self.parse_f64_attr_with_units(&child, "endX", "Line", units)?,
                        y: self.parse_f64_attr_with_units(&child, "endY", "Line", units)?,
                    },
                })),
                "Arc" => Some(UserShapeType::Arc(self.parse_user_arc(&child, units)?)),
                "Polyline" => Some(UserShapeType::Polyline(
                    self.parse_user_polyline(&child, units)?,
                )),
                "UserPrimitiveRef" => self
                    .attr(&child, "id")
                    .map(|id| UserShapeType::UserPrimitiveRef(self.interner.intern(id))),
                _ => None,
            };

            if let Some(shape_type) = shape_type {
                let style_node = if tag_name == "Contour" {
                    self.element_children(&child)
                        .find(|n| self.name(n) == "Polygon")
                        .unwrap_or(child)
                } else {
                    child
                };
                let (line_desc, line_desc_ref, fill_desc) =
                    self.parse_user_shape_style(&style_node, units)?;

                shapes.push(UserShape {
                    shape: shape_type,
                    line_desc,
                    line_desc_ref,
                    fill_desc,
                });
            }
        }

        Ok(UserPrimitive::UserSpecial(UserSpecial { shapes }))
    }

    fn parse_user_shape_style(
        &mut self,
        node: &Node,
        units: Units,
    ) -> Result<(Option<LineDesc>, Option<Symbol>, Option<FillDesc>)> {
        let mut line_desc = None;
        let mut line_desc_ref = None;
        let mut fill_desc = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "LineDesc" => line_desc = Some(self.parse_line_desc(&child, units)?),
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                "FillDesc" => fill_desc = Some(self.parse_fill_desc(&child)?),
                _ => {}
            }
        }

        Ok((line_desc, line_desc_ref, fill_desc))
    }

    fn parse_user_polyline(&mut self, node: &Node, units: Units) -> Result<Polyline> {
        let mut begin = None;
        let mut steps = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "PolyBegin" => {
                    begin = Some(Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyBegin", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyBegin", units)?,
                    });
                }
                "PolyStepSegment" => steps.push(PolyStep::Segment(PolyStepSegment {
                    point: Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyStepSegment", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyStepSegment", units)?,
                    },
                })),
                "PolyStepCurve" => steps.push(PolyStep::Curve(PolyStepCurve {
                    point: Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyStepCurve", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyStepCurve", units)?,
                    },
                    center: Point {
                        x: self.parse_f64_attr_with_units(
                            &child,
                            "centerX",
                            "PolyStepCurve",
                            units,
                        )?,
                        y: self.parse_f64_attr_with_units(
                            &child,
                            "centerY",
                            "PolyStepCurve",
                            units,
                        )?,
                    },
                    clockwise: self.parse_bool_attr(&child, "clockwise")?,
                })),
                _ => {}
            }
        }

        Ok(Polyline {
            begin: begin.ok_or(Ipc2581Error::MissingElement("PolyBegin in Polyline"))?,
            steps,
        })
    }

    fn parse_user_arc(&mut self, node: &Node, units: Units) -> Result<Arc> {
        Ok(Arc {
            start: Point {
                x: self.parse_f64_attr_with_units(node, "startX", "Arc", units)?,
                y: self.parse_f64_attr_with_units(node, "startY", "Arc", units)?,
            },
            end: Point {
                x: self.parse_f64_attr_with_units(node, "endX", "Arc", units)?,
                y: self.parse_f64_attr_with_units(node, "endY", "Arc", units)?,
            },
            center: Point {
                x: self.parse_f64_attr_with_units(node, "centerX", "Arc", units)?,
                y: self.parse_f64_attr_with_units(node, "centerY", "Arc", units)?,
            },
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
        // Parse number as f64 first, then convert to u32 (some files use "1.0")
        let number = match self.attr(node, "number") {
            Some(s) => {
                if let Ok(f) = s.parse::<f64>() {
                    f as u32
                } else {
                    return Err(Ipc2581Error::InvalidAttribute(format!(
                        "Invalid number value: {}",
                        s
                    )));
                }
            }
            None => {
                return Err(Ipc2581Error::MissingAttribute {
                    element: "HistoryRecord",
                    attr: "number",
                });
            }
        };

        let origination = self.required_attr(node, "origination", "HistoryRecord")?;
        let software = self.optional_attr(node, "software");
        let last_change = self.required_attr(node, "lastChange", "HistoryRecord")?;

        // Parse FileRevision child element
        let mut file_revision = None;
        for child in self.element_children(node) {
            if self.name(&child) == "FileRevision" {
                file_revision = Some(self.parse_file_revision(&child)?);
                break;
            }
        }

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

    fn parse_units(&self, s: &str) -> Result<Units> {
        match s {
            "MILLIMETER" => Ok(Units::Millimeter),
            "INCH" => Ok(Units::Inch),
            "MICRON" => Ok(Units::Micron),
            "MILS" => Ok(Units::Mils),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown units: {}",
                s
            ))),
        }
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

    fn optional_attr(&mut self, node: &Node, attr: &str) -> Option<Symbol> {
        self.attr(node, attr).map(|s| self.interner.intern(s))
    }

    fn parse_f64_attr(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
    ) -> Result<f64> {
        let attr_val = self
            .attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })?;
        attr_val
            .parse()
            .map_err(|_| Ipc2581Error::InvalidAttribute(format!("Invalid f64 value for {}", attr)))
    }

    /// Parse an f64 attribute and convert it to millimeters (canonical unit)
    ///
    /// This function takes the source units and converts the value to mm.
    /// All dimensional values in the parsed document are stored in mm.
    fn parse_f64_attr_with_units(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
        units: Units,
    ) -> Result<f64> {
        let value = self.parse_f64_attr(node, attr, element)?;
        Ok(crate::units::to_mm(value, units))
    }

    fn parse_u8_attr(&self, node: &Node, attr: &'static str, element: &'static str) -> Result<u8> {
        let attr_val = self
            .attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })?;
        attr_val.parse().map_err(|_| {
            Ipc2581Error::InvalidAttribute(format!("Invalid u8 value for {} in {}", attr, element))
        })
    }

    fn parse_u32_attr(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
    ) -> Result<u32> {
        let attr_val = self
            .attr(node, attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })?;
        attr_val.parse().map_err(|_| {
            Ipc2581Error::InvalidAttribute(format!("Invalid u32 value for {} in {}", attr, element))
        })
    }

    /// Parse an f64 from a string value and convert to mm using the given units
    fn parse_f64_str_with_units(&self, s: &str, units: Units) -> Result<f64> {
        let value = s
            .parse::<f64>()
            .map_err(|_| Ipc2581Error::InvalidAttribute("Invalid f64 value".to_string()))?;
        Ok(crate::units::to_mm(value, units))
    }

    /// Parse optional f64 attribute (no unit conversion)
    fn parse_optional_f64_attr(&self, node: &Node, attr: &'static str) -> Result<Option<f64>> {
        self.attr(node, attr)
            .map(|v| {
                v.parse::<f64>().map_err(|_| {
                    Ipc2581Error::InvalidAttribute(format!("Invalid f64 value for {}", attr))
                })
            })
            .transpose()
    }

    /// Parse optional u32 attribute
    fn parse_optional_u32_attr(&self, node: &Node, attr: &'static str) -> Result<Option<u32>> {
        self.attr(node, attr)
            .map(|v| {
                v.parse::<u32>().map_err(|_| {
                    Ipc2581Error::InvalidAttribute(format!("Invalid u32 value for {}", attr))
                })
            })
            .transpose()
    }

    /// Parse optional f64 attribute with unit conversion
    fn parse_optional_f64_attr_with_units(
        &self,
        node: &Node,
        attr: &'static str,
        units: Units,
    ) -> Result<Option<f64>> {
        self.attr(node, attr)
            .map(|v| self.parse_f64_str_with_units(v, units))
            .transpose()
    }

    fn parse_bool_attr(&self, node: &Node, attr: &'static str) -> Result<bool> {
        match self.attr(node, attr) {
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(_) => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid bool value for {}",
                attr
            ))),
            None => Err(Ipc2581Error::MissingAttribute {
                element: "unknown",
                attr,
            }),
        }
    }

    fn parse_ecad(&mut self, node: &Node) -> Result<Ecad> {
        // Parse CadHeader first to establish units for the ECAD section
        let cad_header_node = self
            .element_children(node)
            .find(|n| self.name(n) == "CadHeader")
            .ok_or(Ipc2581Error::MissingElement("CadHeader"))?;
        let mut cad_header = self.parse_cad_header(&cad_header_node)?;

        // Store ECAD units for use when parsing dimensions
        self.ecad_units = Some(cad_header.units);

        // Move specs into parser context to avoid cloning
        // We'll move them back after parsing CadData
        self.specs = std::mem::take(&mut cad_header.specs);

        let cad_data_node = self
            .element_children(node)
            .find(|n| self.name(n) == "CadData")
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
        let units = self.parse_units(units)?;

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
            items.push(self.parse_spec_item(&child));
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
                    let dielectric_type = self.attr(&child, "type");
                    // Look for Property with value attribute
                    for prop in self.element_children(&child) {
                        if self.name(&prop) == "Property"
                            && let Some(value_str) = self.attr(&prop, "value")
                            && let Ok(value) = value_str.parse::<f64>()
                        {
                            match dielectric_type {
                                Some("DIELECTRIC_CONSTANT") => dielectric_constant = Some(value),
                                Some("LOSS_TANGENT") => loss_tangent = Some(value),
                                _ => {}
                            }
                        }
                    }
                }
                "Conductor" if self.attr(&child, "type") == Some("WEIGHT") => {
                    for prop in self.element_children(&child) {
                        if self.name(&prop) == "Property"
                            && let Some(value_str) = self.attr(&prop, "value")
                            && let Ok(value) = value_str.parse::<f64>()
                        {
                            // Check unit - should be OZ
                            let unit = self.attr(&prop, "unit").unwrap_or("OZ");
                            if unit.to_uppercase() == "OZ" {
                                copper_weight_oz = Some(value);
                            }
                        }
                    }
                }
                "SurfaceFinish" => {
                    surface_finish = self.parse_surface_finish(&child).ok();
                }
                _ => {}
            }
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

    fn parse_spec_item(&mut self, node: &Node) -> ecad::SpecItem {
        let element_name = self.name(node).to_string();
        let element = self.interner.intern(&element_name);
        let item_type = self.attr(node, "type").map(|s| self.interner.intern(s));
        let comment = self.attr(node, "comment").map(|s| self.interner.intern(s));
        let property_nodes = self
            .element_children(node)
            .filter(|child| self.name(child) == "Property")
            .collect::<Vec<_>>();
        let properties = property_nodes
            .into_iter()
            .map(|child| self.parse_spec_property(&child))
            .collect();

        ecad::SpecItem {
            element,
            kind: spec_item_kind(&element_name),
            item_type,
            comment,
            properties,
        }
    }

    fn parse_spec_property(&mut self, node: &Node) -> ecad::SpecProperty {
        ecad::SpecProperty {
            value: self
                .attr(node, "value")
                .and_then(|value| value.parse::<f64>().ok()),
            text: self.attr(node, "text").map(|s| self.interner.intern(s)),
            unit: self.attr(node, "unit").map(|s| self.interner.intern(s)),
            plus_tol: self
                .attr(node, "plusTol")
                .and_then(|value| value.parse::<f64>().ok()),
            minus_tol: self
                .attr(node, "minusTol")
                .and_then(|value| value.parse::<f64>().ok()),
            tol_percent: self.attr(node, "tolPercent").and_then(parse_optional_bool),
        }
    }

    fn parse_surface_finish(&mut self, node: &Node) -> Result<ecad::SurfaceFinish> {
        // Per IPC-2581C XSD, SurfaceFinish has:
        //   - required attribute "type" (surfaceFinishType)
        //   - optional attribute "comment"
        //   - optional child elements "Product" (0..n)
        //
        // Correct format: <SurfaceFinish type="S"/>
        // KiCad bug format: <SurfaceFinish><Finish type="S"/></SurfaceFinish>

        // First, try the correct IPC-2581C format: type attribute directly on SurfaceFinish
        if let Some(finish_type_str) = self.attr(node, "type") {
            let finish_type = self.parse_finish_type(finish_type_str)?;
            let comment = self.attr(node, "comment").map(|s| self.interner.intern(s));

            let mut products = Vec::new();
            for product_node in self.element_children(node) {
                if self.name(&product_node) == "Product"
                    && let Some(product_name) = self.attr(&product_node, "name")
                {
                    let criteria = self
                        .attr(&product_node, "criteria")
                        .and_then(|s| self.parse_product_criteria(s).ok());

                    products.push(ecad::FinishProduct {
                        name: self.interner.intern(product_name),
                        criteria,
                    });
                }
            }

            return Ok(ecad::SurfaceFinish {
                finish_type,
                comment,
                products,
            });
        }

        // TODO: Remove this fallback once KiCad fixes their IPC-2581 exporter.
        // See: https://gitlab.com/kicad/code/kicad/-/issues/XXXXX
        // Fallback: support incorrect KiCad format with nested Finish element
        // This is non-compliant but allows parsing legacy KiCad exports
        for child in self.element_children(node) {
            if self.name(&child) == "Finish" {
                let finish_type_str = self.attr(&child, "type").unwrap_or("OTHER");
                let finish_type = self.parse_finish_type(finish_type_str)?;
                let comment = self
                    .attr(&child, "comment")
                    .map(|s| self.interner.intern(s));

                let mut products = Vec::new();
                for product_node in self.element_children(&child) {
                    if self.name(&product_node) == "Product"
                        && let Some(product_name) = self.attr(&product_node, "name")
                    {
                        let criteria = self
                            .attr(&product_node, "criteria")
                            .and_then(|s| self.parse_product_criteria(s).ok());

                        products.push(ecad::FinishProduct {
                            name: self.interner.intern(product_name),
                            criteria,
                        });
                    }
                }

                return Ok(ecad::SurfaceFinish {
                    finish_type,
                    comment,
                    products,
                });
            }
        }

        // No type attribute and no Finish element found
        Err(Ipc2581Error::MissingElement(
            "SurfaceFinish: missing required 'type' attribute",
        ))
    }

    fn parse_finish_type(&self, s: &str) -> Result<ecad::FinishType> {
        match s {
            "S" => Ok(ecad::FinishType::S),
            "T" => Ok(ecad::FinishType::T),
            "X" => Ok(ecad::FinishType::X),
            "TLU" => Ok(ecad::FinishType::TLU),
            "ENIG-N" => Ok(ecad::FinishType::EnigN),
            "ENIG-G" => Ok(ecad::FinishType::EnigG),
            "ENEPIG-N" => Ok(ecad::FinishType::EnepigN),
            "ENEPIG-G" => Ok(ecad::FinishType::EnepigG),
            "ENEPIG-P" => Ok(ecad::FinishType::EnepigP),
            "DIG" => Ok(ecad::FinishType::Dig),
            "IAg" => Ok(ecad::FinishType::IAg),
            "ISn" => Ok(ecad::FinishType::ISn),
            "OSP" => Ok(ecad::FinishType::Osp),
            "HT_OSP" => Ok(ecad::FinishType::HtOsp),
            "N" => Ok(ecad::FinishType::N),
            "NB" => Ok(ecad::FinishType::NB),
            "C" => Ok(ecad::FinishType::C),
            "G" => Ok(ecad::FinishType::G),
            "GS" => Ok(ecad::FinishType::GS),
            "GWB-1-G" => Ok(ecad::FinishType::GwbOneG),
            "GWB-1-N" => Ok(ecad::FinishType::GwbOneN),
            "GWB-2-G" => Ok(ecad::FinishType::GwbTwoG),
            "GWB-2-N" => Ok(ecad::FinishType::GwbTwoN),
            _ => Ok(ecad::FinishType::Other),
        }
    }

    fn parse_product_criteria(&self, s: &str) -> Result<ecad::ProductCriteria> {
        match s {
            "ALLOWED" => Ok(ecad::ProductCriteria::Allowed),
            "SUGGESTED" => Ok(ecad::ProductCriteria::Suggested),
            "PREFERRED" => Ok(ecad::ProductCriteria::Preferred),
            "REQUIRED" => Ok(ecad::ProductCriteria::Required),
            "CHOSEN" => Ok(ecad::ProductCriteria::Chosen),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid product criteria: {}",
                s
            ))),
        }
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

        // Convert overall thickness if present
        let overall_thickness = self
            .attr(node, "overallThickness")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Parse whereMeasured attribute
        let where_measured = self
            .attr(node, "whereMeasured")
            .and_then(|s| self.parse_where_measured(s).ok());

        // Parse tolerances
        let tol_plus = self
            .attr(node, "tolPlus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let tol_minus = self
            .attr(node, "tolMinus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

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
            layers,
        })
    }

    fn parse_stackup_layer(&mut self, node: &Node) -> Result<StackupLayer> {
        // StackupLayer is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let layer_ref = self.required_attr(node, "layerOrGroupRef", "StackupLayer")?;

        // Convert thickness if present
        let thickness = self
            .attr(node, "thickness")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Convert tolerances if present
        // NOTE: IPC-2581 spec allows tolPercent attribute to indicate if these are percentages
        // For a pure parser, we should keep the raw values and let downstream code handle interpretation
        // Currently we convert to mm for convenience (TODO: make this a separate normalization step)
        let tol_plus = self
            .attr(node, "tolPlus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let tol_minus = self
            .attr(node, "tolMinus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let layer_number = self.attr(node, "sequence").and_then(|s| s.parse().ok());

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
            .map(|step_type| self.parse_step_type(step_type))
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

    fn parse_step_type(&self, s: &str) -> Result<ecad::StepType> {
        match s {
            "BOARD" => Ok(ecad::StepType::Board),
            "PALLET" => Ok(ecad::StepType::Pallet),
            "IC" => Ok(ecad::StepType::Ic),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid Step type: {}",
                s
            ))),
        }
    }

    fn parse_step_repeat(&mut self, node: &Node) -> Result<StepRepeat> {
        // StepRepeat is in ECAD section, use ECAD units.
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let step_ref = self.required_attr(node, "stepRef", "StepRepeat")?;
        let x = self
            .parse_optional_f64_attr_with_units(node, "x", units)?
            .unwrap_or(0.0);
        let y = self
            .parse_optional_f64_attr_with_units(node, "y", units)?
            .unwrap_or(0.0);
        let nx = self.parse_optional_u32_attr(node, "nx")?.unwrap_or(1);
        let ny = self.parse_optional_u32_attr(node, "ny")?.unwrap_or(1);
        let dx = self
            .parse_optional_f64_attr_with_units(node, "dx", units)?
            .unwrap_or(0.0);
        let dy = self
            .parse_optional_f64_attr_with_units(node, "dy", units)?
            .unwrap_or(0.0);
        let angle = self.parse_optional_f64_attr(node, "angle")?.unwrap_or(0.0);
        let mirror = match self.attr(node, "mirror") {
            Some(value) if value.eq_ignore_ascii_case("true") => true,
            Some(value) if value.eq_ignore_ascii_case("false") => false,
            Some(_) => {
                return Err(Ipc2581Error::InvalidAttribute(
                    "Invalid bool value for mirror".to_string(),
                ));
            }
            None => false,
        };

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
        let x = self.parse_f64_attr_with_units(node, "x", "Datum", units)?;
        let y = self.parse_f64_attr_with_units(node, "y", "Datum", units)?;
        Ok(Datum { x, y })
    }

    fn parse_profile(&mut self, node: &Node) -> Result<Profile> {
        let polygon_node = self
            .element_children(node)
            .find(|n| self.name(n) == "Polygon")
            .ok_or(Ipc2581Error::MissingElement("Polygon in Profile"))?;

        // Profile is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let polygon = self.parse_polygon(&polygon_node, units)?;

        let mut cutouts = Vec::new();
        for child in self.element_children(node) {
            if self.name(&child) == "Cutout" {
                cutouts.push(self.parse_polygon_container(&child, units)?);
            }
        }

        Ok(Profile { polygon, cutouts })
    }

    fn parse_package(&mut self, node: &Node) -> Result<Package> {
        let name = self.required_attr(node, "name", "Package")?;
        let package_type = self.required_attr(node, "type", "Package")?;
        let pin_one = self.attr(node, "pinOne").map(|s| self.interner.intern(s));
        let height = self.attr(node, "height").and_then(|s| s.parse().ok());

        Ok(Package {
            name,
            package_type,
            pin_one,
            height,
        })
    }

    fn parse_component(&mut self, node: &Node) -> Result<Component> {
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let ref_des = self.optional_attr(node, "refDes");
        let package_ref = self.optional_attr(node, "packageRef");
        let mat_des = self.optional_attr(node, "matDes");
        let layer_ref = self.required_attr(node, "layerRef", "Component")?;
        let layer_ref_topside = self.optional_attr(node, "layerRefTopside");
        let mount_type = self.parse_mount_type(self.attr(node, "mountType").ok_or(
            Ipc2581Error::MissingAttribute {
                element: "Component",
                attr: "mountType",
            },
        )?);
        let part = self.required_attr(node, "part", "Component")?;
        let model_ref = self.optional_attr(node, "modelRef");
        let weight = self.parse_optional_f64_attr(node, "weight")?;
        let height = self.parse_optional_f64_attr(node, "height")?;
        let standoff = self.parse_optional_f64_attr(node, "standoff")?;

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
                    xform = Some(self.parse_xform(&child, units));
                }
                "Location" => {
                    location = Some(Location {
                        x: self.parse_f64_attr_with_units(&child, "x", "Location", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "Location", units)?,
                    });
                }
                "SlotCavityRef" => {
                    slot_cavity_ref = self.optional_attr(&child, "id");
                }
                "SpecRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        spec_refs.push(self.interner.intern(id));
                    }
                }
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

    fn parse_mount_type(&self, value: &str) -> MountType {
        match value {
            "SMT" => MountType::Smt,
            "THMT" | "THT" => MountType::Thmt,
            "EMBEDDED" => MountType::Embedded,
            "PRESSFIT" => MountType::PressFit,
            "WIRE_BONDED" => MountType::WireBonded,
            "GLUED" => MountType::Glued,
            "CLAMPED" => MountType::Clamped,
            "SOCKETED" => MountType::Socketed,
            "FORMED" => MountType::Formed,
            "OTHER" => MountType::Other,
            _ => MountType::Other,
        }
    }

    fn parse_logical_net(&mut self, node: &Node) -> Result<LogicalNet> {
        let name = self.required_attr(node, "name", "LogicalNet")?;

        let pin_ref_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "PinRef")
            .collect::<Vec<_>>();
        let pin_refs = pin_ref_nodes
            .into_iter()
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
        let layer_function_str = self.required_attr(node, "layerFunction", "Layer")?;
        let layer_function =
            self.parse_layer_function(self.interner.resolve(layer_function_str))?;

        let side = self
            .attr(node, "side")
            .map(|s| self.parse_side(s))
            .transpose()?;
        let polarity = self
            .attr(node, "polarity")
            .map(|s| self.parse_polarity(s))
            .transpose()?;

        let mut span = None;
        let mut profile = None;
        let mut spec_refs = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "SpecRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        spec_refs.push(self.interner.intern(id));
                    }
                }
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
                "Profile" => {
                    profile = Some(self.parse_profile(&child)?);
                }
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
            profile,
        })
    }

    fn parse_layer_feature(&mut self, node: &Node) -> Result<LayerFeature> {
        let layer_ref = self.required_attr(node, "layerRef", "LayerFeature")?;

        let set_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "Set")
            .collect::<Vec<_>>();
        let sets = set_nodes
            .into_iter()
            .map(|n| self.parse_feature_set(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(LayerFeature { layer_ref, sets })
    }

    fn parse_feature_set(&mut self, node: &Node) -> Result<FeatureSet> {
        let net = self.attr(node, "net").map(|s| self.interner.intern(s));
        let geometry = self.attr(node, "geometry").map(|s| self.interner.intern(s));

        // Parse polarity attribute
        let polarity = self.attr(node, "polarity").and_then(|s| match s {
            "POSITIVE" => Some(Polarity::Positive),
            "NEGATIVE" => Some(Polarity::Negative),
            _ => None,
        });

        let mut features = Vec::new();
        let mut spec_refs = Vec::new();
        let mut nonstandard_attributes = Vec::new();

        for child in self.element_children(node) {
            match self.name(&child) {
                "SpecRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        spec_refs.push(self.interner.intern(id));
                    }
                }
                "Hole" => {
                    let hole = self.parse_hole(&child)?;
                    features.push(ecad::SetFeature::Hole(hole));
                }
                "SlotCavity" => {
                    let slot = self.parse_slot_cavity(&child)?;
                    features.push(ecad::SetFeature::Slot(slot));
                }
                "Pad" => {
                    let pad = self.parse_pad(&child)?;
                    features.push(ecad::SetFeature::Pad(pad));
                }
                "BadBoardMark" | "GlobalFiducial" | "GoodPanelMark" | "LocalFiducial" => {
                    let fiducial = self.parse_fiducial(&child)?;
                    features.push(ecad::SetFeature::Fiducial(fiducial));
                }
                "Polyline" => {
                    let trace = self.parse_trace(&child)?;
                    features.push(ecad::SetFeature::Trace(trace));
                }
                "Features" => {
                    for feature in self.parse_features(&child)? {
                        features.push(feature);
                    }
                }
                "NonstandardAttribute" => {
                    if let Ok(attr) = self.parse_nonstandard_attribute(&child) {
                        nonstandard_attributes.push(attr);
                    }
                }
                _ => {}
            }
        }

        Ok(FeatureSet {
            net,
            geometry,
            polarity,
            spec_refs,
            features,
            nonstandard_attributes,
        })
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
        let kind = match self.name(node) {
            "BadBoardMark" => ecad::FiducialKind::BadBoardMark,
            "GlobalFiducial" => ecad::FiducialKind::Global,
            "GoodPanelMark" => ecad::FiducialKind::GoodPanelMark,
            "LocalFiducial" => ecad::FiducialKind::Local,
            name => {
                return Err(Ipc2581Error::InvalidStructure(format!(
                    "Unknown fiducial element: {name}"
                )));
            }
        };

        let mut location = None;
        let xform = self.parse_xform_child(node, units);
        let mut shape = None;
        let mut pin_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "Location" => {
                    location = Some(Location {
                        x: self.parse_f64_attr_with_units(&child, "x", "Location", units)?,
                        y: self.parse_f64_attr_with_units(&child, "y", "Location", units)?,
                    });
                }
                "PinRef" => pin_ref = Some(self.parse_pin_ref(&child)?),
                "StandardPrimitiveRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        shape = Some(ecad::FiducialShape::StandardPrimitiveRef(
                            self.interner.intern(id),
                        ));
                    }
                }
                name if is_standard_primitive_name(name) => {
                    shape = Some(ecad::FiducialShape::Primitive(
                        self.parse_standard_primitive(&child, units)?,
                    ));
                }
                _ => {}
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

    fn parse_features(&mut self, features_node: &Node) -> Result<Vec<ecad::SetFeature>> {
        let mut features = Vec::new();
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let offset = self.parse_features_location(features_node, units);

        for child in self.element_children(features_node) {
            match self.name(&child) {
                "Polygon" => {
                    if let Some(feature) = self.parse_feature_polygon(&child, units, offset) {
                        features.push(feature);
                    }
                }
                "Polyline" => {
                    if let Ok(polyline) =
                        self.parse_feature_polyline(&child, units, offset.x, offset.y)
                    {
                        features.push(ecad::SetFeature::Polyline(polyline));
                    }
                }
                "Line" => {
                    if let Ok(line) = self.parse_line(&child, units, offset.x, offset.y) {
                        features.push(ecad::SetFeature::Line(line));
                    }
                }
                "Arc" => {
                    if let Ok(arc) = self.parse_feature_arc(&child, units, offset.x, offset.y) {
                        features.push(ecad::SetFeature::Arc(arc));
                    }
                }
                "UserSpecial" => {
                    if let Ok(primitive) = self.parse_user_special(&child, units) {
                        features.push(ecad::SetFeature::UserPrimitive(
                            ecad::FeatureUserPrimitive {
                                primitive,
                                x: offset.x,
                                y: offset.y,
                            },
                        ));
                    }
                }
                "StandardPrimitiveRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        features.push(ecad::SetFeature::StandardPrimitiveRef(
                            ecad::FeaturePrimitiveRef {
                                id: self.interner.intern(id),
                                x: offset.x,
                                y: offset.y,
                            },
                        ));
                    }
                }
                "UserPrimitiveRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        features.push(ecad::SetFeature::UserPrimitiveRef(
                            ecad::FeaturePrimitiveRef {
                                id: self.interner.intern(id),
                                x: offset.x,
                                y: offset.y,
                            },
                        ));
                    }
                }
                _ => {}
            }
        }

        Ok(features)
    }

    fn parse_features_location(&self, features_node: &Node, units: Units) -> Point {
        self.element_children(features_node)
            .find(|child| self.name(child) == "Location")
            .map(|location| Point {
                x: self
                    .attr(&location, "x")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units))
                    .unwrap_or(0.0),
                y: self
                    .attr(&location, "y")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units))
                    .unwrap_or(0.0),
            })
            .unwrap_or(Point { x: 0.0, y: 0.0 })
    }

    fn parse_feature_polygon(
        &mut self,
        node: &Node,
        units: Units,
        offset: Point,
    ) -> Option<ecad::SetFeature> {
        self.parse_polygon(node, units)
            .ok()
            .map(|polygon| ecad::SetFeature::Polygon(Self::translate_polygon(polygon, offset)))
    }

    fn translate_polygon(mut polygon: Polygon, offset: Point) -> Polygon {
        Self::translate_point(&mut polygon.begin, offset);
        for step in &mut polygon.steps {
            match step {
                PolyStep::Segment(segment) => {
                    Self::translate_point(&mut segment.point, offset);
                }
                PolyStep::Curve(curve) => {
                    Self::translate_point(&mut curve.point, offset);
                    Self::translate_point(&mut curve.center, offset);
                }
            }
        }
        polygon
    }

    fn translate_point(point: &mut Point, offset: Point) {
        point.x += offset.x;
        point.y += offset.y;
    }

    fn parse_line(
        &mut self,
        node: &Node,
        units: Units,
        offset_x: f64,
        offset_y: f64,
    ) -> Result<ecad::Line> {
        let start_x = self.parse_f64_attr_with_units(node, "startX", "Line", units)? + offset_x;
        let start_y = self.parse_f64_attr_with_units(node, "startY", "Line", units)? + offset_y;
        let end_x = self.parse_f64_attr_with_units(node, "endX", "Line", units)? + offset_x;
        let end_y = self.parse_f64_attr_with_units(node, "endY", "Line", units)? + offset_y;

        let mut line_width = 0.25;
        let mut line_end = None;
        let mut line_property = None;
        let mut line_desc_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "LineDesc" => {
                    (line_width, line_end, line_property) =
                        self.parse_feature_line_desc(&child, units)?;
                }
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok(ecad::Line {
            start_x,
            start_y,
            end_x,
            end_y,
            line_desc_ref,
            line_width,
            line_end,
            line_property,
        })
    }

    fn parse_feature_arc(
        &mut self,
        node: &Node,
        units: Units,
        offset_x: f64,
        offset_y: f64,
    ) -> Result<ecad::FeatureArc> {
        let arc = self.parse_user_arc(node, units)?;
        let mut line_width = 0.25;
        let mut line_end = None;
        let mut line_property = None;
        let mut line_desc_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "LineDesc" => {
                    (line_width, line_end, line_property) =
                        self.parse_feature_line_desc(&child, units)?;
                }
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok(ecad::FeatureArc {
            start: Point {
                x: arc.start.x + offset_x,
                y: arc.start.y + offset_y,
            },
            end: Point {
                x: arc.end.x + offset_x,
                y: arc.end.y + offset_y,
            },
            center: Point {
                x: arc.center.x + offset_x,
                y: arc.center.y + offset_y,
            },
            clockwise: arc.clockwise,
            line_desc_ref,
            line_width,
            line_end,
            line_property,
        })
    }

    fn parse_feature_polyline(
        &mut self,
        node: &Node,
        units: Units,
        offset_x: f64,
        offset_y: f64,
    ) -> Result<ecad::FeaturePolyline> {
        let mut begin = None;
        let mut steps = Vec::new();
        let mut line_width = 0.25;
        let mut line_end = None;
        let mut line_property = None;
        let mut line_desc_ref = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "PolyBegin" => {
                    begin = Some(Point {
                        x: self.parse_f64_attr_with_units(&child, "x", "PolyBegin", units)?
                            + offset_x,
                        y: self.parse_f64_attr_with_units(&child, "y", "PolyBegin", units)?
                            + offset_y,
                    });
                }
                "PolyStepSegment" => {
                    steps.push(PolyStep::Segment(PolyStepSegment {
                        point: Point {
                            x: self.parse_f64_attr_with_units(
                                &child,
                                "x",
                                "PolyStepSegment",
                                units,
                            )? + offset_x,
                            y: self.parse_f64_attr_with_units(
                                &child,
                                "y",
                                "PolyStepSegment",
                                units,
                            )? + offset_y,
                        },
                    }));
                }
                "PolyStepCurve" => {
                    steps.push(PolyStep::Curve(PolyStepCurve {
                        point: Point {
                            x: self.parse_f64_attr_with_units(
                                &child,
                                "x",
                                "PolyStepCurve",
                                units,
                            )? + offset_x,
                            y: self.parse_f64_attr_with_units(
                                &child,
                                "y",
                                "PolyStepCurve",
                                units,
                            )? + offset_y,
                        },
                        center: Point {
                            x: self.parse_f64_attr_with_units(
                                &child,
                                "centerX",
                                "PolyStepCurve",
                                units,
                            )? + offset_x,
                            y: self.parse_f64_attr_with_units(
                                &child,
                                "centerY",
                                "PolyStepCurve",
                                units,
                            )? + offset_y,
                        },
                        clockwise: self.parse_bool_attr(&child, "clockwise")?,
                    }));
                }
                "LineDesc" => {
                    (line_width, line_end, line_property) =
                        self.parse_feature_line_desc(&child, units)?;
                }
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok(ecad::FeaturePolyline {
            begin: begin.ok_or(Ipc2581Error::MissingElement("PolyBegin in Polyline"))?,
            steps,
            line_desc_ref,
            line_width,
            line_end,
            line_property,
        })
    }

    fn parse_hole(&mut self, node: &Node) -> Result<Hole> {
        // Hole is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.attr(node, "name").map(|s| self.interner.intern(s));
        let diameter = self.parse_f64_attr_with_units(node, "diameter", "Hole", units)?;
        let plating_status_str = self.required_attr(node, "platingStatus", "Hole")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;
        let x = self.parse_f64_attr_with_units(node, "x", "Hole", units)?;
        let y = self.parse_f64_attr_with_units(node, "y", "Hole", units)?;

        Ok(Hole {
            name,
            diameter,
            plating_status,
            x,
            y,
        })
    }

    fn parse_slot_cavity(&mut self, node: &Node) -> Result<Slot> {
        // SlotCavity is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = self.attr(node, "name").map(|s| self.interner.intern(s));
        let plating_status_str = self.required_attr(node, "platingStatus", "SlotCavity")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;

        // Parse Location child element
        let (x, y) = if let Some(location_node) = self
            .element_children(node)
            .find(|n| self.name(n) == "Location")
        {
            let x = self.parse_f64_attr_with_units(&location_node, "x", "Location", units)?;
            let y = self.parse_f64_attr_with_units(&location_node, "y", "Location", units)?;
            (x, y)
        } else {
            (0.0, 0.0)
        };

        // Parse shape - can be Outline OR StandardPrimitive
        // Per IPC-2581 spec 8.2.3.10.6: "The shape is defined by the substitution
        // group Feature, which can be either a user defined shape or a standard
        // primitive shape."
        let shape = if let Some(outline_node) = self
            .element_children(node)
            .find(|n| self.name(n) == "Outline")
        {
            // Outline path with polygon
            if let Some(polygon_node) = self
                .element_children(&outline_node)
                .find(|n| self.name(n) == "Polygon")
            {
                SlotShape::Outline(self.parse_polygon(&polygon_node, units)?)
            } else {
                return Err(Ipc2581Error::MissingElement(
                    "Polygon in SlotCavity Outline",
                ));
            }
        } else {
            // Try to parse as StandardPrimitive (Circle, Oval, RectCenter, etc.)
            // Find first child that is a StandardPrimitive
            let primitive_node = self
                .element_children(node)
                .find(|n| {
                    matches!(
                        self.name(n),
                        "Circle"
                            | "Oval"
                            | "RectCenter"
                            | "RectRound"
                            | "Ellipse"
                            | "Diamond"
                            | "Hexagon"
                            | "Octagon"
                            | "Triangle"
                    )
                })
                .ok_or(Ipc2581Error::MissingElement(
                    "Shape (Outline or StandardPrimitive) in SlotCavity",
                ))?;

            SlotShape::Primitive(self.parse_standard_primitive(&primitive_node, units)?)
        };

        let xform = self.parse_xform_child(node, units);

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

        // Check for x, y as attributes first (legacy format)
        let mut x = self
            .attr(node, "x")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));
        let mut y = self
            .attr(node, "y")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Look for Location child element (standard format)
        for child in self.element_children(node) {
            if self.name(&child) == "Location" {
                x = self
                    .attr(&child, "x")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units));
                y = self
                    .attr(&child, "y")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units));
                break;
            }
        }

        let xform = self.parse_xform_child(node, units);

        // Parse inline StandardPrimitiveRef if present
        let standard_primitive_ref = self
            .element_children(node)
            .find(|n| self.name(n) == "StandardPrimitiveRef")
            .and_then(|n| self.attr(&n, "id"))
            .map(|id| self.interner.intern(id));

        // Parse inline UserPrimitiveRef if present
        let user_primitive_ref = self
            .element_children(node)
            .find(|n| self.name(n) == "UserPrimitiveRef")
            .and_then(|n| self.attr(&n, "id"))
            .map(|id| self.interner.intern(id));

        let mut pin_ref = None;
        for child in self.element_children(node) {
            if self.name(&child) == "PinRef" {
                pin_ref = Some(self.parse_pin_ref(&child)?);
                break;
            }
        }

        Ok(Pad {
            padstack_def_ref,
            x,
            y,
            xform,
            standard_primitive_ref,
            user_primitive_ref,
            pin_ref,
        })
    }

    fn parse_trace(&mut self, node: &Node) -> Result<Trace> {
        // Trace is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        // LineDescRef can be attribute OR child element <LineDescRef id="..."/>
        let mut line_desc_ref = self
            .attr(node, "lineDescRef")
            .map(|s| self.interner.intern(s));

        let mut points = Vec::new();
        let mut steps = Vec::new();
        for child in self.element_children(node) {
            match self.name(&child) {
                "PolyBegin" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "TracePoint", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "TracePoint", units)?;
                    points.push(TracePoint { x, y });
                }
                "PolyStepSegment" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "TracePoint", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "TracePoint", units)?;
                    let point = Point { x, y };
                    points.push(TracePoint { x, y });
                    steps.push(PolyStep::Segment(PolyStepSegment { point }));
                }
                "PolyStepCurve" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "TracePoint", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "TracePoint", units)?;
                    let center_x =
                        self.parse_f64_attr_with_units(&child, "centerX", "PolyStepCurve", units)?;
                    let center_y =
                        self.parse_f64_attr_with_units(&child, "centerY", "PolyStepCurve", units)?;
                    let clockwise = self.parse_bool_attr(&child, "clockwise")?;
                    points.push(TracePoint { x, y });
                    steps.push(PolyStep::Curve(PolyStepCurve {
                        point: Point { x, y },
                        center: Point {
                            x: center_x,
                            y: center_y,
                        },
                        clockwise,
                    }));
                }
                "LineDescRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok(Trace {
            line_desc_ref,
            points,
            steps,
        })
    }

    fn parse_layer_function(&self, s: &str) -> Result<LayerFunction> {
        match s {
            // Conductive layers
            "CONDUCTOR" => Ok(LayerFunction::Conductor),
            "CONDFILM" => Ok(LayerFunction::CondFilm),
            "CONDFOIL" => Ok(LayerFunction::CondFoil),
            "PLANE" => Ok(LayerFunction::Plane),
            "SIGNAL" => Ok(LayerFunction::Signal),
            "MIXED" => Ok(LayerFunction::Mixed),

            // Coating layers (surface finishes)
            "COATINGCOND" => Ok(LayerFunction::CoatingCond),
            "COATINGNONCOND" => Ok(LayerFunction::CoatingNonCond),

            // Soldermask and paste
            "SOLDERMASK" => Ok(LayerFunction::Soldermask),
            "SOLDERPASTE" => Ok(LayerFunction::Solderpaste),
            "PASTEMASK" => Ok(LayerFunction::Pastemask),

            // Silkscreen/Legend
            "SILKSCREEN" => Ok(LayerFunction::Silkscreen),
            "LEGEND" => Ok(LayerFunction::Legend),

            // Drilling and routing
            "DRILL" => Ok(LayerFunction::Drill),
            "ROUT" | "ROUTE" => Ok(LayerFunction::Rout),
            "V_CUT" => Ok(LayerFunction::VCut),
            "SCORE" => Ok(LayerFunction::Score),
            "EDGE_CHAMFER" => Ok(LayerFunction::EdgeChamfer),
            "EDGE_PLATING" => Ok(LayerFunction::EdgePlating),

            // Dielectric layers
            "DIELBASE" => Ok(LayerFunction::DielBase),
            "DIELCORE" => Ok(LayerFunction::DielCore),
            "DIELPREG" => Ok(LayerFunction::DielPreg),
            "DIELADHV" => Ok(LayerFunction::DielAdhv),
            "DIELBONDPLY" => Ok(LayerFunction::DielBondPly),
            "DIELCOVERLAY" => Ok(LayerFunction::DielCoverlay),

            // Component layers
            "COMPONENT_TOP" => Ok(LayerFunction::ComponentTop),
            "COMPONENT_BOTTOM" => Ok(LayerFunction::ComponentBottom),
            "COMPONENT_EMBEDDED" => Ok(LayerFunction::ComponentEmbedded),
            "COMPONENT_FORMED" => Ok(LayerFunction::ComponentFormed),
            "ASSEMBLY" => Ok(LayerFunction::Assembly),

            // Specialized material layers
            "CONDUCTIVE_ADHESIVE" => Ok(LayerFunction::ConductiveAdhesive),
            "GLUE" => Ok(LayerFunction::Glue),
            "HOLEFILL" => Ok(LayerFunction::HoleFill),
            "SOLDERBUMP" => Ok(LayerFunction::SolderBump),
            "STIFFENER" => Ok(LayerFunction::Stiffener),
            "CAPACITIVE" => Ok(LayerFunction::Capacitive),
            "RESISTIVE" => Ok(LayerFunction::Resistive),

            // Documentation and tooling
            "DOCUMENT" => Ok(LayerFunction::Document),
            "GRAPHIC" => Ok(LayerFunction::Graphic),
            "BOARD_OUTLINE" => Ok(LayerFunction::BoardOutline),
            "BOARD_FAB" => Ok(LayerFunction::BoardFab),
            "REWORK" => Ok(LayerFunction::Rework),
            "FIXTURE" => Ok(LayerFunction::Fixture),
            "PROBE" => Ok(LayerFunction::Probe),
            "COURTYARD" => Ok(LayerFunction::Courtyard),
            "LANDPATTERN" => Ok(LayerFunction::LandPattern),
            "THIEVING_KEEP_INOUT" => Ok(LayerFunction::ThievingKeepInout),

            // Composite
            "STACKUP_COMPOSITE" => Ok(LayerFunction::StackupComposite),

            _ => Ok(LayerFunction::Other),
        }
    }

    fn parse_side(&self, s: &str) -> Result<Side> {
        match s {
            "TOP" => Ok(Side::Top),
            "BOTTOM" => Ok(Side::Bottom),
            "BOTH" => Ok(Side::Both),
            "INTERNAL" => Ok(Side::Internal),
            "ALL" => Ok(Side::All),
            "NONE" => Ok(Side::None),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid side: {}",
                s
            ))),
        }
    }

    fn parse_polarity(&self, s: &str) -> Result<Polarity> {
        match s {
            "POSITIVE" => Ok(Polarity::Positive),
            "NEGATIVE" => Ok(Polarity::Negative),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid polarity: {}",
                s
            ))),
        }
    }

    fn parse_where_measured(&self, s: &str) -> Result<WhereMeasured> {
        match s {
            "METAL" => Ok(WhereMeasured::Metal),
            "MASK" => Ok(WhereMeasured::Mask),
            "LAMINATE" => Ok(WhereMeasured::Laminate),
            "OTHER" => Ok(WhereMeasured::Other),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid whereMeasured: {}",
                s
            ))),
        }
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
        let diameter =
            self.parse_f64_attr_with_units(node, "diameter", "PadstackHoleDef", units)?;
        let plating_status_str = self.required_attr(node, "platingStatus", "PadstackHoleDef")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;
        let plus_tol = self.parse_f64_attr_with_units(node, "plusTol", "PadstackHoleDef", units)?;
        let minus_tol =
            self.parse_f64_attr_with_units(node, "minusTol", "PadstackHoleDef", units)?;
        let x = self.parse_f64_attr_with_units(node, "x", "PadstackHoleDef", units)?;
        let y = self.parse_f64_attr_with_units(node, "y", "PadstackHoleDef", units)?;

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
        let pad_use_str = self.required_attr(node, "padUse", "PadstackPadDef")?;
        let pad_use = self.parse_pad_use(self.interner.resolve(pad_use_str))?;

        // Parse StandardPrimitiveRef if present
        let standard_primitive_ref = self
            .element_children(node)
            .find(|n| self.name(n) == "StandardPrimitiveRef")
            .and_then(|n| self.attr(&n, "id"))
            .map(|id| self.interner.intern(id));

        // Parse UserPrimitiveRef if present
        let user_primitive_ref = self
            .element_children(node)
            .find(|n| self.name(n) == "UserPrimitiveRef")
            .and_then(|n| self.attr(&n, "id"))
            .map(|id| self.interner.intern(id));

        Ok(PadstackPadDef {
            layer_ref,
            pad_use,
            standard_primitive_ref,
            user_primitive_ref,
        })
    }

    fn parse_plating_status(&self, s: &str) -> Result<PlatingStatus> {
        match s {
            "PLATED" => Ok(PlatingStatus::Plated),
            "NONPLATED" => Ok(PlatingStatus::NonPlated),
            "VIA" => Ok(PlatingStatus::Via),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid plating status: {}",
                s
            ))),
        }
    }

    fn parse_pad_use(&self, s: &str) -> Result<PadUse> {
        match s {
            "REGULAR" => Ok(PadUse::Regular),
            "ANTIPAD" => Ok(PadUse::Antipad),
            "THERMAL" => Ok(PadUse::Thermal),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Invalid pad use: {}",
                s
            ))),
        }
    }

    fn parse_bom(&mut self, node: &Node) -> Result<Bom> {
        let name = self.required_attr(node, "name", "Bom")?;

        let item_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "BomItem")
            .collect::<Vec<_>>();
        let items = item_nodes
            .into_iter()
            .map(|n| self.parse_bom_item(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(Bom { name, items })
    }

    fn parse_bom_item(&mut self, node: &Node) -> Result<BomItem> {
        let oem_design_number_ref = self.required_attr(node, "OEMDesignNumberRef", "BomItem")?;

        let quantity = self.attr(node, "quantity").and_then(|s| s.parse().ok());
        let pin_count = self.attr(node, "pinCount").and_then(|s| s.parse().ok());

        let category = self.attr(node, "category").map(|s| match s {
            "ELECTRICAL" => BomCategory::Electrical,
            "MECHANICAL" => BomCategory::Mechanical,
            "DOCUMENT" => BomCategory::Document,
            _ => BomCategory::Electrical, // Default
        });

        let description = self
            .attr(node, "description")
            .map(|s| self.interner.intern(s));

        let mut ref_des_list = Vec::new();
        let mut characteristics = None;

        for child in self.element_children(node) {
            match self.name(&child) {
                "RefDes" => ref_des_list.push(self.parse_bom_ref_des(&child)?),
                "Characteristics" => characteristics = Some(self.parse_characteristics(&child)?),
                _ => {}
            }
        }

        Ok(BomItem {
            oem_design_number_ref,
            quantity,
            pin_count,
            category,
            description,
            ref_des_list,
            characteristics,
        })
    }

    fn parse_bom_ref_des(&mut self, node: &Node) -> Result<BomRefDes> {
        let name = self.required_attr(node, "name", "RefDes")?;
        let package_ref = self.required_attr(node, "packageRef", "RefDes")?;
        let layer_ref = self.required_attr(node, "layerRef", "RefDes")?;

        let populate = self
            .attr(node, "populate")
            .map(|s| s == "true")
            .unwrap_or(true);

        Ok(BomRefDes {
            name,
            package_ref,
            populate,
            layer_ref,
        })
    }

    fn parse_characteristics(&mut self, node: &Node) -> Result<Characteristics> {
        let category = self.attr(node, "category").map(|s| match s {
            "ELECTRICAL" => BomCategory::Electrical,
            "MECHANICAL" => BomCategory::Mechanical,
            "DOCUMENT" => BomCategory::Document,
            _ => BomCategory::Electrical,
        });

        let textual_nodes = self
            .element_children(node)
            .filter(|n| self.name(n) == "Textual")
            .collect::<Vec<_>>();
        let textuals = textual_nodes
            .into_iter()
            .map(|n| self.parse_textual_characteristic(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(Characteristics { category, textuals })
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
            .attr(node, "version")
            .and_then(|s| s.parse().ok())
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
                "SpecRef" => {
                    if let Some(id) = self.attr(&child, "id") {
                        spec_refs.push(self.interner.intern(id));
                    }
                }
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

        let qualified = self.attr(node, "qualified").map(|s| s == "true");

        let chosen = self.attr(node, "chosen").map(|s| s == "true");

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

        let rank = self.attr(node, "rank").and_then(|s| s.parse().ok());

        let cost = self.attr(node, "cost").and_then(|s| s.parse().ok());

        let moisture_sensitivity = self
            .attr(node, "moistureSensitivity")
            .and_then(MoistureSensitivity::parse);

        let availability = self.attr(node, "availability").map(|s| s == "true");

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

    fn parse_xform(&self, node: &Node, units: Units) -> Xform {
        let x_offset = self
            .attr(node, "xOffset")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|value| crate::units::to_mm(value, units))
            .unwrap_or(0.0);
        let y_offset = self
            .attr(node, "yOffset")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|value| crate::units::to_mm(value, units))
            .unwrap_or(0.0);
        let rotation = self
            .attr(node, "rotation")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let mirror = self
            .attr(node, "mirror")
            .map(|s| s == "true")
            .unwrap_or(false);
        let face_up = self
            .attr(node, "faceUp")
            .map(|s| s == "true")
            .unwrap_or(false);
        let scale = self
            .attr(node, "scale")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);

        Xform {
            x_offset,
            y_offset,
            rotation,
            mirror,
            face_up,
            scale,
        }
    }

    fn parse_xform_child(&self, node: &Node, units: Units) -> Option<Xform> {
        self.element_children(node)
            .find(|n| self.name(n) == "Xform")
            .map(|n| self.parse_xform(&n, units))
    }
}

fn has_z_axis_dim(doc: &Document, node: &Node) -> bool {
    doc.children_iter(*node)
        .filter(|child| doc.element(*child).is_some())
        .any(|child| {
            let name = doc.element(child).unwrap().name.local_name.as_ref();
            matches!(name, "MaterialCut" | "MaterialLeft")
                || (matches!(name, "Z_AxisDim" | "ZAxisDim")
                    && doc
                        .children_iter(child)
                        .filter(|grandchild| doc.element(*grandchild).is_some())
                        .any(|grandchild| {
                            let grandchild_name =
                                doc.element(grandchild).unwrap().name.local_name.as_ref();
                            matches!(grandchild_name, "MaterialCut" | "MaterialLeft")
                        }))
        })
}

fn parse_optional_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
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
        let doc = uppsala::parse(
            r#"<SlotCavity><Location x="0" y="0"/><MaterialCut depth="0.1"/></SlotCavity>"#,
        )
        .unwrap();
        let root = doc.document_element().unwrap();

        assert!(has_z_axis_dim(&doc, &root));
    }

    #[test]
    fn detects_wrapped_slot_cavity_z_axis_dimensions() {
        let doc = uppsala::parse(
            r#"<SlotCavity><Location x="0" y="0"/><ZAxisDim><MaterialLeft thickness="0.1"/></ZAxisDim></SlotCavity>"#,
        )
        .unwrap();
        let root = doc.document_element().unwrap();

        assert!(has_z_axis_dim(&doc, &root));
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
            .sets
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
    pub bom: Option<Bom>,
    pub avl: Option<Avl>,
}
