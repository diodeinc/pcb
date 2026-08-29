//! Headless port of KiCad's automatic symbol-field placement.
//!
//! KiCad exposes this operation only through eeschema editor actions. This
//! module follows the same geometry-only rules so generated schematics have
//! valid field positions before the editor opens.

use anyhow::Result;
use pcb_sexpr::Sexpr;

use crate::symbol::number;
use crate::{
    CONNECTION_GRID_MM, FieldHorizontalJustify, FieldJustify, FieldVerticalJustify,
    GEOMETRY_EPS_MM, LabelSpin, MirrorAxis, Point, Rotation, Symbol, SymbolDefinition, symbol,
};

const FIELD_ROW_SPACING_MM: f64 = 2.54;
const HPADDING_MM: f64 = 0.635;
const VPADDING_MM: f64 = 0.381;
const ESTIMATED_TEXT_WIDTH_EM: f64 = 1.0;

pub(crate) fn autoplace_symbol_fields(
    symbol: &mut Symbol,
    definition: &SymbolDefinition,
) -> Result<bool> {
    if !symbol.fields_autoplaced {
        return Ok(false);
    }

    let parsed = symbol::ParsedSymbolDefinition::parse(definition)?;
    let pins = parsed.placed_pins(symbol)?;
    let Some(body_bounds) =
        symbol_body_bounds(definition, symbol).or_else(|| visible_pin_bounds(&pins))
    else {
        return Ok(false);
    };
    let layouts = movable_field_layouts(symbol);
    if layouts.is_empty() {
        return Ok(false);
    }

    let occupancy = visible_pin_side_occupancy(&pins);
    let selection = choose_side(
        symbol,
        parsed.power_scope().is_some(),
        body_bounds,
        &occupancy,
    );
    let field_box = place_field_box(
        selection.side,
        body_bounds,
        &occupancy,
        field_box_size(&layouts),
    );

    let mut changed = false;
    let mut cursor_y = field_box.min_y;
    let rotation_deg = match symbol.rotation {
        Rotation::Deg0 | Rotation::Deg180 => 0.0,
        Rotation::Deg90 | Rotation::Deg270 => 90.0,
    };
    for layout in layouts {
        let placement_justify = field_justify(selection);
        let position = field_position(
            selection.side,
            field_box,
            &layout,
            placement_justify,
            &mut cursor_y,
        );
        let justify = stored_field_justify(symbol, rotation_deg, placement_justify);
        let Some(field) = symbol.fields.get_mut(&layout.name) else {
            continue;
        };
        changed |= field.at != position
            || field.rotation_deg != rotation_deg
            || field.justify != Some(justify);
        field.at = position;
        field.rotation_deg = rotation_deg;
        field.justify = Some(justify);
    }
    Ok(changed)
}

pub(crate) fn apply_definition_field_styles(
    symbol: &mut Symbol,
    definition: &SymbolDefinition,
) -> Result<()> {
    for (name, template) in definition.default_fields()? {
        let Some(field) = symbol.fields.get_mut(&name) else {
            continue;
        };
        field.effects = template.effects;
        field.hidden = template.hidden;
        field.do_not_autoplace = template.do_not_autoplace;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy)]
struct SideSelection {
    side: Side,
    pins: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct SideOccupancy {
    count: usize,
    bounds: Option<Bounds>,
}

struct FieldLayout {
    name: String,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bounds {
    pub(crate) fn from_points(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        let mut points = points.into_iter();
        let first = points.next()?;
        let mut bounds = Self {
            min_x: first.x,
            min_y: first.y,
            max_x: first.x,
            max_y: first.y,
        };
        for point in points {
            bounds.include(point);
        }
        Some(bounds)
    }

    pub(crate) fn include(&mut self, point: Point) {
        self.min_x = self.min_x.min(point.x);
        self.min_y = self.min_y.min(point.y);
        self.max_x = self.max_x.max(point.x);
        self.max_y = self.max_y.max(point.y);
    }

    pub(crate) fn union(&mut self, other: Self) {
        self.include(Point::new(other.min_x, other.min_y));
        self.include(Point::new(other.max_x, other.max_y));
    }

    pub fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(self) -> f64 {
        self.max_y - self.min_y
    }

    fn center(self) -> Point {
        Point::new(
            (self.min_x + self.max_x) * 0.5,
            (self.min_y + self.max_y) * 0.5,
        )
    }

    pub(crate) fn translated(self, dx: f64, dy: f64) -> Self {
        Self {
            min_x: self.min_x + dx,
            min_y: self.min_y + dy,
            max_x: self.max_x + dx,
            max_y: self.max_y + dy,
        }
    }
}

pub(crate) fn symbol_visual_bounds(
    symbol: &Symbol,
    definition: &SymbolDefinition,
) -> Result<Option<Bounds>> {
    let parsed = symbol::ParsedSymbolDefinition::parse(definition)?;
    let pins = parsed.placed_pins(symbol)?;
    let mut bounds = symbol_body_bounds(definition, symbol).or_else(|| {
        Bounds::from_points(
            pins.iter()
                .filter(|pin| !pin.hidden)
                .flat_map(|pin| [pin.point, pin.body_point]),
        )
    });
    for field in symbol
        .fields
        .values()
        .filter(|field| !field.hidden && !field.value.trim().is_empty())
    {
        let field_bounds = field_text_bounds(
            symbol,
            field,
            field.value.chars().count().max(1) as f64
                * field.effects.font_size.x.abs()
                * ESTIMATED_TEXT_WIDTH_EM,
            field.effects.font_size.y.abs(),
        );
        match &mut bounds {
            Some(bounds) => bounds.union(field_bounds),
            None => bounds = Some(field_bounds),
        }
    }
    Ok(bounds)
}

pub(crate) fn symbol_geometry_bounds(
    symbol: &Symbol,
    definition: &SymbolDefinition,
) -> Result<Option<Bounds>> {
    let parsed = symbol::ParsedSymbolDefinition::parse(definition)?;
    let pins = parsed.placed_pins(symbol)?;
    let mut bounds = symbol_body_bounds(definition, symbol);
    let pin_bounds = Bounds::from_points(pins.iter().flat_map(|pin| [pin.point, pin.body_point]));
    match (&mut bounds, pin_bounds) {
        (Some(bounds), Some(pin_bounds)) => bounds.union(pin_bounds),
        (None, pin_bounds) => bounds = pin_bounds,
        _ => {}
    }
    Ok(bounds)
}

impl Symbol {
    pub fn visual_bounds(&self, definition: &SymbolDefinition) -> Result<Option<Bounds>> {
        symbol_visual_bounds(self, definition)
    }
}

fn field_text_bounds(
    symbol: &Symbol,
    field: &crate::SymbolField,
    width: f64,
    height: f64,
) -> Bounds {
    let justify = field.justify.unwrap_or(FieldJustify::centered());
    let (min_x, max_x) = match justify.horizontal {
        Some(FieldHorizontalJustify::Left) => (0.0, width),
        Some(FieldHorizontalJustify::Right) => (-width, 0.0),
        Some(FieldHorizontalJustify::Center) | None => (-width * 0.5, width * 0.5),
    };
    let (min_y, max_y) = match justify.vertical {
        Some(FieldVerticalJustify::Top) => (0.0, height),
        Some(FieldVerticalJustify::Bottom) => (-height, 0.0),
        Some(FieldVerticalJustify::Center) | None => (-height * 0.5, height * 0.5),
    };
    Bounds::from_points(
        [
            Point::new(min_x, min_y),
            Point::new(min_x, max_y),
            Point::new(max_x, min_y),
            Point::new(max_x, max_y),
        ]
        .map(|point| {
            let point = transform_field_vector(point, symbol, field.rotation_deg);
            Point::new(field.at.x + point.x, field.at.y + point.y)
        }),
    )
    .expect("text bounds have four corners")
}

fn transform_field_vector(point: Point, symbol: &Symbol, field_rotation_deg: f64) -> Point {
    let radians = field_rotation_deg.to_radians();
    let (sin, cos) = radians.sin_cos();
    symbol::transform_vector(
        Point::new(point.x * cos - point.y * sin, point.x * sin + point.y * cos),
        symbol,
    )
}

fn movable_field_layouts(symbol: &Symbol) -> Vec<FieldLayout> {
    symbol
        .fields
        .values()
        .filter(|field| !field.hidden && !field.do_not_autoplace && !field.value.trim().is_empty())
        .map(|field| {
            let font = field.effects.font_size;
            let width =
                field.value.chars().count().max(1) as f64 * font.x.abs() * ESTIMATED_TEXT_WIDTH_EM;
            FieldLayout {
                name: field.name.clone(),
                width: width.max(font.x.abs()),
                height: font.y.abs().max(CONNECTION_GRID_MM),
            }
        })
        .collect()
}

fn field_box_size(layouts: &[FieldLayout]) -> Point {
    Point::new(
        layouts
            .iter()
            .map(|layout| layout.width)
            .fold(0.0, f64::max),
        layouts.iter().map(field_slot_height).sum(),
    )
}

fn field_slot_height(layout: &FieldLayout) -> f64 {
    snap_up(layout.height.max(FIELD_ROW_SPACING_MM), CONNECTION_GRID_MM)
}

fn snap_up(value: f64, grid: f64) -> f64 {
    ((value - GEOMETRY_EPS_MM) / grid).ceil() * grid
}

fn snap_down(value: f64, grid: f64) -> f64 {
    ((value + GEOMETRY_EPS_MM) / grid).floor() * grid
}

fn symbol_body_bounds(definition: &SymbolDefinition, placed: &Symbol) -> Option<Bounds> {
    let root = definition.sexpr.as_list()?;
    let mut bounds = None;
    for child in root.iter().skip(2) {
        let Some(items) = child.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) == Some("symbol") {
            let name = items.get(1).and_then(Sexpr::as_atom)?;
            let (unit, body_style) = crate::symbol::section_unit_style(name).ok()?;
            if unit != 0 && unit != placed.unit {
                continue;
            }
            if body_style != 0 && body_style != placed.body_style {
                continue;
            }
            include_primitives(&mut bounds, &items[2..], placed);
        } else {
            include_primitive(&mut bounds, items, placed);
        }
    }
    bounds
}

fn include_primitives(bounds: &mut Option<Bounds>, items: &[Sexpr], placed: &Symbol) {
    for item in items {
        if let Some(items) = item.as_list() {
            include_primitive(bounds, items, placed);
        }
    }
}

fn include_primitive(bounds: &mut Option<Bounds>, items: &[Sexpr], placed: &Symbol) {
    let Some(tag) = items.first().and_then(Sexpr::as_sym) else {
        return;
    };
    let local = match tag {
        "polyline" | "bezier" => pcb_sexpr::find_child_list(items, "pts").and_then(|pts| {
            Bounds::from_points(
                pts.iter()
                    .skip(1)
                    .filter_map(Sexpr::as_list)
                    .filter_map(parse_xy),
            )
        }),
        "rectangle" => Bounds::from_points(
            [
                pcb_sexpr::find_child_list(items, "start").and_then(parse_xy),
                pcb_sexpr::find_child_list(items, "end").and_then(parse_xy),
            ]
            .into_iter()
            .flatten(),
        ),
        "circle" => pcb_sexpr::find_child_list(items, "center")
            .and_then(parse_xy)
            .and_then(|center| {
                let radius = pcb_sexpr::find_child_list(items, "radius")?
                    .get(1)
                    .and_then(number)?;
                Bounds::from_points([
                    Point::new(center.x - radius, center.y - radius),
                    Point::new(center.x + radius, center.y + radius),
                ])
            }),
        "arc" => Bounds::from_points(
            ["start", "mid", "end"]
                .into_iter()
                .filter_map(|tag| pcb_sexpr::find_child_list(items, tag).and_then(parse_xy)),
        ),
        _ => None,
    };
    let Some(local) = local else {
        return;
    };
    let transformed = transform_bounds(local, placed);
    match bounds {
        Some(bounds) => bounds.union(transformed),
        None => *bounds = Some(transformed),
    }
}

fn transform_bounds(bounds: Bounds, placed: &Symbol) -> Bounds {
    Bounds::from_points([
        transform_local(Point::new(bounds.min_x, bounds.min_y), placed),
        transform_local(Point::new(bounds.min_x, bounds.max_y), placed),
        transform_local(Point::new(bounds.max_x, bounds.min_y), placed),
        transform_local(Point::new(bounds.max_x, bounds.max_y), placed),
    ])
    .expect("four symbol body corners")
}

fn transform_local(mut point: Point, placed: &Symbol) -> Point {
    point.y = -point.y;
    symbol::transform_point(point, placed)
}

fn visible_pin_bounds(pins: &[symbol::PlacedPin]) -> Option<Bounds> {
    Bounds::from_points(
        pins.iter()
            .filter(|pin| !pin.hidden)
            .flat_map(|pin| [pin.point, pin.body_point]),
    )
}

fn visible_pin_side_occupancy(pins: &[symbol::PlacedPin]) -> [SideOccupancy; 4] {
    let mut occupancy = [SideOccupancy::default(); 4];
    for pin in pins.iter().filter(|pin| !pin.hidden) {
        let side = side_from_spin(pin.outward_spin);
        let bounds = Bounds::from_points([pin.point, pin.body_point]).expect("two pin points");
        let slot = &mut occupancy[side_index(side)];
        slot.count += 1;
        match &mut slot.bounds {
            Some(existing) => existing.union(bounds),
            None => slot.bounds = Some(bounds),
        }
    }
    occupancy
}

fn side_from_spin(spin: LabelSpin) -> Side {
    match spin {
        LabelSpin::Left => Side::Left,
        LabelSpin::Up => Side::Top,
        LabelSpin::Right => Side::Right,
        LabelSpin::Bottom => Side::Bottom,
    }
}

fn side_index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
        Side::Top => 2,
        Side::Bottom => 3,
    }
}

fn choose_side(
    symbol: &Symbol,
    is_power_symbol: bool,
    body: Bounds,
    occupancy: &[SideOccupancy; 4],
) -> SideSelection {
    let preferred = preferred_sides(symbol, is_power_symbol, body);
    preferred
        .iter()
        .copied()
        .find(|side| occupancy[side_index(*side)].count == 0)
        .map(|side| SideSelection { side, pins: 0 })
        .unwrap_or_else(|| {
            let side = preferred
                .iter()
                .copied()
                .min_by_key(|side| occupancy[side_index(*side)].count)
                .unwrap_or(Side::Right);
            SideSelection {
                side,
                pins: occupancy[side_index(side)].count,
            }
        })
}

fn preferred_sides(symbol: &Symbol, is_power_symbol: bool, body: Bounds) -> [Side; 4] {
    if is_power_symbol {
        return match symbol.rotation {
            Rotation::Deg0 => [Side::Top, Side::Bottom, Side::Right, Side::Left],
            Rotation::Deg90 => [Side::Left, Side::Right, Side::Top, Side::Bottom],
            Rotation::Deg180 => [Side::Bottom, Side::Top, Side::Left, Side::Right],
            Rotation::Deg270 => [Side::Right, Side::Left, Side::Top, Side::Bottom],
        };
    }
    if body.width() > body.height() * 3.0 {
        return [Side::Top, Side::Bottom, Side::Right, Side::Left];
    }
    if symbol.mirror == Some(MirrorAxis::X)
        && matches!(symbol.rotation, Rotation::Deg0 | Rotation::Deg180)
    {
        [Side::Left, Side::Top, Side::Right, Side::Bottom]
    } else {
        [Side::Right, Side::Top, Side::Left, Side::Bottom]
    }
}

fn place_field_box(
    side: Side,
    body: Bounds,
    occupancy: &[SideOccupancy; 4],
    size: Point,
) -> Bounds {
    let center = body.center();
    let mut min_x = center.x - size.x * 0.5;
    let mut min_y = center.y - size.y * 0.5;
    match side {
        Side::Right => min_x = body.max_x + HPADDING_MM,
        Side::Left => min_x = body.min_x - HPADDING_MM - size.x,
        Side::Top => min_y = body.min_y - VPADDING_MM - size.y,
        Side::Bottom => min_y = body.max_y + VPADDING_MM,
    }
    if let Some(pin_bounds) = occupancy[side_index(side)].bounds {
        match side {
            Side::Top | Side::Bottom => min_x = pin_bounds.max_x + HPADDING_MM * 2.0,
            Side::Left | Side::Right => min_y = pin_bounds.max_y + VPADDING_MM * 2.0,
        }
    }
    Bounds {
        min_x,
        min_y,
        max_x: min_x + size.x,
        max_y: min_y + size.y,
    }
}

fn field_justify(selection: SideSelection) -> FieldJustify {
    let horizontal = if selection.pins > 0 {
        match selection.side {
            Side::Top | Side::Bottom => FieldHorizontalJustify::Left,
            Side::Left | Side::Right => FieldHorizontalJustify::Center,
        }
    } else {
        match selection.side {
            Side::Right => FieldHorizontalJustify::Left,
            Side::Left => FieldHorizontalJustify::Right,
            Side::Top | Side::Bottom => FieldHorizontalJustify::Center,
        }
    };
    FieldJustify::new(Some(horizontal), Some(FieldVerticalJustify::Center))
}

fn stored_field_justify(
    symbol: &Symbol,
    field_rotation_deg: f64,
    justify: FieldJustify,
) -> FieldJustify {
    let text_axis = transform_field_vector(Point::new(1.0, 0.0), symbol, field_rotation_deg);
    debug_assert!(text_axis.y.abs() <= GEOMETRY_EPS_MM);
    if text_axis.x >= 0.0 {
        return justify;
    }
    FieldJustify::new(
        justify.horizontal.map(|horizontal| match horizontal {
            FieldHorizontalJustify::Left => FieldHorizontalJustify::Right,
            FieldHorizontalJustify::Right => FieldHorizontalJustify::Left,
            FieldHorizontalJustify::Center => FieldHorizontalJustify::Center,
        }),
        justify.vertical,
    )
}

fn field_position(
    side: Side,
    field_box: Bounds,
    layout: &FieldLayout,
    justify: FieldJustify,
    cursor_y: &mut f64,
) -> Point {
    let x = match justify.horizontal {
        Some(FieldHorizontalJustify::Left) => field_box.min_x,
        Some(FieldHorizontalJustify::Right) => field_box.max_x,
        Some(FieldHorizontalJustify::Center) | None => (field_box.min_x + field_box.max_x) * 0.5,
    };
    let height = field_slot_height(layout);
    let y = *cursor_y + height * 0.5;
    *cursor_y += height;
    let mut position = Point::new(x, y);
    match side {
        Side::Right => position.x = snap_up(position.x, CONNECTION_GRID_MM),
        Side::Left => position.x = snap_down(position.x, CONNECTION_GRID_MM),
        Side::Top => position.y = snap_down(position.y, CONNECTION_GRID_MM),
        Side::Bottom => position.y = snap_up(position.y, CONNECTION_GRID_MM),
    }
    position
}

fn parse_xy(items: &[Sexpr]) -> Option<Point> {
    Some(Point::new(number(items.get(1)?)?, number(items.get(2)?)?))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::SymbolField;

    #[test]
    fn places_visible_fields_on_the_first_pin_free_side() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:IC"
              (property "Reference" "U" (at 0 0 0)
                (effects (font (size 1.016 1.016))))
              (property "Value" "IC" (at 0 0 0)
                (effects (font (size 1.27 1.27))))
              (symbol "IC_1_1"
                (rectangle (start -5 -5) (end 5 5))
                (pin input line (at -7.54 0 0) (length 2.54)
                  (name "IN") (number "1"))))"#,
        )
        .unwrap();
        let mut symbol = test_symbol(Point::default(), Rotation::Deg0, None);

        apply_definition_field_styles(&mut symbol, &definition).unwrap();
        assert!(autoplace_symbol_fields(&mut symbol, &definition).unwrap());

        let reference = symbol.field("Reference").unwrap();
        let value = symbol.field("Value").unwrap();
        assert!(reference.at.x > 5.0);
        assert!(value.at.x > 5.0);
        assert!(reference.at.y < value.at.y);
        assert!((value.at.y - reference.at.y - FIELD_ROW_SPACING_MM).abs() < 1.0e-9);
        assert_eq!(reference.effects.font_size.x, 1.016);
    }

    #[test]
    fn preserves_horizontal_display_for_rotated_mirrored_symbols() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:IC"
              (symbol "IC_1_1"
                (rectangle (start -5 -2) (end 5 2))))"#,
        )
        .unwrap();
        let mut symbol = test_symbol(Point::new(10.0, 20.0), Rotation::Deg90, Some(MirrorAxis::X));

        assert!(autoplace_symbol_fields(&mut symbol, &definition).unwrap());

        for field in symbol.fields.values() {
            assert_eq!(field.rotation_deg, 90.0);
            assert_ne!(field.at, symbol.at);
        }
        assert_eq!(
            symbol.field("Reference").unwrap().justify,
            Some(FieldJustify::new(
                Some(FieldHorizontalJustify::Right),
                Some(FieldVerticalJustify::Center),
            ))
        );
    }

    #[test]
    fn visual_bounds_use_the_field_display_rotation() {
        let definition =
            SymbolDefinition::from_kicad_symbol_sexpr(r#"(symbol "Test:IC")"#).unwrap();
        let mut symbol = test_symbol(Point::new(10.0, 20.0), Rotation::Deg90, None);
        symbol.fields.get_mut("Reference").unwrap().hidden = true;
        let value = symbol.fields.get_mut("Value").unwrap();
        value.value = "A long horizontal value".to_string();
        value.rotation_deg = 90.0;
        value.justify = Some(FieldJustify::new(
            Some(FieldHorizontalJustify::Left),
            Some(FieldVerticalJustify::Center),
        ));
        let value_at = value.at;

        let bounds = symbol_visual_bounds(&symbol, &definition).unwrap().unwrap();

        assert!(bounds.width() > bounds.height() * 5.0);
        assert!((bounds.min_x - value_at.x).abs() < GEOMETRY_EPS_MM);
        assert!(bounds.max_x > value_at.x);
    }

    #[test]
    fn upside_down_symbol_fields_keep_their_outward_justification() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Test:IC"
              (symbol "IC_1_1"
                (rectangle (start -5 -2) (end 5 2))))"#,
        )
        .unwrap();
        let mut symbol = test_symbol(Point::new(10.0, 20.0), Rotation::Deg180, None);

        assert!(autoplace_symbol_fields(&mut symbol, &definition).unwrap());

        for field in symbol.fields.values() {
            assert_eq!(
                field.justify,
                Some(FieldJustify::new(
                    Some(FieldHorizontalJustify::Right),
                    Some(FieldVerticalJustify::Center),
                ))
            );
            let bounds = field_text_bounds(&symbol, field, 2.0, 1.0);
            assert!((bounds.min_x - field.at.x).abs() < GEOMETRY_EPS_MM);
            assert!(bounds.max_x > field.at.x);
        }
    }

    fn test_symbol(at: Point, rotation: Rotation, mirror: Option<MirrorAxis>) -> Symbol {
        Symbol {
            id: "symbol".to_string(),
            lib_id: "Test:IC".to_string(),
            unit: 1,
            body_style: 1,
            at,
            rotation,
            mirror,
            fields_autoplaced: true,
            fields: BTreeMap::from([
                (
                    "Reference".to_string(),
                    SymbolField::new("Reference", "U1", at),
                ),
                ("Value".to_string(), SymbolField::new("Value", "IC", at)),
            ]),
            pins: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}
