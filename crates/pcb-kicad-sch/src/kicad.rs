use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
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
pub const GENERATOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy)]
pub struct KicadSchSource<'a> {
    pub file_name: Option<&'a str>,
    pub content: &'a str,
    pub is_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KicadSchFile {
    pub file_name: Option<String>,
    pub content: String,
}

impl SchDocument {
    pub fn from_kicad_sch(content: &str) -> Result<Self> {
        let page = parse_kicad_sch_page(None, content)?;
        Ok(Self {
            root_page_ids: vec![page.id.clone()],
            pages: vec![page],
        })
    }

    pub fn from_kicad_sch_files<'a>(
        files: impl IntoIterator<Item = KicadSchSource<'a>>,
    ) -> Result<Self> {
        let mut document = Self::default();

        for file in files {
            let page = parse_kicad_sch_page(file.file_name, file.content)?;
            if file.is_root {
                document.root_page_ids.push(page.id.clone());
            }
            document.pages.push(page);
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

        Ok(format_kicad_sch_page(page))
    }

    pub fn to_kicad_sch_files(&self) -> Vec<KicadSchFile> {
        self.pages
            .iter()
            .map(|page| KicadSchFile {
                file_name: page.file_name.clone(),
                content: format_kicad_sch_page(page),
            })
            .collect()
    }
}

impl SymbolDefinition {
    /// Parse a KiCad library symbol definition from either a raw `(symbol ...)`
    /// S-expression or a wrapper containing one.
    pub fn from_kicad_symbol_sexpr(content: &str) -> Result<Self> {
        let root = parse(content).map_err(|err| anyhow!("failed to parse KiCad symbol: {err}"))?;
        let definition = symbol_definition_from_sexpr(&root)
            .ok_or_else(|| anyhow!("expected KiCad symbol definition"))?;
        validate_symbol_graphics_for_kicad_10(&definition.sexpr).with_context(|| {
            format!(
                "symbol '{}' is not compatible with KiCad 10",
                definition.lib_id
            )
        })?;
        Ok(definition)
    }

    pub fn to_kicad_symbol_library_sexpr(&self) -> String {
        format!(
            "(kicad_symbol_lib {})",
            format_tree(&self.sexpr, FormatMode::Normal).trim()
        )
    }

    pub(crate) fn default_fields(&self) -> Result<BTreeMap<String, SymbolField>> {
        let items = SexprList::from_sexpr(&self.sexpr)
            .with_context(|| format!("symbol '{}' definition is not a list", self.lib_id))?;
        let mut fields = BTreeMap::new();
        for child in items.children_from(2) {
            let Some(property) = SexprList::from_sexpr(child) else {
                continue;
            };
            if property.tag() != Some("property") {
                continue;
            }
            let field = parse_property(property)?;
            if fields.insert(field.name.clone(), field).is_some() {
                bail!("symbol '{}' has duplicate property names", self.lib_id);
            }
        }
        Ok(fields)
    }
}

pub fn parse_kicad_sch_page(file_name: Option<&str>, content: &str) -> Result<SchPage> {
    let root = parse(content).map_err(|err| anyhow!("failed to parse KiCad schematic: {err}"))?;
    parse_kicad_sch_root(file_name, &root)
}

fn parse_kicad_sch_root(file_name: Option<&str>, root: &Sexpr) -> Result<SchPage> {
    let root =
        SexprList::from_sexpr(root).ok_or_else(|| anyhow!("expected kicad_sch root list"))?;

    if root.tag() != Some("kicad_sch") {
        bail!("expected kicad_sch root");
    }

    let mut page_id = None;
    let mut version = None;
    let mut paper = Paper::default();
    let mut items = Vec::new();
    let mut library = SymbolLibrary::default();

    for child in root.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            items.push(SchItem::Unsupported(child.clone()));
            continue;
        };

        match list.tag() {
            Some("version") => version = list.i64(1),
            Some("generator") | Some("generator_version") => {}
            Some("uuid") => {
                page_id = list.string(1);
            }
            Some("paper") => {
                paper = parse_paper(list)?;
            }
            Some("lib_symbols") => {
                library = parse_lib_symbols(list)?;
            }
            Some("symbol") => {
                items.push(SchItem::Symbol(parse_symbol(list)?));
            }
            Some("wire") => {
                items.push(SchItem::Wire(parse_wire(list)?));
            }
            Some("junction") => {
                items.push(SchItem::Junction(parse_junction(list)?));
            }
            Some("no_connect") => {
                items.push(SchItem::NoConnect(parse_no_connect(list)?));
            }
            Some("label")
            | Some("global_label")
            | Some("hierarchical_label")
            | Some("netclass_flag")
            | Some("directive_label") => {
                items.push(SchItem::Label(parse_label(list)?));
            }
            Some("sheet") => {
                items.push(SchItem::Sheet(parse_sheet(list)?));
            }
            Some("sheet_instances") => {
                items.push(SchItem::Unsupported(child.clone()));
            }
            _ => items.push(SchItem::Unsupported(child.clone())),
        }
    }

    let version = version.ok_or_else(|| anyhow!("kicad_sch missing version"))?;
    if version != KICAD_SCH_VERSION {
        bail!(
            "unsupported KiCad schematic version {version}; expected KiCad 10 version {KICAD_SCH_VERSION}"
        );
    }
    let page_id = page_id.ok_or_else(|| anyhow!("kicad_sch missing uuid"))?;
    Ok(SchPage {
        id: page_id,
        file_name: file_name.map(str::to_string),
        library,
        paper,
        items,
    })
}

fn format_kicad_sch_page(page: &SchPage) -> String {
    let mut root = vec![
        Sexpr::symbol("kicad_sch"),
        Sexpr::list(vec![
            Sexpr::symbol("version"),
            Sexpr::int(KICAD_SCH_VERSION),
        ]),
        Sexpr::list(vec![Sexpr::symbol("generator"), Sexpr::string(GENERATOR)]),
        Sexpr::list(vec![
            Sexpr::symbol("generator_version"),
            Sexpr::string(GENERATOR_VERSION),
        ]),
        Sexpr::list(vec![Sexpr::symbol("uuid"), Sexpr::string(&page.id)]),
        paper_to_sexpr(&page.paper),
        library_to_sexpr(&page.library),
    ];

    root.extend(page.items.iter().map(|item| item_to_sexpr(item, page)));

    format!(
        "{}\n",
        format_tree(&Sexpr::list(root), FormatMode::Normal).trim_end()
    )
}

fn parse_lib_symbols(items: SexprList<'_>) -> Result<SymbolLibrary> {
    let mut definitions = BTreeMap::new();
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };

        if list.tag() != Some("symbol") {
            unsupported.push(child.clone());
            continue;
        }

        let definition = symbol_definition_from_symbol_items(child, list)
            .context("lib_symbols contains a symbol without a library id")?;
        if definitions
            .insert(definition.lib_id.clone(), definition)
            .is_some()
        {
            bail!("lib_symbols contains duplicate symbol definition");
        }
    }

    Ok(SymbolLibrary {
        definitions,
        unsupported,
    })
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

fn parse_symbol(items: SexprList<'_>) -> Result<Symbol> {
    let mut id = None;
    let mut lib_id = None;
    let mut unit = None;
    let mut body_style = 1;
    let mut at = None;
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
                at = Some(parse_at(list)?);
            }
            Some("unit") => {
                unit = Some(positive_u32(
                    "symbol unit",
                    list.i64(1).context("symbol unit is not an integer")?,
                )?);
            }
            Some("body_style") => {
                body_style = positive_u32(
                    "symbol body_style",
                    list.i64(1).context("symbol body_style is not an integer")?,
                )?;
            }
            Some("mirror") => {
                mirror = Some(
                    list.get(1)
                        .and_then(parse_mirror_axis)
                        .context("symbol mirror must be x or y")?,
                );
            }
            Some("fields_autoplaced") => {
                fields_autoplaced = list.bool_or(1, true, "fields_autoplaced")?;
            }
            Some("uuid") => {
                id = list.string(1);
            }
            Some("property") => {
                let field = parse_property(list)?;
                if fields.insert(field.name.clone(), field).is_some() {
                    bail!("symbol contains duplicate property name");
                }
            }
            Some("pin") => {
                pins.push(parse_pin(list)?);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let lib_id = lib_id.context("symbol missing lib_id")?;
    let (at, rotation) = at.with_context(|| format!("symbol {lib_id} missing at"))?;
    Ok(Symbol {
        id: id.ok_or_else(|| anyhow!("symbol {lib_id} missing uuid"))?,
        lib_id,
        unit: unit.context("symbol missing unit")?,
        body_style,
        at,
        rotation,
        mirror,
        fields_autoplaced,
        fields,
        pins,
        unsupported,
    })
}

fn positive_u32(field: &str, value: i64) -> Result<u32> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{field} must be a positive 32-bit integer, found {value}"))
}

fn parse_wire(items: SexprList<'_>) -> Result<Wire> {
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
                points = Some(parse_wire_points(list)?);
            }
            Some("uuid") => {
                id = list.string(1);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    let (a, b) = points.context("wire missing pts")?;
    Ok(Wire {
        id: id.ok_or_else(|| anyhow!("wire missing uuid"))?,
        a,
        b,
        unsupported,
    })
}

fn parse_junction(items: SexprList<'_>) -> Result<Junction> {
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
                at = Some(parse_at(list)?.0);
            }
            Some("uuid") => {
                id = list.string(1);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    Ok(Junction {
        id: id.ok_or_else(|| anyhow!("junction missing uuid"))?,
        at: at.context("junction missing at")?,
        unsupported,
    })
}

fn parse_no_connect(items: SexprList<'_>) -> Result<NoConnect> {
    let mut id = None;
    let mut at = None;
    let mut unsupported = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("at") => at = Some(parse_at(list)?.0),
            Some("uuid") => id = list.string(1),
            _ => unsupported.push(child.clone()),
        }
    }

    Ok(NoConnect {
        id: id.ok_or_else(|| anyhow!("no_connect missing uuid"))?,
        at: at.context("no_connect missing at")?,
        unsupported,
    })
}

fn parse_sheet(items: SexprList<'_>) -> Result<Sheet> {
    let mut id = None;
    let mut file = None;
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
                file = Some(parse_property(list)?);
            }
            Some("pin") => {
                pins.push(parse_sheet_pin(list)?);
            }
            _ => unsupported.push(child.clone()),
        }
    }

    Ok(Sheet {
        id: id.context("sheet missing uuid")?,
        file: file.context("sheet missing Sheetfile property")?,
        pins,
        unsupported,
    })
}

fn parse_sheet_pin(items: SexprList<'_>) -> Result<SheetPin> {
    let name = items.string(1).context("sheet pin missing name")?;
    let shape = items
        .get(2)
        .and_then(parse_label_shape)
        .with_context(|| format!("sheet pin {name} missing or invalid shape"))?;
    let mut id = None;
    let mut at = None;
    let mut unsupported = Vec::new();
    for child in items.children_from(3) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("at") => at = Some(parse_at(list)?),
            Some("uuid") => id = list.string(1),
            _ => unsupported.push(child.clone()),
        }
    }
    let (at, rotation) = at.with_context(|| format!("sheet pin {name} missing at"))?;
    Ok(SheetPin {
        id: id.ok_or_else(|| anyhow!("sheet pin {name} missing uuid"))?,
        name,
        at,
        rotation,
        shape,
        unsupported,
    })
}

fn parse_label(items: SexprList<'_>) -> Result<Label> {
    let tag = items.tag().context("label missing type")?;
    let text = items.string(1).context("label missing text")?;

    let mut id = None;
    let mut at = None;
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
                let (point, parsed_spin) = parse_label_at(list)?;
                at = Some(point);
                spin = parsed_spin;
            }
            Some("shape") => {
                shape = list
                    .get(1)
                    .and_then(parse_label_shape)
                    .with_context(|| format!("{tag} {text} has invalid shape"))?;
            }
            Some("fields_autoplaced") => {
                fields_autoplaced = list.bool_or(1, true, "fields_autoplaced")?;
            }
            Some("effects") => {
                let parsed = parse_effects(list)?;
                effects = parsed.effects;
                parsed_justify = merge_field_justify(parsed_justify, parsed.justify);
            }
            Some("uuid") => {
                id = list.string(1);
            }
            Some("property") => {
                let field = parse_property(list)?;
                if fields.insert(field.name.clone(), field).is_some() {
                    bail!("{tag} {text} contains duplicate property name");
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
        "netclass_flag" | "directive_label" => LabelKind::Directive { shape },
        _ => bail!("unsupported label type {tag}"),
    };
    let at = at.with_context(|| format!("{tag} {text} missing at"))?;

    Ok(Label {
        id: id.ok_or_else(|| anyhow!("{tag} {text} missing uuid"))?,
        text,
        at,
        kind,
        spin,
        effects,
        fields_autoplaced,
        fields,
        unsupported,
    })
}

fn parse_label_at(items: SexprList<'_>) -> Result<(Point, LabelSpin)> {
    let x = items.f64(1).context("label at missing or invalid x")?;
    let y = items.f64(2).context("label at missing or invalid y")?;

    let degrees = items.f64(3).unwrap_or(0.0);
    let spin = kicad_angle_to_label_spin(degrees)
        .ok_or_else(|| anyhow!("unsupported label rotation {degrees}; expected a finite angle"))?;
    Ok((Point::new(x, y), spin))
}

fn parse_at(items: SexprList<'_>) -> Result<(Point, Rotation)> {
    let x = items.f64(1).context("at missing or invalid x")?;
    let y = items.f64(2).context("at missing or invalid y")?;

    let degrees = items.i64(3).unwrap_or(0);
    let rotation = Rotation::from_degrees(degrees).ok_or_else(|| {
        anyhow!("unsupported symbol rotation {degrees}; expected 0, 90, 180, or 270")
    })?;
    Ok((Point::new(x, y), rotation))
}

fn parse_wire_points(items: SexprList<'_>) -> Result<(Point, Point)> {
    let mut points = Vec::new();

    for child in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(child) else {
            continue;
        };

        if list.tag() == Some("xy") {
            points.push(parse_xy(list)?);
        }
    }

    match points.as_slice() {
        [a, b] => Ok((*a, *b)),
        _ => bail!("wire must contain exactly two xy points"),
    }
}

fn parse_xy(items: SexprList<'_>) -> Result<Point> {
    Ok(Point::new(
        items.f64(1).context("xy missing or invalid x")?,
        items.f64(2).context("xy missing or invalid y")?,
    ))
}

fn parse_property(items: SexprList<'_>) -> Result<SymbolField> {
    let mut cursor = 1;
    let private = items.atom(cursor) == Some("private");
    if private {
        cursor += 1;
    }

    let name = items.string(cursor).context("property missing name")?;
    let value = items
        .string(cursor + 1)
        .with_context(|| format!("property {name} missing value"))?;
    let mut field = SymbolField::new(name, value, Point::default());
    field.private = private;
    let mut at = None;

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
                at = Some(parse_field_at(list)?);
            }
            Some("hide") => {
                field.hidden = list.bool_or(1, true, "property hide")?;
            }
            Some("do_not_autoplace") => {
                field.do_not_autoplace = list.bool_or(1, true, "property do_not_autoplace")?;
            }
            Some("effects") => {
                let parsed = parse_effects(list)?;
                field.effects = parsed.effects;
                field.justify = merge_field_justify(field.justify, parsed.justify);
                field.hidden |= parsed.hidden;
            }
            _ => field.unsupported.push(child.clone()),
        }
    }

    if is_internal_kicad_metadata_property(&field.name) {
        field.hidden = true;
    }
    let (point, rotation_deg) =
        at.with_context(|| format!("property {} missing at", field.name))?;
    field.at = point;
    field.rotation_deg = rotation_deg;

    Ok(field)
}

fn parse_field_at(items: SexprList<'_>) -> Result<(Point, f64)> {
    Ok((
        Point::new(
            items.f64(1).context("property at missing or invalid x")?,
            items.f64(2).context("property at missing or invalid y")?,
        ),
        items.f64(3).unwrap_or(0.0).rem_euclid(360.0),
    ))
}

#[derive(Debug, Clone, PartialEq)]
struct ParsedTextEffects {
    effects: TextEffects,
    justify: Option<FieldJustify>,
    hidden: bool,
}

fn parse_effects(items: SexprList<'_>) -> Result<ParsedTextEffects> {
    let mut effects = TextEffects::default();
    let mut justify = None;
    let mut hidden = false;

    for item in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(item) else {
            if item.as_atom() == Some("hide") {
                hidden = true;
            } else {
                effects.unsupported.push(item.clone());
            }
            continue;
        };

        match list.tag() {
            Some("font") => {
                effects = parse_font_effects(list, effects)?;
            }
            Some("justify") => {
                justify = merge_field_justify(justify, parse_field_justify(list)?);
            }
            Some("hide") => {
                hidden |= list.bool_or(1, true, "effects hide")?;
            }
            _ => effects.unsupported.push(item.clone()),
        }
    }

    Ok(ParsedTextEffects {
        effects,
        justify,
        hidden,
    })
}

fn parse_font_effects(items: SexprList<'_>, mut effects: TextEffects) -> Result<TextEffects> {
    for item in items.children_from(1) {
        let Some(list) = SexprList::from_sexpr(item) else {
            effects.font_unsupported.push(item.clone());
            continue;
        };

        match list.tag() {
            Some("size") => {
                effects.font_size = TextSize::new(
                    list.f64(1).context("font size missing or invalid x")?,
                    list.f64(2).context("font size missing or invalid y")?,
                );
            }
            Some("thickness") => {
                effects.thickness = Some(
                    list.f64(1)
                        .context("font thickness missing or invalid value")?,
                );
            }
            Some("bold") => {
                effects.bold = list.bool_or(1, true, "font bold")?;
            }
            Some("italic") => {
                effects.italic = list.bool_or(1, true, "font italic")?;
            }
            _ => effects.font_unsupported.push(item.clone()),
        }
    }

    Ok(effects)
}

fn parse_field_justify(items: SexprList<'_>) -> Result<Option<FieldJustify>> {
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
            value => bail!("invalid effects justification {value:?}"),
        }
    }

    Ok((!justify.is_empty()).then_some(justify))
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

fn parse_pin(items: SexprList<'_>) -> Result<PinInstance> {
    let number = items.string(1).context("pin missing number")?;

    let mut id = None;
    let mut alternate = None;
    let mut unsupported = Vec::new();
    for child in items.children_from(2) {
        let Some(list) = SexprList::from_sexpr(child) else {
            unsupported.push(child.clone());
            continue;
        };
        match list.tag() {
            Some("uuid") => id = list.string(1),
            Some("alternate") => alternate = list.string(1),
            _ => unsupported.push(child.clone()),
        }
    }

    Ok(PinInstance {
        number: number.clone(),
        id: id.ok_or_else(|| anyhow!("pin {number} missing uuid"))?,
        alternate,
        unsupported,
    })
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
    items.push(field_to_sexpr(&sheet.file));
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
            Sexpr::int(pin.rotation.degrees()),
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

    if label.fields_autoplaced {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("fields_autoplaced"),
            Sexpr::symbol("yes"),
        ]));
    }

    items.push(text_effects_to_sexpr(
        &label.effects,
        Some(label_spin_justify(label.kind, label.spin)),
    ));
    items.push(Sexpr::list(vec![
        Sexpr::symbol("uuid"),
        Sexpr::string(&label.id),
    ]));
    items.extend(label.fields.values().map(field_to_sexpr));
    items.extend(label.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn field_to_sexpr(field: &SymbolField) -> Sexpr {
    let mut items = vec![Sexpr::symbol("property")];
    if field.private {
        items.push(Sexpr::symbol("private"));
    }
    items.extend([
        Sexpr::string(&field.name),
        Sexpr::string(&field.value),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(field.at.x),
            Sexpr::float(field.at.y),
            angle_to_sexpr(field.rotation_deg.rem_euclid(360.0)),
        ]),
    ]);

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

    items.push(text_effects_to_sexpr(&field.effects, field.justify));

    items.extend(field.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn pin_to_sexpr(pin: &PinInstance) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("pin"),
        Sexpr::string(&pin.number),
        Sexpr::list(vec![Sexpr::symbol("uuid"), Sexpr::string(&pin.id)]),
    ];
    if let Some(alternate) = &pin.alternate {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("alternate"),
            Sexpr::string(alternate),
        ]));
    }
    items.extend(pin.unsupported.iter().cloned());
    Sexpr::list(items)
}

fn library_to_sexpr(library: &SymbolLibrary) -> Sexpr {
    let mut items = vec![Sexpr::symbol("lib_symbols")];
    items.extend(
        library
            .definitions
            .values()
            .map(|definition| definition.sexpr.clone()),
    );
    items.extend(library.unsupported.iter().cloned());
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

fn validate_symbol_graphics_for_kicad_10(sexpr: &Sexpr) -> Result<()> {
    let Some(items) = sexpr.as_list() else {
        return Ok(());
    };

    if list_tag(items) == Some("fill") {
        for item in &items[1..] {
            let Some(type_items) = item.as_list() else {
                continue;
            };
            if list_tag(type_items) != Some("type") {
                continue;
            }
            match type_items.get(1).context("fill type is missing")?.as_sym() {
                Some(
                    "none" | "outline" | "hatch" | "reverse_hatch" | "cross_hatch" | "color"
                    | "background",
                ) => {}
                Some(value) => bail!("unsupported KiCad 10 fill type '{value}'"),
                None => bail!("fill type must be an unquoted symbol"),
            }
        }
    }

    for item in items {
        validate_symbol_graphics_for_kicad_10(item)?;
    }
    Ok(())
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

fn parse_paper(items: SexprList<'_>) -> Result<Paper> {
    let name = items.string(1).context("paper missing name")?;

    if name == "User" {
        return Ok(Paper::Custom {
            width_mm: items
                .f64(2)
                .context("custom paper missing or invalid width")?,
            height_mm: items
                .f64(3)
                .context("custom paper missing or invalid height")?,
        });
    }

    let mut portrait = false;
    for value in items.children_from(2) {
        match value.as_atom() {
            Some("portrait") => portrait = true,
            value => bail!("named paper {name} has invalid option {value:?}"),
        }
    }
    Ok(Paper::Named { name, portrait })
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

fn text_effects_to_sexpr(effects: &TextEffects, justify: Option<FieldJustify>) -> Sexpr {
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
    font_items.extend(effects.font_unsupported.iter().cloned());

    let mut items = vec![Sexpr::symbol("effects"), Sexpr::list(font_items)];
    if let Some(justify) = justify {
        let atoms = field_justify_atoms(justify);
        if !atoms.is_empty() {
            let mut justify_items = vec![Sexpr::symbol("justify")];
            justify_items.extend(atoms.into_iter().map(Sexpr::symbol));
            items.push(Sexpr::list(justify_items));
        }
    }
    items.extend(effects.unsupported.iter().cloned());

    Sexpr::list(items)
}

fn label_kind_token(kind: LabelKind) -> &'static str {
    match kind {
        LabelKind::Local => "label",
        LabelKind::Global { .. } => "global_label",
        LabelKind::Hierarchical { .. } => "hierarchical_label",
        LabelKind::Directive { .. } => "netclass_flag",
    }
}

fn default_label_shape_for_tag(tag: &str) -> LabelShape {
    match tag {
        "global_label" => LabelShape::Bidirectional,
        "hierarchical_label" => LabelShape::Input,
        "netclass_flag" | "directive_label" => LabelShape::Round,
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
        LabelKind::Global { shape }
        | LabelKind::Hierarchical { shape }
        | LabelKind::Directive { shape } => Some(shape),
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
        LabelKind::Global { .. } | LabelKind::Hierarchical { .. } | LabelKind::Directive { .. } => {
            None
        }
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

    fn bool_or(self, index: usize, default: bool, field: &str) -> Result<bool> {
        let Some(value) = self.get(index) else {
            return Ok(default);
        };
        atom_bool(value).with_context(|| format!("{field} must be yes or no"))
    }

    fn children_from(self, index: usize) -> impl Iterator<Item = &'a Sexpr> {
        self.items.get(index..).unwrap_or_default().iter()
    }
}

#[cfg(test)]
fn find_child<'a>(items: &'a [Sexpr], tag: &str) -> Option<&'a [Sexpr]> {
    items.iter().find_map(|item| {
        let list = item.as_list()?;
        (list_tag(list) == Some(tag)).then_some(list)
    })
}

#[cfg(test)]
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
        SexprKind::F64(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value <= i64::MAX as f64 =>
        {
            Some(*value as i64)
        }
        SexprKind::F64(_) => None,
        SexprKind::Symbol(value) | SexprKind::String(value) => value.parse().ok(),
        SexprKind::List(_) => None,
    }
}

fn atom_bool(value: &Sexpr) -> Option<bool> {
    match value.as_atom()? {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn atom_f64(value: &Sexpr) -> Option<f64> {
    let value = match &value.kind {
        SexprKind::F64(value) => Some(*value),
        SexprKind::Int(value) => Some(*value as f64),
        SexprKind::Symbol(value) | SexprKind::String(value) => value.parse().ok(),
        SexprKind::List(_) => None,
    }?;
    value.is_finite().then_some(value)
}

/// Decode the brace escapes KiCad applies before displaying text.
pub(crate) fn unescape_text(source: &str) -> String {
    let characters = source.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '{' {
            output.push(characters[index]);
            index += 1;
            continue;
        }

        let previous = index.checked_sub(1).map(|index| characters[index]);
        let mut depth = 1;
        let mut end = index + 1;
        while end < characters.len() && depth > 0 {
            match characters[end] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        let terminated = depth == 0;
        let token_end = if terminated {
            end - 1
        } else {
            characters.len()
        };
        let token = characters[index + 1..token_end].iter().collect::<String>();
        let token = unescape_text(&token);

        if !terminated {
            output.push('{');
            output.push_str(&token);
        } else if matches!(previous, Some('$' | '~' | '^' | '_')) {
            output.push('{');
            output.push_str(&token);
            output.push('}');
        } else if let Some(value) = unescaped_token(&token) {
            output.push_str(value);
        } else {
            output.push('{');
            output.push_str(&token);
            output.push('}');
        }
        index = end;
    }
    output
}

fn unescaped_token(token: &str) -> Option<&'static str> {
    match token {
        "dblquote" => Some("\""),
        "quote" => Some("'"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "backslash" => Some("\\"),
        "slash" => Some("/"),
        "bar" => Some("|"),
        "comma" => Some(","),
        "colon" => Some(":"),
        "space" => Some(" "),
        "dollar" => Some("$"),
        "tab" => Some("\t"),
        "return" => Some("\n"),
        "brace" => Some("{"),
        _ => None,
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
        assert_eq!(document.pages[0].library.definitions.len(), 1);
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
    fn rejects_pre_kicad_10_schematics() {
        let error =
            SchDocument::from_kicad_sch(&SAMPLE.replace("20260306", "20231120")).unwrap_err();

        assert!(error.to_string().contains("expected KiCad 10"));
    }

    #[test]
    fn malformed_supported_electrical_item_is_an_error() {
        let content = SAMPLE.replace("(pts (xy 10 20) (xy 30 20))", "(pts (xy 10 20))");

        let error = SchDocument::from_kicad_sch(&content).unwrap_err();

        assert!(error.to_string().contains("wire must contain exactly two"));
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
  (netclass_flag ""
    (length 2.54)
    (shape round)
    (at 80 20 0)
    (effects (font (size 1.27 1.27)))
    (uuid "directive")
    (property "Net Class" "Power" (at 80 20 0))
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

        let SchItem::Label(directive) = &document.pages[0].items[7] else {
            panic!("expected directive label");
        };
        assert_eq!(
            directive.kind,
            LabelKind::Directive {
                shape: LabelShape::Round,
            }
        );
        assert_eq!(directive.at, Point::new(80.0, 20.0));
        assert!(directive.fields.contains_key("Net Class"));

        let formatted = document.to_kicad_sch().expect("format schematic");
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse schematic");

        assert_eq!(reparsed.pages[0].items, document.pages[0].items);
    }

    #[test]
    fn semantic_text_effect_edits_are_serialized() {
        let content = SAMPLE.replace(
            r#"  (sheet_instances"#,
            r#"  (label "EDIT" (at 40 20 0)
    (effects (font (size 1.27 1.27)) (justify left bottom))
    (uuid "label-edit"))
  (sheet_instances"#,
        );
        let mut document = SchDocument::from_kicad_sch(&content).unwrap();
        let label = document.pages[0]
            .items
            .iter_mut()
            .find_map(|item| match item {
                SchItem::Label(label) => Some(label),
                _ => None,
            })
            .unwrap();
        label.effects.font_size = TextSize::new(2.0, 2.0);
        label.effects.bold = true;

        let formatted = document.to_kicad_sch().unwrap();
        let reparsed = SchDocument::from_kicad_sch(&formatted).unwrap();
        let SchItem::Label(label) = &reparsed.pages[0].items[3] else {
            panic!("expected label");
        };
        assert_eq!(label.effects.font_size, TextSize::new(2.0, 2.0));
        assert!(label.effects.bold);
    }

    #[test]
    fn placed_pin_alternate_round_trips() {
        let content = SAMPLE.replace(
            r#"(pin "1" (uuid "pin-1"))"#,
            r#"(pin "1" (alternate "ALT") (uuid "pin-1"))"#,
        );
        let document = SchDocument::from_kicad_sch(&content).unwrap();
        let SchItem::Symbol(symbol) = &document.pages[0].items[2] else {
            panic!("expected symbol");
        };
        assert_eq!(symbol.pins[0].alternate.as_deref(), Some("ALT"));

        let reparsed = SchDocument::from_kicad_sch(&document.to_kicad_sch().unwrap()).unwrap();
        assert_eq!(reparsed, document);
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
    fn rejects_non_kicad_10_symbol_fill_types() {
        let error = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Device:D"
              (polyline
                (stroke (width 0.254) (type solid))
                (fill (type solid))))"#,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported KiCad 10 fill type 'solid'"));

        let error = SymbolDefinition::from_kicad_symbol_sexpr(
            r#"(symbol "Device:D" (polyline (fill (type invented))))"#,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported KiCad 10 fill type 'invented'"));
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
        assert_eq!(reparsed.pages[0].library.definitions.len(), 1);
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
        assert!(formatted.contains(&format!("(generator_version \"{GENERATOR_VERSION}\")")));
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
            |item| matches!(item, SchItem::Sheet(sheet) if sheet.file_name() == "aSheet.kicad_sch" && sheet.pins.len() == 2)
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
    fn label_fields_autoplaced_round_trips_without_properties() {
        let content = SAMPLE.replace(
            "  (sheet_instances",
            r#"  (label "NET"
    (at 20 20 0)
    (fields_autoplaced yes)
    (effects (font (size 1.27 1.27)))
    (uuid "label-1")
  )
  (sheet_instances"#,
        );
        let document = SchDocument::from_kicad_sch(&content).expect("parse schematic");
        let label = document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.id == "label-1" => Some(label),
                _ => None,
            })
            .expect("parsed label");
        assert!(label.fields_autoplaced);
        assert!(label.fields.is_empty());

        let formatted = document.to_kicad_sch().expect("format schematic");
        let reparsed = SchDocument::from_kicad_sch(&formatted).expect("reparse schematic");
        let label = reparsed.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.id == "label-1" => Some(label),
                _ => None,
            })
            .expect("round-tripped label");

        assert!(label.fields_autoplaced);
        assert!(label.fields.is_empty());
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
                is_root: true,
            },
            KicadSchSource {
                file_name: Some("b.kicad_sch"),
                content: &SAMPLE.replace("page-1", "page-2"),
                is_root: false,
            },
        ])
        .expect("parse pages");

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].file_name.as_deref(), Some("a.kicad_sch"));
        assert_eq!(document.pages[0].library.definitions.len(), 1);
        assert_eq!(document.pages[1].library.definitions.len(), 1);

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
