use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use pcb_sexpr::{
    Sexpr, SexprKind,
    formatter::{FormatMode, format_tree},
    parse,
};

use crate::model::{
    FieldHorizontalJustify, FieldJustify, FieldVerticalJustify, Junction, Label, LabelKind,
    LabelShape, LabelSpin, MirrorAxis, NoConnect, Paper, PinInstance, Point, Rotation, SchDocument,
    SchItem, SchPage, Sheet, SheetPin, Symbol, SymbolDefinition, SymbolField, SymbolLibrary,
    TextEffects, TextSize, Wire,
};

pub const KICAD_SCH_VERSION: i64 = 20260306;
pub const GENERATOR: &str = "diode";

fn format_sexpr(value: &Sexpr, _indent: usize) -> String {
    format_tree(value, FormatMode::Normal)
        .trim_end()
        .to_string()
}

#[derive(Debug, Clone, Copy)]
pub struct KicadSchSource<'a> {
    pub file_name: Option<&'a str>,
    pub content: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KicadSchFile {
    pub file_name: Option<String>,
    pub content: String,
}

impl SchDocument {
    pub fn from_kicad_sch(content: &str) -> Result<Self> {
        let (page, library) = parse_kicad_sch_page(None, content)?;
        Ok(Self {
            pages: vec![page],
            library,
        })
    }

    pub fn from_kicad_sch_files<'a>(
        files: impl IntoIterator<Item = KicadSchSource<'a>>,
    ) -> Result<Self> {
        let mut document = Self::default();

        for file in files {
            let (page, library) = parse_kicad_sch_page(file.file_name, file.content)?;
            document.pages.push(page);
            document.library.merge(library);
        }

        Ok(document)
    }

    pub fn to_kicad_sch(&self) -> Result<String> {
        let [page] = self.pages.as_slice() else {
            bail!(
                "to_kicad_sch requires exactly one page, found {}",
                self.pages.len()
            );
        };

        Ok(format_kicad_sch_page(page, &self.library))
    }

    pub fn to_kicad_sch_files(&self) -> Vec<KicadSchFile> {
        self.pages
            .iter()
            .map(|page| KicadSchFile {
                file_name: page.file_name.clone(),
                content: format_kicad_sch_page(page, &self.library),
            })
            .collect()
    }
}

impl SymbolDefinition {
    /// Parse a KiCad library symbol definition from either a raw `(symbol ...)`
    /// S-expression or a wrapper containing one.
    pub fn from_kicad_symbol_sexpr(content: &str) -> Result<Self> {
        let root = parse(content).map_err(|err| anyhow!("failed to parse KiCad symbol: {err}"))?;
        symbol_definition_from_sexpr(&root)
            .ok_or_else(|| anyhow!("expected KiCad symbol definition"))
    }

    pub fn to_kicad_symbol_library_sexpr(&self) -> String {
        format!("(kicad_symbol_lib {})", format_sexpr(&self.sexpr, 0).trim())
    }
}

pub fn parse_kicad_sch_page(
    file_name: Option<&str>,
    content: &str,
) -> Result<(SchPage, SymbolLibrary)> {
    let root = parse(content).map_err(|err| anyhow!("failed to parse KiCad schematic: {err}"))?;
    parse_kicad_sch_root(file_name, &root)
}

fn parse_kicad_sch_root(file_name: Option<&str>, root: &Sexpr) -> Result<(SchPage, SymbolLibrary)> {
    let root =
        SexprList::from_sexpr(root).ok_or_else(|| anyhow!("expected kicad_sch root list"))?;

    if root.tag() != Some("kicad_sch") {
        bail!("expected kicad_sch root");
    }

    let mut page_id = None;
    let mut paper = Paper::default();
    let mut page_number = "1".to_string();
    let mut items = Vec::new();
    let mut library = SymbolLibrary::default();

    for child in root.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            items.push(SchItem::Unsupported(child.clone()));
            continue;
        };

        match list.tag() {
            Some("version") | Some("generator") => {}
            Some("uuid") => {
                page_id = list.string(1);
            }
            Some("paper") => {
                paper = parse_paper(list);
            }
            Some("lib_symbols") => {
                library = parse_lib_symbols(list);
            }
            Some("symbol") => {
                if let Some(symbol) = parse_symbol(list)? {
                    items.push(SchItem::Symbol(symbol));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("wire") => {
                if let Some(wire) = parse_wire(list)? {
                    items.push(SchItem::Wire(wire));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("junction") => {
                if let Some(junction) = parse_junction(list)? {
                    items.push(SchItem::Junction(junction));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("no_connect") => {
                if let Some(no_connect) = parse_no_connect(list)? {
                    items.push(SchItem::NoConnect(no_connect));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("label") | Some("global_label") | Some("hierarchical_label") => {
                if let Some(label) = parse_label(list)? {
                    items.push(SchItem::Label(label));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("sheet") => {
                if let Some(sheet) = parse_sheet(list)? {
                    items.push(SchItem::Sheet(sheet));
                } else {
                    items.push(SchItem::Unsupported(child.clone()));
                }
            }
            Some("sheet_instances") => {
                if let Some(parsed) = parse_root_page_number(list) {
                    page_number = parsed;
                }
                items.push(SchItem::Unsupported(child.clone()));
            }
            _ => items.push(SchItem::Unsupported(child.clone())),
        }
    }

    let page_id = page_id.ok_or_else(|| anyhow!("kicad_sch missing uuid"))?;
    Ok((
        SchPage {
            id: page_id,
            file_name: file_name.map(str::to_string),
            paper,
            page_number,
            items,
        },
        library,
    ))
}

fn format_kicad_sch_page(page: &SchPage, library: &SymbolLibrary) -> String {
    let mut root = vec![
        Sexpr::symbol("kicad_sch"),
        Sexpr::list(vec![
            Sexpr::symbol("version"),
            Sexpr::int(KICAD_SCH_VERSION),
        ]),
        Sexpr::list(vec![Sexpr::symbol("generator"), Sexpr::string(GENERATOR)]),
        Sexpr::list(vec![Sexpr::symbol("uuid"), Sexpr::string(&page.id)]),
        paper_to_sexpr(&page.paper),
        library_to_sexpr(library),
    ];

    root.extend(page.items.iter().map(|item| item_to_sexpr(item, page)));

    format!("{}\n", format_sexpr(&Sexpr::list(root), 0))
}

fn parse_lib_symbols(items: SexprList<'_>) -> SymbolLibrary {
    let mut definitions = BTreeMap::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            continue;
        };

        if list.tag() != Some("symbol") {
            continue;
        }

        if let Some(definition) = symbol_definition_from_symbol_items(child, list) {
            definitions.insert(definition.lib_id.clone(), definition);
        }
    }

    SymbolLibrary { definitions }
}

fn symbol_definition_from_sexpr(root: &Sexpr) -> Option<SymbolDefinition> {
    let items = SexprList::from_sexpr(root)?;
    match items.tag() {
        Some("symbol") => symbol_definition_from_symbol_items(root, items),
        Some("lib_symbols") | Some("kicad_symbol_lib") => {
            items.children_from(1).find_map(|child| {
                let child_items = SexprList::from_sexpr(child)?;
                if child_items.tag() == Some("symbol") {
                    symbol_definition_from_symbol_items(child, child_items)
                } else {
                    None
                }
            })
        }
        _ => None,
    }
}

fn symbol_definition_from_symbol_items(
    root: &Sexpr,
    items: SexprList<'_>,
) -> Option<SymbolDefinition> {
    let lib_id = items.string(1)?;
    let mut sexpr = root.clone();
    normalize_internal_metadata_properties(&mut sexpr);

    Some(SymbolDefinition { lib_id, sexpr })
}

fn parse_symbol(items: SexprList<'_>) -> Result<Option<Symbol>> {
    let mut id = None;
    let mut lib_id = None;
    let mut unit = 1;
    let mut body_style = 1;
    let mut at = Point::default();
    let mut rotation = Rotation::default();
    let mut mirror = None;
    let mut fields_autoplaced = false;
    let mut fields = BTreeMap::new();
    let mut pins = Vec::new();
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };

        match list.tag() {
            Some("lib_id") => {
                lib_id = list.string(1);
            }
            Some("at") => {
                if let Some((point, rot)) = parse_at(list)? {
                    at = point;
                    rotation = rot;
                }
            }
            Some("unit") => {
                if let Some(value) = list.i64(1) {
                    unit = value.max(1) as u32;
                }
            }
            Some("body_style") => {
                if let Some(value) = list.i64(1) {
                    body_style = value.max(1) as u32;
                }
            }
            Some("mirror") => {
                mirror = list.get(1).and_then(parse_mirror_axis);
            }
            Some("fields_autoplaced") => {
                fields_autoplaced = list.bool_or(1, true);
            }
            Some("uuid") => {
                id = list.string(1);
            }
            Some("property") => {
                if let Some(field) = parse_property(list) {
                    fields.insert(field.name.clone(), field);
                }
            }
            Some("pin") => {
                if let Some(pin) = parse_pin(list)? {
                    pins.push(pin);
                }
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let Some(lib_id) = lib_id else {
        return Ok(None);
    };

    Ok(Some(Symbol {
        id: id.ok_or_else(|| anyhow!("symbol {lib_id} missing uuid"))?,
        lib_id,
        unit,
        body_style,
        at,
        rotation,
        mirror,
        fields_autoplaced,
        fields,
        pins,
        unsupported,
    }))
}

fn parse_wire(items: SexprList<'_>) -> Result<Option<Wire>> {
    let mut id = None;
    let mut points = None;
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };

        match list.tag() {
            Some("pts") => {
                points = parse_wire_points(list)?;
            }
            Some("uuid") => {
                id = list.string(1);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let Some((a, b)) = points else {
        return Ok(None);
    };

    Ok(Some(Wire {
        id: id.ok_or_else(|| anyhow!("wire missing uuid"))?,
        a,
        b,
        unsupported,
    }))
}

fn parse_junction(items: SexprList<'_>) -> Result<Option<Junction>> {
    let mut id = None;
    let mut at = None;
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };

        match list.tag() {
            Some("at") => {
                at = parse_at(list)?.map(|(point, _)| point);
            }
            Some("uuid") => {
                id = list.string(1);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let Some(at) = at else {
        return Ok(None);
    };

    Ok(Some(Junction {
        id: id.ok_or_else(|| anyhow!("junction missing uuid"))?,
        at,
        unsupported,
    }))
}

fn parse_no_connect(items: SexprList<'_>) -> Result<Option<NoConnect>> {
    let mut id = None;
    let mut at = None;
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("at") => at = parse_at(list)?.map(|(point, _)| point),
            Some("uuid") => id = list.string(1),
            _ => unsupported.push(child.clone()),
        }
    }

    let Some(at) = at else {
        return Ok(None);
    };
    Ok(Some(NoConnect {
        id: id.ok_or_else(|| anyhow!("no_connect missing uuid"))?,
        at,
        unsupported,
    }))
}

fn parse_sheet(items: SexprList<'_>) -> Result<Option<Sheet>> {
    let mut id = None;
    let mut file_name = None;
    let mut pins = Vec::new();
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("uuid") => id = list.string(1),
            Some("property") if list.string(1).as_deref() == Some("Sheetfile") => {
                file_name = list.string(2);
                unsupported.push(child.clone());
            }
            Some("pin") => {
                if let Some(pin) = parse_sheet_pin(list)? {
                    pins.push(pin);
                } else {
                    unsupported.push(child.clone());
                }
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let (Some(id), Some(file_name)) = (id, file_name) else {
        return Ok(None);
    };
    Ok(Some(Sheet {
        id,
        file_name,
        pins,
        unsupported,
    }))
}

fn parse_sheet_pin(items: SexprList<'_>) -> Result<Option<SheetPin>> {
    let (Some(name), Some(shape)) = (items.string(1), items.get(2).and_then(parse_label_shape))
    else {
        return Ok(None);
    };
    let mut id = None;
    let mut at = None;
    let mut unsupported = Vec::new();
    for child in items.children_from(3) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("at") => at = parse_at(list)?.map(|(point, _)| point),
            Some("uuid") => id = list.string(1),
            _ => unsupported.push(child.clone()),
        }
    }
    let Some(at) = at else {
        return Ok(None);
    };
    Ok(Some(SheetPin {
        id: id.ok_or_else(|| anyhow!("sheet pin {name} missing uuid"))?,
        name,
        at,
        shape,
        unsupported,
    }))
}

fn parse_label(items: SexprList<'_>) -> Result<Option<Label>> {
    let Some(tag) = items.tag() else {
        return Ok(None);
    };
    let Some(text) = items.string(1) else {
        return Ok(None);
    };

    let mut id = None;
    let mut at = Point::default();
    let mut spin = LabelSpin::Right;
    let mut effects = TextEffects::default();
    let mut parsed_justify = None;
    let mut shape = default_label_shape_for_tag(tag);
    let mut fields_autoplaced = false;
    let mut fields = BTreeMap::new();
    let mut unsupported = Vec::new();

    for child in items.children_from(2) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };

        match list.tag() {
            Some("at") => {
                if let Some((point, parsed_spin)) = parse_label_at(list)? {
                    at = point;
                    spin = parsed_spin;
                }
            }
            Some("shape") => {
                if let Some(parsed) = list.get(1).and_then(parse_label_shape) {
                    shape = parsed;
                }
            }
            Some("fields_autoplaced") => {
                fields_autoplaced = list.bool_or(1, true);
            }
            Some("effects") => {
                let parsed = parse_effects(list);
                effects = parsed.effects;
                parsed_justify = merge_field_justify(parsed_justify, parsed.justify);
                unsupported.push(child.clone());
            }
            Some("uuid") => {
                id = list.string(1);
            }
            Some("property") => {
                if let Some(field) = parse_property(list) {
                    fields.insert(field.name.clone(), field);
                }
            }
            _ => unsupported.push(child.clone()),
        }
    }

    if let Some(justify) = parsed_justify {
        spin = label_spin_from_justify(spin, justify);
    }

    let kind = match tag {
        "label" => LabelKind::Local,
        "global_label" => LabelKind::Global { shape },
        "hierarchical_label" => LabelKind::Hierarchical { shape },
        _ => return Ok(None),
    };

    Ok(Some(Label {
        id: id.ok_or_else(|| anyhow!("{tag} {text} missing uuid"))?,
        text,
        at,
        kind,
        spin,
        effects,
        fields_autoplaced,
        fields,
        unsupported,
    }))
}

fn parse_label_at(items: SexprList<'_>) -> Result<Option<(Point, LabelSpin)>> {
    let (Some(x), Some(y)) = (items.f64(1), items.f64(2)) else {
        return Ok(None);
    };

    let degrees = items.f64(3).unwrap_or(0.0);
    let spin = kicad_angle_to_label_spin(degrees)
        .ok_or_else(|| anyhow!("unsupported label rotation {degrees}; expected a finite angle"))?;
    Ok(Some((Point::new(x, y), spin)))
}

fn parse_at(items: SexprList<'_>) -> Result<Option<(Point, Rotation)>> {
    let (Some(x), Some(y)) = (items.f64(1), items.f64(2)) else {
        return Ok(None);
    };

    let degrees = items.i64(3).unwrap_or(0);
    let rotation = Rotation::from_degrees(degrees).ok_or_else(|| {
        anyhow!("unsupported symbol rotation {degrees}; expected 0, 90, 180, or 270")
    })?;
    Ok(Some((Point::new(x, y), rotation)))
}

fn parse_wire_points(items: SexprList<'_>) -> Result<Option<(Point, Point)>> {
    let mut points = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            continue;
        };

        if list.tag() == Some("xy") {
            let Some(point) = parse_xy(list) else {
                continue;
            };
            points.push(point);
        }
    }

    match points.as_slice() {
        [a, b] => Ok(Some((*a, *b))),
        [] => Ok(None),
        _ => bail!("wire must contain exactly two xy points"),
    }
}

fn parse_xy(items: SexprList<'_>) -> Option<Point> {
    Some(Point::new(items.f64(1)?, items.f64(2)?))
}

fn parse_property(items: SexprList<'_>) -> Option<SymbolField> {
    let mut cursor = 1;
    if items.atom(cursor) == Some("private") {
        cursor += 1;
    }

    let name = items.string(cursor)?;
    let value = items.string(cursor + 1)?;
    let mut field = SymbolField::new(name, value, Point::default());

    for child in items.children_from(cursor + 2) {
        if child.as_atom() == Some("hide") {
            field.hidden = true;
            continue;
        }

        let Some(list) = SexprList::from_sexpr(child) else {
            field.unsupported.push(child.clone());
            continue;
        };

        match list.tag() {
            Some("at") => {
                if let Some((point, rotation_deg)) = parse_field_at(list) {
                    field.at = point;
                    field.rotation_deg = rotation_deg;
                }
            }
            Some("hide") => {
                field.hidden = list.bool_or(1, true);
            }
            Some("do_not_autoplace") => {
                field.do_not_autoplace = list.bool_or(1, true);
            }
            Some("effects") => {
                let parsed = parse_effects(list);
                field.justify = merge_field_justify(field.justify, parsed.justify);
                field.hidden |= parsed.hidden;
                field.unsupported.push(child.clone());
            }
            _ => field.unsupported.push(child.clone()),
        }
    }

    if is_internal_kicad_metadata_property(&field.name) {
        field.hidden = true;
    }

    Some(field)
}

fn parse_field_at(items: SexprList<'_>) -> Option<(Point, f64)> {
    Some((
        Point::new(items.f64(1)?, items.f64(2)?),
        items.f64(3).unwrap_or(0.0).rem_euclid(360.0),
    ))
}

#[derive(Debug, Clone, PartialEq)]
struct ParsedTextEffects {
    effects: TextEffects,
    justify: Option<FieldJustify>,
    hidden: bool,
}

fn parse_effects(items: SexprList<'_>) -> ParsedTextEffects {
    let mut effects = TextEffects::default();
    let mut justify = None;
    let mut hidden = false;

    for item in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(item) else {
            hidden |= item.as_atom() == Some("hide");
            continue;
        };

        match list.tag() {
            Some("font") => {
                effects = parse_font_effects(list, effects);
            }
            Some("justify") => {
                justify = merge_field_justify(justify, parse_field_justify(list));
            }
            Some("hide") => {
                hidden |= list.bool_or(1, true);
            }
            _ => {}
        }
    }

    ParsedTextEffects {
        effects,
        justify,
        hidden,
    }
}

fn parse_font_effects(items: SexprList<'_>, mut effects: TextEffects) -> TextEffects {
    for item in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(item) else {
            continue;
        };

        match list.tag() {
            Some("size") => {
                if let (Some(x), Some(y)) = (list.f64(1), list.f64(2)) {
                    effects.font_size = TextSize::new(x, y);
                }
            }
            Some("thickness") => {
                effects.thickness = list.f64(1);
            }
            Some("bold") => {
                effects.bold = list.bool_or(1, true);
            }
            Some("italic") => {
                effects.italic = list.bool_or(1, true);
            }
            _ => {}
        }
    }

    effects
}

fn parse_field_justify(items: SexprList<'_>) -> Option<FieldJustify> {
    let mut justify = FieldJustify::default();

    for value in items.children_from(1) {
        match value.as_atom() {
            Some("left") => justify.horizontal = Some(FieldHorizontalJustify::Left),
            Some("right") => justify.horizontal = Some(FieldHorizontalJustify::Right),
            Some("top") => justify.vertical = Some(FieldVerticalJustify::Top),
            Some("bottom") => justify.vertical = Some(FieldVerticalJustify::Bottom),
            Some("center") => {
                justify.horizontal = Some(FieldHorizontalJustify::Center);
                justify.vertical = Some(FieldVerticalJustify::Center);
            }
            _ => {}
        }
    }

    (!justify.is_empty()).then_some(justify)
}

fn merge_field_justify(
    existing: Option<FieldJustify>,
    next: Option<FieldJustify>,
) -> Option<FieldJustify> {
    let mut merged = existing.unwrap_or_default();
    if let Some(next) = next {
        if next.horizontal.is_some() {
            merged.horizontal = next.horizontal;
        }
        if next.vertical.is_some() {
            merged.vertical = next.vertical;
        }
    }

    (!merged.is_empty()).then_some(merged)
}

fn parse_pin(items: SexprList<'_>) -> Result<Option<PinInstance>> {
    let Some(number) = items.string(1) else {
        return Ok(None);
    };

    let id = items
        .child("uuid")
        .and_then(|uuid| uuid.string(1))
        .ok_or_else(|| anyhow!("pin {number} missing uuid"))?;

    Ok(Some(PinInstance { number, id }))
}

fn parse_root_page_number(items: SexprList<'_>) -> Option<String> {
    for child in items.children_from(1) {
        let list = SexprList::from_sexpr(child)?;
        if list.tag() != Some("path") {
            continue;
        }
        if list.atom(1) != Some("/") {
            continue;
        }
        return list.child("page").and_then(|page| page.string(1));
    }

    None
}

fn item_to_sexpr(item: &SchItem, _page: &SchPage) -> Sexpr {
    match item {
        SchItem::Symbol(symbol) => symbol_to_sexpr(symbol),
        SchItem::Wire(wire) => wire_to_sexpr(wire),
        SchItem::Junction(junction) => junction_to_sexpr(junction),
        SchItem::NoConnect(no_connect) => no_connect_to_sexpr(no_connect),
        SchItem::Label(label) => label_to_sexpr(label),
        SchItem::Sheet(sheet) => sheet_to_sexpr(sheet),
        SchItem::Unsupported(sexpr) => sexpr.clone(),
    }
}

fn no_connect_to_sexpr(no_connect: &NoConnect) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("no_connect"),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(no_connect.at.x),
            Sexpr::float(no_connect.at.y),
        ]),
    ];
    items.extend(no_connect.unsupported.iter().cloned());
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&no_connect.id),
    ]));
    Sexpr::list(items)
}

fn sheet_to_sexpr(sheet: &Sheet) -> Sexpr {
    let mut items = vec![Sexpr::symbol("sheet")];
    items.extend(sheet.unsupported.iter().cloned());
    items.extend(sheet.pins.iter().map(sheet_pin_to_sexpr));
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&sheet.id),
    ]));
    Sexpr::list(items)
}

fn sheet_pin_to_sexpr(pin: &SheetPin) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("pin"),
        Sexpr::string(&pin.name),
        Sexpr::symbol(label_shape_token(pin.shape)),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(pin.at.x),
            Sexpr::float(pin.at.y),
            Sexpr::int(0),
        ]),
    ];
    items.extend(pin.unsupported.iter().cloned());
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&pin.id),
    ]));
    Sexpr::list(items)
}

fn symbol_to_sexpr(symbol: &Symbol) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("symbol"),
        Sexpr::list(vec![Sexpr::symbol("lib_id"), Sexpr::string(&symbol.lib_id)]),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(symbol.at.x),
            Sexpr::float(symbol.at.y),
            Sexpr::int(symbol.rotation.degrees()),
        ]),
        Sexpr::list(vec![Sexpr::symbol("unit"), Sexpr::int(symbol.unit as i64)]),
        Sexpr::list(vec![
            Sexpr::symbol("body_style"),
            Sexpr::int(symbol.body_style as i64),
        ]),
    ];

    if let Some(axis) = symbol.mirror {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("mirror"),
            Sexpr::symbol(match axis {
                MirrorAxis::X => "x",
                MirrorAxis::Y => "y",
            }),
        ]));
    }

    if symbol.fields_autoplaced {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("fields_autoplaced"),
            Sexpr::symbol("yes"),
        ]));
    }

    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&symbol.id),
    ]));

    items.extend(symbol.fields.values().map(field_to_sexpr));
    items.extend(symbol.pins.iter().map(pin_to_sexpr));
    items.extend(symbol.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn wire_to_sexpr(wire: &Wire) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("wire"),
        Sexpr::list(vec![
            Sexpr::symbol("pts"),
            xy_to_sexpr(wire.a),
            xy_to_sexpr(wire.b),
        ]),
    ];
    items.extend(wire.unsupported.iter().cloned());
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&wire.id),
    ]));
    Sexpr::list(items)
}

fn junction_to_sexpr(junction: &Junction) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("junction"),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(junction.at.x),
            Sexpr::float(junction.at.y),
        ]),
    ];
    items.extend(junction.unsupported.iter().cloned());
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&junction.id),
    ]));
    Sexpr::list(items)
}

fn label_to_sexpr(label: &Label) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol(label_kind_token(label.kind)),
        Sexpr::string(&label.text),
    ];

    if let Some(shape) = label_shape(label.kind) {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("shape"),
            Sexpr::symbol(label_shape_token(shape)),
        ]));
    }

    items.push(Sexpr::list(vec![
        Sexpr::symbol("at"),
        Sexpr::float(label.at.x),
        Sexpr::float(label.at.y),
        Sexpr::int(label_spin_to_kicad_angle(label.spin)),
    ]));

    if label.fields_autoplaced && !label.fields.is_empty() {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("fields_autoplaced"),
            Sexpr::symbol("yes"),
        ]));
    }

    if !has_tag(&label.unsupported, "effects") {
        items.push(text_effects_to_sexpr(
            label.effects,
            Some(label_spin_justify(label.kind, label.spin)),
        ));
    }
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&label.id),
    ]));
    items.extend(label.fields.values().map(field_to_sexpr));
    items.extend(label.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn field_to_sexpr(field: &SymbolField) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("property"),
        Sexpr::string(&field.name),
        Sexpr::string(&field.value),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(field.at.x),
            Sexpr::float(field.at.y),
            angle_to_sexpr(field.rotation_deg.rem_euclid(360.0)),
        ]),
    ];

    if field.hidden || is_internal_kicad_metadata_property(&field.name) {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("hide"),
            Sexpr::symbol("yes"),
        ]));
    }

    if field.do_not_autoplace {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("do_not_autoplace"),
            Sexpr::symbol("yes"),
        ]));
    }

    items.extend(field.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn pin_to_sexpr(pin: &PinInstance) -> Sexpr {
    Sexpr::list(vec![
        Sexpr::symbol("pin"),
        Sexpr::string(&pin.number),
        Sexpr::list(vec![Sexpr::symbol("uuid"), Sexpr::string(&pin.id)]),
    ])
}

fn library_to_sexpr(library: &SymbolLibrary) -> Sexpr {
    let mut items = vec![Sexpr::symbol("lib_symbols")];
    items.extend(
        library
            .definitions
            .values()
            .map(|definition| definition.sexpr.clone()),
    );
    Sexpr::list(items)
}

fn normalize_internal_metadata_properties(sexpr: &mut Sexpr) {
    let SexprKind::List(items) = &mut sexpr.kind else {
        return;
    };

    if list_tag(items) == Some("property")
        && property_name(items).is_some_and(is_internal_kicad_metadata_property)
    {
        ensure_property_hidden(items);
    }

    for item in items {
        normalize_internal_metadata_properties(item);
    }
}

fn property_name(items: &[Sexpr]) -> Option<&str> {
    let mut cursor = 1;
    if items.get(cursor).and_then(Sexpr::as_atom) == Some("private") {
        cursor += 1;
    }
    items.get(cursor).and_then(Sexpr::as_atom)
}

fn ensure_property_hidden(items: &mut Vec<Sexpr>) {
    if items.iter_mut().any(force_existing_hide_marker) {
        return;
    }

    let insert_at = items
        .iter()
        .position(|item| {
            item.as_list()
                .is_some_and(|list| list_tag(list) == Some("effects"))
        })
        .unwrap_or(items.len());
    items.insert(
        insert_at,
        Sexpr::list(vec![Sexpr::symbol("hide"), Sexpr::symbol("yes")]),
    );
}

fn force_existing_hide_marker(item: &mut Sexpr) -> bool {
    if item.as_atom() == Some("hide") {
        return true;
    }

    let SexprKind::List(items) = &mut item.kind else {
        return false;
    };
    match list_tag(items) {
        Some("hide") => {
            if items.len() == 1 {
                items.push(Sexpr::symbol("yes"));
            } else {
                items[1] = Sexpr::symbol("yes");
            }
            true
        }
        Some("effects") => items[1..].iter_mut().any(force_existing_hide_marker),
        _ => false,
    }
}

fn is_internal_kicad_metadata_property(name: &str) -> bool {
    matches!(name, "ki_keywords" | "ki_fp_filters")
}

fn paper_to_sexpr(paper: &Paper) -> Sexpr {
    match paper {
        Paper::Named { name, portrait } => {
            let mut items = vec![Sexpr::symbol("paper"), Sexpr::string(name)];
            if *portrait {
                items.push(Sexpr::symbol("portrait"));
            }
            Sexpr::list(items)
        }
        Paper::Custom {
            width_mm,
            height_mm,
        } => Sexpr::list(vec![
            Sexpr::symbol("paper"),
            Sexpr::string("User"),
            Sexpr::float(*width_mm),
            Sexpr::float(*height_mm),
        ]),
    }
}

fn parse_paper(items: SexprList<'_>) -> Paper {
    let Some(name) = items.string(1) else {
        return Paper::default();
    };

    if name == "User"
        && let (Some(width_mm), Some(height_mm)) = (items.f64(2), items.f64(3))
    {
        return Paper::Custom {
            width_mm,
            height_mm,
        };
    }

    Paper::Named {
        name,
        portrait: items
            .children_from(2)
            .any(|item| item.as_atom() == Some("portrait")),
    }
}

fn xy_to_sexpr(point: Point) -> Sexpr {
    Sexpr::list(vec![
        Sexpr::symbol("xy"),
        Sexpr::float(point.x),
        Sexpr::float(point.y),
    ])
}

fn angle_to_sexpr(degrees: f64) -> Sexpr {
    let rounded = degrees.round();
    if (degrees - rounded).abs() < 1e-9 {
        Sexpr::int(rounded as i64)
    } else {
        Sexpr::float(degrees)
    }
}

fn text_effects_to_sexpr(effects: TextEffects, justify: Option<FieldJustify>) -> Sexpr {
    let mut font_items = vec![
        Sexpr::symbol("font"),
        Sexpr::list(vec![
            Sexpr::symbol("size"),
            Sexpr::float(effects.font_size.x),
            Sexpr::float(effects.font_size.y),
        ]),
    ];

    if let Some(thickness) = effects.thickness {
        font_items.push(Sexpr::list(vec![
            Sexpr::symbol("thickness"),
            Sexpr::float(thickness),
        ]));
    }
    if effects.bold {
        font_items.push(Sexpr::list(vec![
            Sexpr::symbol("bold"),
            Sexpr::symbol("yes"),
        ]));
    }
    if effects.italic {
        font_items.push(Sexpr::list(vec![
            Sexpr::symbol("italic"),
            Sexpr::symbol("yes"),
        ]));
    }

    let mut items = vec![Sexpr::symbol("effects"), Sexpr::list(font_items)];
    if let Some(justify) = justify {
        let atoms = field_justify_atoms(justify);
        if !atoms.is_empty() {
            let mut justify_items = vec![Sexpr::symbol("justify")];
            justify_items.extend(atoms.into_iter().map(Sexpr::symbol));
            items.push(Sexpr::list(justify_items));
        }
    }

    Sexpr::list(items)
}

fn label_kind_token(kind: LabelKind) -> &'static str {
    match kind {
        LabelKind::Local => "label",
        LabelKind::Global { .. } => "global_label",
        LabelKind::Hierarchical { .. } => "hierarchical_label",
    }
}

fn default_label_shape_for_tag(tag: &str) -> LabelShape {
    match tag {
        "global_label" => LabelShape::Bidirectional,
        "hierarchical_label" => LabelShape::Input,
        _ => LabelShape::Input,
    }
}

fn parse_label_shape(value: &Sexpr) -> Option<LabelShape> {
    match value.as_atom()? {
        "input" => Some(LabelShape::Input),
        "output" => Some(LabelShape::Output),
        "bidirectional" => Some(LabelShape::Bidirectional),
        "tri_state" => Some(LabelShape::TriState),
        "passive" => Some(LabelShape::Passive),
        "dot" => Some(LabelShape::Dot),
        "round" => Some(LabelShape::Round),
        "diamond" => Some(LabelShape::Diamond),
        "rectangle" => Some(LabelShape::Rectangle),
        _ => None,
    }
}

fn label_shape_token(shape: LabelShape) -> &'static str {
    match shape {
        LabelShape::Input => "input",
        LabelShape::Output => "output",
        LabelShape::Bidirectional => "bidirectional",
        LabelShape::TriState => "tri_state",
        LabelShape::Passive => "passive",
        LabelShape::Dot => "dot",
        LabelShape::Round => "round",
        LabelShape::Diamond => "diamond",
        LabelShape::Rectangle => "rectangle",
    }
}

fn label_shape(kind: LabelKind) -> Option<LabelShape> {
    match kind {
        LabelKind::Local => None,
        LabelKind::Global { shape } | LabelKind::Hierarchical { shape } => Some(shape),
    }
}

fn kicad_angle_to_label_spin(degrees: f64) -> Option<LabelSpin> {
    if !degrees.is_finite() {
        return None;
    }

    let normalized = degrees.rem_euclid(360.0);

    // KiCad applies EDA_ANGLE::KeepUpright() before deriving label spin. The
    // later effects/justify block carries the LEFT/BOTTOM distinction.
    if normalized <= 45.0 || normalized >= 315.0 || (normalized > 135.0 && normalized <= 225.0) {
        Some(LabelSpin::Right)
    } else {
        Some(LabelSpin::Up)
    }
}

fn label_spin_to_kicad_angle(spin: LabelSpin) -> i64 {
    match spin {
        LabelSpin::Right => 0,
        LabelSpin::Up => 90,
        LabelSpin::Left => 180,
        LabelSpin::Bottom => 270,
    }
}

fn label_spin_from_justify(spin: LabelSpin, justify: FieldJustify) -> LabelSpin {
    match justify.horizontal {
        Some(FieldHorizontalJustify::Right) if spin.is_vertical() => LabelSpin::Bottom,
        Some(FieldHorizontalJustify::Right) => LabelSpin::Left,
        Some(FieldHorizontalJustify::Left) if spin.is_vertical() => LabelSpin::Up,
        Some(FieldHorizontalJustify::Left) => LabelSpin::Right,
        Some(FieldHorizontalJustify::Center) | None => spin,
    }
}

fn label_spin_justify(kind: LabelKind, spin: LabelSpin) -> FieldJustify {
    let vertical = match kind {
        LabelKind::Local => Some(FieldVerticalJustify::Bottom),
        LabelKind::Global { .. } | LabelKind::Hierarchical { .. } => None,
    };
    FieldJustify::new(Some(spin.horizontal_justify()), vertical)
}

fn field_justify_atoms(justify: FieldJustify) -> Vec<&'static str> {
    let mut atoms = Vec::new();

    match justify.horizontal {
        Some(FieldHorizontalJustify::Left) => atoms.push("left"),
        Some(FieldHorizontalJustify::Right) => atoms.push("right"),
        Some(FieldHorizontalJustify::Center) | None => {}
    }
    match justify.vertical {
        Some(FieldVerticalJustify::Top) => atoms.push("top"),
        Some(FieldVerticalJustify::Bottom) => atoms.push("bottom"),
        Some(FieldVerticalJustify::Center) | None => {}
    }

    atoms
}

/// Read-only view over a parsed S-expression list.
///
/// KiCad parsers use this instead of repeatedly combining `as_list`, indexed
/// access, and atom conversion. The underlying expressions remain available so
/// unknown children can still be retained verbatim.
#[derive(Clone, Copy)]
struct SexprList<'a> {
    items: &'a [Sexpr],
}

impl<'a> SexprList<'a> {
    fn from_sexpr(value: &'a Sexpr) -> Option<Self> {
        Some(Self {
            items: value.as_list()?,
        })
    }

    fn tag(self) -> Option<&'a str> {
        self.items.first().and_then(Sexpr::as_sym)
    }

    fn get(self, index: usize) -> Option<&'a Sexpr> {
        self.items.get(index)
    }

    fn atom(self, index: usize) -> Option<&'a str> {
        self.get(index)?.as_atom()
    }

    fn string(self, index: usize) -> Option<String> {
        self.atom(index).map(str::to_string)
    }

    fn i64(self, index: usize) -> Option<i64> {
        atom_i64(self.get(index)?)
    }

    fn f64(self, index: usize) -> Option<f64> {
        atom_f64(self.get(index)?)
    }

    fn bool_or(self, index: usize, default: bool) -> bool {
        self.get(index).and_then(atom_bool).unwrap_or(default)
    }

    fn children_from(self, index: usize) -> impl Iterator<Item = &'a Sexpr> {
        self.items.get(index..).unwrap_or_default().iter()
    }

    fn child(self, tag: &str) -> Option<Self> {
        self.children_from(1).find_map(|item| {
            let child = Self::from_sexpr(item)?;
            (child.tag() == Some(tag)).then_some(child)
        })
    }
}

#[cfg(test)]
fn find_child<'a>(items: &'a [Sexpr], tag: &str) -> Option<&'a [Sexpr]> {
    items.iter().find_map(|item| {
        let list = item.as_list()?;
        (list_tag(list) == Some(tag)).then_some(list)
    })
}

fn has_tag(items: &[Sexpr], tag: &str) -> bool {
    items.iter().any(|item| {
        item.as_list()
            .is_some_and(|items| list_tag(items) == Some(tag))
    })
}

fn list_tag(items: &[Sexpr]) -> Option<&str> {
    items.first().and_then(Sexpr::as_sym)
}

fn parse_mirror_axis(value: &Sexpr) -> Option<MirrorAxis> {
    match value.as_atom()? {
        "x" => Some(MirrorAxis::X),
        "y" => Some(MirrorAxis::Y),
        _ => None,
    }
}

fn atom_i64(value: &Sexpr) -> Option<i64> {
    match &value.kind {
        SexprKind::Int(value) => Some(*value),
        SexprKind::F64(value) => Some(*value as i64),
        SexprKind::Symbol(value) | SexprKind::String(value) => value.parse().ok(),
        SexprKind::List(_) => None,
    }
}

fn atom_bool(value: &Sexpr) -> Option<bool> {
    match value.as_atom()? {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

fn atom_f64(value: &Sexpr) -> Option<f64> {
    match &value.kind {
        SexprKind::F64(value) => Some(*value),
        SexprKind::Int(value) => Some(*value as f64),
        SexprKind::Symbol(value) | SexprKind::String(value) => value.parse().ok(),
        SexprKind::List(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KICAD_10_SYMBOL_FIXTURE: &str = include_str!("../test-data/kicad-10/shared1.kicad_sch");
    const KICAD_10_UNSUPPORTED_FIXTURE: &str =
        include_str!("../test-data/kicad-10/unsupported-items.kicad_sch");

    const SAMPLE: &str = r#"
(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (uuid "page-1")
  (paper "A4")
  (lib_symbols
    (symbol "Device:R"
      (property "Reference" "R")
      (symbol "R_1_1"
        (pin passive line (at 0 0 0) (length 2.54)
          (name "1")
          (number "1")
        )
      )
    )
  )
  (wire
    (pts (xy 10 20) (xy 30 20))
    (stroke (width 0) (type solid) (color 0 0 0 0))
    (uuid "wire-1")
  )
  (junction
    (at 20 20)
    (diameter 0)
    (color 0 0 0 0)
    (uuid "junction-1")
  )
  (symbol
    (lib_id "Device:R")
    (at 10 20 90)
    (unit 1)
    (body_style 1)
    (fields_autoplaced yes)
    (uuid "sym-1")
    (property "Reference" "R1" (at 10 17.46 0) (effects (font (size 1.27 1.27)) (justify left top)))
    (property "Value" "10k" (at 10 22.54 90) (hide yes) (do_not_autoplace))
    (property "Footprint" "Resistor_SMD:R_0402" (at 10 20 0) (hide yes))
    (pin "1" (uuid "pin-1"))
    (pin "2" (uuid "pin-2"))
  )
  (sheet_instances
    (path "/" (page "2"))
  )
)
"#;

    #[test]
    fn parses_supported_items() {
        let document = SchDocument::from_kicad_sch(SAMPLE).expect("parse schematic");

        assert_eq!(document.pages.len(), 1);
        assert_eq!(document.pages[0].id, "page-1");
        assert_eq!(document.pages[0].page_number, "2");
        assert_eq!(document.library.definitions.len(), 1);
        assert_eq!(document.pages[0].items.len(), 4);

        let SchItem::Symbol(symbol) = &document.pages[0].items[2] else {
            panic!("expected symbol");
        };
        assert_eq!(symbol.lib_id, "Device:R");
        assert_eq!(symbol.at, Point::new(10.0, 20.0));
        assert_eq!(symbol.rotation, Rotation::Deg90);
        assert!(symbol.fields_autoplaced);
        assert_eq!(symbol.field_value("Reference").unwrap(), "R1");
        assert_eq!(
            symbol.field("Reference").unwrap().at,
            Point::new(10.0, 17.46)
        );
        assert_eq!(
            symbol.field("Reference").unwrap().justify,
            Some(FieldJustify::new(
                Some(FieldHorizontalJustify::Left),
                Some(FieldVerticalJustify::Top),
            ))
        );
        assert_eq!(symbol.field("Value").unwrap().rotation_deg, 90.0);
        assert!(symbol.field("Value").unwrap().hidden);
        assert!(symbol.field("Value").unwrap().do_not_autoplace);
        assert_eq!(
            symbol.field_value("Footprint").unwrap(),
            "Resistor_SMD:R_0402"
        );
        assert!(symbol.field("Footprint").unwrap().hidden);
        assert_eq!(symbol.pins.len(), 2);

        let SchItem::Wire(wire) = &document.pages[0].items[0] else {
            panic!("expected wire");
        };
        assert_eq!(wire.a, Point::new(10.0, 20.0));
        assert_eq!(wire.b, Point::new(30.0, 20.0));

        let SchItem::Junction(junction) = &document.pages[0].items[1] else {
            panic!("expected junction");
        };
        assert_eq!(junction.id, "junction-1");
        assert_eq!(junction.at, Point::new(20.0, 20.0));
    }

    #[test]
    fn parses_and_formats_label_items() {
        let content = SAMPLE.replace(
            r#"  (sheet_instances"#,
            r#"  (label "SDA"
    (at 40 20 180)
    (effects (font (size 1.5 1.5) (thickness 0.15) (bold yes) (italic yes)) (justify right bottom))
    (uuid "label-local")
  )
  (global_label "RESET"
    (shape input)
    (at 50 20 90)
    (effects (font (size 1.27 1.27)) (justify left))
    (uuid "label-global")
  )
  (hierarchical_label "BUS"
    (shape output)
    (at 60 20 270)
    (effects (font (size 1.27 1.27)) (justify right))
    (uuid "label-hier")
  )
  (label "RAW_ANGLE"
    (at 70 20 181.5)
    (effects (font (size 1.27 1.27)))
    (uuid "label-raw-angle")
  )
  (sheet_instances"#,
        );
        let document = SchDocument::from_kicad_sch(&content).expect("parse schematic");

        let SchItem::Label(local) = &document.pages[0].items[3] else {
            panic!("expected local label");
        };
        assert_eq!(local.kind, LabelKind::Local);
        assert_eq!(local.text, "SDA");
        assert_eq!(local.at, Point::new(40.0, 20.0));
        assert_eq!(local.spin, LabelSpin::Left);
        assert_eq!(local.effects.font_size, TextSize::new(1.5, 1.5));
        assert_eq!(local.effects.thickness, Some(0.15));
        assert!(local.effects.bold);
        assert!(local.effects.italic);

        let SchItem::Label(global) = &document.pages[0].items[4] else {
            panic!("expected global label");
        };
        assert_eq!(
            global.kind,
            LabelKind::Global {
                shape: LabelShape::Input,
            }
        );
        assert_eq!(global.spin, LabelSpin::Up);

        let SchItem::Label(hierarchical) = &document.pages[0].items[5] else {
            panic!("expected hierarchical label");
        };
        assert_eq!(
            hierarchical.kind,
            LabelKind::Hierarchical {
                shape: LabelShape::Output,
            }
        );
        assert_eq!(hierarchical.spin, LabelSpin::Bottom);

        let SchItem::Label(raw_angle) = &document.pages[0].items[6] else {
            panic!("expected raw-angle label");
        };
        assert_eq!(raw_angle.spin, LabelSpin::Right);

        let formatted = document.to_kicad_sch().expect("format schematic");
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse schematic");

        assert_eq!(reparsed.pages[0].items, document.pages[0].items);
    }

    #[test]
    fn parses_raw_symbol_definition() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Device:C" (property "Reference" "C"))"#,
        )
        .expect("parse symbol definition");

        assert_eq!(definition.lib_id, "Device:C");
        let reparsed = parse(&definition.to_kicad_symbol_library_sexpr()).expect("reparse wrapper");
        assert_eq!(
            reparsed
                .as_list()
                .and_then(|items| items.first())
                .and_then(Sexpr::as_sym),
            Some("kicad_symbol_lib")
        );
    }

    #[test]
    fn parses_wrapped_symbol_definition() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(kicad_symbol_lib (version 20240214) (symbol "Device:R"))"#,
        )
        .expect("parse wrapped symbol definition");

        assert_eq!(definition.lib_id, "Device:R");
    }

    #[test]
    fn hides_internal_metadata_in_symbol_definitions() {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Device:R"
              (property "Reference" "R" (at 0 2.54 0))
              (property "ki_keywords" "R res resistor" (at 0 0 0)
                (effects (font (size 1.27 1.27))))
              (property "ki_fp_filters" "R_*" (at 0 0 0)
                (effects (font (size 1.27 1.27))))
            )"#,
        )
        .expect("parse symbol definition");

        let symbol = definition.sexpr.as_list().expect("symbol sexpr");
        let keywords = find_property(symbol, "ki_keywords").expect("keywords property");
        let filters = find_property(symbol, "ki_fp_filters").expect("filters property");

        assert!(find_child(keywords, "hide").is_some());
        assert!(find_child(filters, "hide").is_some());
    }

    #[test]
    fn formats_and_reparses_single_page() {
        let document = SchDocument::from_kicad_sch(SAMPLE).expect("parse schematic");
        let formatted = document.to_kicad_sch().expect("format schematic");
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse schematic");

        assert_eq!(reparsed.pages.len(), 1);
        assert_eq!(reparsed.library.definitions.len(), 1);
        assert_eq!(reparsed.pages[0].items.len(), 4);
        assert_eq!(reparsed.pages[0].items, document.pages[0].items);
    }

    #[test]
    fn kicad_10_symbol_file_round_trips_semantically() {
        let document =
            SchDocument::from_kicad_sch(KICAD_10_SYMBOL_FIXTURE).expect("parse KiCad 10 fixture");
        let symbol = document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Symbol(symbol) => Some(symbol),
                _ => None,
            })
            .expect("placed symbol");

        assert!(has_tag(&symbol.unsupported, "in_pos_files"));
        assert!(has_tag(&symbol.unsupported, "dnp"));
        assert!(has_tag(&symbol.unsupported, "instances"));
        assert!(has_tag(
            &symbol.field("Reference").unwrap().unsupported,
            "show_name"
        ));

        let formatted = document.to_kicad_sch().expect("format KiCad 10 fixture");
        assert!(formatted.contains("(generator_version \"10.0\")"));
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse KiCad 10 fixture");
        assert_eq!(reparsed, document);
    }

    #[test]
    fn kicad_10_mixed_items_round_trip_semantically() {
        let document = SchDocument::from_kicad_sch(KICAD_10_UNSUPPORTED_FIXTURE)
            .expect("parse KiCad 10 fixture");
        let unsupported = document.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Unsupported(sexpr) => Some(sexpr),
                _ => None,
            })
            .collect::<Vec<_>>();
        let contains = |tag| {
            unsupported.iter().any(|sexpr| {
                sexpr
                    .as_list()
                    .is_some_and(|items| list_tag(items) == Some(tag))
            })
        };

        assert!(contains("text"));
        assert_eq!(
            document.pages[0]
                .items
                .iter()
                .filter(|item| matches!(item, SchItem::NoConnect(_)))
                .count(),
            2
        );
        assert!(document.pages[0].items.iter().any(
            |item| matches!(item, SchItem::Sheet(sheet) if sheet.file_name == "aSheet.kicad_sch" && sheet.pins.len() == 2)
        ));

        let formatted = document.to_kicad_sch().expect("format KiCad 10 fixture");
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse KiCad 10 fixture");
        assert_eq!(reparsed, document);
    }

    #[test]
    fn parses_bare_fields_autoplaced_token() {
        let content = SAMPLE.replace("(fields_autoplaced yes)", "(fields_autoplaced)");
        let document = SchDocument::from_kicad_sch(&content).expect("parse schematic");
        let SchItem::Symbol(symbol) = &document.pages[0].items[2] else {
            panic!("expected symbol");
        };

        assert!(symbol.fields_autoplaced);
    }

    #[test]
    fn hides_internal_metadata_in_placed_symbols() {
        let content = SAMPLE.replace(
            r#"    (pin "1" (uuid "pin-1"))"#,
            r#"    (property "ki_keywords" "R res resistor" (at 10 20 0)
      (effects (font (size 1.27 1.27))))
    (property "ki_fp_filters" "R_*" (at 10 20 0)
      (effects (font (size 1.27 1.27))))
    (pin "1" (uuid "pin-1"))"#,
        );
        let document = SchDocument::from_kicad_sch(&content).expect("parse schematic");

        let SchItem::Symbol(symbol) = &document.pages[0].items[2] else {
            panic!("expected symbol");
        };
        assert!(symbol.field("ki_keywords").unwrap().hidden);
        assert!(symbol.field("ki_fp_filters").unwrap().hidden);

        let formatted = document.to_kicad_sch().expect("format schematic");
        let root = parse(&formatted).expect("parse formatted schematic");
        let items = root.as_list().expect("formatted schematic root");
        let symbol = items
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|items| list_tag(items) == Some("symbol"))
            .expect("formatted symbol");
        assert!(find_child(find_property(symbol, "ki_keywords").unwrap(), "hide").is_some());
        assert!(find_child(find_property(symbol, "ki_fp_filters").unwrap(), "hide").is_some());
    }

    #[test]
    fn parses_and_formats_portrait_named_paper() {
        let content = SAMPLE.replace(r#"(paper "A4")"#, r#"(paper "A4" portrait)"#);
        let document = SchDocument::from_kicad_sch(&content).expect("parse schematic");

        assert_eq!(
            document.pages[0].paper,
            Paper::Named {
                name: "A4".to_string(),
                portrait: true,
            }
        );

        let formatted = document.to_kicad_sch().expect("format schematic");
        let formatted_root = parse(&formatted).expect("parse formatted schematic");
        let formatted_items = formatted_root.as_list().expect("formatted schematic root");
        let paper = find_child(formatted_items, "paper").expect("formatted paper");
        assert_eq!(paper.get(1).and_then(Sexpr::as_atom), Some("A4"));
        assert_eq!(paper.get(2).and_then(Sexpr::as_atom), Some("portrait"));
    }

    #[test]
    fn exports_document_coordinates_as_kicad_page_coordinates() {
        let document = SchDocument::from_kicad_sch(SAMPLE).expect("parse schematic");
        let formatted = document.to_kicad_sch().expect("format schematic");
        let root = parse(&formatted).expect("parse formatted schematic");
        let items = root.as_list().expect("formatted schematic root");

        let symbol = items
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|items| list_tag(items) == Some("symbol"))
            .expect("formatted symbol");
        let at = find_child(symbol, "at").expect("formatted symbol at");
        assert_eq!(at.get(1).and_then(atom_f64), Some(10.0));
        assert_eq!(at.get(2).and_then(atom_f64), Some(20.0));
        assert_eq!(at.get(3).and_then(atom_i64), Some(90));

        let wire = items
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|items| list_tag(items) == Some("wire"))
            .expect("formatted wire");
        let pts = find_child(wire, "pts").expect("formatted wire points");
        let xy = pts
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|items| list_tag(items) == Some("xy"))
            .expect("formatted wire point");
        assert_eq!(xy.get(1).and_then(atom_f64), Some(10.0));
        assert_eq!(xy.get(2).and_then(atom_f64), Some(20.0));
    }

    #[test]
    fn supports_multiple_page_files() {
        let document = SchDocument::from_kicad_sch_files([
            KicadSchSource {
                file_name: Some("a.kicad_sch"),
                content: SAMPLE,
            },
            KicadSchSource {
                file_name: Some("b.kicad_sch"),
                content: &SAMPLE.replace("page-1", "page-2"),
            },
        ])
        .expect("parse pages");

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].file_name.as_deref(), Some("a.kicad_sch"));
        assert_eq!(document.library.definitions.len(), 1);

        let files = document.to_kicad_sch_files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].file_name.as_deref(), Some("b.kicad_sch"));
    }

    fn find_property<'a>(items: &'a [Sexpr], name: &str) -> Option<&'a [Sexpr]> {
        items
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|items| list_tag(items) == Some("property") && property_name(items) == Some(name))
    }
}
