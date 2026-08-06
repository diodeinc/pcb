use std::collections::BTreeSet;

use pcb_sexpr::Sexpr;

use crate::{MirrorAxis, Point, Rotation, Symbol, SymbolDefinition};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SymbolPin {
    pub name: String,
    pub number: String,
    pub point: Point,
    pub electrical_type: String,
    pub hidden: bool,
}

impl SymbolPin {
    pub fn is_no_connect(&self) -> bool {
        self.electrical_type == "no_connect"
    }

    pub fn is_hidden_power_input(&self) -> bool {
        self.hidden && self.electrical_type == "power_in"
    }
}

pub(crate) fn unit_indices(definition: &SymbolDefinition) -> Vec<u32> {
    let mut units = BTreeSet::new();
    let Some(items) = definition.sexpr.as_list() else {
        return vec![1];
    };
    for child in &items[2..] {
        let Some(section) = child.as_list() else {
            continue;
        };
        if section.first().and_then(Sexpr::as_sym) != Some("symbol") {
            continue;
        }
        let Some(name) = section.get(1).and_then(Sexpr::as_atom) else {
            continue;
        };
        let (unit, _) = section_unit_style(name);
        if unit > 0 {
            units.insert(unit);
        }
    }
    if units.is_empty() {
        vec![1]
    } else {
        units.into_iter().collect()
    }
}

pub(crate) fn placed_pins(
    definition: &SymbolDefinition,
    symbol: &Symbol,
    include_hidden: bool,
) -> Vec<SymbolPin> {
    let Some(items) = definition.sexpr.as_list() else {
        return Vec::new();
    };
    let mut pins = parse_pins(items, include_hidden);
    for child in &items[2..] {
        let Some(section) = child.as_list() else {
            continue;
        };
        if section.first().and_then(Sexpr::as_sym) != Some("symbol") {
            continue;
        }
        let Some(name) = section.get(1).and_then(Sexpr::as_atom) else {
            continue;
        };
        let (unit, body_style) = section_unit_style(name);
        if matches_unit(unit, symbol.unit) && matches_body_style(body_style, symbol.body_style) {
            pins.extend(parse_pins(section, include_hidden));
        }
    }
    pins.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.name.cmp(&b.name)));
    pins.dedup_by(|a, b| a.number == b.number && a.name == b.name && a.point == b.point);
    pins.into_iter()
        .map(|mut pin| {
            // KiCad parses library-local symbol coordinates with Y inverted,
            // unlike schematic-page coordinates.
            pin.point.y = -pin.point.y;
            pin.point = transform_point(pin.point, symbol);
            pin
        })
        .collect()
}

fn parse_pins(items: &[Sexpr], include_hidden: bool) -> Vec<SymbolPin> {
    items
        .iter()
        .filter_map(Sexpr::as_list)
        .filter(|items| items.first().and_then(Sexpr::as_sym) == Some("pin"))
        .filter_map(|items| parse_pin(items, include_hidden))
        .collect()
}

fn parse_pin(items: &[Sexpr], include_hidden: bool) -> Option<SymbolPin> {
    let electrical_type = items.get(1)?.as_atom()?.to_string();
    let hidden = items.iter().any(|item| {
        item.as_atom() == Some("hide")
            || item
                .as_list()
                .is_some_and(|list| list.first().and_then(Sexpr::as_sym) == Some("hide"))
    });
    if hidden && !include_hidden {
        return None;
    }
    let at = child_list(items, "at")?;
    let point = Point::new(number(at.get(1)?)?, number(at.get(2)?)?);
    let name = child_list(items, "name")
        .and_then(|list| list.get(1))
        .and_then(Sexpr::as_atom)
        .filter(|name| *name != "~")
        .unwrap_or_default()
        .to_string();
    let number = child_list(items, "number")
        .and_then(|list| list.get(1))
        .and_then(Sexpr::as_atom)?
        .to_string();
    Some(SymbolPin {
        name,
        number,
        point,
        electrical_type,
        hidden,
    })
}

pub(crate) fn duplicate_pin_numbers_are_jumpers(definition: &SymbolDefinition) -> bool {
    definition
        .sexpr
        .find_list("duplicate_pin_numbers_are_jumpers")
        .and_then(|items| items.get(1))
        .and_then(Sexpr::as_atom)
        == Some("yes")
}

pub(crate) fn jumper_pin_groups(definition: &SymbolDefinition) -> Vec<BTreeSet<String>> {
    let Some(groups) = definition.sexpr.find_list("jumper_pin_groups") else {
        return Vec::new();
    };
    groups
        .iter()
        .skip(1)
        .filter_map(Sexpr::as_list)
        .map(|group| {
            group
                .iter()
                .filter_map(Sexpr::as_atom)
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .filter(|group| group.len() > 1)
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

fn section_unit_style(name: &str) -> (u32, u32) {
    let mut parts = name.rsplitn(3, '_');
    let style = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let unit = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    (unit, style)
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
        assert_eq!(unit_indices(&definition), vec![1, 2]);

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
            placed_pins(&definition, &symbol, false),
            vec![SymbolPin {
                name: "B".into(),
                number: "2".into(),
                point: Point::new(10.0, 17.46),
                electrical_type: "input".into(),
                hidden: false,
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
