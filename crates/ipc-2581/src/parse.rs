use crate::types::*;
use crate::{Interner, Ipc2581Error, Result, Symbol};
use bumpalo::Bump;
use roxmltree::{Document, Node};

/// Parser context holding the arena and string interner
pub struct Parser<'arena> {
    #[allow(dead_code)]
    pub arena: &'arena Bump,
    pub interner: Interner,
}

impl<'arena> Parser<'arena> {
    pub fn new(arena: &'arena Bump) -> Self {
        Self {
            arena,
            interner: Interner::new(),
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

        // Parse Content (required)
        let content_node = root
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Content")
            .ok_or(Ipc2581Error::MissingElement("Content"))?;
        let content = self.parse_content(&content_node)?;

        // Parse optional sections
        let logistic_header = root
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "LogisticHeader")
            .map(|n| self.parse_logistic_header(&n))
            .transpose()?;

        let history_record = root
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "HistoryRecord")
            .map(|n| self.parse_history_record(&n))
            .transpose()?;

        Ok(ParsedIpc2581 {
            revision,
            content,
            logistic_header,
            history_record,
        })
    }

    fn parse_content(&mut self, node: &Node) -> Result<Content> {
        let role_ref = self.required_attr(node, "roleRef", "Content")?;

        // Parse FunctionMode
        let function_mode_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "FunctionMode")
            .ok_or(Ipc2581Error::MissingElement("FunctionMode"))?;
        let function_mode = self.parse_function_mode(&function_mode_node)?;

        // Parse StepRef elements
        let step_refs = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "StepRef")
            .map(|n| self.required_attr(&n, "name", "StepRef"))
            .collect::<Result<Vec<_>>>()?;

        // Parse LayerRef elements
        let layer_refs = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "LayerRef")
            .map(|n| self.required_attr(&n, "name", "LayerRef"))
            .collect::<Result<Vec<_>>>()?;

        // Parse BomRef elements
        let bom_refs = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "BomRef")
            .map(|n| self.required_attr(&n, "name", "BomRef"))
            .collect::<Result<Vec<_>>>()?;

        // Parse AvlRef elements
        let avl_refs = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "AvlRef")
            .map(|n| self.required_attr(&n, "name", "AvlRef"))
            .collect::<Result<Vec<_>>>()?;

        // Parse dictionaries
        let dictionary_color = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DictionaryColor")
            .map(|n| self.parse_dictionary_color(&n))
            .transpose()?
            .unwrap_or_default();

        let dictionary_line_desc = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DictionaryLineDesc")
            .map(|n| self.parse_dictionary_line_desc(&n))
            .transpose()?
            .unwrap_or_default();

        let dictionary_fill_desc = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DictionaryFillDesc")
            .map(|n| self.parse_dictionary_fill_desc(&n))
            .transpose()?
            .unwrap_or_default();

        let dictionary_standard = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DictionaryStandard")
            .map(|n| self.parse_dictionary_standard(&n))
            .transpose()?
            .unwrap_or_default();

        let dictionary_user = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DictionaryUser")
            .map(|n| self.parse_dictionary_user(&n))
            .transpose()?
            .unwrap_or_default();

        Ok(Content {
            role_ref,
            function_mode,
            step_refs,
            layer_refs,
            bom_refs,
            avl_refs,
            dictionary_color,
            dictionary_line_desc,
            dictionary_fill_desc,
            dictionary_standard,
            dictionary_user,
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

        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryLineDesc")
            .map(|n| self.parse_entry_line_desc(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryLineDesc { units, entries })
    }

    fn parse_entry_line_desc(&mut self, node: &Node) -> Result<EntryLineDesc> {
        let id = self.required_attr(node, "id", "EntryLineDesc")?;

        let line_desc_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "LineDesc")
            .ok_or(Ipc2581Error::MissingElement("LineDesc"))?;

        let line_desc = self.parse_line_desc(&line_desc_node)?;

        Ok(EntryLineDesc { id, line_desc })
    }

    fn parse_line_desc(&mut self, node: &Node) -> Result<LineDesc> {
        let line_width = self.parse_f64_attr(node, "lineWidth", "LineDesc")?;
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

    fn parse_dictionary_standard(&mut self, node: &Node) -> Result<DictionaryStandard> {
        let units = node
            .attribute("units")
            .map(|s| self.parse_units(s))
            .transpose()?;

        let entries = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "EntryStandard")
            .map(|n| self.parse_entry_standard(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(DictionaryStandard { units, entries })
    }

    fn parse_entry_standard(&mut self, node: &Node) -> Result<EntryStandard> {
        let id = self.required_attr(node, "id", "EntryStandard")?;

        // Find the primitive child element
        let primitive_node = node
            .children()
            .find(|n| n.is_element())
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive"))?;

        let primitive = self.parse_standard_primitive(&primitive_node)?;

        Ok(EntryStandard { id, primitive })
    }

    fn parse_standard_primitive(&mut self, node: &Node) -> Result<StandardPrimitive> {
        match node.tag_name().name() {
            "Circle" => {
                let diameter = self.parse_f64_attr(node, "diameter", "Circle")?;
                Ok(StandardPrimitive::Circle(Circle { diameter }))
            }
            "RectCenter" => {
                let width = self.parse_f64_attr(node, "width", "RectCenter")?;
                let height = self.parse_f64_attr(node, "height", "RectCenter")?;
                Ok(StandardPrimitive::RectCenter(RectCenter { width, height }))
            }
            "RectRound" => {
                let width = self.parse_f64_attr(node, "width", "RectRound")?;
                let height = self.parse_f64_attr(node, "height", "RectRound")?;
                let radius = self.parse_f64_attr(node, "radius", "RectRound")?;
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
                let width = self.parse_f64_attr(node, "width", "Oval")?;
                let height = self.parse_f64_attr(node, "height", "Oval")?;
                Ok(StandardPrimitive::Oval(Oval { width, height }))
            }
            "Contour" => {
                // Parse Polygon and Cutouts
                let polygon_node = node
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
                    .ok_or(Ipc2581Error::MissingElement("Polygon"))?;

                let polygon = self.parse_polygon(&polygon_node)?;

                let cutouts = node
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "Cutout")
                    .map(|n| self.parse_polygon(&n))
                    .collect::<Result<Vec<_>>>()?;

                Ok(StandardPrimitive::Contour(Contour { polygon, cutouts }))
            }
            name => Err(Ipc2581Error::InvalidStructure(format!(
                "Unknown standard primitive: {}",
                name
            ))),
        }
    }

    fn parse_polygon(&mut self, node: &Node) -> Result<Polygon> {
        let mut begin: Option<PolyBegin> = None;
        let mut steps = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "PolyBegin" => {
                    let x = self.parse_f64_attr(&child, "x", "PolyBegin")?;
                    let y = self.parse_f64_attr(&child, "y", "PolyBegin")?;
                    begin = Some(PolyBegin { x, y });
                }
                "PolyStepSegment" => {
                    let x = self.parse_f64_attr(&child, "x", "PolyStepSegment")?;
                    let y = self.parse_f64_attr(&child, "y", "PolyStepSegment")?;
                    steps.push(PolyStep::Segment(PolyStepSegment { x, y }));
                }
                "PolyStepCurve" => {
                    let x = self.parse_f64_attr(&child, "x", "PolyStepCurve")?;
                    let y = self.parse_f64_attr(&child, "y", "PolyStepCurve")?;
                    let center_x = self.parse_f64_attr(&child, "centerX", "PolyStepCurve")?;
                    let center_y = self.parse_f64_attr(&child, "centerY", "PolyStepCurve")?;
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

    fn parse_dictionary_user(&mut self, _node: &Node) -> Result<DictionaryUser> {
        // Simplified for now
        Ok(DictionaryUser::default())
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

        Ok(HistoryRecord {
            number,
            origination,
            software,
            last_change,
            file_revision: None,
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
}

/// Parsed IPC-2581 document (before transferring to user arena)
#[derive(Debug)]
pub struct ParsedIpc2581 {
    pub revision: Symbol,
    pub content: Content,
    pub logistic_header: Option<LogisticHeader>,
    pub history_record: Option<HistoryRecord>,
}
