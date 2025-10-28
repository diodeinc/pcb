use crate::{FillDesc, LineDesc, PadStackDef, StandardPrimitive, Symbol, UserPrimitive};
use std::collections::HashMap;

/// BoardContext holds parsed and normalized data for the export pipeline
///
/// This is the output of Stage 0 (Input Readiness). It provides:
/// - Normalized units (everything in millimeters)
/// - Dictionary lookups (padstacks, line descriptors, standard shapes)
/// - Bounding box metadata
/// - Validation statistics
#[derive(Debug)]
pub struct BoardContext {
    /// Board name (from Step)
    pub board_name: String,

    /// Units used in original file (for reference)
    pub original_units: String,

    /// Conversion factor from original units to millimeters
    pub to_mm_factor: f64,

    /// All padstack definitions indexed by name
    pub padstack_defs: HashMap<Symbol, PadStackDef>,

    /// Line descriptor dictionary (for trace widths)
    pub line_descriptors: HashMap<Symbol, LineDesc>,

    /// Fill descriptor dictionary (for fill properties)
    pub fill_descriptors: HashMap<Symbol, FillDesc>,

    /// Standard primitive dictionary (for pad shapes)
    pub standard_primitives: HashMap<Symbol, StandardPrimitive>,

    /// User primitive dictionary (for custom pad shapes)
    pub user_primitives: HashMap<Symbol, UserPrimitive>,

    /// Validation statistics
    pub stats: BoardStats,
}

/// Validation and feature statistics
#[derive(Debug, Clone)]
pub struct BoardStats {
    pub layer_count: usize,
    pub copper_layer_count: usize,
    pub drill_layer_count: usize,
    pub padstack_def_count: usize,
    pub line_desc_count: usize,
    pub fill_desc_count: usize,
    pub standard_primitive_count: usize,
    pub user_primitive_count: usize,
    pub feature_set_count: usize,
    pub pad_count: usize,
    pub trace_count: usize,
    pub hole_count: usize,
    pub slot_count: usize,
}

impl BoardStats {
    pub fn new() -> Self {
        Self {
            layer_count: 0,
            copper_layer_count: 0,
            drill_layer_count: 0,
            padstack_def_count: 0,
            line_desc_count: 0,
            fill_desc_count: 0,
            standard_primitive_count: 0,
            user_primitive_count: 0,
            feature_set_count: 0,
            pad_count: 0,
            trace_count: 0,
            hole_count: 0,
            slot_count: 0,
        }
    }

    pub fn print_summary(&self) {
        println!("━━━ Board Statistics ━━━");
        println!("  Layers:              {}", self.layer_count);
        println!("    Copper:            {}", self.copper_layer_count);
        println!("    Drill:             {}", self.drill_layer_count);
        println!("  Dictionaries:");
        println!("    Padstack Defs:     {}", self.padstack_def_count);
        println!("    Line Descriptors:  {}", self.line_desc_count);
        println!("    Fill Descriptors:  {}", self.fill_desc_count);
        println!("    Std Primitives:    {}", self.standard_primitive_count);
        if self.user_primitive_count > 0 {
            println!("    User Primitives:   {}", self.user_primitive_count);
        }
        println!("  Features:");
        println!("    Sets:              {}", self.feature_set_count);
        println!("    Pads:              {}", self.pad_count);
        println!("    Traces:            {}", self.trace_count);
        println!("    Holes:             {}", self.hole_count);
        println!("    Slots:             {}", self.slot_count);
    }
}

impl Default for BoardStats {
    fn default() -> Self {
        Self::new()
    }
}
