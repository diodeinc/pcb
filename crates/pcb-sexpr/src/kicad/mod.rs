//! KiCad-specific S-expression helpers.
//!
//! This module groups KiCad-related parsing utilities in submodules:
//! - [`props`] - common "property-like" query helpers
//! - [`netlist`] - KiCad netlist (`kicadsexpr`) helpers
//! - [`schematic`] - KiCad schematic (`.kicad_sch`) helpers
//! - [`symbol`] - KiCad symbol library (`.kicad_sym`) helpers

pub mod netlist;
pub mod props;
pub mod schematic;
pub mod symbol;

pub use netlist::sheetpath;
pub use props::{child_list, int_prop, string_list_prop, string_prop, sym_prop, yes_no_prop};
pub use schematic::{
    schematic_at, schematic_instance_path, schematic_instance_paths, schematic_pins,
    schematic_properties,
};
