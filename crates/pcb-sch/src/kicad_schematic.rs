//! Module for converting pcb_sch::Schematic to KiCad schematic format (.kicad_sch)

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use pcb_sexpr::{format_sexpr, parse, Sexpr};
use uuid::Uuid;

use crate::hierarchical_layout::{HierarchicalLayout, Size};
use crate::{Instance, InstanceKind, InstanceRef, Net, Schematic};

/// Enable debug mode to render component bounding boxes
/// Set this to true to visualize component bounds, layout allocations, and module boundaries
const DEBUG_MODE: bool = false;

/// Errors that can occur during schematic conversion
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    #[error("Missing symbol_path attribute for component {0}")]
    MissingSymbolPath(String),

    #[error("Failed to read symbol file {0}: {1}")]
    SymbolFileReadError(PathBuf, std::io::Error),

    #[error("Failed to parse symbol file {0}: {1}")]
    SymbolFileParseError(PathBuf, String),

    #[error("Symbol {0} not found in library {1}")]
    SymbolNotFound(String, PathBuf),

    #[error("Invalid instance reference: {0}")]
    InvalidInstanceRef(String),

    #[error(
        "KiCad symbol directory not found. Please set KICAD_SYMBOL_DIR environment variable or install KiCad"
    )]
    KiCadSymbolDirNotFound,
}

/// Minimal symbol info needed for creating instances
#[derive(Debug, Clone)]
struct SymbolInfo {
    name: String,
    reference: String,
    value: String,
    footprint: Option<String>,
    raw_sexpr: Sexpr,             // Store the complete symbol S-expression
    bounds: (f64, f64, f64, f64), // (min_x, min_y, max_x, max_y) of the symbol
    origin_offset: (f64, f64),    // Offset from symbol origin to top-left of bounds
}

/// Stores basic information about a global label that is attached to a component
/// (position and a very rough size estimate). This is used only for debugging
/// rectangles/packing calculations – *not* for the schematic output itself,
/// so an approximate size is perfectly sufficient here.
#[derive(Debug, Clone)]
struct LabelInfo {
    #[allow(dead_code)]
    position: (f64, f64),
    #[allow(dead_code)]
    width: f64,
    #[allow(dead_code)]
    height: f64,
}

/// Convert a pcb_sch::Schematic to a KiCad schematic file
pub fn to_kicad_schematic(sch: &Schematic, output_path: &Path) -> Result<String, ConversionError> {
    let mut converter = SchematicConverter::with_debug(DEBUG_MODE);
    converter.convert(sch, output_path)
}

struct SchematicConverter {
    /// Map from component instance ref to its KiCad symbol
    symbols: Vec<SchematicSymbol>,
    /// Map from instance ref to assigned UUID
    uuid_map: HashMap<InstanceRef, String>,
    /// Collected library symbols (symbol name -> symbol info)
    lib_symbols: HashMap<String, SymbolInfo>,
    /// Global labels for nets
    global_labels: Vec<GlobalLabel>,
    /// Wires for net connections
    wires: Vec<Wire>,
    /// Junctions for net intersections
    junctions: Vec<Junction>,
    /// Map from component instance ref to connected net names
    component_nets: HashMap<InstanceRef, Vec<String>>,
    /// Hierarchical layout engine
    layout_engine: HierarchicalLayout,
    /// Debug rectangles for component bounding boxes
    rectangles: Vec<Rectangle>,
    /// Text labels for module names
    texts: Vec<Text>,
    /// Map from component instance ref to its associated global label info (position + size)
    component_label_positions: HashMap<InstanceRef, Vec<LabelInfo>>,
    /// Debug mode flag - when true, renders component bounding boxes
    debug_mode: bool,
}

#[derive(Debug)]
struct SchematicSymbol {
    lib_id: String,
    position: (f64, f64),
    unit: i32,
    in_bom: bool,
    on_board: bool,
    uuid: String,
    reference: String,
    value: String,
    footprint: Option<String>,
    properties: HashMap<String, String>,
}

#[derive(Debug)]
struct GlobalLabel {
    text: String,
    position: (f64, f64),
    angle: f64,
    uuid: String,
    justify: Option<String>,
}

#[derive(Debug)]
struct Wire {
    points: Vec<(f64, f64)>,
    uuid: String,
}

#[derive(Debug)]
struct Junction {
    position: (f64, f64),
    uuid: String,
}

#[derive(Debug)]
struct Rectangle {
    start: (f64, f64),
    end: (f64, f64),
    uuid: String,
    color: Option<(u8, u8, u8, u8)>, // RGBA color for debug mode
}

#[derive(Debug)]
struct Text {
    content: String,
    position: (f64, f64),
    angle: f64,
    uuid: String,
}

impl SchematicConverter {
    fn with_debug(debug_mode: bool) -> Self {
        Self {
            symbols: Vec::new(),
            uuid_map: HashMap::new(),
            lib_symbols: HashMap::new(),
            global_labels: Vec::new(),
            wires: Vec::new(),
            junctions: Vec::new(),
            component_nets: HashMap::new(),
            layout_engine: HierarchicalLayout::new(10.0),
            rectangles: Vec::new(),
            texts: Vec::new(),
            component_label_positions: HashMap::new(),
            debug_mode,
        }
    }

    /// Find the KiCad symbol library directory
    fn find_kicad_symbol_dir() -> Option<PathBuf> {
        // Try different locations based on the platform
        let possible_paths = if cfg!(target_os = "macos") {
            vec![
                PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"),
                PathBuf::from("/Library/Application Support/kicad/symbols"),
                dirs::home_dir()
                    .map(|h| h.join("Library/Application Support/kicad/symbols"))
                    .unwrap_or_default(),
            ]
        } else if cfg!(target_os = "windows") {
            vec![
                PathBuf::from("C:\\Program Files\\KiCad\\share\\kicad\\symbols"),
                PathBuf::from("C:\\Program Files (x86)\\KiCad\\share\\kicad\\symbols"),
                dirs::config_dir()
                    .map(|c| c.join("kicad\\symbols"))
                    .unwrap_or_default(),
            ]
        } else {
            // Linux and other Unix-like systems
            vec![
                PathBuf::from("/usr/share/kicad/symbols"),
                PathBuf::from("/usr/local/share/kicad/symbols"),
                PathBuf::from("/opt/kicad/share/kicad/symbols"),
                dirs::home_dir()
                    .map(|h| h.join(".local/share/kicad/symbols"))
                    .unwrap_or_default(),
            ]
        };

        // Also check KICAD_SYMBOL_DIR environment variable
        if let Ok(env_path) = std::env::var("KICAD_SYMBOL_DIR") {
            let mut paths = vec![PathBuf::from(env_path)];
            paths.extend(possible_paths);
            return paths.into_iter().find(|p| p.exists());
        }

        possible_paths.into_iter().find(|p| p.exists())
    }

    fn convert(&mut self, sch: &Schematic, output_path: &Path) -> Result<String, ConversionError> {
        log::debug!("Starting KiCad schematic conversion");

        // First pass: collect component-net associations
        log::debug!("Collecting component-net associations");
        for (net_name, net) in &sch.nets {
            for port_ref in &net.ports {
                if let Ok(comp_ref) = self.get_component_ref(port_ref) {
                    self.component_nets
                        .entry(comp_ref)
                        .or_default()
                        .push(net_name.clone());
                }
            }
        }

        // Second pass: collect all components and their symbols
        log::debug!("Processing {} instances", sch.instances.len());
        for (inst_ref, instance) in &sch.instances {
            if instance.kind == InstanceKind::Component {
                log::debug!("Processing component: {inst_ref}");
                self.process_component(inst_ref, instance, output_path)?;
            }
        }

        // Build the module hierarchy for the new layout engine
        self.build_module_hierarchy(sch);

        // Calculate hierarchical layout
        log::debug!("Calculating hierarchical layout");
        let bounding_boxes = self.layout_engine.layout();

        // Convert BoundingBox positions to (f64, f64) positions for symbols
        let mut positions = HashMap::new();
        for (id, bbox) in &bounding_boxes {
            // Find the InstanceRef that corresponds to this string ID
            for (inst_ref, uuid) in &self.uuid_map {
                if uuid == id {
                    positions.insert(inst_ref.clone(), (bbox.position.x, bbox.position.y));

                    // In debug mode, create a rectangle for the layout engine's bounding box
                    if self.debug_mode {
                        let rect = Rectangle {
                            start: (bbox.position.x, bbox.position.y),
                            end: (
                                bbox.position.x + bbox.size.width,
                                bbox.position.y + bbox.size.height,
                            ),
                            uuid: Uuid::new_v4().to_string(),
                            color: Some((0, 255, 0, 128)), // Green with transparency for layout bounds
                        };

                        log::debug!(
                            "Layout engine bounds for {}: start=({}, {}), size=({}, {})",
                            inst_ref,
                            bbox.position.x,
                            bbox.position.y,
                            bbox.size.width,
                            bbox.size.height
                        );

                        self.rectangles.push(rect);
                    }

                    break;
                }
            }
        }

        // Create rectangles and labels for modules
        for (id, bbox) in &bounding_boxes {
            // Check if this is a module (not a component UUID)
            if !self.uuid_map.values().any(|uuid| uuid == id) {
                // Check if this module has more than one child
                let has_multiple_children = self.layout_engine.module_has_multiple_children(id);

                if has_multiple_children {
                    // This is a module with multiple children - create a rectangle and label for it
                    let rect = Rectangle {
                        start: (bbox.position.x, bbox.position.y),
                        end: (
                            bbox.position.x + bbox.size.width,
                            bbox.position.y + bbox.size.height,
                        ),
                        uuid: Uuid::new_v4().to_string(),
                        color: if self.debug_mode {
                            Some((0, 0, 255, 255)) // Blue for modules
                        } else {
                            None
                        },
                    };
                    self.rectangles.push(rect);

                    // Extract a readable name from the module ID
                    let module_name = if id.contains("root") && !id.contains('.') {
                        "root".to_string()
                    } else {
                        // The ID is the full InstanceRef string (path:module.instance.path)
                        // Extract the instance path part after the colon
                        if let Some(path_part) = id.split(':').nth(1) {
                            // Get the last component of the path
                            path_part
                                .split('.')
                                .next_back()
                                .unwrap_or(path_part)
                                .to_string()
                        } else {
                            id.to_string()
                        }
                    };

                    // Estimate text width (rough approximation: 0.7 * character count * font size)
                    let text_width = module_name.len() as f64 * 1.27 * 0.7;

                    // Add text label in the bottom-left corner with offset by half the text width
                    let text = Text {
                        content: module_name,
                        position: (
                            bbox.position.x + text_width / 2.0 + 2.0,
                            bbox.position.y + bbox.size.height - 2.0,
                        ),
                        angle: 0.0,
                        uuid: Uuid::new_v4().to_string(),
                    };
                    self.texts.push(text);
                }
            }
        }

        // Update symbol positions and create bounding box rectangles
        // First collect the data we need to avoid borrow checker issues
        let symbol_data: Vec<(String, InstanceRef)> = self
            .symbols
            .iter()
            .filter_map(|symbol| {
                self.uuid_map
                    .iter()
                    .find(|(_, uuid)| *uuid == &symbol.uuid)
                    .map(|(inst_ref, _)| (symbol.uuid.clone(), inst_ref.clone()))
            })
            .collect();

        // Now update positions
        for (uuid, inst_ref) in symbol_data {
            if let Some(position) = positions.get(&inst_ref) {
                // Find the symbol with this UUID and update its position
                if let Some(symbol) = self.symbols.iter_mut().find(|s| s.uuid == uuid) {
                    // Get the symbol info to find the origin offset
                    if let Some(symbol_info) = self.lib_symbols.get(&symbol.lib_id) {
                        // Adjust position by the symbol's origin offset
                        symbol.position = (
                            position.0 + symbol_info.origin_offset.0,
                            position.1 + symbol_info.origin_offset.1,
                        );

                        log::debug!(
                            "Component {} positioned at ({}, {}) with offset ({}, {})",
                            inst_ref,
                            symbol.position.0,
                            symbol.position.1,
                            symbol_info.origin_offset.0,
                            symbol_info.origin_offset.1
                        );

                        // In debug mode, create a rectangle for the component's actual bounds
                        if self.debug_mode {
                            // Get the actual symbol bounds
                            let (min_x, min_y, max_x, max_y) = symbol_info.bounds;

                            // Create a rectangle showing the component's actual bounds
                            // Note: KiCad schematic has Y increasing downward
                            let rect = Rectangle {
                                start: (
                                    symbol.position.0 + min_x,
                                    symbol.position.1 - min_y, // Flip Y coordinate
                                ),
                                end: (
                                    symbol.position.0 + max_x,
                                    symbol.position.1 - max_y, // Flip Y coordinate
                                ),
                                uuid: Uuid::new_v4().to_string(),
                                color: Some((255, 0, 0, 255)), // Red for components
                            };

                            log::debug!(
                                "Component {} - Debug bounding box: start=({}, {}), end=({}, {})",
                                inst_ref,
                                rect.start.0,
                                rect.start.1,
                                rect.end.0,
                                rect.end.1
                            );

                            self.rectangles.push(rect);

                            // Add a small debug label
                            let debug_text = Text {
                                content: format!(
                                    "C:{}",
                                    inst_ref.instance_path.last().unwrap_or(&"?".to_string())
                                ),
                                position: (
                                    symbol.position.0 + min_x,
                                    symbol.position.1 - max_y - 2.0, // Above the component
                                ),
                                angle: 0.0,
                                uuid: Uuid::new_v4().to_string(),
                            };
                            self.texts.push(debug_text);
                        }
                    } else {
                        // Fallback if symbol info not found
                        symbol.position = *position;
                        log::warn!(
                            "No symbol info found for {}, using position without offset",
                            symbol.lib_id
                        );
                    }
                }

                // Get the bounds from the layout engine
                if let Some(bounds) = self.get_component_bounds_from_engine(&uuid) {
                    // Create a rectangle for the bounding box
                    let rect = Rectangle {
                        start: (position.0 + bounds.0, position.1 - bounds.1),
                        end: (position.0 + bounds.2, position.1 - bounds.3),
                        uuid: Uuid::new_v4().to_string(),
                        color: None,
                    };

                    log::debug!(
                        "Component {} - Creating bounding box: start=({}, {}), end=({}, {})",
                        inst_ref,
                        rect.start.0,
                        rect.start.1,
                        rect.end.0,
                        rect.end.1
                    );

                    self.rectangles.push(rect);
                } else {
                    log::warn!("No bounds found for component {inst_ref}");
                }
            }
        }

        // Third pass: create net connections
        log::debug!("Processing {} nets", sch.nets.len());
        for (net_name, net) in &sch.nets {
            log::debug!("Processing net: {net_name}");
            self.process_net(net_name, net, sch)?;
        }

        // Add debug legend if in debug mode
        if self.debug_mode {
            self.add_debug_legend();
        }

        // Build the final schematic file
        log::debug!("Generating schematic S-expression");
        let result = self.generate_schematic_sexpr(output_path);
        log::debug!("Conversion complete");
        Ok(result)
    }

    fn process_component(
        &mut self,
        inst_ref: &InstanceRef,
        instance: &Instance,
        _output_path: &Path,
    ) -> Result<(), ConversionError> {
        log::debug!("Processing component {inst_ref}");

        // Get symbol path from attributes
        let symbol_path = instance
            .attributes
            .get("symbol_path")
            .and_then(|v| v.string())
            .ok_or_else(|| ConversionError::MissingSymbolPath(inst_ref.to_string()))?;

        log::debug!("Component {inst_ref} has symbol_path: {symbol_path}");

        // Try to load the symbol
        let symbol_result = if symbol_path.contains(':')
            && !symbol_path.contains('/')
            && !symbol_path.contains('\\')
        {
            // This is a KiCad library reference
            log::debug!("Loading symbol from KiCad library: {symbol_path}");
            self.load_symbol_from_library(symbol_path)
        } else {
            // This is a file path
            let symbol_path = PathBuf::from(symbol_path);
            log::debug!("Loading symbol from file: {symbol_path:?}");
            self.load_symbol(&symbol_path)
        };

        // Handle symbol loading errors gracefully
        let (symbol_info, lib_id) = match symbol_result {
            Ok(result) => {
                log::debug!("Successfully loaded symbol for {inst_ref}");
                result
            }
            Err(e) => {
                // Log warning and skip this component
                log::warn!("Failed to load symbol for component {inst_ref}: {e}");
                log::warn!("Skipping component {inst_ref} in schematic output");
                return Ok(());
            }
        };

        // Generate UUID for this instance
        let uuid = Uuid::new_v4().to_string();
        self.uuid_map.insert(inst_ref.clone(), uuid.clone());

        // Calculate extended bounds that include space for labels
        let extended_bounds = self.calculate_extended_bounds(inst_ref, symbol_info.bounds);

        log::debug!(
            "Component {} - Symbol bounds: {:?}, Extended bounds: {:?}",
            inst_ref,
            symbol_info.bounds,
            extended_bounds
        );

        // Add component to hierarchical layout engine using its UUID as ID
        let size = Size::new(
            extended_bounds.2 - extended_bounds.0, // width
            extended_bounds.3 - extended_bounds.1, // height
        );
        self.layout_engine.set_component_size(uuid.clone(), size);

        // Create the symbol instance (position will be set later)
        let symbol = SchematicSymbol {
            lib_id: lib_id.clone(),
            position: (0.0, 0.0), // Will be updated after layout calculation
            unit: 1,
            in_bom: true,
            on_board: true,
            uuid: uuid.clone(),
            reference: instance
                .reference_designator
                .clone()
                .unwrap_or_else(|| format!("U{}", self.symbols.len() + 1)),
            value: instance
                .attributes
                .get("mpn")
                .or_else(|| instance.attributes.get("type"))
                .and_then(|v| v.string())
                .map(|s| s.to_string())
                .unwrap_or_else(|| symbol_info.value.clone()),
            footprint: symbol_info.footprint.clone(),
            properties: {
                let mut props: HashMap<String, String> = instance
                    .attributes
                    .iter()
                    .filter_map(|(k, v)| v.string().map(|s| (k.clone(), s.to_string())))
                    .collect();

                // Add the instance path as a property
                props.insert("Path".to_string(), inst_ref.to_string());

                props
            },
        };

        self.symbols.push(symbol);

        // Add the library symbol if not already added
        // Use the full lib_id as the key to ensure uniqueness
        self.lib_symbols.entry(lib_id.clone()).or_insert_with(|| {
            // Update the symbol name in the raw S-expression to include the library prefix
            let mut updated_symbol_info = symbol_info;
            updated_symbol_info.name = lib_id.clone();

            // Update the symbol name in the raw S-expression
            if let Sexpr::List(ref mut items) = updated_symbol_info.raw_sexpr {
                if items.len() >= 2 {
                    items[1] = Sexpr::string(lib_id.clone());
                }
            }

            updated_symbol_info
        });

        log::debug!("Component {inst_ref} processed successfully");
        Ok(())
    }

    fn load_symbol_from_library(
        &mut self,
        library_ref: &str,
    ) -> Result<(SymbolInfo, String), ConversionError> {
        log::debug!("Loading symbol from library reference: {library_ref}");

        // Parse the library reference (Library:Name)
        let parts: Vec<&str> = library_ref.split(':').collect();
        if parts.len() != 2 {
            return Err(ConversionError::SymbolFileParseError(
                PathBuf::from(library_ref),
                "Invalid library reference format. Expected 'Library:Name'".to_string(),
            ));
        }

        let library_name = parts[0];
        let symbol_name = parts[1];
        log::debug!("Looking for symbol '{symbol_name}' in library '{library_name}'");

        // Find the KiCad symbol directory
        log::debug!("Finding KiCad symbol directory");
        let kicad_symbol_dir =
            Self::find_kicad_symbol_dir().ok_or(ConversionError::KiCadSymbolDirNotFound)?;
        log::debug!("KiCad symbol directory: {kicad_symbol_dir:?}");

        // Construct the path to the KiCad library
        let kicad_lib_path = kicad_symbol_dir.join(format!("{library_name}.kicad_sym"));
        log::debug!("Loading symbol file: {kicad_lib_path:?}");

        // Read and parse the symbol file
        let content = fs::read_to_string(&kicad_lib_path)
            .map_err(|e| ConversionError::SymbolFileReadError(kicad_lib_path.clone(), e))?;
        log::debug!("Read {} bytes from symbol file", content.len());

        log::debug!("Parsing symbol file");
        let sexpr = parse(&content).map_err(|e| {
            ConversionError::SymbolFileParseError(kicad_lib_path.clone(), e.to_string())
        })?;
        log::debug!("Symbol file parsed successfully");

        // Find the specific symbol in the library
        log::debug!("Searching for symbol '{symbol_name}' in parsed data");
        let symbol_info = self
            .find_symbol_in_library(&sexpr, symbol_name)
            .ok_or_else(|| {
                ConversionError::SymbolNotFound(symbol_name.to_string(), kicad_lib_path.clone())
            })?;

        let lib_id = format!("{library_name}:{symbol_name}");
        log::debug!("Symbol loaded successfully with lib_id: {lib_id}");
        Ok((symbol_info, lib_id))
    }

    fn load_symbol(&mut self, symbol_path: &Path) -> Result<(SymbolInfo, String), ConversionError> {
        log::debug!("Loading symbol from file: {symbol_path:?}");

        // Generate a nickname based on the file name
        let nickname = symbol_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lib")
            .to_string();

        // Read and parse the symbol file
        let content = fs::read_to_string(symbol_path)
            .map_err(|e| ConversionError::SymbolFileReadError(symbol_path.to_path_buf(), e))?;
        log::debug!("Read {} bytes from symbol file", content.len());

        log::debug!("Parsing symbol file");
        let sexpr = parse(&content).map_err(|e| {
            ConversionError::SymbolFileParseError(symbol_path.to_path_buf(), e.to_string())
        })?;
        log::debug!("Symbol file parsed successfully");

        // Find the first symbol in the library
        log::debug!("Finding first symbol in library");
        let symbol_info = self.find_first_symbol_in_library(&sexpr).ok_or_else(|| {
            ConversionError::SymbolNotFound("first symbol".to_string(), symbol_path.to_path_buf())
        })?;

        let lib_id = format!("{}:{}", nickname, symbol_info.name);
        log::debug!("Symbol loaded successfully with lib_id: {lib_id}");
        Ok((symbol_info, lib_id))
    }

    fn find_symbol_in_library(&self, sexpr: &Sexpr, symbol_name: &str) -> Option<SymbolInfo> {
        log::debug!("Searching for symbol '{symbol_name}' in S-expression");
        match sexpr {
            Sexpr::List(items) => {
                log::debug!("Searching through {} top-level items", items.len());
                for (i, item) in items.iter().enumerate() {
                    if let Sexpr::List(symbol_data) = item {
                        if symbol_data.len() >= 2 {
                            if let (Some(tag), Some(name)) = (
                                symbol_data.first().and_then(|s| s.as_atom()),
                                symbol_data.get(1).and_then(|s| s.as_atom()),
                            ) {
                                log::trace!("Item {i}: tag='{tag}', name='{name}'");
                                if tag == "symbol" && name == symbol_name {
                                    log::debug!("Found symbol '{symbol_name}'");
                                    return self.extract_symbol_info(item.clone());
                                }
                            }
                        }
                    }
                }
                log::debug!("Symbol '{symbol_name}' not found");
            }
            _ => {
                log::debug!("Top-level S-expression is not a list");
            }
        }
        None
    }

    fn find_first_symbol_in_library(&self, sexpr: &Sexpr) -> Option<SymbolInfo> {
        log::debug!("Finding first symbol in library");
        match sexpr {
            Sexpr::List(items) => {
                log::debug!("Searching through {} top-level items", items.len());
                for (i, item) in items.iter().enumerate() {
                    if let Sexpr::List(symbol_data) = item {
                        if let Some(tag) = symbol_data.first().and_then(|s| s.as_atom()) {
                            log::trace!("Item {i}: tag='{tag}'");
                            if tag == "symbol" {
                                log::debug!("Found first symbol");
                                return self.extract_symbol_info(item.clone());
                            }
                        }
                    }
                }
                log::debug!("No symbols found in library");
            }
            _ => {
                log::debug!("Top-level S-expression is not a list");
            }
        }
        None
    }

    fn extract_symbol_info(&self, symbol_sexpr: Sexpr) -> Option<SymbolInfo> {
        log::debug!("Extracting symbol info");
        if let Sexpr::List(symbol_data) = &symbol_sexpr {
            let name = symbol_data
                .get(1)
                .and_then(|s| s.as_atom())
                .map(|s| s.to_string())?;

            log::debug!("Extracting info for symbol '{name}'");

            let mut info = SymbolInfo {
                name: name.clone(),
                reference: "U".to_string(),
                value: name.clone(),
                footprint: None,
                raw_sexpr: symbol_sexpr.clone(),
                bounds: (0.0, 0.0, 0.0, 0.0),
                origin_offset: (0.0, 0.0),
            };

            // Extract just the properties we need
            let mut prop_count = 0;
            for item in &symbol_data[2..] {
                if let Sexpr::List(item_data) = item {
                    if let Some(tag) = item_data.first().and_then(|s| s.as_atom()) {
                        if tag == "property" {
                            if let (Some(key), Some(value)) = (
                                item_data.get(1).and_then(|s| s.as_atom()),
                                item_data.get(2).and_then(|s| s.as_atom()),
                            ) {
                                prop_count += 1;
                                log::trace!("Property: {key} = {value}");
                                match key {
                                    "Reference" => info.reference = value.to_string(),
                                    "Value" => info.value = value.to_string(),
                                    "Footprint" => info.footprint = Some(value.to_string()),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }

            // Calculate symbol bounds
            info.bounds = self.calculate_symbol_bounds(symbol_data);
            log::debug!("Symbol '{}' has bounds: {:?}", name, info.bounds);

            // Calculate origin offset (from symbol origin to top-left of bounds)
            info.origin_offset = (-info.bounds.0, -info.bounds.1);
            log::debug!("Symbol '{}' origin offset: {:?}", name, info.origin_offset);

            log::debug!("Extracted {prop_count} properties for symbol '{name}'");
            Some(info)
        } else {
            log::debug!("Symbol S-expression is not a list");
            None
        }
    }

    fn calculate_symbol_bounds(&self, symbol_data: &[Sexpr]) -> (f64, f64, f64, f64) {
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        let mut found_graphics = false;

        log::trace!(
            "Calculating symbol bounds from {} elements",
            symbol_data.len()
        );

        // Look for graphical elements in the symbol
        for item in symbol_data {
            if let Sexpr::List(item_data) = item {
                if let Some(tag) = item_data.first().and_then(|s| s.as_atom()) {
                    match tag {
                        "rectangle" | "polyline" | "circle" | "arc" => {
                            log::trace!("Found {tag} element");
                            // Extract coordinates from these elements
                            self.update_bounds_from_element(
                                item_data, &mut min_x, &mut max_x, &mut min_y, &mut max_y,
                            );
                            found_graphics = true;
                        }
                        "pin" => {
                            log::trace!("Found pin element");
                            // Pins also contribute to bounds, but only their base position
                            self.update_bounds_from_pin(
                                item_data, &mut min_x, &mut max_x, &mut min_y, &mut max_y,
                            );
                            found_graphics = true;
                        }
                        "symbol" => {
                            // Handle generic nested symbol blocks
                            let sub_items = if item_data.len() > 2 {
                                &item_data[2..]
                            } else {
                                &[]
                            };

                            if !sub_items.is_empty() {
                                log::trace!(
                                    "Processing nested symbol with {} items",
                                    sub_items.len()
                                );
                                let sub_bounds = self.calculate_symbol_bounds(sub_items);
                                if sub_bounds.0 != f64::MAX {
                                    min_x = min_x.min(sub_bounds.0);
                                    max_x = max_x.max(sub_bounds.2);
                                    min_y = min_y.min(sub_bounds.1);
                                    max_y = max_y.max(sub_bounds.3);
                                    found_graphics = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // If no bounds found, use a small default size
        if min_x == f64::MAX || !found_graphics {
            log::warn!("No bounds found for symbol: {symbol_data:?}");
            (-10.0, -10.0, 10.0, 10.0) // Small default size centered at origin
        } else {
            log::trace!(
                "Calculated bounds: min_x={min_x}, min_y={min_y}, max_x={max_x}, max_y={max_y}",
            );
            // Return the actual bounds as found
            (min_x, min_y, max_x, max_y)
        }
    }

    fn update_bounds_from_element(
        &self,
        element: &[Sexpr],
        min_x: &mut f64,
        max_x: &mut f64,
        min_y: &mut f64,
        max_y: &mut f64,
    ) {
        // Look for coordinate data in the element
        for item in element {
            if let Sexpr::List(sub_items) = item {
                if let Some(tag) = sub_items.first().and_then(|s| s.as_atom()) {
                    match tag {
                        "start" | "end" | "at" | "center" => {
                            if let (Some(x_str), Some(y_str)) = (
                                sub_items.get(1).and_then(|s| s.as_atom()),
                                sub_items.get(2).and_then(|s| s.as_atom()),
                            ) {
                                if let (Ok(x), Ok(y)) = (x_str.parse::<f64>(), y_str.parse::<f64>())
                                {
                                    *min_x = min_x.min(x);
                                    *max_x = max_x.max(x);
                                    *min_y = min_y.min(y);
                                    *max_y = max_y.max(y);
                                }
                            }
                        }
                        "pts" => {
                            // For polylines, check all points
                            for pt in &sub_items[1..] {
                                if let Sexpr::List(pt_data) = pt {
                                    if let Some("xy") = pt_data.first().and_then(|s| s.as_atom()) {
                                        if let (Some(x_str), Some(y_str)) = (
                                            pt_data.get(1).and_then(|s| s.as_atom()),
                                            pt_data.get(2).and_then(|s| s.as_atom()),
                                        ) {
                                            if let (Ok(x), Ok(y)) =
                                                (x_str.parse::<f64>(), y_str.parse::<f64>())
                                            {
                                                *min_x = min_x.min(x);
                                                *max_x = max_x.max(x);
                                                *min_y = min_y.min(y);
                                                *max_y = max_y.max(y);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn update_bounds_from_pin(
        &self,
        pin_data: &[Sexpr],
        min_x: &mut f64,
        max_x: &mut f64,
        min_y: &mut f64,
        max_y: &mut f64,
    ) {
        // Pins have an "at" position - we only want the base position, not the extended pin
        for item in pin_data {
            if let Sexpr::List(sub_items) = item {
                if let Some(tag) = sub_items.first().and_then(|s| s.as_atom()) {
                    if tag == "at" {
                        if let (Some(x_str), Some(y_str)) = (
                            sub_items.get(1).and_then(|s| s.as_atom()),
                            sub_items.get(2).and_then(|s| s.as_atom()),
                        ) {
                            if let (Ok(x), Ok(y)) = (x_str.parse::<f64>(), y_str.parse::<f64>()) {
                                log::trace!("Pin at ({x}, {y})");
                                // Only use the pin base position
                                *min_x = min_x.min(x);
                                *max_x = max_x.max(x);
                                *min_y = min_y.min(y);
                                *max_y = max_y.max(y);
                            }
                        }
                    }
                }
            }
        }
    }

    fn process_net(
        &mut self,
        net_name: &str,
        net: &Net,
        sch: &Schematic,
    ) -> Result<(), ConversionError> {
        // For each net, create global labels at pin positions
        for port_ref in &net.ports {
            // Get the component that owns this port
            let comp_ref = match self.get_component_ref(port_ref) {
                Ok(ref_) => ref_,
                Err(e) => {
                    log::warn!("Failed to get component reference for port {port_ref}: {e}");
                    continue;
                }
            };

            // Try to obtain the pin *number* first (from the port's "pad" attribute).
            let pin_identifier_owned: Option<String> = sch
                .instances
                .get(port_ref)
                .and_then(|inst| inst.attributes.get("pad"))
                .and_then(|v| v.string())
                .map(|s| s.to_string());

            // Fallback to the port name if no pad attribute present.
            let pin_identifier = pin_identifier_owned.as_deref().unwrap_or_else(|| {
                port_ref
                    .instance_path
                    .last()
                    .map(|s| s.as_str())
                    .unwrap_or("")
            });

            // Get the symbol position and lib_id
            if let Some(symbol_uuid) = self.uuid_map.get(&comp_ref) {
                if let Some(symbol) = self.symbols.iter().find(|s| &s.uuid == symbol_uuid) {
                    // Get the symbol definition to find pin position
                    if let Some(symbol_info) = self.lib_symbols.get(&symbol.lib_id) {
                        // Find the actual pin position
                        if let Some((pin_pos, pin_angle)) = self.find_pin_position(
                            &symbol_info.raw_sexpr,
                            pin_identifier,
                            symbol.position,
                        ) {
                            // Justification based on pin orientation:
                            // 0° (pin points right): label on left side, right-justified
                            // 90° (pin points up): label below, left-justified
                            // 180° (pin points left): label on right side, left-justified
                            // 270° (pin points down): label above, right-justified
                            let justify = match pin_angle.round() as i32 % 360 {
                                0 => Some("right".to_string()),
                                90 => Some("right".to_string()),
                                180 => Some("left".to_string()),
                                270 => Some("left".to_string()),
                                _ => Some("left".to_string()), // Default
                            };

                            let global_label = GlobalLabel {
                                text: net_name.to_string(),
                                position: pin_pos,
                                angle: pin_angle, // Use the pin angle for label orientation
                                uuid: Uuid::new_v4().to_string(),
                                justify,
                            };

                            self.global_labels.push(global_label);

                            // Estimate label dimensions based on number of characters.
                            // Default KiCad font height is 1.27 mm. Empirically each
                            // character is roughly 0.6× the height wide.
                            const FONT_HEIGHT: f64 = 1.27;
                            const CHAR_WIDTH_FACTOR: f64 = 0.6; // very rough
                            let mut est_width =
                                net_name.chars().count() as f64 * FONT_HEIGHT * CHAR_WIDTH_FACTOR;
                            let mut est_height = FONT_HEIGHT;

                            // Provide a small margin around the text so we don't clip
                            // descenders/etc.
                            est_width += 0.5;
                            est_height += 0.3;

                            // If the text is rotated 90° or 270°, swap width/height.
                            match (pin_angle.round() as i32).rem_euclid(360) {
                                90 | 270 => {
                                    std::mem::swap(&mut est_width, &mut est_height);
                                }
                                _ => {}
                            }

                            let label_info = LabelInfo {
                                position: pin_pos,
                                width: est_width,
                                height: est_height,
                            };

                            // Track the label info for this component
                            self.component_label_positions
                                .entry(comp_ref.clone())
                                .or_default()
                                .push(label_info);
                        } else {
                            // Fallback to default position if pin not found
                            log::warn!(
                                "Pin '{}' not found in symbol {}, using default position",
                                pin_identifier,
                                symbol.lib_id
                            );

                            let default_pos = (symbol.position.0 + 10.0, symbol.position.1);
                            let global_label = GlobalLabel {
                                text: net_name.to_string(),
                                position: default_pos,
                                angle: 0.0,
                                uuid: Uuid::new_v4().to_string(),
                                justify: None,
                            };

                            self.global_labels.push(global_label);

                            // Same rough size estimation for fallback case.
                            const FONT_HEIGHT: f64 = 1.27;
                            const CHAR_WIDTH_FACTOR: f64 = 0.6;
                            let est_width =
                                net_name.chars().count() as f64 * FONT_HEIGHT * CHAR_WIDTH_FACTOR
                                    + 0.5;
                            let est_height = FONT_HEIGHT + 0.3;

                            let label_info = LabelInfo {
                                position: default_pos,
                                width: est_width,
                                height: est_height,
                            };

                            self.component_label_positions
                                .entry(comp_ref.clone())
                                .or_default()
                                .push(label_info);
                        }
                    } else {
                        log::warn!("Symbol definition not found for {}", symbol.lib_id);
                    }
                }
            } else {
                // Component was likely skipped due to symbol loading error
                log::warn!(
                    "Component {comp_ref} not found in schematic (likely skipped due to symbol loading error)"
                );
            }
        }

        Ok(())
    }

    fn get_component_ref(&self, port_ref: &InstanceRef) -> Result<InstanceRef, ConversionError> {
        // Extract component reference from port reference
        let mut comp_path = port_ref.instance_path.clone();
        if comp_path.is_empty() {
            return Err(ConversionError::InvalidInstanceRef(port_ref.to_string()));
        }
        comp_path.pop(); // Remove the port name

        Ok(InstanceRef {
            module: port_ref.module.clone(),
            instance_path: comp_path,
        })
    }

    fn generate_schematic_sexpr(&self, output_path: &Path) -> String {
        let mut schematic_items = vec![
            // Header
            Sexpr::list(vec![Sexpr::atom("version"), Sexpr::atom("20231120")]),
            Sexpr::list(vec![Sexpr::atom("generator"), Sexpr::string("diode_sch")]),
            Sexpr::list(vec![
                Sexpr::atom("uuid"),
                Sexpr::atom(Uuid::new_v4().to_string()),
            ]),
            Sexpr::list(vec![Sexpr::atom("paper"), Sexpr::string("A4")]),
            // Title block
            Sexpr::list(vec![
                Sexpr::atom("title_block"),
                Sexpr::list(vec![
                    Sexpr::atom("title"),
                    Sexpr::string("Converted from Diode"),
                ]),
                Sexpr::list(vec![
                    Sexpr::atom("date"),
                    Sexpr::string(chrono::Local::now().format("%Y-%m-%d").to_string()),
                ]),
            ]),
        ];

        // Library symbols - just copy them as-is
        if !self.lib_symbols.is_empty() {
            let mut lib_symbols_items = vec![Sexpr::atom("lib_symbols")];
            for symbol_info in self.lib_symbols.values() {
                lib_symbols_items.push(symbol_info.raw_sexpr.clone());
            }
            schematic_items.push(Sexpr::list(lib_symbols_items));
        }

        // Junctions
        for junction in &self.junctions {
            schematic_items.push(self.junction_to_sexpr(junction));
        }

        // Wires
        for wire in &self.wires {
            schematic_items.push(self.wire_to_sexpr(wire));
        }

        // Rectangles (bounding boxes)
        for rectangle in &self.rectangles {
            schematic_items.push(self.rectangle_to_sexpr(rectangle));
        }

        // Text labels
        for text in &self.texts {
            schematic_items.push(self.text_to_sexpr(text));
        }

        // Global labels
        for label in &self.global_labels {
            schematic_items.push(self.global_label_to_sexpr(label));
        }

        // Symbols
        for symbol in &self.symbols {
            schematic_items.push(self.symbol_to_sexpr(symbol, output_path));
        }

        // Sheet instances
        schematic_items.push(Sexpr::list(vec![
            Sexpr::atom("sheet_instances"),
            Sexpr::list(vec![
                Sexpr::atom("path"),
                Sexpr::string("/"),
                Sexpr::list(vec![Sexpr::atom("page"), Sexpr::string("1")]),
            ]),
        ]));

        // Build the complete schematic S-expression
        let schematic_sexpr = Sexpr::list({
            let mut items = vec![Sexpr::atom("kicad_sch")];
            items.extend(schematic_items);
            items
        });

        // Convert to string with proper formatting
        format_sexpr(&schematic_sexpr, 0)
    }

    fn junction_to_sexpr(&self, junction: &Junction) -> Sexpr {
        Sexpr::list(vec![
            Sexpr::atom("junction"),
            Sexpr::list(vec![
                Sexpr::atom("at"),
                Sexpr::atom(junction.position.0.to_string()),
                Sexpr::atom(junction.position.1.to_string()),
            ]),
            Sexpr::list(vec![Sexpr::atom("diameter"), Sexpr::atom("0")]),
            Sexpr::list(vec![
                Sexpr::atom("color"),
                Sexpr::atom("0"),
                Sexpr::atom("0"),
                Sexpr::atom("0"),
                Sexpr::atom("0"),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("uuid"),
                Sexpr::atom(junction.uuid.clone()),
            ]),
        ])
    }

    fn wire_to_sexpr(&self, wire: &Wire) -> Sexpr {
        let mut pts_items = vec![Sexpr::atom("pts")];
        for point in &wire.points {
            pts_items.push(Sexpr::list(vec![
                Sexpr::atom("xy"),
                Sexpr::atom(point.0.to_string()),
                Sexpr::atom(point.1.to_string()),
            ]));
        }

        Sexpr::list(vec![
            Sexpr::atom("wire"),
            Sexpr::list(pts_items),
            Sexpr::list(vec![
                Sexpr::atom("stroke"),
                Sexpr::list(vec![Sexpr::atom("width"), Sexpr::atom("0")]),
                Sexpr::list(vec![Sexpr::atom("type"), Sexpr::atom("default")]),
            ]),
            Sexpr::list(vec![Sexpr::atom("uuid"), Sexpr::atom(wire.uuid.clone())]),
        ])
    }

    fn global_label_to_sexpr(&self, label: &GlobalLabel) -> Sexpr {
        // Determine justification based on angle
        let justify_value = if let Some(ref justify) = label.justify {
            justify.clone()
        } else {
            // Default justification based on angle
            match label.angle.round() as i32 % 360 {
                0 => "right".to_string(), // Pin points right: label on left, right-justified
                90 => "right".to_string(), // Pin points up: label below, left-justified
                180 => "left".to_string(), // Pin points left: label on right, left-justified
                270 => "left".to_string(), // Pin points down: label above, right-justified
                _ => "left".to_string(),  // Default for other angles
            }
        };

        Sexpr::list(vec![
            Sexpr::atom("global_label"),
            Sexpr::string(label.text.clone()),
            Sexpr::list(vec![Sexpr::atom("shape"), Sexpr::atom("input")]),
            Sexpr::list(vec![
                Sexpr::atom("at"),
                Sexpr::atom(label.position.0.to_string()),
                Sexpr::atom(label.position.1.to_string()),
                Sexpr::atom(label.angle.to_string()),
            ]),
            Sexpr::list(vec![Sexpr::atom("fields_autoplaced")]),
            Sexpr::list(vec![
                Sexpr::atom("effects"),
                Sexpr::list(vec![
                    Sexpr::atom("font"),
                    Sexpr::list(vec![
                        Sexpr::atom("size"),
                        Sexpr::atom("1.27"),
                        Sexpr::atom("1.27"),
                    ]),
                ]),
                Sexpr::list(vec![Sexpr::atom("justify"), Sexpr::atom(justify_value)]),
            ]),
            Sexpr::list(vec![Sexpr::atom("uuid"), Sexpr::atom(label.uuid.clone())]),
        ])
    }

    fn symbol_to_sexpr(&self, symbol: &SchematicSymbol, output_path: &Path) -> Sexpr {
        let mut symbol_items = vec![
            Sexpr::atom("symbol"),
            Sexpr::list(vec![
                Sexpr::atom("lib_id"),
                Sexpr::string(symbol.lib_id.clone()),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("at"),
                Sexpr::atom(symbol.position.0.to_string()),
                Sexpr::atom(symbol.position.1.to_string()),
                Sexpr::atom("0"),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("unit"),
                Sexpr::atom(symbol.unit.to_string()),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("in_bom"),
                Sexpr::atom(if symbol.in_bom { "yes" } else { "no" }),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("on_board"),
                Sexpr::atom(if symbol.on_board { "yes" } else { "no" }),
            ]),
            Sexpr::list(vec![Sexpr::atom("dnp"), Sexpr::atom("no")]),
            Sexpr::list(vec![Sexpr::atom("fields_autoplaced")]),
            Sexpr::list(vec![Sexpr::atom("uuid"), Sexpr::atom(symbol.uuid.clone())]),
        ];

        // Properties
        symbol_items.push(self.create_property_sexpr(
            "Reference",
            &symbol.reference,
            symbol.position.0,
            symbol.position.1 - 5.0,
            false,
        ));

        symbol_items.push(self.create_property_sexpr(
            "Value",
            &symbol.value,
            symbol.position.0,
            symbol.position.1 + 5.0,
            false,
        ));

        if let Some(footprint) = &symbol.footprint {
            symbol_items.push(self.create_property_sexpr(
                "Footprint",
                footprint,
                symbol.position.0,
                symbol.position.1 + 10.0,
                true,
            ));
        }

        // Additional properties
        for (key, value) in &symbol.properties {
            if key != "symbol_path" && key != "mpn" && key != "type" {
                symbol_items.push(self.create_property_sexpr(
                    key,
                    value,
                    symbol.position.0,
                    symbol.position.1 + 15.0,
                    true,
                ));
            }
        }

        // Instances
        symbol_items.push(Sexpr::list(vec![
            Sexpr::atom("instances"),
            Sexpr::list(vec![
                Sexpr::atom("project"),
                Sexpr::string(
                    output_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("project"),
                ),
                Sexpr::list(vec![
                    Sexpr::atom("path"),
                    Sexpr::string(format!("/{}", symbol.uuid)),
                    Sexpr::list(vec![
                        Sexpr::atom("reference"),
                        Sexpr::string(symbol.reference.clone()),
                    ]),
                    Sexpr::list(vec![
                        Sexpr::atom("unit"),
                        Sexpr::atom(symbol.unit.to_string()),
                    ]),
                ]),
            ]),
        ]));

        Sexpr::list(symbol_items)
    }

    fn create_property_sexpr(&self, key: &str, value: &str, x: f64, y: f64, hide: bool) -> Sexpr {
        let mut property_items = vec![
            Sexpr::atom("property"),
            Sexpr::string(key),
            Sexpr::string(value),
            Sexpr::list(vec![
                Sexpr::atom("at"),
                Sexpr::atom(x.to_string()),
                Sexpr::atom(y.to_string()),
                Sexpr::atom("0"),
            ]),
        ];

        let mut effects_items = vec![
            Sexpr::atom("effects"),
            Sexpr::list(vec![
                Sexpr::atom("font"),
                Sexpr::list(vec![
                    Sexpr::atom("size"),
                    Sexpr::atom("1.27"),
                    Sexpr::atom("1.27"),
                ]),
            ]),
        ];

        if hide {
            effects_items.push(Sexpr::atom("hide"));
        }

        property_items.push(Sexpr::list(effects_items));

        Sexpr::list(property_items)
    }

    fn find_pin_position(
        &self,
        symbol_data: &Sexpr,
        pin_name: &str,
        symbol_position: (f64, f64),
    ) -> Option<((f64, f64), f64)> {
        // Delegate to recursive helper that understands nested sub-symbols.
        // For now we ignore rotation inside sub-symbols as most library parts keep rotation at 0°.
        self.find_pin_with_transform(symbol_data, pin_name, symbol_position, (0.0, 0.0))
    }

    /// Recursively search for the pin while accumulating local offsets from any nested sub-symbols.
    /// For now we ignore rotation inside sub-symbols as most library parts keep rotation at 0°.
    fn find_pin_with_transform(
        &self,
        sexpr: &Sexpr,
        pin_name: &str,
        symbol_position: (f64, f64),
        local_offset: (f64, f64),
    ) -> Option<((f64, f64), f64)> {
        if let Sexpr::List(items) = sexpr {
            // First, attempt to match a pin at this level (using current local_offset)
            for item in items {
                if let Sexpr::List(item_data) = item {
                    if let Some(tag) = item_data.first().and_then(|s| s.as_atom()) {
                        if tag == "pin" {
                            if let Some(mut result) = self.check_pin(item_data, pin_name) {
                                // KiCad symbol coordinates have +Y upward, but schematic coordinates have +Y downward.
                                // Therefore, subtract the local Y (pin_y + offsets) from the symbol Y.
                                result.0 .0 += symbol_position.0 + local_offset.0;
                                result.0 .1 = symbol_position.1 - (local_offset.1 + result.0 .1);
                                return Some(result);
                            }
                        }
                    }
                }
            }

            // If not found, recurse into nested symbols / lists.
            for item in items {
                if let Sexpr::List(item_data) = item {
                    if let Some(tag) = item_data.first().and_then(|s| s.as_atom()) {
                        if tag == "symbol" {
                            // Extract the local "at" offset of this sub-symbol if present.
                            let mut sub_offset = (0.0, 0.0);
                            for sub_item in item_data {
                                if let Sexpr::List(at_data) = sub_item {
                                    if let Some("at") = at_data.first().and_then(|s| s.as_atom()) {
                                        if let (Some(x_str), Some(y_str)) = (
                                            at_data.get(1).and_then(|s| s.as_atom()),
                                            at_data.get(2).and_then(|s| s.as_atom()),
                                        ) {
                                            if let (Ok(x), Ok(y)) =
                                                (x_str.parse::<f64>(), y_str.parse::<f64>())
                                            {
                                                sub_offset = (x, y);
                                            }
                                        }
                                    }
                                }
                            }

                            // Combine offsets (rotation ignored)
                            let combined_offset =
                                (local_offset.0 + sub_offset.0, local_offset.1 + sub_offset.1);

                            if let Some(res) = self.find_pin_with_transform(
                                item,
                                pin_name,
                                symbol_position,
                                combined_offset,
                            ) {
                                return Some(res);
                            }
                        } else if let Some(res) = self.find_pin_with_transform(
                            item,
                            pin_name,
                            symbol_position,
                            local_offset,
                        ) {
                            return Some(res);
                        }
                    }
                }
            }
        }
        None
    }

    fn check_pin(&self, pin_data: &[Sexpr], pin_name: &str) -> Option<((f64, f64), f64)> {
        let mut is_matching_pin = false;
        let mut pin_x = 0.0;
        let mut pin_y = 0.0;
        let mut _pin_length = 0.0;
        let mut pin_angle = 0.0;

        for item in pin_data {
            if let Sexpr::List(sub_items) = item {
                if let Some(tag) = sub_items.first().and_then(|s| s.as_atom()) {
                    match tag {
                        "name" | "number" => {
                            if let Some(value) = sub_items.get(1).and_then(|s| s.as_atom()) {
                                if value == pin_name {
                                    is_matching_pin = true;
                                }
                            }
                        }
                        "at" => {
                            if let (Some(x_str), Some(y_str)) = (
                                sub_items.get(1).and_then(|s| s.as_atom()),
                                sub_items.get(2).and_then(|s| s.as_atom()),
                            ) {
                                if let (Ok(x), Ok(y)) = (x_str.parse::<f64>(), y_str.parse::<f64>())
                                {
                                    pin_x = x;
                                    pin_y = y;
                                }
                                // Angle is optional
                                if let Some(angle_str) = sub_items.get(3).and_then(|s| s.as_atom())
                                {
                                    if let Ok(angle) = angle_str.parse::<f64>() {
                                        pin_angle = angle;
                                    }
                                }
                            }
                        }
                        "length" => {
                            if let Some(length_str) = sub_items.get(1).and_then(|s| s.as_atom()) {
                                if let Ok(length) = length_str.parse::<f64>() {
                                    _pin_length = length;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if is_matching_pin {
            // Return the pin's local coordinates. The absolute position
            // will be calculated by find_pin_with_transform.
            Some(((pin_x, pin_y), pin_angle))
        } else {
            None
        }
    }

    /// Calculate symbol bounds including space for net labels
    fn calculate_extended_bounds(
        &self,
        _inst_ref: &InstanceRef,
        base_bounds: (f64, f64, f64, f64),
    ) -> (f64, f64, f64, f64) {
        let (min_x, min_y, max_x, max_y) = base_bounds;

        // The bounds already represent the full extent of the symbol
        // We just return them as-is since the symbol origin offset is handled separately
        (min_x, min_y, max_x, max_y)
    }

    fn rectangle_to_sexpr(&self, rectangle: &Rectangle) -> Sexpr {
        let mut rect_items = vec![
            Sexpr::atom("rectangle"),
            Sexpr::list(vec![
                Sexpr::atom("start"),
                Sexpr::atom(rectangle.start.0.to_string()),
                Sexpr::atom(rectangle.start.1.to_string()),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("end"),
                Sexpr::atom(rectangle.end.0.to_string()),
                Sexpr::atom(rectangle.end.1.to_string()),
            ]),
        ];

        // Add stroke with color if specified
        let stroke_items = if let Some((r, g, b, a)) = rectangle.color {
            vec![
                Sexpr::atom("stroke"),
                Sexpr::list(vec![Sexpr::atom("width"), Sexpr::atom("0.254")]), // Slightly thicker for debug
                Sexpr::list(vec![Sexpr::atom("type"), Sexpr::atom("default")]),
                Sexpr::list(vec![
                    Sexpr::atom("color"),
                    Sexpr::atom(r.to_string()),
                    Sexpr::atom(g.to_string()),
                    Sexpr::atom(b.to_string()),
                    Sexpr::atom(a.to_string()),
                ]),
            ]
        } else {
            vec![
                Sexpr::atom("stroke"),
                Sexpr::list(vec![Sexpr::atom("width"), Sexpr::atom("0")]),
                Sexpr::list(vec![Sexpr::atom("type"), Sexpr::atom("default")]),
            ]
        };
        rect_items.push(Sexpr::list(stroke_items));

        rect_items.push(Sexpr::list(vec![
            Sexpr::atom("fill"),
            Sexpr::list(vec![Sexpr::atom("type"), Sexpr::atom("none")]),
        ]));
        rect_items.push(Sexpr::list(vec![
            Sexpr::atom("uuid"),
            Sexpr::atom(rectangle.uuid.clone()),
        ]));

        Sexpr::list(rect_items)
    }

    fn text_to_sexpr(&self, text: &Text) -> Sexpr {
        Sexpr::list(vec![
            Sexpr::atom("text"),
            Sexpr::string(text.content.clone()),
            Sexpr::list(vec![Sexpr::atom("exclude_from_sim"), Sexpr::atom("no")]),
            Sexpr::list(vec![
                Sexpr::atom("at"),
                Sexpr::atom(text.position.0.to_string()),
                Sexpr::atom(text.position.1.to_string()),
                Sexpr::atom(text.angle.to_string()),
            ]),
            Sexpr::list(vec![
                Sexpr::atom("effects"),
                Sexpr::list(vec![
                    Sexpr::atom("font"),
                    Sexpr::list(vec![
                        Sexpr::atom("size"),
                        Sexpr::atom("1.27"),
                        Sexpr::atom("1.27"),
                    ]),
                ]),
            ]),
            Sexpr::list(vec![Sexpr::atom("uuid"), Sexpr::atom(text.uuid.clone())]),
        ])
    }

    fn build_module_hierarchy(&mut self, sch: &Schematic) {
        // Start from the root instance if it exists
        if let Some(root_ref) = &sch.root_ref {
            if let Some(root_instance) = sch.instances.get(root_ref) {
                // Traverse the hierarchy starting from the root
                self.traverse_hierarchy(root_ref, root_instance, sch);
            }
        } else {
            // Fallback: if no root is specified, treat all top-level components as root items
            for (inst_ref, instance) in &sch.instances {
                if instance.kind == InstanceKind::Component && inst_ref.instance_path.is_empty() {
                    if let Some(_uuid) = self.uuid_map.get(inst_ref) {
                        // This is a root-level component with no parent
                        // It will be laid out at the top level
                    }
                }
            }
        }
    }

    /// Recursively traverse the instance hierarchy
    fn traverse_hierarchy(
        &mut self,
        current_ref: &InstanceRef,
        current_instance: &Instance,
        sch: &Schematic,
    ) {
        match current_instance.kind {
            InstanceKind::Module => {
                // This is a module - create a module ID for it
                let module_id = current_ref.to_string();

                // Collect all direct children (components and sub-modules)
                let mut children_ids = Vec::new();

                // Process all children of this module
                for child_ref in current_instance.children.values() {
                    if let Some(child_instance) = sch.instances.get(child_ref) {
                        match child_instance.kind {
                            InstanceKind::Component => {
                                // This is a component - add it to the layout engine
                                if let Some(uuid) = self.uuid_map.get(child_ref) {
                                    children_ids.push(uuid.clone());
                                }
                            }
                            InstanceKind::Module => {
                                // This is a sub-module - we'll process it recursively
                                let sub_module_id = child_ref.to_string();
                                children_ids.push(sub_module_id.clone());
                            }
                            _ => {
                                // Skip other instance types (Port, Pin, Interface)
                            }
                        }

                        // Recursively process this child
                        self.traverse_hierarchy(child_ref, child_instance, sch);
                    }
                }

                // Register this module with its children
                if !children_ids.is_empty() {
                    self.layout_engine
                        .add_module(module_id.clone(), children_ids);
                }
            }
            InstanceKind::Component => {
                // This is a component - it's already been added to the layout engine
                // Nothing more to do here
            }
            _ => {
                // Skip other instance types
            }
        }
    }

    fn get_component_bounds_from_engine(&self, _uuid: &str) -> Option<(f64, f64, f64, f64)> {
        // For now, return None - the bounds are managed internally by the layout engine
        // In a full implementation, we would query the layout engine for the bounds
        None
    }

    /// Add a legend explaining the debug visualization
    fn add_debug_legend(&mut self) {
        let legend_x = 10.0;
        let legend_y = 10.0;
        let line_height = 5.0;

        // Title
        self.texts.push(Text {
            content: "DEBUG LEGEND:".to_string(),
            position: (legend_x, legend_y),
            angle: 0.0,
            uuid: Uuid::new_v4().to_string(),
        });

        // Red rectangle legend
        self.rectangles.push(Rectangle {
            start: (legend_x, legend_y + line_height),
            end: (legend_x + 10.0, legend_y + line_height + 3.0),
            uuid: Uuid::new_v4().to_string(),
            color: Some((255, 0, 0, 255)),
        });
        self.texts.push(Text {
            content: "Component actual bounds".to_string(),
            position: (legend_x + 12.0, legend_y + line_height + 1.5),
            angle: 0.0,
            uuid: Uuid::new_v4().to_string(),
        });

        // Green rectangle legend
        self.rectangles.push(Rectangle {
            start: (legend_x, legend_y + 2.0 * line_height),
            end: (legend_x + 10.0, legend_y + 2.0 * line_height + 3.0),
            uuid: Uuid::new_v4().to_string(),
            color: Some((0, 255, 0, 128)),
        });
        self.texts.push(Text {
            content: "Layout engine allocated space".to_string(),
            position: (legend_x + 12.0, legend_y + 2.0 * line_height + 1.5),
            angle: 0.0,
            uuid: Uuid::new_v4().to_string(),
        });

        // Blue rectangle legend
        self.rectangles.push(Rectangle {
            start: (legend_x, legend_y + 3.0 * line_height),
            end: (legend_x + 10.0, legend_y + 3.0 * line_height + 3.0),
            uuid: Uuid::new_v4().to_string(),
            color: Some((0, 0, 255, 255)),
        });
        self.texts.push(Text {
            content: "Module boundaries".to_string(),
            position: (legend_x + 12.0, legend_y + 3.0 * line_height + 1.5),
            angle: 0.0,
            uuid: Uuid::new_v4().to_string(),
        });
    }
}

/// Write a KiCad schematic file to disk
pub fn write_schematic_file(schematic_content: &str, path: &Path) -> Result<(), std::io::Error> {
    fs::write(path, schematic_content)
}
