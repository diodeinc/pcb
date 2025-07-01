pub mod kicad;

use anyhow::Result;
use kicad::symbol::KicadSymbol;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Default)]
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Part {
    pub part_number: String,
    pub url: String,
}

#[derive(Debug)]
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
