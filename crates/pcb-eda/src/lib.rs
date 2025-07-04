pub mod kicad;

use anyhow::Result;
use kicad::symbol::KicadSymbol;
use kicad::symbol_library::KicadSymbolLibrary;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Default, Clone)]
pub struct Symbol {
    pub name: String,
    pub footprint: String,
    pub in_bom: bool,
    pub pins: Vec<Pin>,
    pub datasheet: Option<String>,
    pub manufacturer: Option<String>,
    pub mpn: Option<String>,
    pub distributors: HashMap<String, Part>,
    pub description: Option<String>,
    pub properties: HashMap<String, String>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Part {
    pub part_number: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Pin {
    pub name: String,
    pub number: String,
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
}

/// A symbol library that can contain multiple symbols
pub struct SymbolLibrary {
    symbols: Vec<Symbol>,
}

impl SymbolLibrary {
    /// Parse a symbol library from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        let extension = path.extension().unwrap_or("".as_ref()).to_str();
        let error = io::Error::other("Unsupported file type");
        match extension {
            Some("kicad_sym") => {
                let lib = KicadSymbolLibrary::from_file(path)?;
                Ok(SymbolLibrary {
                    symbols: lib.into_symbols(),
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
                    symbols: lib.into_symbols(),
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
