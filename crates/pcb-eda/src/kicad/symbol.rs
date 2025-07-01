use crate::{Part, Pin, Symbol};
use anyhow::Result;
use sexp::{parse, Atom, Sexp};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Default)]
pub struct KicadSymbol {
    name: String,
    footprint: String,
    in_bom: bool,
    pins: Vec<KicadPin>,
    mpn: Option<String>,
    manufacturer: Option<String>,
    datasheet_url: Option<String>,
    description: Option<String>,
    distributors: HashMap<String, Part>,
    properties: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct KicadPin {
    name: String,
    number: String,
}

impl From<KicadSymbol> for Symbol {
    fn from(symbol: KicadSymbol) -> Self {
        Symbol {
            name: symbol.name,
            footprint: symbol.footprint,
            in_bom: symbol.in_bom,
            mpn: symbol.mpn,
            datasheet: symbol.datasheet_url,
            manufacturer: symbol.manufacturer,
            description: symbol.description,
            distributors: symbol.distributors,
            properties: symbol.properties,
            pins: symbol
                .pins
                .into_iter()
                .map(|pin| Pin {
                    name: pin.name,
                    number: pin.number,
                })
                .collect(),
        }
    }
}

impl FromStr for KicadSymbol {
    type Err = anyhow::Error;

    fn from_str(content: &str) -> Result<Self> {
        let sexp = parse(content)?;

        // Find the 'symbol' S-expression
        let symbol_sexp = match sexp {
            Sexp::List(kicad_symbol_lib) => kicad_symbol_lib
                .into_iter()
                .find_map(|item| match item {
                    Sexp::List(ref symbol_list) => match symbol_list.first() {
                        Some(Sexp::Atom(Atom::S(ref sym))) if sym == "symbol" => {
                            Some(symbol_list.clone())
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .ok_or(anyhow::anyhow!("No 'symbol' expression found"))?,
            _ => return Err(anyhow::anyhow!("Invalid S-expression format")),
        };

        parse_symbol(&symbol_sexp)
    }
}

impl KicadSymbol {
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_str(&content)
    }
}

fn parse_symbol(symbol_data: &[Sexp]) -> Result<KicadSymbol> {
    // Extract the symbol name
    let name = symbol_data
        .get(1)
        .and_then(|sexp| match sexp {
            Sexp::Atom(Atom::S(name)) => Some(name.clone()),
            _ => None,
        })
        .ok_or(anyhow::anyhow!("Symbol name not found"))?;

    let mut symbol = KicadSymbol {
        name,
        ..Default::default()
    };

    for prop in &symbol_data[2..] {
        if let Sexp::List(prop_list) = prop {
            if let Some(Sexp::Atom(Atom::S(prop_name))) = prop_list.first() {
                match prop_name.as_str() {
                    "in_bom" => parse_in_bom(&mut symbol, prop_list),
                    "property" => parse_property(&mut symbol, prop_list),
                    "pin" => {
                        if let Some(pin) = parse_pin(prop_list) {
                            symbol.pins.push(pin)
                        }
                    }
                    _ if prop_name.starts_with("symbol") => {
                        // This is the nested symbol section which may contain pins
                        parse_symbol_section(&mut symbol, prop_list);
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(symbol)
}

// New function to parse the nested symbol section which contains pins in new format
fn parse_symbol_section(symbol: &mut KicadSymbol, section_data: &[Sexp]) {
    for item in section_data {
        if let Sexp::List(pin_data) = item {
            if let Some(Sexp::Atom(Atom::S(type_name))) = pin_data.first() {
                if type_name == "pin" {
                    if let Some(pin) = parse_pin_from_section(pin_data) {
                        symbol.pins.push(pin);
                    }
                }
            }
        }
    }
}

// New function to parse pins from the nested symbol section
fn parse_pin_from_section(pin_data: &[Sexp]) -> Option<KicadPin> {
    // Format: (pin unspecified line (at X Y Z) (length L) (name "Name") (number "N"))
    let mut pin = KicadPin::default();

    // Extract name and number from the pin data
    for item in pin_data {
        if let Sexp::List(attr_data) = item {
            if attr_data.len() >= 2 {
                if let Some(Sexp::Atom(Atom::S(attr_name))) = attr_data.first() {
                    if attr_name == "name" && attr_data.len() >= 2 {
                        if let Some(Sexp::Atom(Atom::S(name))) = attr_data.get(1) {
                            pin.name = name.clone();
                        }
                    } else if attr_name == "number" && attr_data.len() >= 2 {
                        if let Some(Sexp::Atom(Atom::S(number))) = attr_data.get(1) {
                            pin.number = number.clone();
                        }
                    }
                }
            }
        }
    }

    // Only return the pin if we have both name and number
    if !pin.name.is_empty() && !pin.number.is_empty() {
        Some(pin)
    } else {
        None
    }
}

fn parse_in_bom(symbol: &mut KicadSymbol, prop_list: &[Sexp]) {
    symbol.in_bom = prop_list
        .get(1)
        .map(|v| matches!(v, Sexp::Atom(Atom::S(ref s)) if s == "yes"))
        .unwrap_or(false);
}

fn parse_property(symbol: &mut KicadSymbol, prop_list: &[Sexp]) {
    if let (Some(Sexp::Atom(Atom::S(key))), Some(Sexp::Atom(Atom::S(value)))) =
        (prop_list.get(1), prop_list.get(2))
    {
        match key.as_str() {
            "Footprint" => {
                // Handle footprint values that include a library prefix like "C146731:SOIC-8_..."
                if let Some(colon_index) = value.find(':') {
                    symbol.footprint = value[(colon_index + 1)..].to_string();
                } else {
                    symbol.footprint = value.clone();
                }
            }
            "Datasheet" => symbol.datasheet_url = Some(value.clone()),
            "Manufacturer_Name" => symbol.manufacturer = Some(value.clone()),
            "Manufacturer_Part_Number" => symbol.mpn = Some(value.clone()),
            "ki_description" => symbol.description = Some(value.clone()),
            "LCSC Part" => {
                if symbol.mpn.is_none() {
                    symbol.mpn = Some(value.clone());
                }
            }
            "Value" => {
                if symbol.mpn.is_none() && symbol.name == value.clone() {
                    symbol.mpn = Some(value.clone());
                }
            }
            key if key.ends_with("Part Number") => {
                let distributor = key.trim_end_matches(" Part Number");
                symbol
                    .distributors
                    .entry(distributor.to_string())
                    .or_default()
                    .part_number = value.clone();
            }
            key if key.ends_with("Price/Stock") => {
                let distributor = key.trim_end_matches(" Price/Stock");
                symbol
                    .distributors
                    .entry(distributor.to_string())
                    .or_default()
                    .url = value.clone();
            }
            _ => {}
        }

        // Record every property we encounter – irrespective of whether it
        // was handled explicitly above – so we retain the full set of
        // key/value pairs from the KiCad symbol file.
        symbol.properties.insert(key.clone(), value.clone());
    }
}

fn parse_pin(pin_list: &[Sexp]) -> Option<KicadPin> {
    let mut pin = KicadPin::default();

    for item in pin_list {
        if let Sexp::List(prop_list) = item {
            if let (Some(Sexp::Atom(Atom::S(prop_name))), Some(Sexp::Atom(Atom::S(value)))) =
                (prop_list.first(), prop_list.get(1))
            {
                match prop_name.as_str() {
                    "name" => pin.name = value.clone(),
                    "number" => pin.number = value.clone(),
                    _ => {}
                }
            }
        }
    }

    Some(pin)
}
