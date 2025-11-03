use crate::types::*;
use crate::{Interner, Ipc2581Error, Result, Symbol};
use roxmltree::{Document, Node};

/// Parser context holding the string interner and unit context
pub struct Parser {
    pub interner: Interner,
    /// Current ECAD units for converting dimensions (set when parsing CadHeader)
    ecad_units: Option<Units>,
    /// Specs from CadHeader (set when parsing CadHeader, used by StackupLayer parsing)
    specs: std::collections::HashMap<Symbol, ecad::Spec>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            interner: Interner::new(),
            ecad_units: None,
            specs: std::collections::HashMap::new(),
        }
    }

    pub fn parse_document(&mut self, doc: &Document) -> Result<ParsedIpc2581> {
        let root = doc.root_element();

        // Verify root element
        if root.tag_name().name() != "IPC-2581" {
            return Err(Ipc2581Error::InvalidStructure(format!(
                "Expected root element 'IPC-2581', found '{}'",
                root.tag_name().name()
            )));
        }

        // Parse revision
        let revision = root
            .attribute("revision")
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

        for child in root.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Content" => content_node = Some(child),
                "LogisticHeader" => logistic_header = Some(self.parse_logistic_header(&child)?),
                "HistoryRecord" => history_record = Some(self.parse_history_record(&child)?),
                "Ecad" => ecad = Some(self.parse_ecad(&child)?),
                "Bom" => bom = Some(self.parse_bom(&child)?),
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

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
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

        let level = node
            .attribute("level")
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
        match s {
            "FULL" => Ok(Level::Full),
            "PARTIAL" => Ok(Level::Partial),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown level: {}",
                s
            ))),
        }
    }

    fn parse_dictionary_color(&mut self, node: &Node) -> Result<DictionaryColor> {
        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryColor")
            .map(|n| self.parse_entry_color(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryColor { entries })
    }

    fn parse_entry_color(&mut self, node: &Node) -> Result<EntryColor> {
        let id = self.required_attr(node, "id", "EntryColor")?;

        let color_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Color")
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
        let units = node
            .attribute("units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryLineDesc")
            .map(|n| self.parse_entry_line_desc(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryLineDesc { units, entries })
    }

    fn parse_entry_line_desc(&mut self, node: &Node, units: Units) -> Result<EntryLineDesc> {
        let id = self.required_attr(node, "id", "EntryLineDesc")?;

        let line_desc_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "LineDesc")
            .ok_or(Ipc2581Error::MissingElement("LineDesc"))?;

        let line_desc = self.parse_line_desc(&line_desc_node, units)?;

        Ok(EntryLineDesc { id, line_desc })
    }

    fn parse_line_desc(&mut self, node: &Node, units: Units) -> Result<LineDesc> {
        let line_width = self.parse_f64_attr_with_units(node, "lineWidth", "LineDesc", units)?;
        let line_end_str = self.required_attr(node, "lineEnd", "LineDesc")?;
        let line_end = self.parse_line_end(self.interner.resolve(line_end_str))?;

        let line_property = node
            .attribute("lineProperty")
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
            "DASHED" => Ok(LineProperty::Dashed),
            "DOTTED" => Ok(LineProperty::Dotted),
            _ => Err(Ipc2581Error::InvalidAttribute(format!(
                "Unknown lineProperty: {}",
                s
            ))),
        }
    }

    fn parse_dictionary_fill_desc(&mut self, _node: &Node) -> Result<DictionaryFillDesc> {
        // Simplified for now
        Ok(DictionaryFillDesc::default())
    }

    fn parse_fill_desc(&mut self, node: &Node) -> Result<FillDesc> {
        let fill_property_str = self.required_attr(node, "fillProperty", "FillDesc")?;
        let fill_property = self.parse_fill_property(self.interner.resolve(fill_property_str))?;

        let angle1 = node
            .attribute("angle1")
            .map(|s| s.parse::<f64>())
            .transpose()
            .map_err(|_| Ipc2581Error::InvalidAttribute("angle1".to_string()))?;

        let angle2 = node
            .attribute("angle2")
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

    /// Parse optional FillDesc and LineDesc children from a primitive node
    fn parse_fill_and_line_desc(
        &mut self,
        node: &Node,
    ) -> Result<(Option<FillProperty>, Option<Symbol>)> {
        let mut fill_property = None;
        let mut line_desc_ref = None;

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "FillDesc" => {
                    let fill_desc = self.parse_fill_desc(&child)?;
                    fill_property = Some(fill_desc.fill_property);
                }
                "LineDescRef" => {
                    if let Some(id) = child.attribute("id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok((fill_property, line_desc_ref))
    }

    fn parse_dictionary_standard(&mut self, node: &Node) -> Result<DictionaryStandard> {
        let units = node
            .attribute("units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryStandard")
            .map(|n| self.parse_entry_standard(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryStandard { units, entries })
    }

    fn parse_entry_standard(&mut self, node: &Node, units: Units) -> Result<EntryStandard> {
        let id = self.required_attr(node, "id", "EntryStandard")?;

        // Find the primitive child element
        let primitive_node = node
            .children()
            .find(|n| n.is_element())
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive"))?;

        let primitive = self.parse_standard_primitive(&primitive_node, units)?;

        Ok(EntryStandard { id, primitive })
    }

    fn parse_standard_primitive(&mut self, node: &Node, units: Units) -> Result<StandardPrimitive> {
        match node.tag_name().name() {
            "Circle" => {
                let diameter = self.parse_f64_attr_with_units(node, "diameter", "Circle", units)?;

                // Parse optional FillDesc child
                let (fill_property, line_desc_ref) = self.parse_fill_and_line_desc(node)?;

                Ok(StandardPrimitive::Circle(Circle {
                    diameter,
                    fill_property,
                    line_desc_ref,
                }))
            }
            "RectCenter" => {
                let width = self.parse_f64_attr_with_units(node, "width", "RectCenter", units)?;
                let height = self.parse_f64_attr_with_units(node, "height", "RectCenter", units)?;

                // Parse optional FillDesc child
                let (fill_property, line_desc_ref) = self.parse_fill_and_line_desc(node)?;

                Ok(StandardPrimitive::RectCenter(RectCenter {
                    width,
                    height,
                    fill_property,
                    line_desc_ref,
                }))
            }
            "RectRound" => {
                let width = self.parse_f64_attr_with_units(node, "width", "RectRound", units)?;
                let height = self.parse_f64_attr_with_units(node, "height", "RectRound", units)?;
                let radius = self.parse_f64_attr_with_units(node, "radius", "RectRound", units)?;
                let upper_right = self.parse_bool_attr(node, "upperRight").unwrap_or(false);
                let upper_left = self.parse_bool_attr(node, "upperLeft").unwrap_or(false);
                let lower_right = self.parse_bool_attr(node, "lowerRight").unwrap_or(false);
                let lower_left = self.parse_bool_attr(node, "lowerLeft").unwrap_or(false);

                Ok(StandardPrimitive::RectRound(RectRound {
                    width,
                    height,
                    radius,
                    upper_right,
                    upper_left,
                    lower_right,
                    lower_left,
                }))
            }
            "Oval" => {
                let width = self.parse_f64_attr_with_units(node, "width", "Oval", units)?;
                let height = self.parse_f64_attr_with_units(node, "height", "Oval", units)?;
                Ok(StandardPrimitive::Oval(Oval { width, height }))
            }
            "Contour" => {
                // Parse Polygon and Cutouts
                let polygon_node = node
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
                    .ok_or(Ipc2581Error::MissingElement("Polygon"))?;

                let polygon = self.parse_polygon(&polygon_node, units)?;

                let cutouts = node
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "Cutout")
                    .map(|n| self.parse_polygon(&n, units))
                    .collect::<Result<Vec<_>>>()?;

                Ok(StandardPrimitive::Contour(Contour { polygon, cutouts }))
            }
            name => Err(Ipc2581Error::InvalidStructure(format!(
                "Unknown standard primitive: {}",
                name
            ))),
        }
    }

    fn parse_polygon(&mut self, node: &Node, units: Units) -> Result<Polygon> {
        let mut begin: Option<PolyBegin> = None;
        let mut steps = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "PolyBegin" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "PolyBegin", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "PolyBegin", units)?;
                    begin = Some(PolyBegin { x, y });
                }
                "PolyStepSegment" => {
                    let x =
                        self.parse_f64_attr_with_units(&child, "x", "PolyStepSegment", units)?;
                    let y =
                        self.parse_f64_attr_with_units(&child, "y", "PolyStepSegment", units)?;
                    steps.push(PolyStep::Segment(PolyStepSegment { x, y }));
                }
                "PolyStepCurve" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "PolyStepCurve", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "PolyStepCurve", units)?;
                    let center_x =
                        self.parse_f64_attr_with_units(&child, "centerX", "PolyStepCurve", units)?;
                    let center_y =
                        self.parse_f64_attr_with_units(&child, "centerY", "PolyStepCurve", units)?;
                    let clockwise = self.parse_bool_attr(&child, "clockwise")?;
                    steps.push(PolyStep::Curve(PolyStepCurve {
                        x,
                        y,
                        center_x,
                        center_y,
                        clockwise,
                    }));
                }
                _ => {} // Ignore other elements like LineDesc, FillDesc
            }
        }

        let begin = begin.ok_or(Ipc2581Error::MissingElement("PolyBegin"))?;
        Ok(Polygon { begin, steps })
    }

    fn parse_dictionary_user(&mut self, node: &Node) -> Result<DictionaryUser> {
        let units = node
            .attribute("units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        // Use MILLIMETER as default if not specified
        let dict_units = units.unwrap_or(Units::Millimeter);

        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryUser")
            .map(|n| self.parse_entry_user(&n, dict_units))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryUser { units, entries })
    }

    fn parse_entry_user(&mut self, node: &Node, units: Units) -> Result<EntryUser> {
        let id = self.required_attr(node, "id", "EntryUser")?;

        // Find the primitive child element (currently only supporting UserSpecial)
        let primitive_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "UserSpecial")
            .ok_or(Ipc2581Error::MissingElement("UserPrimitive"))?;

        let primitive = self.parse_user_special(&primitive_node, units)?;

        Ok(EntryUser { id, primitive })
    }

    fn parse_user_special(&mut self, node: &Node, units: Units) -> Result<UserPrimitive> {
        let mut shapes = Vec::new();

        for child in node.children() {
            if !child.is_element() {
                continue;
            }

            let tag_name = child.tag_name().name();

            // Parse shape based on tag name
            let shape_type = match tag_name {
                "Circle" => {
                    let diameter =
                        self.parse_f64_attr_with_units(&child, "diameter", "Circle", units)?;
                    let (fill_property, line_desc_ref) = self.parse_fill_and_line_desc(&child)?;
                    Some(UserShapeType::Circle(Circle {
                        diameter,
                        fill_property,
                        line_desc_ref,
                    }))
                }
                "RectCenter" => {
                    let width =
                        self.parse_f64_attr_with_units(&child, "width", "RectCenter", units)?;
                    let height =
                        self.parse_f64_attr_with_units(&child, "height", "RectCenter", units)?;
                    let (fill_property, line_desc_ref) = self.parse_fill_and_line_desc(&child)?;
                    Some(UserShapeType::RectCenter(RectCenter {
                        width,
                        height,
                        fill_property,
                        line_desc_ref,
                    }))
                }
                "Oval" => {
                    let width = self.parse_f64_attr_with_units(&child, "width", "Oval", units)?;
                    let height = self.parse_f64_attr_with_units(&child, "height", "Oval", units)?;
                    Some(UserShapeType::Oval(Oval { width, height }))
                }
                "Polygon" => {
                    let polygon = self.parse_polygon(&child, units)?;
                    Some(UserShapeType::Polygon(polygon))
                }
                // TODO: Add Polyline parsing when needed
                _ => None, // Skip unknown shape types
            };

            if let Some(shape_type) = shape_type {
                // Parse optional LineDesc and FillDesc children
                let line_desc = child
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == "LineDesc")
                    .map(|n| self.parse_line_desc(&n, units))
                    .transpose()?;

                let fill_desc = child
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == "FillDesc")
                    .map(|n| self.parse_fill_desc(&n))
                    .transpose()?;

                shapes.push(UserShape {
                    shape: shape_type,
                    line_desc,
                    fill_desc,
                });
            }
        }

        Ok(UserPrimitive::UserSpecial(UserSpecial { shapes }))
    }

    fn parse_logistic_header(&mut self, _node: &Node) -> Result<LogisticHeader> {
        // Simplified for now
        Ok(LogisticHeader {
            roles: vec![],
            enterprises: vec![],
            persons: vec![],
        })
    }

    fn parse_history_record(&mut self, node: &Node) -> Result<HistoryRecord> {
        // Parse number as f64 first, then convert to u32 (some files use "1.0")
        let number = match node.attribute("number") {
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
                })
            }
        };

        let origination = self.required_attr(node, "origination", "HistoryRecord")?;
        let software = self.optional_attr(node, "software");
        let last_change = self.required_attr(node, "lastChange", "HistoryRecord")?;

        // Parse FileRevision child element
        let mut file_revision = None;
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "FileRevision" {
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
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "SoftwarePackage" {
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
        node.attribute(attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })
            .map(|s| self.interner.intern(s))
    }

    fn optional_attr(&mut self, node: &Node, attr: &str) -> Option<Symbol> {
        node.attribute(attr).map(|s| self.interner.intern(s))
    }

    fn parse_f64_attr(
        &self,
        node: &Node,
        attr: &'static str,
        element: &'static str,
    ) -> Result<f64> {
        let attr_val = node
            .attribute(attr)
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
        let attr_val = node
            .attribute(attr)
            .ok_or(Ipc2581Error::MissingAttribute { element, attr })?;
        attr_val
            .parse()
            .map_err(|_| Ipc2581Error::InvalidAttribute(format!("Invalid u8 value for {}", attr)))
    }

    fn parse_bool_attr(&self, node: &Node, attr: &'static str) -> Result<bool> {
        match node.attribute(attr) {
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
        let cad_header_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "CadHeader")
            .ok_or(Ipc2581Error::MissingElement("CadHeader"))?;
        let mut cad_header = self.parse_cad_header(&cad_header_node)?;

        // Store ECAD units for use when parsing dimensions
        self.ecad_units = Some(cad_header.units);

        // Move specs into parser context to avoid cloning
        // We'll move them back after parsing CadData
        self.specs = std::mem::take(&mut cad_header.specs);

        let cad_data_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "CadData")
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
        let units = node
            .attribute("units")
            .ok_or(Ipc2581Error::MissingAttribute {
                element: "CadHeader",
                attr: "units",
            })?;
        let units = self.parse_units(units)?;

        // Parse Spec elements
        let mut specs = std::collections::HashMap::new();
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "Spec" {
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

        // Parse child elements for material and dielectric properties
        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "General" => {
                    // Extract material from General type="MATERIAL"
                    if child.attribute("type") == Some("MATERIAL") {
                        // Look for Property, ColorTerm, and Color elements
                        for prop in child.children().filter(|n| n.is_element()) {
                            match prop.tag_name().name() {
                                "Property" => {
                                    if let Some(text) = prop.attribute("text") {
                                        if !text.is_empty() {
                                            let text_sym = self.interner.intern(text);
                                            // Store all property texts
                                            properties.push(text_sym);
                                            // Take the first non-empty material text we find
                                            if material.is_none() {
                                                material = Some(text_sym);
                                            }
                                        }
                                    }
                                }
                                "ColorTerm" => {
                                    // Parse ColorTerm name attribute (e.g., "GREEN", "WHITE", "BLACK")
                                    if let Some(color_name) = prop.attribute("name") {
                                        color_term = Some(self.interner.intern(color_name));
                                    }
                                }
                                "Color" => {
                                    // Parse Color r, g, b attributes (0-255)
                                    if let (Some(r_str), Some(g_str), Some(b_str)) = (
                                        prop.attribute("r"),
                                        prop.attribute("g"),
                                        prop.attribute("b"),
                                    ) {
                                        if let (Ok(r), Ok(g), Ok(b)) = (
                                            r_str.parse::<u8>(),
                                            g_str.parse::<u8>(),
                                            b_str.parse::<u8>(),
                                        ) {
                                            color_rgb = Some((r, g, b));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "Dielectric" => {
                    let dielectric_type = child.attribute("type");
                    // Look for Property with value attribute
                    for prop in child.children().filter(|n| n.is_element()) {
                        if prop.tag_name().name() == "Property" {
                            if let Some(value_str) = prop.attribute("value") {
                                if let Ok(value) = value_str.parse::<f64>() {
                                    match dielectric_type {
                                        Some("DIELECTRIC_CONSTANT") => {
                                            dielectric_constant = Some(value)
                                        }
                                        Some("LOSS_TANGENT") => loss_tangent = Some(value),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                "Conductor" => {
                    // Parse copper weight from Conductor type="WEIGHT"
                    if child.attribute("type") == Some("WEIGHT") {
                        for prop in child.children().filter(|n| n.is_element()) {
                            if prop.tag_name().name() == "Property" {
                                if let Some(value_str) = prop.attribute("value") {
                                    if let Ok(value) = value_str.parse::<f64>() {
                                        // Check unit - should be OZ
                                        let unit = prop.attribute("unit").unwrap_or("OZ");
                                        if unit.to_uppercase() == "OZ" {
                                            copper_weight_oz = Some(value);
                                        }
                                    }
                                }
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

    fn parse_surface_finish(&mut self, node: &Node) -> Result<ecad::SurfaceFinish> {
        // Parse Finish element
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "Finish" {
                let finish_type_str = child.attribute("type").unwrap_or("OTHER");
                let finish_type = self.parse_finish_type(finish_type_str)?;
                let comment = child.attribute("comment").map(|s| self.interner.intern(s));

                let mut products = Vec::new();
                for product_node in child.children().filter(|n| n.is_element()) {
                    if product_node.tag_name().name() == "Product" {
                        if let Some(product_name) = product_node.attribute("name") {
                            let criteria = product_node
                                .attribute("criteria")
                                .and_then(|s| self.parse_product_criteria(s).ok());

                            products.push(ecad::FinishProduct {
                                name: self.interner.intern(product_name),
                                criteria,
                            });
                        }
                    }
                }

                return Ok(ecad::SurfaceFinish {
                    finish_type,
                    comment,
                    products,
                });
            }
        }

        // No Finish element found
        Err(Ipc2581Error::MissingElement("Finish in SurfaceFinish"))
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

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
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
        let overall_thickness = node
            .attribute("overallThickness")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Parse whereMeasured attribute
        let where_measured = node
            .attribute("whereMeasured")
            .and_then(|s| self.parse_where_measured(s).ok());

        // Parse tolerances
        let tol_plus = node
            .attribute("tolPlus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let tol_minus = node
            .attribute("tolMinus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let mut layers = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "StackupGroup" {
                // StackupGroup contains StackupLayer elements
                for layer_node in child.children().filter(|n| n.is_element()) {
                    if layer_node.tag_name().name() == "StackupLayer" {
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
        let thickness = node
            .attribute("thickness")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Convert tolerances if present
        // NOTE: IPC-2581 spec allows tolPercent attribute to indicate if these are percentages
        // For a pure parser, we should keep the raw values and let downstream code handle interpretation
        // Currently we convert to mm for convenience (TODO: make this a separate normalization step)
        let tol_plus = node
            .attribute("tolPlus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let tol_minus = node
            .attribute("tolMinus")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        let layer_number = node.attribute("sequence").and_then(|s| s.parse().ok());

        // Look up material and dielectric properties from Spec via SpecRef
        let mut material = None;
        let mut spec_ref = None;
        let mut dielectric_constant = None;
        let mut loss_tangent = None;

        // Parse SpecRef child element
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "SpecRef" {
                if let Some(spec_id) = child.attribute("id") {
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

        // Single pass through children
        let mut datum = None;
        let mut profile = None;
        let mut padstack_defs = Vec::new();
        let mut packages = Vec::new();
        let mut components = Vec::new();
        let mut logical_nets = Vec::new();
        let mut phy_net_groups = Vec::new();
        let mut layer_features = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Datum" => datum = Some(self.parse_datum(&child)?),
                "Profile" => profile = Some(self.parse_profile(&child)?),
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
            datum,
            profile,
            padstack_defs,
            packages,
            components,
            logical_nets,
            phy_net_groups,
            layer_features,
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
        let polygon_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
            .ok_or(Ipc2581Error::MissingElement("Polygon in Profile"))?;

        // Profile is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);
        let polygon = self.parse_polygon(&polygon_node, units)?;

        // Parse cutouts (voids within the board outline)
        let mut cutouts = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "Cutout" {
                // Cutout contains a Polygon child
                if let Some(cutout_polygon_node) = child
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
                {
                    cutouts.push(self.parse_polygon(&cutout_polygon_node, units)?);
                }
            }
        }

        Ok(Profile { polygon, cutouts })
    }

    fn parse_package(&mut self, node: &Node) -> Result<Package> {
        let name = self.required_attr(node, "name", "Package")?;
        let package_type = self.required_attr(node, "type", "Package")?;
        let pin_one = node.attribute("pinOne").map(|s| self.interner.intern(s));
        let height = node.attribute("height").and_then(|s| s.parse().ok());

        Ok(Package {
            name,
            package_type,
            pin_one,
            height,
        })
    }

    fn parse_component(&mut self, node: &Node) -> Result<Component> {
        let ref_des = self.required_attr(node, "refDes", "Component")?;
        let package_ref = self.required_attr(node, "packageRef", "Component")?;
        let layer_ref = self.required_attr(node, "layerRef", "Component")?;

        let mount_type = node.attribute("mountType").map(|s| match s {
            "SMT" => MountType::Smt,
            "THT" => MountType::Tht,
            _ => MountType::Other,
        });

        let part = node.attribute("part").map(|s| self.interner.intern(s));

        Ok(Component {
            ref_des,
            package_ref,
            layer_ref,
            mount_type,
            part,
        })
    }

    fn parse_logical_net(&mut self, node: &Node) -> Result<LogicalNet> {
        let name = self.required_attr(node, "name", "LogicalNet")?;

        let pin_refs = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "PinRef")
            .map(|n| self.parse_pin_ref(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(LogicalNet { name, pin_refs })
    }

    fn parse_pin_ref(&mut self, node: &Node) -> Result<PinRef> {
        let component_ref = self.required_attr(node, "componentRef", "PinRef")?;
        let pin = self.required_attr(node, "pin", "PinRef")?;
        Ok(PinRef { component_ref, pin })
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

        let side = node
            .attribute("side")
            .map(|s| self.parse_side(s))
            .transpose()?;
        let polarity = node
            .attribute("polarity")
            .map(|s| self.parse_polarity(s))
            .transpose()?;

        // Parse layer-specific Profile (for rigid-flex)
        let mut profile = None;
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "Profile" {
                profile = Some(self.parse_profile(&child)?);
                break;
            }
        }

        Ok(Layer {
            name,
            layer_function,
            side,
            polarity,
            profile,
        })
    }

    fn parse_layer_feature(&mut self, node: &Node) -> Result<LayerFeature> {
        let layer_ref = self.required_attr(node, "layerRef", "LayerFeature")?;

        let sets = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Set")
            .map(|n| self.parse_feature_set(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(LayerFeature { layer_ref, sets })
    }

    fn parse_feature_set(&mut self, node: &Node) -> Result<FeatureSet> {
        let net = node.attribute("net").map(|s| self.interner.intern(s));
        let geometry = node.attribute("geometry").map(|s| self.interner.intern(s));

        // Parse polarity attribute
        let polarity = node.attribute("polarity").and_then(|s| match s {
            "POSITIVE" => Some(Polarity::Positive),
            "NEGATIVE" => Some(Polarity::Negative),
            _ => None,
        });

        let mut holes = Vec::new();
        let mut slots = Vec::new();
        let mut pads = Vec::new();
        let mut traces = Vec::new();
        let mut polygons = Vec::new();
        let mut lines = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Hole" => holes.push(self.parse_hole(&child)?),
                "SlotCavity" => slots.push(self.parse_slot_cavity(&child)?),
                "Pad" => pads.push(self.parse_pad(&child)?),
                "Polyline" => traces.push(self.parse_trace(&child)?),
                "Features" => {
                    // Parse polygons and lines from Features
                    let (feat_polygons, feat_lines) = self.parse_features(&child);
                    polygons.extend(feat_polygons);
                    lines.extend(feat_lines);
                }
                _ => {}
            }
        }

        Ok(FeatureSet {
            net,
            geometry,
            polarity,
            holes,
            slots,
            pads,
            traces,
            polygons,
            lines,
        })
    }

    fn parse_features(&mut self, features_node: &Node) -> (Vec<Polygon>, Vec<ecad::Line>) {
        let mut polygons = Vec::new();
        let mut lines = Vec::new();
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        // Check for Location offset (applies to all geometry in Features)
        let mut offset_x = 0.0;
        let mut offset_y = 0.0;
        for child in features_node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "Location" {
                offset_x = child
                    .attribute("x")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units))
                    .unwrap_or(0.0);
                offset_y = child
                    .attribute("y")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units))
                    .unwrap_or(0.0);
                break;
            }
        }

        for child in features_node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Polygon" => {
                    if let Ok(poly) = self.parse_polygon(&child, units) {
                        polygons.push(poly);
                    }
                }
                "Polyline" => {
                    // Polyline traces in Features
                    lines.extend(self.parse_polyline_to_lines(&child, units, offset_x, offset_y));
                }
                "UserSpecial" => {
                    // UserSpecial contains Contour > Polygon OR Line
                    for inner in child.children().filter(|n| n.is_element()) {
                        match inner.tag_name().name() {
                            "Contour" => {
                                for poly_node in inner.children().filter(|n| n.is_element()) {
                                    if poly_node.tag_name().name() == "Polygon" {
                                        if let Ok(poly) = self.parse_polygon(&poly_node, units) {
                                            polygons.push(poly);
                                        }
                                    }
                                }
                            }
                            "Line" => {
                                if let Ok(line) = self.parse_line(&inner, units) {
                                    lines.push(line);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        (polygons, lines)
    }

    fn parse_line(&mut self, node: &Node, units: Units) -> Result<ecad::Line> {
        let start_x = self.parse_f64_attr_with_units(node, "startX", "Line", units)?;
        let start_y = self.parse_f64_attr_with_units(node, "startY", "Line", units)?;
        let end_x = self.parse_f64_attr_with_units(node, "endX", "Line", units)?;
        let end_y = self.parse_f64_attr_with_units(node, "endY", "Line", units)?;

        let mut line_width = 0.25;
        let mut line_end = None;

        // Look for LineDesc child
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "LineDesc" {
                line_width = child
                    .attribute("lineWidth")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units))
                    .unwrap_or(0.25);

                line_end = child.attribute("lineEnd").and_then(|s| match s {
                    "ROUND" => Some(LineEnd::Round),
                    "SQUARE" => Some(LineEnd::Square),
                    "FLAT" => Some(LineEnd::Flat),
                    _ => None,
                });
                break;
            }
        }

        Ok(ecad::Line {
            start_x,
            start_y,
            end_x,
            end_y,
            line_width,
            line_end,
        })
    }

    fn parse_polyline_to_lines(
        &mut self,
        node: &Node,
        units: Units,
        offset_x: f64,
        offset_y: f64,
    ) -> Vec<ecad::Line> {
        let mut out = Vec::new();
        let mut current_x = offset_x;
        let mut current_y = offset_y;
        let mut line_width = 0.25;
        let mut line_end = None;

        // Parse points and LineDesc, tessellating curves
        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "PolyBegin" => {
                    current_x = self
                        .parse_f64_attr_with_units(&child, "x", "PolyBegin", units)
                        .unwrap_or(0.0)
                        + offset_x;
                    current_y = self
                        .parse_f64_attr_with_units(&child, "y", "PolyBegin", units)
                        .unwrap_or(0.0)
                        + offset_y;
                }
                "PolyStepSegment" => {
                    let x = self
                        .parse_f64_attr_with_units(&child, "x", "PolyStepSegment", units)
                        .unwrap_or(0.0)
                        + offset_x;
                    let y = self
                        .parse_f64_attr_with_units(&child, "y", "PolyStepSegment", units)
                        .unwrap_or(0.0)
                        + offset_y;

                    out.push(ecad::Line {
                        start_x: current_x,
                        start_y: current_y,
                        end_x: x,
                        end_y: y,
                        line_width,
                        line_end,
                    });

                    current_x = x;
                    current_y = y;
                }
                "PolyStepCurve" => {
                    // TODO: Properly handle arcs in UserSpecial
                    // Currently we skip arcs to keep parser pure (no tessellation dependencies)
                    // This should store raw Arc data and let downstream code handle tessellation
                    let end_x = self
                        .parse_f64_attr_with_units(&child, "x", "PolyStepCurve", units)
                        .unwrap_or(0.0)
                        + offset_x;
                    let end_y = self
                        .parse_f64_attr_with_units(&child, "y", "PolyStepCurve", units)
                        .unwrap_or(0.0)
                        + offset_y;

                    // For now, create straight line from current to end point
                    // Proper fix: store Arc data in UserSpecial and tessellate in ipc2581-tools
                    out.push(ecad::Line {
                        start_x: current_x,
                        start_y: current_y,
                        end_x,
                        end_y,
                        line_width,
                        line_end,
                    });

                    current_x = end_x;
                    current_y = end_y;
                }
                "LineDesc" => {
                    line_width = child
                        .attribute("lineWidth")
                        .and_then(|s| s.parse::<f64>().ok())
                        .map(|v| crate::units::to_mm(v, units))
                        .unwrap_or(0.25);
                    line_end = child.attribute("lineEnd").and_then(|s| match s {
                        "ROUND" => Some(LineEnd::Round),
                        "SQUARE" => Some(LineEnd::Square),
                        "FLAT" => Some(LineEnd::Flat),
                        _ => None,
                    });
                }
                _ => {}
            }
        }

        out
    }

    fn parse_hole(&mut self, node: &Node) -> Result<Hole> {
        // Hole is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let name = node.attribute("name").map(|s| self.interner.intern(s));
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

        let name = node.attribute("name").map(|s| self.interner.intern(s));
        let plating_status_str = self.required_attr(node, "platingStatus", "SlotCavity")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;

        // Parse Location child element
        let (x, y) = if let Some(location_node) = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Location")
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
        let shape = if let Some(outline_node) = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Outline")
        {
            // Outline path with polygon
            if let Some(polygon_node) = outline_node
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
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
            let primitive_node = node
                .children()
                .find(|n| {
                    n.is_element()
                        && matches!(
                            n.tag_name().name(),
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

        Ok(Slot {
            name,
            shape,
            plating_status,
            x,
            y,
        })
    }

    fn parse_pad(&mut self, node: &Node) -> Result<Pad> {
        // Pad is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        let padstack_def_ref = node
            .attribute("padstackDefRef")
            .map(|s| self.interner.intern(s));

        // Check for x, y as attributes first (legacy format)
        let mut x = node
            .attribute("x")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));
        let mut y = node
            .attribute("y")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| crate::units::to_mm(v, units));

        // Look for Location child element (standard format)
        for child in node.children() {
            if child.tag_name().name() == "Location" {
                x = child
                    .attribute("x")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units));
                y = child
                    .attribute("y")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|v| crate::units::to_mm(v, units));
                break;
            }
        }

        // Parse Xform child element if present
        let mut xform = None;
        for child in node.children() {
            if child.tag_name().name() == "Xform" {
                xform = Some(self.parse_xform(&child));
                break;
            }
        }

        // Parse inline StandardPrimitiveRef if present
        let standard_primitive_ref = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "StandardPrimitiveRef")
            .and_then(|n| n.attribute("id"))
            .map(|id| self.interner.intern(id));

        // Parse inline UserPrimitiveRef if present
        let user_primitive_ref = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "UserPrimitiveRef")
            .and_then(|n| n.attribute("id"))
            .map(|id| self.interner.intern(id));

        Ok(Pad {
            padstack_def_ref,
            x,
            y,
            xform,
            standard_primitive_ref,
            user_primitive_ref,
        })
    }

    fn parse_trace(&mut self, node: &Node) -> Result<Trace> {
        // Trace is in ECAD section, use ECAD units
        let units = self.ecad_units.unwrap_or(Units::Millimeter);

        // LineDescRef can be attribute OR child element <LineDescRef id="..."/>
        let mut line_desc_ref = node
            .attribute("lineDescRef")
            .map(|s| self.interner.intern(s));

        let mut points = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "PolyBegin" | "PolyStepSegment" => {
                    let x = self.parse_f64_attr_with_units(&child, "x", "TracePoint", units)?;
                    let y = self.parse_f64_attr_with_units(&child, "y", "TracePoint", units)?;
                    points.push(TracePoint { x, y });
                }
                "LineDescRef" => {
                    if let Some(id) = child.attribute("id") {
                        line_desc_ref = Some(self.interner.intern(id));
                    }
                }
                _ => {}
            }
        }

        Ok(Trace {
            line_desc_ref,
            points,
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

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
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
        let standard_primitive_ref = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "StandardPrimitiveRef")
            .and_then(|n| n.attribute("id"))
            .map(|id| self.interner.intern(id));

        // Parse UserPrimitiveRef if present
        let user_primitive_ref = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "UserPrimitiveRef")
            .and_then(|n| n.attribute("id"))
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

        let items = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "BomItem")
            .map(|n| self.parse_bom_item(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(Bom { name, items })
    }

    fn parse_bom_item(&mut self, node: &Node) -> Result<BomItem> {
        let oem_design_number_ref = self.required_attr(node, "OEMDesignNumberRef", "BomItem")?;

        let quantity = node.attribute("quantity").and_then(|s| s.parse().ok());
        let pin_count = node.attribute("pinCount").and_then(|s| s.parse().ok());

        let category = node.attribute("category").map(|s| match s {
            "ELECTRICAL" => BomCategory::Electrical,
            "MECHANICAL" => BomCategory::Mechanical,
            _ => BomCategory::Electrical, // Default
        });

        let mut ref_des_list = Vec::new();
        let mut characteristics = None;

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
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
            ref_des_list,
            characteristics,
        })
    }

    fn parse_bom_ref_des(&mut self, node: &Node) -> Result<BomRefDes> {
        let name = self.required_attr(node, "name", "RefDes")?;
        let package_ref = self.required_attr(node, "packageRef", "RefDes")?;
        let layer_ref = self.required_attr(node, "layerRef", "RefDes")?;

        let populate = node
            .attribute("populate")
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
        let category = node.attribute("category").map(|s| match s {
            "ELECTRICAL" => BomCategory::Electrical,
            "MECHANICAL" => BomCategory::Mechanical,
            _ => BomCategory::Electrical,
        });

        let textuals = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Textual")
            .map(|n| self.parse_textual_characteristic(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(Characteristics { category, textuals })
    }

    fn parse_textual_characteristic(&mut self, node: &Node) -> Result<TextualCharacteristic> {
        let definition_source = node
            .attribute("definitionSource")
            .map(|s| self.interner.intern(s));
        let name = node
            .attribute("textualCharacteristicName")
            .map(|s| self.interner.intern(s));
        let value = node
            .attribute("textualCharacteristicValue")
            .map(|s| self.interner.intern(s));

        Ok(TextualCharacteristic {
            definition_source,
            name,
            value,
        })
    }

    fn parse_xform(&self, node: &Node) -> Xform {
        let x_offset = node
            .attribute("xOffset")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let y_offset = node
            .attribute("yOffset")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let rotation = node
            .attribute("rotation")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let mirror = node
            .attribute("mirror")
            .map(|s| s == "true")
            .unwrap_or(false);
        let scale = node
            .attribute("scale")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);

        Xform {
            x_offset,
            y_offset,
            rotation,
            mirror,
            scale,
        }
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
}
