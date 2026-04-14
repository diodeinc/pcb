pub mod kicad;

use anyhow::Result;
use kicad::symbol::KicadSymbol;
use kicad::symbol_library::KicadSymbolLibrary;
use pcb_sexpr::Sexpr;
use serde::Serialize;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    pub footprint: String,
    pub reference: String,
    pub in_bom: bool,
    pub pins: Vec<Pin>,
    pub datasheet: Option<String>,
    pub manufacturer: Option<String>,
    pub mpn: Option<String>,
    pub distributors: HashMap<String, Part>,
    pub description: Option<String>,
    pub properties: HashMap<String, String>,
    #[serde(skip)]
    pub raw_sexp: Option<Sexpr>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Serialize)]
pub struct Part {
    pub part_number: String,
    pub url: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Pin {
    pub name: String,
    pub number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub electrical_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphical_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<PinAt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternates: Vec<PinAlternate>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Serialize)]
pub struct PinAlternate {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub electrical_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphical_style: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct PinAt {
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// KiCad uses `~` as a placeholder for an absent text value in several contexts.
pub fn usable_kicad_field_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "~").then_some(trimmed)
}

pub fn is_placeholder_kicad_pin_name(name: &str) -> bool {
    usable_kicad_field_value(name).is_none()
}

impl Pin {
    /// KiCad uses `~` as a placeholder pin name for "unnamed" pins.
    ///
    /// When consuming KiCad symbols, treat unnamed pins as being identified by their number so
    /// connectivity mappings stay stable and match Zener's Symbol signal naming semantics.
    pub fn signal_name(&self) -> &str {
        if is_placeholder_kicad_pin_name(&self.name) {
            &self.number
        } else {
            &self.name
        }
    }
}

impl Symbol {
    pub fn from_file(path: &Path) -> Result<Self> {
        let extension = path.extension().unwrap_or("".as_ref()).to_str();
        let error = io::Error::other("Unsupported file type");
        match extension {
            Some("kicad_sym") => Ok(KicadSymbol::from_file(path)?.into()),
            _ => Err(anyhow::anyhow!(error)),
        }
    }

    pub fn from_string(contents: &str, file_type: &str) -> Result<Self> {
        match file_type {
            "kicad_sym" => Ok(KicadSymbol::from_str(contents)?.into()),
            _ => Err(anyhow::anyhow!("Unsupported file type: {}", file_type)),
        }
    }

    pub fn raw_sexp(&self) -> Option<&Sexpr> {
        self.raw_sexp.as_ref()
    }
}

/// A symbol library that can contain multiple symbols
pub struct SymbolLibrary {
    symbols: Vec<Symbol>,
}

impl SymbolLibrary {
    /// Parse a symbol library from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        if path.is_dir() {
            let lib = KicadSymbolLibrary::from_file(path)?;
            return Ok(SymbolLibrary {
                symbols: lib.into_symbols_lazy()?,
            });
        }

        let extension = path.extension().unwrap_or("".as_ref()).to_str();
        let error = io::Error::other("Unsupported file type");
        match extension {
            Some("kicad_sym") => {
                let lib = KicadSymbolLibrary::from_file(path)?;
                Ok(SymbolLibrary {
                    symbols: lib.into_symbols_lazy()?,
                })
            }
            _ => Err(anyhow::anyhow!(error)),
        }
    }

    /// Parse a symbol library from a string
    pub fn from_string(contents: &str, file_type: &str) -> Result<Self> {
        match file_type {
            "kicad_sym" => {
                let lib = KicadSymbolLibrary::from_string(contents)?;
                Ok(SymbolLibrary {
                    symbols: lib.into_symbols_lazy()?,
                })
            }
            _ => Err(anyhow::anyhow!("Unsupported file type: {}", file_type)),
        }
    }

    /// Get all symbols in the library
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// Get a symbol by name
    pub fn get_symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// Get the names of all symbols in the library
    pub fn symbol_names(&self) -> Vec<&str> {
        self.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    /// Get the first symbol in the library (for backwards compatibility)
    pub fn first_symbol(&self) -> Option<&Symbol> {
        self.symbols.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_kicad_field_value_filters_placeholders() {
        assert_eq!(usable_kicad_field_value("value"), Some("value"));
        assert_eq!(usable_kicad_field_value("  value  "), Some("value"));
        assert_eq!(usable_kicad_field_value(""), None);
        assert_eq!(usable_kicad_field_value("   "), None);
        assert_eq!(usable_kicad_field_value("~"), None);
    }

    #[test]
    fn signal_name_falls_back_for_placeholder_pin_names() {
        let pin = Pin {
            name: "~".to_string(),
            number: "42".to_string(),
            ..Default::default()
        };
        assert_eq!(pin.signal_name(), "42");

        let named_pin = Pin {
            name: "VCC".to_string(),
            number: "1".to_string(),
            ..Default::default()
        };
        assert_eq!(named_pin.signal_name(), "VCC");
    }
}
