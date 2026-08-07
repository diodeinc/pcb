use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use pcb_sexpr::Sexpr;

use crate::{MirrorAxis, Point, Rotation, Symbol, SymbolDefinition};

const MAX_EXPANDED_STACKED_PIN_NUMBERS: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SymbolPin {
    pub name: String,
    pub number: String,
    pub numbers: BTreeSet<String>,
    pub point: Point,
    pub electrical_type: String,
    pub hidden: bool,
    alternates: BTreeMap<String, String>,
}

impl SymbolPin {
    pub fn is_hidden_power_input(&self) -> bool {
        self.hidden && self.electrical_type == "power_in"
    }

    pub fn is_power_input(&self) -> bool {
        self.electrical_type == "power_in"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerScope {
    Local,
    Global,
}

pub(crate) fn power_scope(definition: &SymbolDefinition) -> Result<Option<PowerScope>> {
    let items = definition
        .sexpr
        .as_list()
        .with_context(|| format!("symbol {} definition is not a list", definition.lib_id))?;
    let Some(power) = child_list(items, "power") else {
        return Ok(None);
    };
    match power.get(1).and_then(Sexpr::as_atom) {
        None => Ok(Some(PowerScope::Global)),
        Some("local") => Ok(Some(PowerScope::Local)),
        Some("global") => Ok(Some(PowerScope::Global)),
        value => bail!(
            "symbol {} has invalid power scope {:?}; expected bare, local, or global",
            definition.lib_id,
            value
        ),
    }
}

pub(crate) fn unit_indices(definition: &SymbolDefinition) -> Result<Vec<u32>> {
    let mut units = BTreeSet::new();
    let items = definition
        .sexpr
        .as_list()
        .with_context(|| format!("symbol {} definition is not a list", definition.lib_id))?;
    for child in &items[2..] {
        let Some(section) = child.as_list() else {
            continue;
        };
        if section.first().and_then(Sexpr::as_sym) != Some("symbol") {
            continue;
        }
        let name = section
            .get(1)
            .and_then(Sexpr::as_atom)
            .context("nested symbol missing name")?;
        let (unit, _) = section_unit_style(name)?;
        if unit > 0 {
            units.insert(unit);
        }
    }
    if units.is_empty() {
        Ok(vec![1])
    } else {
        Ok(units.into_iter().collect())
    }
}

pub(crate) fn placed_pins(
    definition: &SymbolDefinition,
    symbol: &Symbol,
    include_hidden: bool,
) -> Result<Vec<SymbolPin>> {
    let Some(items) = definition.sexpr.as_list() else {
        bail!("symbol {} definition is not a list", definition.lib_id);
    };
    let mut pins = parse_pins(items, include_hidden)?;
    for child in &items[2..] {
        let Some(section) = child.as_list() else {
            continue;
        };
        if section.first().and_then(Sexpr::as_sym) != Some("symbol") {
            continue;
        }
        let name = section
            .get(1)
            .and_then(Sexpr::as_atom)
            .context("nested symbol missing name")?;
        let (unit, body_style) = section_unit_style(name)?;
        if matches_unit(unit, symbol.unit) && matches_body_style(body_style, symbol.body_style) {
            pins.extend(parse_pins(section, include_hidden)?);
        }
    }
    pins.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.name.cmp(&b.name)));
    pins.dedup_by(|a, b| a.number == b.number && a.name == b.name && a.point == b.point);
    let pin_number_counts = pins.iter().fold(BTreeMap::new(), |mut counts, pin| {
        *counts.entry(pin.number.as_str()).or_insert(0usize) += 1;
        counts
    });
    let mut placed_alternates = BTreeMap::new();
    for pin in &symbol.pins {
        let Some(alternate) = pin.alternate.as_deref() else {
            continue;
        };
        if placed_alternates
            .insert(pin.number.as_str(), alternate)
            .is_some()
        {
            bail!(
                "symbol {} selects more than one alternate for pin {}",
                symbol.id,
                pin.number
            );
        }
    }
    for number in placed_alternates.keys() {
        match pin_number_counts.get(number).copied() {
            None => bail!(
                "symbol {} selects an alternate for undefined pin {number}",
                symbol.id
            ),
            Some(1) => {}
            Some(count) => bail!(
                "symbol {} selects one alternate for {count} definition pins numbered {number}",
                symbol.id
            ),
        }
    }
    let resolved = pins
        .into_iter()
        .map(|mut pin| {
            if let Some(alternate) = placed_alternates.remove(pin.number.as_str()) {
                let electrical_type = pin.alternates.get(alternate).ok_or_else(|| {
                    anyhow!(
                        "symbol {} pin {} selects undefined alternate {alternate}",
                        symbol.id,
                        pin.number
                    )
                })?;
                pin.name = crate::kicad::unescape_text(alternate);
                pin.electrical_type = electrical_type.clone();
            }
            // KiCad parses library-local symbol coordinates with Y inverted,
            // unlike schematic-page coordinates.
            pin.point.y = -pin.point.y;
            pin.point = transform_point(pin.point, symbol);
            Ok(pin)
        })
        .collect::<Result<Vec<_>>>()?;
    debug_assert!(placed_alternates.is_empty());
    Ok(resolved)
}

fn parse_pins(items: &[Sexpr], include_hidden: bool) -> Result<Vec<SymbolPin>> {
    let mut pins = Vec::new();
    for items in items.iter().filter_map(Sexpr::as_list) {
        if items.first().and_then(Sexpr::as_sym) != Some("pin") {
            continue;
        }
        if let Some(pin) = parse_pin(items, include_hidden)? {
            pins.push(pin);
        }
    }
    Ok(pins)
}

fn parse_pin(items: &[Sexpr], include_hidden: bool) -> Result<Option<SymbolPin>> {
    let electrical_type = items
        .get(1)
        .and_then(Sexpr::as_atom)
        .context("symbol pin missing electrical type")?
        .to_string();
    validate_electrical_type(&electrical_type)?;
    let hidden = pin_hidden(items)?;
    if hidden && !include_hidden {
        return Ok(None);
    }
    let at = child_list(items, "at").context("symbol pin missing at")?;
    let point = Point::new(
        number(at.get(1).context("symbol pin missing x")?).context("invalid symbol pin x")?,
        number(at.get(2).context("symbol pin missing y")?).context("invalid symbol pin y")?,
    );
    let name = child_list(items, "name")
        .and_then(|list| list.get(1))
        .and_then(Sexpr::as_atom)
        .filter(|name| *name != "~")
        .map(crate::kicad::unescape_text)
        .unwrap_or_default();
    let number = child_list(items, "number")
        .and_then(|list| list.get(1))
        .and_then(Sexpr::as_atom)
        .context("symbol pin missing number")?
        .to_string();
    let numbers = expand_stacked_pin_number(&crate::kicad::unescape_text(&number))?;
    let mut alternates = BTreeMap::new();
    for alternate in items
        .iter()
        .filter_map(Sexpr::as_list)
        .filter(|list| list.first().and_then(Sexpr::as_sym) == Some("alternate"))
    {
        let name = alternate
            .get(1)
            .and_then(Sexpr::as_atom)
            .context("pin alternate missing name")?;
        let electrical_type = alternate
            .get(2)
            .and_then(Sexpr::as_atom)
            .context("pin alternate missing electrical type")?;
        validate_electrical_type(electrical_type)?;
        if alternates
            .insert(name.to_string(), electrical_type.to_string())
            .is_some()
        {
            bail!("pin {number} has duplicate alternate {name}");
        }
    }
    Ok(Some(SymbolPin {
        name,
        number,
        numbers,
        point,
        electrical_type,
        hidden,
        alternates,
    }))
}

fn pin_hidden(items: &[Sexpr]) -> Result<bool> {
    let mut hidden = false;
    for item in items {
        if item.as_atom() == Some("hide") {
            // KiCad versions before 20241004 used a bare `hide` token.
            hidden = true;
            continue;
        }
        let Some(list) = item.as_list() else {
            continue;
        };
        if list.first().and_then(Sexpr::as_sym) != Some("hide") {
            continue;
        }
        hidden = match list.get(1).and_then(Sexpr::as_atom) {
            Some("yes") => true,
            Some("no") => false,
            _ => bail!("symbol pin hide must be yes or no"),
        };
    }
    Ok(hidden)
}

fn validate_electrical_type(electrical_type: &str) -> Result<()> {
    if matches!(
        electrical_type,
        "input"
            | "output"
            | "bidirectional"
            | "tri_state"
            | "passive"
            | "free"
            | "unspecified"
            | "power_in"
            | "power_out"
            | "open_collector"
            | "open_emitter"
            | "no_connect"
    ) {
        Ok(())
    } else {
        bail!("unsupported KiCad pin electrical type {electrical_type}")
    }
}

pub(crate) fn expand_stacked_pin_number(number: &str) -> Result<BTreeSet<String>> {
    let literal = || Ok(BTreeSet::from([number.to_string()]));
    let has_open = number.contains('[');
    let has_close = number.contains(']');
    if has_open || has_close {
        if !(number.starts_with('[') && number.ends_with(']')) {
            return literal();
        }
    } else {
        return literal();
    }

    let mut expanded = BTreeSet::new();
    for part in number[1..number.len() - 1].split(',').map(str::trim) {
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let Some((start_prefix, start_value)) = alpha_numeric_pin(start.trim()) else {
                return literal();
            };
            let Some((end_prefix, end_value)) = alpha_numeric_pin(end.trim()) else {
                return literal();
            };
            if start_prefix != end_prefix || start_value > end_value {
                return literal();
            }
            let range_len = usize::try_from(end_value - start_value)
                .ok()
                .and_then(|difference| difference.checked_add(1))
                .context("stacked pin range length exceeds platform limits")?;
            if range_len > MAX_EXPANDED_STACKED_PIN_NUMBERS
                || expanded.len() > MAX_EXPANDED_STACKED_PIN_NUMBERS - range_len
            {
                bail!(
                    "stacked pin number {number} expands beyond the limit of {MAX_EXPANDED_STACKED_PIN_NUMBERS} pins"
                );
            }
            for value in start_value..=end_value {
                expanded.insert(format!("{start_prefix}{value}"));
            }
        } else {
            expanded.insert(part.to_string());
            if expanded.len() > MAX_EXPANDED_STACKED_PIN_NUMBERS {
                bail!(
                    "stacked pin number {number} expands beyond the limit of {MAX_EXPANDED_STACKED_PIN_NUMBERS} pins"
                );
            }
        }
    }
    if expanded.is_empty() {
        literal()
    } else {
        Ok(expanded)
    }
}

fn alpha_numeric_pin(value: &str) -> Option<(&str, i64)> {
    let digit_start = value
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!character.is_ascii_digit()).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    (digit_start < value.len()).then(|| {
        let (prefix, digits) = value.split_at(digit_start);
        digits.parse().ok().map(|number| (prefix, number))
    })?
}

pub(crate) fn duplicate_pin_numbers_are_jumpers(definition: &SymbolDefinition) -> Result<bool> {
    let Some(items) = definition
        .sexpr
        .find_list("duplicate_pin_numbers_are_jumpers")
    else {
        return Ok(false);
    };
    match items.get(1).and_then(Sexpr::as_atom) {
        Some("yes") => Ok(true),
        Some("no") => Ok(false),
        value => bail!(
            "symbol {} has invalid duplicate_pin_numbers_are_jumpers value {:?}",
            definition.lib_id,
            value
        ),
    }
}

pub(crate) fn jumper_pin_groups(definition: &SymbolDefinition) -> Result<Vec<BTreeSet<String>>> {
    let Some(groups) = definition.sexpr.find_list("jumper_pin_groups") else {
        return Ok(Vec::new());
    };
    groups[1..]
        .iter()
        .map(|group| {
            group
                .as_list()
                .context("jumper_pin_groups entry must be a list")?
                .iter()
                .map(|pin| {
                    pin.as_atom()
                        .context("jumper_pin_groups pin must be a string")
                        .map(str::to_string)
                })
                .collect()
        })
        .collect()
}

fn child_list<'a>(items: &'a [Sexpr], tag: &str) -> Option<&'a [Sexpr]> {
    items.iter().find_map(|item| {
        let list = item.as_list()?;
        (list.first().and_then(Sexpr::as_sym) == Some(tag)).then_some(list)
    })
}

fn number(value: &Sexpr) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_int().map(|value| value as f64))
        .or_else(|| value.as_atom()?.parse().ok())
}

fn section_unit_style(name: &str) -> Result<(u32, u32)> {
    let mut parts = name.rsplitn(3, '_');
    let style = parts
        .next()
        .context("nested symbol missing body style")?
        .parse()
        .with_context(|| format!("nested symbol {name} has invalid body style"))?;
    let unit = parts
        .next()
        .context("nested symbol missing unit")?
        .parse()
        .with_context(|| format!("nested symbol {name} has invalid unit"))?;
    Ok((unit, style))
}

fn matches_unit(section: u32, selected: u32) -> bool {
    section == 0 || section == selected
}

fn matches_body_style(section: u32, selected: u32) -> bool {
    section == 0 || section == selected
}

fn transform_point(mut point: Point, symbol: &Symbol) -> Point {
    // KiCad composes the symbol matrix as rotation * mirror, so the mirror is
    // applied to the library-local point before the rotation.
    point = match symbol.mirror {
        None => point,
        Some(MirrorAxis::X) => Point::new(point.x, -point.y),
        Some(MirrorAxis::Y) => Point::new(-point.x, point.y),
    };
    point = match symbol.rotation {
        Rotation::Deg0 => point,
        Rotation::Deg90 => Point::new(point.y, -point.x),
        Rotation::Deg180 => Point::new(-point.x, -point.y),
        Rotation::Deg270 => Point::new(-point.y, point.x),
    };
    Point::new(symbol.at.x + point.x, symbol.at.y + point.y)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::SymbolField;

    #[test]
    fn extracts_units_and_transforms_selected_unit_pins() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Device:Multi"
              (symbol "Multi_1_1"
                (pin input line (at -2.54 0 0) (length 2.54)
                  (name "A") (number "1")))
              (symbol "Multi_2_1"
                (pin input line (at 0 2.54 90) (length 2.54)
                  (name "B") (number "2"))))"#,
        )
        .unwrap();
        assert_eq!(unit_indices(&definition).unwrap(), vec![1, 2]);

        let symbol = Symbol {
            id: "symbol".into(),
            lib_id: "Device:Multi".into(),
            unit: 2,
            body_style: 1,
            at: Point::new(10.0, 20.0),
            rotation: Rotation::Deg0,
            mirror: None,
            fields_autoplaced: false,
            fields: BTreeMap::<String, SymbolField>::new(),
            pins: Vec::new(),
            unsupported: Vec::new(),
        };
        assert_eq!(
            placed_pins(&definition, &symbol, false).unwrap(),
            vec![SymbolPin {
                name: "B".into(),
                number: "2".into(),
                numbers: BTreeSet::from(["2".into()]),
                point: Point::new(10.0, 17.46),
                electrical_type: "input".into(),
                hidden: false,
                alternates: BTreeMap::new(),
            }]
        );
    }

    #[test]
    fn transforms_pins_with_kicad_rotation_then_mirror_matrix() {
        let mut symbol = Symbol {
            id: "symbol".into(),
            lib_id: "Test:Symbol".into(),
            unit: 1,
            body_style: 1,
            at: Point::new(10.0, 20.0),
            rotation: Rotation::Deg90,
            mirror: Some(MirrorAxis::X),
            fields_autoplaced: false,
            fields: BTreeMap::new(),
            pins: Vec::new(),
            unsupported: Vec::new(),
        };

        // KiCad's matrix is R90 * MirrorX: (2, 3) -> (2, -3) -> (-3, -2).
        assert_eq!(
            transform_point(Point::new(2.0, 3.0), &symbol),
            Point::new(7.0, 18.0)
        );

        symbol.mirror = Some(MirrorAxis::Y);
        // R90 * MirrorY: (2, 3) -> (-2, 3) -> (3, 2).
        assert_eq!(
            transform_point(Point::new(2.0, 3.0), &symbol),
            Point::new(13.0, 22.0)
        );
    }
}
