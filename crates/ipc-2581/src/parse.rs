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

    fn parse_ecad(&mut self, node: &Node) -> Result<Ecad> {
        let cad_data_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "CadData")
            .ok_or(Ipc2581Error::MissingElement("CadData"))?;
        let cad_data = self.parse_cad_data(&cad_data_node)?;

        Ok(Ecad { cad_data })
    }

    fn parse_cad_data(&mut self, node: &Node) -> Result<CadData> {
        let mut steps = Vec::new();
        let mut layers = Vec::new();
        let mut stackup = None;

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Step" => steps.push(self.parse_step(&child)?),
                "Layer" => layers.push(self.parse_layer(&child)?),
                "Stackup" => stackup = Some(self.parse_stackup(&child)?),
                _ => {}
            }
        }

        Ok(CadData {
            steps,
            layers,
            stackup,
        })
    }

    fn parse_stackup(&mut self, node: &Node) -> Result<Stackup> {
        let name = self.required_attr(node, "name", "Stackup")?;
        let overall_thickness = node
            .attribute("overallThickness")
            .and_then(|s| s.parse().ok());

        let mut layers = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "StackupGroup" => {
                    // StackupGroup contains StackupLayer elements
                    for layer_node in child.children().filter(|n| n.is_element()) {
                        if layer_node.tag_name().name() == "StackupLayer" {
                            layers.push(self.parse_stackup_layer(&layer_node)?);
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Stackup {
            name,
            overall_thickness,
            layers,
        })
    }

    fn parse_stackup_layer(&mut self, node: &Node) -> Result<StackupLayer> {
        let layer_ref = self.required_attr(node, "layerOrGroupRef", "StackupLayer")?;
        let thickness = node.attribute("thickness").and_then(|s| s.parse().ok());
        let material = node.attribute("material").map(|s| self.interner.intern(s));
        let dielectric_constant = node
            .attribute("dielectricConstant")
            .and_then(|s| s.parse().ok());
        let layer_number = node.attribute("sequence").and_then(|s| s.parse().ok());

        Ok(StackupLayer {
            layer_ref,
            thickness,
            material,
            dielectric_constant,
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
        let x = self.parse_f64_attr(node, "x", "Datum")?;
        let y = self.parse_f64_attr(node, "y", "Datum")?;
        Ok(Datum { x, y })
    }

    fn parse_profile(&mut self, node: &Node) -> Result<Profile> {
        let polygon_node = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "Polygon")
            .ok_or(Ipc2581Error::MissingElement("Polygon in Profile"))?;
        let polygon = self.parse_polygon(&polygon_node)?;
        Ok(Profile { polygon })
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

        Ok(Layer {
            name,
            layer_function,
            side,
            polarity,
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
        let mut holes = Vec::new();
        let mut pads = Vec::new();
        let mut traces = Vec::new();

        for child in node.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "Hole" => holes.push(self.parse_hole(&child)?),
                "Pad" => pads.push(self.parse_pad(&child)?),
                "Polyline" => traces.push(self.parse_trace(&child)?),
                _ => {}
            }
        }

        Ok(FeatureSet {
            holes,
            pads,
            traces,
        })
    }

    fn parse_hole(&mut self, node: &Node) -> Result<Hole> {
        let name = node.attribute("name").map(|s| self.interner.intern(s));
        let diameter = self.parse_f64_attr(node, "diameter", "Hole")?;
        let plating_status_str = self.required_attr(node, "platingStatus", "Hole")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;
        let x = self.parse_f64_attr(node, "x", "Hole")?;
        let y = self.parse_f64_attr(node, "y", "Hole")?;

        Ok(Hole {
            name,
            diameter,
            plating_status,
            x,
            y,
        })
    }

    fn parse_pad(&mut self, node: &Node) -> Result<Pad> {
        let padstack_def_ref = node
            .attribute("padstackDefRef")
            .map(|s| self.interner.intern(s));
        let x = node.attribute("x").and_then(|s| s.parse().ok());
        let y = node.attribute("y").and_then(|s| s.parse().ok());
        let rotation = node.attribute("rotation").and_then(|s| s.parse().ok());

        Ok(Pad {
            padstack_def_ref,
            x,
            y,
            rotation,
        })
    }

    fn parse_trace(&mut self, node: &Node) -> Result<Trace> {
        let line_desc_ref = node
            .attribute("lineDescRef")
            .map(|s| self.interner.intern(s));

        let mut points = Vec::new();
        for child in node.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == "PolyBegin" || child.tag_name().name() == "PolyStepSegment" {
                let x = self.parse_f64_attr(&child, "x", "TracePoint")?;
                let y = self.parse_f64_attr(&child, "y", "TracePoint")?;
                points.push(TracePoint { x, y });
            }
        }

        Ok(Trace {
            line_desc_ref,
            points,
        })
    }

    fn parse_layer_function(&self, s: &str) -> Result<LayerFunction> {
        match s {
            "CONDUCTOR" => Ok(LayerFunction::Conductor),
            "CONDFILM" => Ok(LayerFunction::CondFilm),
            "CONDFOIL" => Ok(LayerFunction::CondFoil),
            "PLANE" => Ok(LayerFunction::Plane),
            "SIGNAL" => Ok(LayerFunction::Signal),
            "MIXED" => Ok(LayerFunction::Mixed),
            "SOLDERMASK" => Ok(LayerFunction::Soldermask),
            "SOLDERPASTE" => Ok(LayerFunction::Solderpaste),
            "SILKSCREEN" => Ok(LayerFunction::Silkscreen),
            "LEGEND" => Ok(LayerFunction::Legend),
            "DRILL" => Ok(LayerFunction::Drill),
            "ROUT" => Ok(LayerFunction::Rout),
            "V_CUT" => Ok(LayerFunction::VCut),
            "DIELBASE" => Ok(LayerFunction::DielBase),
            "DIELCORE" => Ok(LayerFunction::DielCore),
            "DIELPREG" => Ok(LayerFunction::DielPreg),
            "DOCUMENT" => Ok(LayerFunction::Document),
            "GRAPHIC" => Ok(LayerFunction::Graphic),
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
        let name = self.required_attr(node, "name", "PadstackHoleDef")?;
        let diameter = self.parse_f64_attr(node, "diameter", "PadstackHoleDef")?;
        let plating_status_str = self.required_attr(node, "platingStatus", "PadstackHoleDef")?;
        let plating_status =
            self.parse_plating_status(self.interner.resolve(plating_status_str))?;
        let plus_tol = self.parse_f64_attr(node, "plusTol", "PadstackHoleDef")?;
        let minus_tol = self.parse_f64_attr(node, "minusTol", "PadstackHoleDef")?;
        let x = self.parse_f64_attr(node, "x", "PadstackHoleDef")?;
        let y = self.parse_f64_attr(node, "y", "PadstackHoleDef")?;

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

        Ok(PadstackPadDef { layer_ref, pad_use })
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

        let ref_des_list = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "RefDes")
            .map(|n| self.parse_bom_ref_des(&n))
            .collect::<Result<Vec<_>>>()?;

        Ok(BomItem {
            oem_design_number_ref,
            quantity,
            pin_count,
            category,
            ref_des_list,
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
