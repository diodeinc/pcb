use crate::kicad::metadata::SymbolMetadata;
use crate::{Part, Pin, PinAt, Symbol};
use anyhow::Result;
use pcb_sexpr::{Sexpr, SexprKind, parse};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Default, Clone, Serialize)]
pub struct KicadSymbol {
    pub(super) name: String,
    pub(super) reference: String,
    pub(super) extends: Option<String>,
    pub(super) footprint: String,
    pub(super) in_bom: bool,
    pub(super) pins: Vec<KicadPin>,
    pub(super) mpn: Option<String>,
    pub(super) manufacturer: Option<String>,
    pub(super) datasheet_url: Option<String>,
    pub(super) description: Option<String>,
    pub(super) distributors: HashMap<String, Part>,
    pub(super) properties: HashMap<String, String>,
    pub(super) raw_sexp: Option<Sexpr>,
}

impl KicadSymbol {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn extends(&self) -> Option<&str> {
        self.extends.as_deref()
    }

    pub fn raw_sexp(&self) -> Option<&Sexpr> {
        self.raw_sexp.as_ref()
    }

    pub fn pins(&self) -> &[KicadPin] {
        &self.pins
    }

    pub fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    pub fn metadata(&self) -> SymbolMetadata {
        let mut metadata = SymbolMetadata::from_property_iter(
            self.properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );

        if metadata.primary.reference.is_none() && !self.reference.is_empty() {
            metadata.primary.reference = Some(self.reference.clone());
        }
        if metadata.primary.footprint.is_none() && !self.footprint.is_empty() {
            metadata.primary.footprint = Some(self.footprint.clone());
        }
        if metadata.primary.datasheet.is_none() {
            metadata.primary.datasheet = self.datasheet_url.clone();
        }
        if metadata.primary.description.is_none() {
            metadata.primary.description = self.description.clone();
        }

        metadata
    }
}

impl KicadPin {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn number(&self) -> &str {
        &self.number
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct KicadPin {
    pub(super) name: String,
    pub(super) number: String,
    pub(super) electrical_type: Option<String>,
    pub(super) graphical_style: Option<String>,
    pub(super) at: Option<PinAt>,
    pub(super) length: Option<f64>,
    pub(super) hidden: bool,
}

impl From<KicadSymbol> for Symbol {
    fn from(symbol: KicadSymbol) -> Self {
        Symbol {
            name: symbol.name,
            footprint: symbol.footprint,
            reference: symbol.reference,
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
                    electrical_type: pin.electrical_type,
                    graphical_style: pin.graphical_style,
                    at: pin.at,
                    length: pin.length,
                    hidden: pin.hidden,
                })
                .collect(),
            raw_sexp: symbol.raw_sexp,
        }
    }
}

impl FromStr for KicadSymbol {
    type Err = anyhow::Error;

    fn from_str(content: &str) -> Result<Self> {
        let sexp = parse(content)?;

        // Find the 'symbol' S-expression
        let symbol_sexp = match sexp.kind {
            SexprKind::List(kicad_symbol_lib) => kicad_symbol_lib
                .into_iter()
                .find_map(|item| match &item.kind {
                    SexprKind::List(symbol_list) => match symbol_list.first().map(|s| &s.kind) {
                        Some(SexprKind::Symbol(sym)) if sym == "symbol" => {
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

pub(super) fn parse_symbol(symbol_data: &[Sexpr]) -> Result<KicadSymbol> {
    // Extract the symbol name
    let name = symbol_data
        .get(1)
        .and_then(|sexp| match &sexp.kind {
            SexprKind::Symbol(name) | SexprKind::String(name) => Some(name.clone()),
            _ => None,
        })
        .ok_or(anyhow::anyhow!("Symbol name not found"))?;

    let mut symbol = KicadSymbol {
        name,
        raw_sexp: Some(Sexpr::list(symbol_data.to_vec())),
        ..Default::default()
    };
    let mut nested_pin_groups: BTreeMap<u32, Vec<NestedStylePins>> = BTreeMap::new();

    for prop in &symbol_data[2..] {
        if let SexprKind::List(prop_list) = &prop.kind
            && let Some(SexprKind::Symbol(prop_name)) = prop_list.first().map(|s| &s.kind)
        {
            match prop_name.as_str() {
                "extends" => {
                    if let Some(SexprKind::Symbol(parent_name) | SexprKind::String(parent_name)) =
                        prop_list.get(1).map(|s| &s.kind)
                    {
                        symbol.extends = Some(parent_name.clone());
                    }
                }
                "in_bom" => parse_in_bom(&mut symbol, prop_list),
                "property" => parse_property(&mut symbol, prop_list),
                "pin" => {
                    if let Some(pin) = parse_pin(prop_list) {
                        symbol.pins.push(pin)
                    }
                }
                _ if prop_name.starts_with("symbol") => {
                    // Nested symbol sections contain unit/style-specific graphics + pins.
                    let (unit, style) = nested_symbol_unit_style(prop_list);
                    let pins = parse_symbol_section(prop_list);
                    let named_pin_count = pins.iter().filter(|p| is_named_pin(p)).count();
                    nested_pin_groups
                        .entry(unit)
                        .or_default()
                        .push(NestedStylePins {
                            style,
                            named_pin_count,
                            pins,
                        });
                }
                _ => {}
            }
        }
    }

    for (_unit, style_candidates) in nested_pin_groups {
        if let Some(best) = style_candidates
            .into_iter()
            .max_by_key(|c| (c.named_pin_count, Reverse(c.style)))
        {
            symbol.pins.extend(best.pins);
        }
    }

    Ok(symbol)
}

struct NestedStylePins {
    style: u32,
    named_pin_count: usize,
    pins: Vec<KicadPin>,
}

fn is_named_pin(pin: &KicadPin) -> bool {
    !pin.name.is_empty() && pin.name != "~"
}

// Parse pins from a nested symbol section.
fn parse_symbol_section(section_data: &[Sexpr]) -> Vec<KicadPin> {
    let mut pins = Vec::new();
    for item in section_data {
        if let SexprKind::List(pin_data) = &item.kind
            && let Some(SexprKind::Symbol(type_name)) = pin_data.first().map(|s| &s.kind)
            && type_name == "pin"
            && let Some(pin) = parse_pin_from_section(pin_data)
        {
            pins.push(pin);
        }
    }
    pins
}

fn nested_symbol_unit_style(section_data: &[Sexpr]) -> (u32, u32) {
    section_data
        .get(1)
        .and_then(|n| n.as_str().or_else(|| n.as_sym()))
        .map(|name| {
            // Parse trailing `_<unit>_<style>` without constraining the base name.
            let mut parts = name.rsplitn(3, '_');
            let style = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();
            let unit = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();
            (unit, style)
        })
        .unwrap_or((0, 0))
}

fn parse_pin_common(pin_data: &[Sexpr]) -> KicadPin {
    // Format: (pin <electrical_type> <graphical_style> (at X Y Z) (length L) (name "Name") (number "N"))
    let mut pin = KicadPin {
        electrical_type: pin_data
            .get(1)
            .and_then(Sexpr::as_sym)
            .map(ToOwned::to_owned),
        graphical_style: pin_data
            .get(2)
            .and_then(Sexpr::as_sym)
            .map(ToOwned::to_owned),
        ..Default::default()
    };

    // Extract known pin attributes.
    for item in pin_data.iter().skip(3) {
        match &item.kind {
            SexprKind::Symbol(sym) if sym == "hide" => {
                pin.hidden = true;
            }
            SexprKind::List(attr_data) => {
                let Some(attr_name) = attr_data.first().and_then(Sexpr::as_sym) else {
                    continue;
                };
                match attr_name {
                    "name" => {
                        if let Some(name) = attr_data.get(1).and_then(Sexpr::as_str) {
                            pin.name = name.to_string();
                        }
                    }
                    "number" => {
                        if let Some(number) = attr_data.get(1).and_then(Sexpr::as_str) {
                            pin.number = number.to_string();
                        }
                    }
                    "at" => pin.at = parse_pin_at(attr_data),
                    "length" => pin.length = parse_number(attr_data.get(1)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pin
}

fn parse_pin_from_section(pin_data: &[Sexpr]) -> Option<KicadPin> {
    // KiCad allows unnamed pins (name=""); keep them and let higher layers fall back to number.
    let pin = parse_pin_common(pin_data);
    if !pin.number.is_empty() {
        Some(pin)
    } else {
        None
    }
}

fn parse_in_bom(symbol: &mut KicadSymbol, prop_list: &[Sexpr]) {
    symbol.in_bom = prop_list
        .get(1)
        .map(|v| matches!(&v.kind, SexprKind::Symbol(s) if s == "yes"))
        .unwrap_or(false);
}

fn parse_property(symbol: &mut KicadSymbol, prop_list: &[Sexpr]) {
    let key = prop_list.get(1).and_then(|s| match &s.kind {
        SexprKind::Symbol(k) | SexprKind::String(k) => Some(k.clone()),
        _ => None,
    });
    let value = prop_list.get(2).and_then(|s| match &s.kind {
        SexprKind::Symbol(v) | SexprKind::String(v) => Some(v.clone()),
        _ => None,
    });
    if let (Some(key), Some(value)) = (key, value) {
        match key.as_str() {
            "Reference" => symbol.reference = value.clone(),
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

fn parse_pin(pin_list: &[Sexpr]) -> Option<KicadPin> {
    let pin = parse_pin_common(pin_list);
    if !pin.number.is_empty() {
        Some(pin)
    } else {
        None
    }
}

fn parse_pin_at(at: &[Sexpr]) -> Option<PinAt> {
    let x = parse_number(at.get(1))?;
    let y = parse_number(at.get(2))?;
    let rotation = parse_number(at.get(3));
    Some(PinAt { x, y, rotation })
}

fn parse_number(node: Option<&Sexpr>) -> Option<f64> {
    node.and_then(|n| n.as_float().or_else(|| n.as_int().map(|v| v as f64)))
}
