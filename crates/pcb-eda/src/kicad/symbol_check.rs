//! Semantic checks for one symbol of a KiCad symbol library.
//!
//! Errors are symbols KiCad refuses to load or whose pin numbers cannot be
//! resolved, warnings are valid symbols that are almost certainly wrong, and
//! advice is KLC-derived style. An unnamed pin is neither: KiCad 10 writes it
//! as `(name "")` and it takes its number as its signal name.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pcb_sexpr::edit::{Edit, apply};
use pcb_sexpr::{Sexpr, SexprKind, Span};

use super::symbol::{
    KicadPin, KicadSymbol, nested_symbol_unit_style, parse_bool_atom, parse_pin_common,
};
use super::symbol_library::{KicadSymbolLibrary, scan_format_version};
use crate::is_placeholder_kicad_pin_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Advice,
}

#[derive(Debug, Clone)]
pub struct SymbolIssue {
    pub kind: &'static str,
    pub severity: Severity,
    pub message: String,
    pub help: &'static str,
    /// Index of the library source that `span` points into.
    pub source: usize,
    pub span: Span,
    /// Edits to that source that resolve the issue; empty when resolving it
    /// takes a decision.
    pub fix: Vec<Edit>,
}

/// Numbered pads of the footprint a symbol is paired with.
#[derive(Debug, Clone, Copy)]
pub struct FootprintPads<'a> {
    pub name: &'a str,
    pub numbers: &'a BTreeSet<String>,
}

struct Rule {
    kind: &'static str,
    severity: Severity,
    help: &'static str,
}

impl Rule {
    const fn new(kind: &'static str, severity: Severity, help: &'static str) -> Self {
        Self {
            kind,
            severity,
            help,
        }
    }

    fn issue(&self, source: usize, span: Span, message: String) -> SymbolIssue {
        SymbolIssue {
            kind: self.kind,
            severity: self.severity,
            message,
            help: self.help,
            source,
            span,
            fix: Vec::new(),
        }
    }
}

use Severity::{Advice, Error, Warning};

const PARSE: Rule = Rule::new(
    "symbol.parse",
    Error,
    "KiCad cannot load this file; fix the form at this location",
);
const PIN_EMPTY_NUMBER: Rule = Rule::new(
    "symbol.pin.empty_number",
    Error,
    "set the number to the footprint pad this pin connects to",
);
const PIN_NO_PAD: Rule = Rule::new(
    "symbol.pin.no_pad",
    Error,
    "give the pin the number of the pad it connects to, or add the pad to the footprint",
);
const PIN_DUPLICATE: Rule = Rule::new(
    "symbol.pin.duplicate",
    Error,
    "give each pin a unique number, or place both at one location with one name to form a pin stack",
);
const UNIT_NAMING: Rule = Rule::new(
    "symbol.unit.naming",
    Error,
    "name nested units `<symbol>_<unit>_<style>`, e.g. `<symbol>_1_1`",
);
const EXTENDS: Rule = Rule::new(
    "symbol.extends",
    Error,
    "define the parent symbol in this library, or remove `extends`",
);
const PIN_HIDDEN_POWER: Rule = Rule::new(
    "symbol.pin.hidden_power",
    Warning,
    "show the pin; hidden duplicates in a pin stack use `passive`",
);
const PIN_OVERLAP: Rule = Rule::new(
    "symbol.pin.overlap",
    Warning,
    "move one pin, or give stacked pins the same name",
);
const PIN_NC_TYPE: Rule = Rule::new(
    "symbol.pin.nc_type",
    Warning,
    "use `no_connect` for a pin that must stay open, `free` for one with no internal connection, or name the pin after its function",
);
const PIN_POWER_CONFLICT: Rule = Rule::new(
    "symbol.pin.power_conflict",
    Warning,
    "a name is either a supply input or a supply output; use one type for every pin with this name",
);
const PAD_NO_PIN: Rule = Rule::new(
    "symbol.pad.no_pin",
    Warning,
    "give mechanical and shield pads a pin, or leave the pad without a number",
);
const UNIT_EMPTY: Rule = Rule::new(
    "symbol.unit.empty",
    Warning,
    "add the missing pins or graphics, or remove what is empty",
);
const PROPERTY_MISSING: Rule = Rule::new(
    "symbol.property.missing",
    Warning,
    "add the property; `Reference` holds the designator prefix and `Value` the symbol name",
);
const NUMBER_FORMAT: Rule = Rule::new(
    "symbol.number.format",
    Warning,
    "write a plain decimal such as `0` or `0.001`",
);
const STYLE_VALUE_NAME: Rule = Rule::new(
    "style.symbol.value_name",
    Advice,
    "set `Value` to the symbol name",
);
const STYLE_PIN_GRID: Rule = Rule::new(
    "style.symbol.pin.grid",
    Advice,
    "place pin origins on the 2.54 mm (100 mil) grid",
);
const STYLE_PIN_LENGTH: Rule = Rule::new(
    "style.symbol.pin.length",
    Advice,
    "use a pin length in 1.27 mm (50 mil) steps",
);
const STYLE_ORIGIN: Rule = Rule::new(
    "style.symbol.origin",
    Advice,
    "centre the symbol on the origin",
);
const STYLE_OUTLINE: Rule = Rule::new(
    "style.symbol.outline",
    Advice,
    "draw the body outline with a 0.254 mm (10 mil) stroke",
);
const STYLE_TEXT_SIZE: Rule = Rule::new(
    "style.symbol.text_size",
    Advice,
    "use 1.27 mm (50 mil) pin name and number text",
);
const STYLE_NAME_OFFSET: Rule = Rule::new(
    "style.symbol.name_offset",
    Advice,
    "keep the pin-name offset between 0.508 and 1.27 mm",
);
const STYLE_STACK: Rule = Rule::new(
    "style.symbol.stack",
    Advice,
    "keep one visible pin with the functional type; hidden stack members use `passive`",
);

const ELECTRICAL_TYPES: &[&str] = &[
    "input",
    "output",
    "bidirectional",
    "tri_state",
    "passive",
    "free",
    "unspecified",
    "power_in",
    "power_out",
    "open_collector",
    "open_emitter",
    "no_connect",
    "unconnected",
];
const GRAPHIC_STYLES: &[&str] = &[
    "line",
    "inverted",
    "clock",
    "inverted_clock",
    "input_low",
    "clock_low",
    "output_low",
    "edge_clock_high",
    "non_logic",
];
/// What KiCad's parser accepts directly inside a symbol and inside a unit.
const SYMBOL_FORMS: &[&str] = &[
    "arc",
    "bezier",
    "body_styles",
    "circle",
    "duplicate_pin_numbers_are_jumpers",
    "embedded_files",
    "embedded_fonts",
    "exclude_from_sim",
    "extends",
    "in_bom",
    "in_pos_files",
    "jumper_pin_groups",
    "on_board",
    "pin",
    "pin_names",
    "pin_numbers",
    "polyline",
    "power",
    "property",
    "rectangle",
    "symbol",
    "text",
    "text_box",
];
const UNIT_FORMS: &[&str] = &[
    "unit_name",
    "arc",
    "bezier",
    "circle",
    "pin",
    "polyline",
    "rectangle",
    "text",
    "text_box",
];
const JUSTIFY: &[&str] = &["left", "right", "top", "bottom", "mirror"];
const FILL_TYPES: &[&str] = &[
    "none",
    "outline",
    "background",
    "color",
    "hatch",
    "reverse_hatch",
    "cross_hatch",
];
const STROKE_TYPES: &[&str] = &[
    "default",
    "dash",
    "dot",
    "dash_dot",
    "dash_dot_dot",
    "solid",
];
const NC_NAMES: &[&str] = &["NC", "N/C", "DNC", "DNU"];

const GRID_NM: i64 = 2_540_000;
const HALF_GRID_NM: i64 = 1_270_000;
const TEXT_SIZE_NM: i64 = 1_270_000;
const OUTLINE_NM: i64 = 254_000;
const NAME_OFFSET_NM: std::ops::RangeInclusive<i64> = 508_000..=1_270_000;

/// The sources of a library with the fixable issues of symbol `name` fixed.
///
/// A fix can uncover the next issue, as a comment hides all that follows it,
/// so this repeats until none is left. Each edit removes what it was made for.
pub fn fix(mut sources: Vec<String>, name: &str) -> Vec<String> {
    loop {
        let Ok(library) = KicadSymbolLibrary::from_sources(sources.clone()) else {
            return sources;
        };
        let issues = check_library(&library)
            .map_or_else(|| check_symbol(&library, name, None), |issue| vec![issue]);
        let mut edits = vec![Vec::new(); sources.len()];
        for issue in issues {
            edits[issue.source].extend(issue.fix);
        }
        if edits.iter().all(Vec::is_empty) {
            return sources;
        }
        sources = sources
            .iter()
            .zip(edits)
            .map(|(text, edits)| apply(text, edits))
            .collect();
    }
}

/// Check that KiCad can read the files of `library` at all. Symbol loading
/// reads one definition at a time, leniently, and notices none of this.
pub fn check_library(library: &KicadSymbolLibrary) -> Option<SymbolIssue> {
    let mut sources = library.sources().iter().enumerate();
    sources.find_map(|(source, text)| {
        let fault = pcb_sexpr::scan::malformed(text)
            .map(|(offset, fault)| (offset, format!("file does not parse: {fault}")));
        let header = || {
            let message = "library does not open with `(kicad_symbol_lib (version …)`";
            (scan_format_version(text).is_none()).then(|| (0, message.to_string()))
        };
        let (offset, message) = fault.or_else(header)?;
        let mut issue = PARSE.issue(source, Span::new(offset, offset + 1), message);
        // KiCad reads nothing from a comment, so every one of them can go.
        if text[offset..].starts_with(';') {
            let comments = pcb_sexpr::scan::comments(text);
            issue.fix = comments
                .map(|comment| Edit::delete(Span::new(comment.start, comment.end)))
                .collect();
        }
        Some(issue)
    })
}

/// Check symbol `name` of `library`. Issues are ordered by source position.
pub fn check_symbol(
    library: &KicadSymbolLibrary,
    name: &str,
    footprint: Option<FootprintPads>,
) -> Vec<SymbolIssue> {
    let find = |name: &str| {
        let (source, offset, node) = library.definition(name).ok()??;
        Some(Def {
            source,
            offset,
            name: name.to_string(),
            node,
        })
    };
    let Some(symbol) = find(name) else {
        return Vec::new();
    };

    // The symbol followed by each symbol it extends, nearest parent first.
    let mut chain = vec![symbol];
    loop {
        let def = &chain[chain.len() - 1];
        let Some(target) = child(def.items(), "extends")
            .and_then(Sexpr::as_list)
            .and_then(|extends| extends.get(1))
        else {
            break;
        };
        let parent = target
            .as_atom()
            .and_then(find)
            .filter(|parent| chain.iter().all(|def| def.name != parent.name));
        let Some(parent) = parent else {
            let message = format!(
                "{}: extends {}, which this library does not define",
                def.name,
                quoted(target)
            );
            return vec![def.issue(&EXTENDS, target.span, message)];
        };
        chain.push(parent);
    }
    // A derived symbol that draws its own units replaces its parent's.
    let body = chain
        .iter()
        .find(|def| has_section(def.items(), &["symbol", "pin"]))
        .unwrap_or(&chain[chain.len() - 1]);

    let mut issues = Vec::new();
    check_properties(&chain, &mut issues);
    for def in &chain {
        check_forms(def, &mut issues);
    }
    // Pads are compared with the pins a component gets from loading. A
    // footprint without pads is a mechanical outline and a symbol without
    // pins a mechanical part: neither has a pinout to compare.
    let loaded = library.resolved(name).ok().flatten();
    let pairing = footprint
        .zip(loaded.as_deref())
        .filter(|(footprint, loaded)| !footprint.numbers.is_empty() && !loaded.pins().is_empty());
    check_body(body, pairing, &mut issues);
    issues.sort_by_key(|issue| (issue.source, issue.span.start));
    issues
}

/// A symbol as written in the library, before `extends` resolution.
struct Def {
    source: usize,
    /// Byte offset of the definition, which its node's spans are relative to.
    offset: usize,
    name: String,
    node: Arc<Sexpr>,
}

impl Def {
    fn items(&self) -> &[Sexpr] {
        self.node.as_list().unwrap_or_default()
    }

    fn name_span(&self) -> Span {
        self.items().get(1).map_or(self.node.span, |name| name.span)
    }

    /// `span` of the definition's node as a span of its source.
    fn in_source(&self, span: Span) -> Span {
        Span::new(span.start + self.offset, span.end + self.offset)
    }

    fn issue(&self, rule: &Rule, span: Span, message: String) -> SymbolIssue {
        rule.issue(self.source, self.in_source(span), message)
    }

    fn edit(&self, span: Span, text: impl Into<String>) -> Edit {
        Edit {
            span: self.in_source(span),
            text: text.into(),
        }
    }
}

struct Pin<'a> {
    unit: u32,
    style: u32,
    node: &'a Sexpr,
    items: &'a [Sexpr],
    parsed: KicadPin,
    /// Position in nanometres.
    at: Option<(i64, i64)>,
}

impl Pin<'_> {
    fn name(&self) -> &str {
        &self.parsed.name
    }

    fn number(&self) -> &str {
        &self.parsed.number
    }

    fn electrical_type(&self) -> &str {
        self.parsed.electrical_type.as_deref().unwrap_or_default()
    }

    fn is_no_connect(&self) -> bool {
        matches!(self.electrical_type(), "no_connect" | "unconnected")
    }

    /// `pin "5" (VBUS)`, or `pin "5"` for an unnamed pin.
    fn describe(&self) -> String {
        if is_placeholder_kicad_pin_name(self.name()) {
            format!("pin \"{}\"", self.number())
        } else {
            format!("pin \"{}\" ({})", self.number(), self.name())
        }
    }

    /// The position as written in the file, e.g. `(11.43, 0)`.
    fn at_text(&self) -> String {
        match child(self.items, "at").and_then(Sexpr::as_list) {
            Some([_, x, y, ..]) => format!("({}, {})", raw(x), raw(y)),
            _ => "(?, ?)".to_string(),
        }
    }

    /// Span of the value of the `name` or `number` child, else of the pin head.
    fn value_span(&self, attribute: &str) -> Span {
        child(self.items, attribute)
            .and_then(Sexpr::as_list)
            .and_then(|list| list.get(1))
            .map_or_else(|| head_span(self.node), |value| value.span)
    }
}

fn check_properties(chain: &[Def], issues: &mut Vec<SymbolIssue>) {
    let symbol = &chain[0];
    let property = |key: &str| {
        chain.iter().find_map(|def| {
            sections(def.items(), &["property"])
                .filter_map(Sexpr::as_list)
                .find(|property| property.get(1).and_then(Sexpr::as_atom) == Some(key))
                .and_then(|property| property.get(2))
                .map(|value| (def, value))
        })
    };

    for key in ["Reference", "Value"] {
        if property(key).is_none() {
            let message = format!("{}: property `{key}` is missing", symbol.name);
            issues.push(symbol.issue(&PROPERTY_MISSING, symbol.name_span(), message));
        }
    }
    if let Some((def, value)) = property("Value")
        && value.as_atom() != Some(symbol.name.as_str())
    {
        let message = format!(
            "{}: `Value` is {}, not the symbol name",
            symbol.name,
            quoted(value)
        );
        issues.push(def.issue(&STYLE_VALUE_NAME, value.span, message));
    }
}

/// Walk every form of a definition for what KiCad rejects or would not write.
fn check_forms(def: &Def, issues: &mut Vec<SymbolIssue>) {
    let mut report = |rule: &Rule, span: Span, message: String, fix: Option<Edit>| {
        let mut issue = def.issue(rule, span, format!("{}: {message}", def.name));
        issue.fix.extend(fix);
        issues.push(issue);
    };

    for item in def.items().iter().skip(2) {
        check_child(item, "a symbol", SYMBOL_FORMS, &mut report);
        let children = item.as_list().unwrap_or_default();
        let inner = match children.first().and_then(Sexpr::as_sym) {
            Some("symbol") => Some(("a unit symbol", UNIT_FORMS, 2)),
            Some("pin_names") => Some(("pin_names", &["offset", "hide"][..], 1)),
            Some("pin_numbers") => Some(("pin_numbers", &["hide"][..], 1)),
            _ => None,
        };
        if let Some((parent, known, skip)) = inner {
            for child in children.iter().skip(skip) {
                check_child(child, parent, known, &mut report);
            }
        }
    }

    let mut stack = vec![&*def.node];
    while let Some(node) = stack.pop() {
        match (&node.kind, &node.raw_atom) {
            (SexprKind::List(items), _) => {
                let head = items.first().and_then(Sexpr::as_sym).unwrap_or_default();
                let values: &[Sexpr] = match head {
                    "fill" | "stroke" => child(items, "type")
                        .and_then(Sexpr::as_list)
                        .map_or(&[], |kind| kind.get(1..2).unwrap_or_default()),
                    "justify" => &items[1..],
                    _ => &[],
                };
                let known = match head {
                    "fill" => FILL_TYPES,
                    "stroke" => STROKE_TYPES,
                    _ => JUSTIFY,
                };
                for value in values {
                    if !value.as_sym().is_some_and(|value| known.contains(&value)) {
                        let message = format!(
                            "`{}` is not a {head} KiCad has; it has {}",
                            raw(value),
                            known.join(", ")
                        );
                        // The two values tools most often invent: a solid
                        // fill is `outline`, and centred text has no `justify`.
                        let fix = match (head, raw(value)) {
                            ("fill", "solid") => Some(def.edit(value.span, "outline")),
                            ("justify", "center") if items.len() == 2 => {
                                Some(def.edit(node.span, ""))
                            }
                            ("justify", "center") => Some(def.edit(value.span, "")),
                            _ => None,
                        };
                        report(&PARSE, value.span, message, fix);
                    }
                }
                stack.extend(items.iter().rev());
            }
            (SexprKind::Int(_) | SexprKind::F64(_), Some(raw)) if !is_plain_decimal(raw) => {
                report(
                    &NUMBER_FORMAT,
                    node.span,
                    format!("`{raw}` is not a plain decimal"),
                    plain_decimal(raw).map(|plain| def.edit(node.span, plain)),
                );
            }
            _ => {}
        }
    }
}

/// Report `item` unless it is a form KiCad accepts inside `parent`.
fn check_child(
    item: &Sexpr,
    parent: &str,
    known: &[&str],
    report: &mut impl FnMut(&Rule, Span, String, Option<Edit>),
) {
    let head = match item.as_list() {
        Some(list) => list.first().and_then(Sexpr::as_sym),
        // `hide` is the one bare word KiCad still reads, from older files.
        None => item.as_sym().filter(|atom| *atom == "hide"),
    };
    if !head.is_some_and(|head| known.contains(&head)) {
        let (span, found) = match item.as_list() {
            Some(list) => (head_span(item), list.first().map_or("()", raw)),
            None => (item.span, raw(item)),
        };
        let message = format!("`{found}` is not something KiCad accepts in {parent}");
        report(&PARSE, span, message, None);
    }
}

fn is_plain_decimal(raw: &str) -> bool {
    let unsigned = raw.strip_prefix('-').unwrap_or(raw);
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    // `.5` is what older KiCad files and `pcb import` write for 0.5.
    let integer = if integer.is_empty() { "0" } else { integer };
    let negative_zero = raw.starts_with('-') && unsigned.bytes().all(|b| matches!(b, b'0' | b'.'));
    digits(integer) && digits(fraction) && !negative_zero
}

/// The value of `raw` to the nanometre, as a plain decimal.
fn plain_decimal(raw: &str) -> Option<String> {
    let value = raw.parse::<f64>().ok().filter(|value| value.is_finite())?;
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    Some(if text == "-0" { "0" } else { text }.to_string())
}

/// What a symbol draws: its pins, its filled body rectangles, and its units.
struct Body<'a> {
    def: &'a Def,
    pins: Vec<Pin<'a>>,
    /// Owning unit, node, and bounds in nanometres.
    outlines: Vec<(u32, &'a Sexpr, [i64; 4])>,
    /// Unit number → span of the name of its first nested symbol.
    units: BTreeMap<u32, Span>,
}

struct Reporter<'a> {
    symbol: &'a Def,
    multi_unit: bool,
    issues: &'a mut Vec<SymbolIssue>,
}

impl Reporter<'_> {
    /// Report against the symbol, naming `unit` when the symbol has several.
    fn push(&mut self, rule: &Rule, unit: u32, span: Span, message: String) {
        let name = &self.symbol.name;
        let message = if self.multi_unit && unit > 0 {
            format!("{name} unit {unit}: {message}")
        } else {
            format!("{name}: {message}")
        };
        self.issues.push(self.symbol.issue(rule, span, message));
    }
}

fn check_body(
    def: &Def,
    pairing: Option<(FootprintPads, &KicadSymbol)>,
    issues: &mut Vec<SymbolIssue>,
) {
    let mut body = Body {
        def,
        pins: Vec::new(),
        outlines: Vec::new(),
        units: BTreeMap::new(),
    };
    let mut unit_names: Option<BTreeSet<String>> = None;
    for section in sections(def.items(), &["symbol", "pin"]) {
        let items = section.as_list().unwrap_or_default();
        if items[0].as_sym() == Some("pin") {
            body.pins.push(pin(0, 0, section));
            continue;
        }
        let (unit, style) = nested_symbol_unit_style(items);
        if let Some(name) = items.get(1) {
            body.units.entry(unit).or_insert(name.span);
            let written = name.as_atom().unwrap_or_default();
            if !is_unit_name(written, &def.name) {
                let message = format!(
                    "{}: nested symbol {} is not named `{}_<unit>_<style>`",
                    def.name,
                    quoted(name),
                    def.name
                );
                let mut issue = def.issue(&UNIT_NAMING, name.span, message);
                // A name that ends in a unit and a style only has the wrong
                // prefix, unless another unit already holds the right name.
                let unit_names = unit_names.get_or_insert_with(|| {
                    sections(def.items(), &["symbol"])
                        .filter_map(|unit| Some(unit.as_list()?.get(1)?.as_atom()?.to_string()))
                        .collect()
                });
                let renamed = unit_rename(written, &def.name)
                    .filter(|renamed| unit_names.insert(renamed.clone()));
                issue.fix.extend(renamed.map(|renamed| {
                    def.edit(name.span, pcb_sexpr::formatter::quote_string(&renamed))
                }));
                issues.push(issue);
            }
        }
        let pins = sections(items, &["pin"]);
        body.pins.extend(pins.map(|node| pin(unit, style, node)));
        let filled = sections(items, &["rectangle"]).filter(|node| {
            descend(node, &["fill", "type"]).and_then(|fill| fill.as_list()?.get(1)?.as_sym())
                == Some("background")
        });
        body.outlines
            .extend(filled.filter_map(|node| Some((unit, node, rectangle_bounds(node)?))));
    }

    let mut out = Reporter {
        symbol: def,
        multi_unit: body.units.keys().filter(|unit| **unit > 0).count() > 1,
        issues,
    };
    for pin in &body.pins {
        check_pin(pin, &mut out);
    }
    check_pin_style(&body, &mut out);
    check_pin_types(&body, &mut out);
    // Pads are compared against pin numbers only once the numbers are sound.
    if check_numbering(&body, &mut out)
        && let Some((footprint, loaded)) = pairing
    {
        check_footprint(&body, loaded, footprint, &mut out);
    }
    check_stacks(&body, &mut out);
    check_power_names(&body, &mut out);
    check_units(&body, &mut out);
    check_layout(&body, &mut out);
}

fn check_pin(pin: &Pin, out: &mut Reporter) {
    let desc = pin.describe();
    let items = pin.items;

    for (index, known, what) in [
        (1, ELECTRICAL_TYPES, "electrical type"),
        (2, GRAPHIC_STYLES, "graphic style"),
    ] {
        let value = items.get(index).and_then(Sexpr::as_sym);
        if !value.is_some_and(|value| known.contains(&value)) {
            let span = items
                .get(index)
                .map_or_else(|| head_span(pin.node), |node| node.span);
            let found = items.get(index).map_or("nothing", raw);
            let message = format!("{desc} has unknown {what} `{found}`");
            out.push(&PARSE, pin.unit, span, message);
        }
    }
    if let Some(at) = child(items, "at") {
        let rotation = pin.parsed.at.as_ref().map(|at| at.rotation.unwrap_or(0.0));
        if !rotation.is_some_and(|r| [0.0, 90.0, 180.0, 270.0].contains(&r)) {
            let message = format!("{desc} position is not `(at <x> <y> <0|90|180|270>)`");
            out.push(&PARSE, pin.unit, at.span, message);
        }
    }
    if let Some(length) = child(items, "length")
        && pin.parsed.length.is_none()
    {
        let message = format!("{desc} length is not a number");
        out.push(&PARSE, pin.unit, length.span, message);
    }
}

/// Pins that miss a style rule the same way share one finding: a symbol drawn
/// half a step off has every pin off the grid.
fn check_pin_style(body: &Body, out: &mut Reporter) {
    let mut causes: BTreeMap<String, (&Rule, Span, Vec<&Pin>)> = BTreeMap::new();
    let mut found = |rule: &'static Rule, cause: String, span: Span, pin| {
        let (.., pins) = causes.entry(cause).or_insert((rule, span, Vec::new()));
        pins.push(pin);
    };
    for pin in &body.pins {
        let items = pin.items;
        if let Some(length) = child(items, "length")
            && let Some(mm) = pin.parsed.length
            && nm(mm) % HALF_GRID_NM != 0
        {
            let cause = format!("length {mm} mm is not a multiple of 1.27 mm");
            found(&STYLE_PIN_LENGTH, cause, length.span, pin);
        }
        if let (Some((x, y)), false) = (pin.at, pin.parsed.hidden) {
            // KLC lets no-connect pins sit on the body edge, a half step off.
            let grid = if pin.is_no_connect() {
                HALF_GRID_NM
            } else {
                GRID_NM
            };
            if x % grid != 0 || y % grid != 0 {
                let span = child(items, "at").map_or_else(|| head_span(pin.node), |at| at.span);
                let cause = "off the 2.54 mm grid".to_string();
                found(&STYLE_PIN_GRID, cause, span, pin);
            }
        }
        let text_size = ["name", "number"].into_iter().find_map(|attribute| {
            let size = descend(child(items, attribute)?, &["effects", "font", "size"])?;
            let height = number(size.as_list()?.get(1)?)?;
            (nm(height) != TEXT_SIZE_NM).then_some((attribute, height, size.span))
        });
        if let Some((attribute, height, span)) = text_size {
            let cause = format!("{attribute} text is {height} mm, not 1.27 mm");
            found(&STYLE_TEXT_SIZE, cause, span, pin);
        }
    }
    for (cause, (rule, span, pins)) in causes {
        out.push(rule, 0, span, format!("{cause}: {}", describe_all(&pins)));
    }
}

/// Pin types that contradict how a pin is drawn or named. Pins that share a
/// cause share a warning: a ball-grid memory can have a hundred `NC` balls.
fn check_pin_types(body: &Body, out: &mut Reporter) {
    let is_power_symbol = child(body.def.items(), "power").is_some();
    // On a relay or switch, `NC` beside `NO` is the normally-closed contact.
    let contacts: BTreeSet<u32> = body
        .pins
        .iter()
        .filter(|pin| pin.name().eq_ignore_ascii_case("NO"))
        .map(|pin| pin.unit)
        .collect();
    let mut hidden_power: BTreeMap<&str, Vec<&Pin>> = BTreeMap::new();
    let mut nc: BTreeMap<(&str, &str), Vec<&Pin>> = BTreeMap::new();
    for pin in &body.pins {
        if pin.parsed.hidden && pin.electrical_type() == "power_in" && !is_power_symbol {
            hidden_power.entry(pin.name()).or_default().push(pin);
        }
        if NC_NAMES
            .iter()
            .any(|nc| pin.name().eq_ignore_ascii_case(nc))
            && !pin.is_no_connect()
            && pin.electrical_type() != "free"
            && !(pin.name().eq_ignore_ascii_case("NC")
                && contacts.iter().any(|unit| coexist(*unit, pin.unit)))
        {
            nc.entry((pin.name(), pin.electrical_type()))
                .or_default()
                .push(pin);
        }
    }
    for pins in hidden_power.values() {
        let verb = if pins.len() == 1 { "creates" } else { "create" };
        let message = format!(
            "hidden power_in {} {verb} an implicit global net",
            describe_all(pins)
        );
        out.push(
            &PIN_HIDDEN_POWER,
            pins[0].unit,
            head_span(pins[0].node),
            message,
        );
    }
    for ((_, kind), pins) in &nc {
        let verb = if pins.len() == 1 { "is" } else { "are" };
        let message = format!("{} {verb} `{kind}`, not `no_connect`", describe_all(pins));
        out.push(&PIN_NC_TYPE, pins[0].unit, head_span(pins[0].node), message);
    }
}

/// Whether every pin has a number that no other pin contradicts.
fn check_numbering(body: &Body, out: &mut Reporter) -> bool {
    let jumpers = child(body.def.items(), "duplicate_pin_numbers_are_jumpers")
        .and_then(Sexpr::as_list)
        .and_then(|flag| flag.get(1))
        .and_then(parse_bool_atom)
        .unwrap_or(false);
    let mut sound = true;
    let mut by_number: BTreeMap<&str, Vec<&Pin>> = BTreeMap::new();
    for pin in &body.pins {
        if pin.number().is_empty() {
            sound = false;
            let message = format!("pin named \"{}\" has no number", pin.name());
            let span = pin.value_span("number");
            out.push(&PIN_EMPTY_NUMBER, pin.unit, span, message);
            continue;
        }
        let earlier = by_number.entry(pin.number()).or_default();
        // Alternate body styles redraw the same pins; units share one frame each.
        let conflict = earlier.iter().find(|prev| {
            !jumpers
                && coexist(prev.style, pin.style)
                && (prev.name() != pin.name()
                    || (coexist(prev.unit, pin.unit) && prev.at != pin.at))
        });
        if let Some(prev) = conflict {
            sound = false;
            let message = format!(
                "pin \"{}\" is \"{}\" at {} and \"{}\" at {}",
                pin.number(),
                prev.name(),
                prev.at_text(),
                pin.name(),
                pin.at_text()
            );
            let span = pin.value_span("number");
            out.push(&PIN_DUPLICATE, pin.unit, span, message);
        }
        earlier.push(pin);
    }
    sound
}

fn check_stacks(body: &Body, out: &mut Reporter) {
    let mut by_location: BTreeMap<(u32, u32, i64, i64), Vec<&Pin>> = BTreeMap::new();
    for pin in &body.pins {
        if let Some((x, y)) = pin.at {
            by_location
                .entry((pin.unit, pin.style, x, y))
                .or_default()
                .push(pin);
        }
    }
    for stack in by_location.values().filter(|stack| stack.len() > 1) {
        let first = stack[0];
        let same_name = stack.iter().all(|pin| pin.name() == first.name());
        // A repeated number at one location is the duplicate check's concern,
        // and no-connect pins take no wire, so a stack of them hides nothing.
        let others: Vec<&Pin> = stack[1..]
            .iter()
            .copied()
            .filter(|pin| pin.name() != first.name() && pin.number() != first.number())
            .filter(|pin| !(pin.is_no_connect() && first.is_no_connect()))
            .collect();
        if let Some(pin) = others.first() {
            let verb = if others.len() == 1 { "sits" } else { "sit" };
            let message = format!(
                "{} {verb} on {} at {}",
                describe_all(&others),
                first.describe(),
                first.at_text()
            );
            out.push(&PIN_OVERLAP, pin.unit, head_span(pin.node), message);
        }
        // Registry convention stacks hidden no-connect pins.
        if !same_name || stack.iter().all(|pin| pin.is_no_connect()) {
            continue;
        }
        let visible = stack.iter().filter(|pin| !pin.parsed.hidden).count();
        if visible != 1 {
            let message = format!(
                "pin stack \"{}\" at {} has {visible} visible pins",
                first.name(),
                first.at_text()
            );
            out.push(&STYLE_STACK, first.unit, head_span(first.node), message);
        }
        // Hidden power inputs are already a warning.
        for pin in stack.iter().filter(|pin| {
            pin.parsed.hidden && !matches!(pin.electrical_type(), "passive" | "power_in")
        }) {
            let message = format!(
                "hidden stack member {} is `{}`",
                pin.describe(),
                pin.electrical_type()
            );
            out.push(&STYLE_STACK, pin.unit, head_span(pin.node), message);
        }
    }
}

fn check_power_names(body: &Body, out: &mut Reporter) {
    let mut power: BTreeMap<&str, [Option<&Pin>; 2]> = BTreeMap::new();
    for pin in &body.pins {
        let slot = match pin.electrical_type() {
            "power_in" => 0,
            "power_out" => 1,
            _ => continue,
        };
        if !is_placeholder_kicad_pin_name(pin.name()) {
            power.entry(pin.name()).or_default()[slot].get_or_insert(pin);
        }
    }
    for (name, types) in &power {
        if let [Some(input), Some(output)] = types {
            let message = format!(
                "\"{name}\" is power_in on pin \"{}\" and power_out on pin \"{}\"",
                input.number(),
                output.number()
            );
            let span = head_span(output.node);
            out.push(&PIN_POWER_CONFLICT, output.unit, span, message);
        }
    }
}

/// A symbol without any pins is mechanical; a pinless unit among pinned ones
/// is a gap.
fn check_units(body: &Body, out: &mut Reporter) {
    if body.units.is_empty() && body.pins.is_empty() {
        let message = "symbol draws nothing: it has no units, graphics or pins".to_string();
        out.push(&UNIT_EMPTY, 0, body.def.name_span(), message);
    } else if !body.pins.is_empty() {
        for (unit, span) in body.units.iter().filter(|(unit, _)| **unit > 0) {
            if !body.pins.iter().any(|pin| coexist(pin.unit, *unit)) {
                out.push(&UNIT_EMPTY, 0, *span, format!("unit {unit} has no pins"));
            }
        }
    }
}

fn check_footprint(
    body: &Body,
    loaded: &KicadSymbol,
    footprint: FootprintPads,
    out: &mut Reporter,
) {
    let numbers: BTreeSet<&str> = loaded.pins().iter().map(KicadPin::number).collect();
    let padless: Vec<&str> = numbers
        .iter()
        .copied()
        .filter(|number| !footprint.numbers.contains(*number))
        .collect();
    if let Some(first) = padless.first() {
        let message = format!(
            "{} {} no pad in {}",
            list("pin", padless.iter().map(|number| format!("\"{number}\""))),
            if padless.len() == 1 { "has" } else { "have" },
            footprint.name
        );
        // A native stack such as `[1-4]` has no pin written with this number.
        let drawn = body.pins.iter().find(|pin| pin.number() == *first);
        let span = drawn.map_or(body.def.name_span(), |pin| pin.value_span("number"));
        out.push(&PIN_NO_PAD, 0, span, message);
    }
    let pinless: Vec<&str> = footprint
        .numbers
        .iter()
        .map(String::as_str)
        .filter(|pad| !pad.is_empty() && !numbers.contains(pad))
        .collect();
    if !pinless.is_empty() {
        let message = format!(
            "{} of {} {} no symbol pin",
            list("pad", pinless.iter().map(|pad| format!("\"{pad}\""))),
            footprint.name,
            if pinless.len() == 1 { "has" } else { "have" }
        );
        let span = body.def.name_span();
        out.push(&PAD_NO_PIN, 0, span, message);
    }
}

fn check_layout(body: &Body, out: &mut Reporter) {
    if let Some(pin_names) = child(body.def.items(), "pin_names")
        && let Some(offset) = child(pin_names.as_list().unwrap_or_default(), "offset")
        && let Some(mm) = offset.as_list().and_then(|offset| number(offset.get(1)?))
        && !is_hidden(pin_names)
        // An offset of 0 draws names outside the body, where no range applies.
        && nm(mm) != 0
        && !NAME_OFFSET_NM.contains(&nm(mm))
    {
        let message = format!("pin-name offset is {mm} mm");
        out.push(&STYLE_NAME_OFFSET, 0, offset.span, message);
    }

    for (unit, node, _) in &body.outlines {
        if let Some(width) = descend(node, &["stroke", "width"])
            && let Some(mm) = width.as_list().and_then(|width| number(width.get(1)?))
            && nm(mm) != OUTLINE_NM
        {
            let message = format!("body outline stroke is {mm} mm, not 0.254 mm");
            out.push(&STYLE_OUTLINE, *unit, width.span, message);
        }
    }

    // KLC S3.1: a unit is centred by its one body rectangle, else by its pins.
    // Sections shared by every unit form the only unit of a symbol without any.
    let mut units: Vec<(u32, Span)> = body
        .units
        .iter()
        .filter(|(unit, _)| **unit > 0)
        .map(|(unit, span)| (*unit, *span))
        .collect();
    if units.is_empty() {
        units.push((0, body.def.name_span()));
    }
    for (unit, span) in units {
        let outlines: Vec<_> = body
            .outlines
            .iter()
            .filter(|(owner, ..)| coexist(*owner, unit))
            .collect();
        let (bounds, span) = match outlines.as_slice() {
            [(_, node, bounds)] => (*bounds, head_span(node)),
            _ => {
                let at = || {
                    body.pins
                        .iter()
                        .filter(move |pin| coexist(pin.unit, unit))
                        .filter_map(|pin| pin.at)
                };
                let (Some(x0), Some(x1), Some(y0), Some(y1)) = (
                    at().map(|at| at.0).min(),
                    at().map(|at| at.0).max(),
                    at().map(|at| at.1).min(),
                    at().map(|at| at.1).max(),
                ) else {
                    continue;
                };
                ([x0, y0, x1, y1], span)
            }
        };
        let centre = ((bounds[0] + bounds[2]) / 2, (bounds[1] + bounds[3]) / 2);
        if centre.0.abs() > HALF_GRID_NM || centre.1.abs() > HALF_GRID_NM {
            let message = format!(
                "centred on ({}, {}), not the origin",
                centre.0 as f64 / 1e6,
                centre.1 as f64 / 1e6
            );
            out.push(&STYLE_ORIGIN, unit, span, message);
        }
    }
}

fn pin(unit: u32, style: u32, node: &Sexpr) -> Pin<'_> {
    let items = node.as_list().unwrap_or_default();
    let parsed = parse_pin_common(items);
    Pin {
        unit,
        style,
        node,
        items,
        at: parsed.at.as_ref().map(|at| (nm(at.x), nm(at.y))),
        parsed,
    }
}

/// Unit and style 0 are shared by every unit and style.
fn coexist(a: u32, b: u32) -> bool {
    a == b || a == 0 || b == 0
}

/// Units drop the library nickname of a `<library>:<symbol>` name.
fn unit_prefix(symbol: &str) -> &str {
    symbol.split_once(':').map_or(symbol, |(_, item)| item)
}

/// `name` under the prefix of `symbol`, when it ends in `_<unit>_<style>`.
fn unit_rename(name: &str, symbol: &str) -> Option<String> {
    let mut parts = name.rsplitn(3, '_');
    let mut index = || parts.next()?.parse::<u32>().ok();
    let (style, unit) = (index()?, index()?);
    Some(format!("{}_{unit}_{style}", unit_prefix(symbol)))
}

fn is_unit_name(name: &str, symbol: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix(unit_prefix(symbol))
        .and_then(|rest| rest.strip_prefix('_'))
    else {
        return false;
    };
    let mut parts = suffix.split('_');
    let mut index = || parts.next().is_some_and(|part| part.parse::<u32>().is_ok());
    index() && index() && parts.next().is_none()
}

fn rectangle_bounds(node: &Sexpr) -> Option<[i64; 4]> {
    let corner = |name: &str| {
        let corner = child(node.as_list()?, name)?.as_list()?;
        Some((nm(number(corner.get(1)?)?), nm(number(corner.get(2)?)?)))
    };
    let (start, end) = (corner("start")?, corner("end")?);
    Some([
        start.0.min(end.0),
        start.1.min(end.1),
        start.0.max(end.0),
        start.1.max(end.1),
    ])
}

fn is_hidden(node: &Sexpr) -> bool {
    node.as_list().unwrap_or_default().iter().any(|item| {
        item.as_sym() == Some("hide")
            || item.as_list().is_some_and(|hide| {
                hide.first().and_then(Sexpr::as_sym) == Some("hide")
                    && hide.get(1).and_then(parse_bool_atom).unwrap_or(true)
            })
    })
}

/// Direct child lists of `items` whose head is one of `heads`.
fn sections<'a>(items: &'a [Sexpr], heads: &'a [&str]) -> impl Iterator<Item = &'a Sexpr> {
    items.iter().skip(1).filter(|item| {
        item.as_list()
            .and_then(|list| list.first()?.as_sym())
            .is_some_and(|head| heads.contains(&head))
    })
}

fn has_section(items: &[Sexpr], heads: &[&str]) -> bool {
    sections(items, heads).next().is_some()
}

fn child<'a>(items: &'a [Sexpr], head: &str) -> Option<&'a Sexpr> {
    items
        .iter()
        .skip(1)
        .find(|item| item.as_list().and_then(|list| list.first()?.as_sym()) == Some(head))
}

fn descend<'a>(node: &'a Sexpr, path: &[&str]) -> Option<&'a Sexpr> {
    path.iter()
        .try_fold(node, |node, head| child(node.as_list()?, head))
}

/// Span of a list's opening line: its head and the atoms that follow it.
fn head_span(node: &Sexpr) -> Span {
    let end = node
        .as_list()
        .unwrap_or_default()
        .iter()
        .take_while(|item| item.as_list().is_none())
        .last()
        .map_or(node.span.end, |item| item.span.end);
    Span::new(node.span.start, end)
}

fn number(node: &Sexpr) -> Option<f64> {
    node.as_float().or_else(|| node.as_int().map(|v| v as f64))
}

fn nm(mm: f64) -> i64 {
    (mm * 1e6).round() as i64
}

/// An atom as written in the file.
fn raw(node: &Sexpr) -> &str {
    node.raw_atom
        .as_deref()
        .or_else(|| node.as_atom())
        .unwrap_or("(…)")
}

fn quoted(node: &Sexpr) -> String {
    format!("\"{}\"", raw(node))
}

/// One pin as `pin "5" (VBUS)`; several as `pins "5", "7" (VBUS)` when they
/// share a name, else each with its own.
fn describe_all(pins: &[&Pin]) -> String {
    let [first, rest @ ..] = pins else {
        return String::new();
    };
    if rest.is_empty() {
        return first.describe();
    }
    let named = |pin: &Pin| !is_placeholder_kicad_pin_name(pin.name());
    if rest.iter().all(|pin| pin.name() == first.name()) {
        let numbers = list(
            "pin",
            pins.iter().map(|pin| format!("\"{}\"", pin.number())),
        );
        return match named(first) {
            true => format!("{numbers} ({})", first.name()),
            false => numbers,
        };
    }
    let each = pins.iter().map(|pin| match named(pin) {
        true => format!("\"{}\" ({})", pin.number(), pin.name()),
        false => format!("\"{}\"", pin.number()),
    });
    list("pin", each)
}

/// `pin "7"`, `pins "7", "8"`, or the first eight followed by a count.
fn list(noun: &str, items: impl ExactSizeIterator<Item = String>) -> String {
    const SHOWN: usize = 8;
    let count = items.len();
    let shown: Vec<String> = items.take(SHOWN).collect();
    let more = match count.saturating_sub(SHOWN) {
        0 => String::new(),
        more => format!(" and {more} more"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{noun}{plural} {}{more}", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: &str = r#"(kicad_symbol_lib
  (version 20251024)
  (symbol "U"
    (pin_names (offset 0.508))
    (property "Reference" "U")
    (property "Value" "U")
    (symbol "U_0_1"
      (rectangle (start -2.54 2.54) (end 2.54 -2.54) (stroke (width 0.254)) (fill (type background))))
    (symbol "U_1_1"
      (pin input line (at -5.08 0 0) (length 2.54)
        (name "IN" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin power_in line (at 5.08 0 180) (length 2.54)
        (name "VCC" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))))
  (symbol "D" (extends "U") (property "Value" "D")))"#;

    fn check(source: &str, name: &str, footprint: Option<FootprintPads>) -> Vec<SymbolIssue> {
        let library = KicadSymbolLibrary::from_string(source).expect("library should scan");
        check_symbol(&library, name, footprint)
    }

    fn kinds(source: &str, name: &str) -> Vec<&'static str> {
        check(source, name, None)
            .iter()
            .map(|issue| issue.kind)
            .collect()
    }

    #[test]
    fn fixes_leave_what_kicad_would_have_written() {
        let dirty = [
            (
                "(version 20251024)",
                "(version 20251024) ; generated\n  ;; \"U\" (draft",
            ),
            ("(type background)", "(type solid)"),
            ("(at -5.08 0 0)", "(at -5.08 -0.0 1e-15)"),
            ("\"U_1_1\"", "\"Draft_1_1\""),
            (
                "(name \"IN\" (effects (font (size 1.27 1.27))))",
                "(name \"IN\" (effects (font (size 1.27 1.27)) (justify center)))",
            ),
            (
                "(name \"VCC\" (effects (font (size 1.27 1.27))))",
                "(name \"VCC\" (effects (font (size 1.27 1.27)) (justify center left)))",
            ),
        ];
        let dirty = dirty.iter().fold(CLEAN.to_string(), |text, (from, to)| {
            text.replacen(from, to, 1)
        });
        let fixed = CLEAN
            .replacen("(type background)", "(type outline)", 1)
            .replacen(
                "(name \"VCC\" (effects (font (size 1.27 1.27))))",
                "(name \"VCC\" (effects (font (size 1.27 1.27)) (justify left)))",
                1,
            );
        assert_eq!(fix(vec![dirty], "U"), [fixed]);

        // What takes a decision is reported and left as written.
        for (from, to) in [
            ("\"U_1_1\"", "\"Draft\""),
            ("\"U_0_1\"", "\"Draft_1_1\""),
            ("(type background)", "(type filled)"),
            ("(number \"2\"", "(number \"1\""),
        ] {
            let source = CLEAN.replacen(from, to, 1);
            assert!(!kinds(&source, "U").is_empty(), "{to}");
            assert_eq!(fix(vec![source.clone()], "U"), [source], "{to}");
        }
    }

    #[test]
    fn each_defect_is_one_issue_of_its_kind() {
        assert!(kinds(CLEAN, "U").is_empty());
        assert!(kinds(CLEAN, "D").is_empty());
        assert!(kinds(&CLEAN.replacen("(offset 0.508)", "(offset .508)", 1), "U").is_empty());
        let library_issue = |sources: Vec<String>| {
            let library = KicadSymbolLibrary::from_sources(sources).unwrap();
            check_library(&library).map(|issue| (issue.source, issue.message))
        };
        assert_eq!(library_issue(vec![CLEAN.to_string()]), None);
        for (from, to, message) in [
            ("))\n  (symbol \"D\"", ")\n  (symbol \"D\"", "unclosed `(`"),
            (
                "(symbol \"U\"",
                "; the part\n  (symbol \"U\"",
                "starts a comment",
            ),
            ("(version 20251024)", "", "does not open with"),
        ] {
            // Every file of a split library is held to this, not only the first.
            let broken = CLEAN.replacen(from, to, 1);
            let (source, found) = library_issue(vec![CLEAN.to_string(), broken]).unwrap();
            assert!(source == 1 && found.contains(message), "{from:?}: {found}");
        }

        let defects: &[(&[(&str, &str)], &str)] = &[
            (&[("pin input", "pin open_drain")], "symbol.parse"),
            (&[("(at -5.08 0 0)", "(at -5.08 0 45)")], "symbol.parse"),
            (&[("(type background)", "(type solid)")], "symbol.parse"),
            (
                &[("(size 1.27 1.27))", "(size 1.27 1.27)) (justify center)")],
                "symbol.parse",
            ),
            (
                &[(
                    "(property \"Reference\"",
                    "(offset 0) (property \"Reference\"",
                )],
                "symbol.parse",
            ),
            (&[("(rectangle", "outline (rectangle")], "symbol.parse"),
            (
                &[("(number \"1\"", "(number \"\"")],
                "symbol.pin.empty_number",
            ),
            (
                &[("(number \"2\"", "(number \"1\"")],
                "symbol.pin.duplicate",
            ),
            (&[("\"U_1_1\"", "\"X_1_1\"")], "symbol.unit.naming"),
            (
                &[("(property \"Value\" \"U\")", "")],
                "symbol.property.missing",
            ),
            (
                &[("\"Value\" \"U\"", "\"Value\" \"X\"")],
                "style.symbol.value_name",
            ),
            (
                &[("(at 5.08 0 180)", "(at 5.08 0 180) (hide yes)")],
                "symbol.pin.hidden_power",
            ),
            (
                &[("(at 5.08 0 180)", "(at -5.08 0 0)")],
                "symbol.pin.overlap",
            ),
            (&[("\"IN\"", "\"NC\"")], "symbol.pin.nc_type"),
            (
                &[("pin input", "pin power_out"), ("\"IN\"", "\"VCC\"")],
                "symbol.pin.power_conflict",
            ),
            (
                &[("(length 2.54)", "(length 2.54e0)")],
                "symbol.number.format",
            ),
            (
                &[("(symbol \"U_1_1\"", "(symbol \"U_2_1\") (symbol \"U_1_1\"")],
                "symbol.unit.empty",
            ),
            (
                &[("(at -5.08 0 0)", "(at -5.08 1.27 0)")],
                "style.symbol.pin.grid",
            ),
            (
                &[("(length 2.54)", "(length 2)")],
                "style.symbol.pin.length",
            ),
            (
                &[("(start -2.54 2.54) (end 2.54", "(start 0 2.54) (end 7.62")],
                "style.symbol.origin",
            ),
            (&[("(width 0.254)", "(width 0)")], "style.symbol.outline"),
            (
                &[("(size 1.27 1.27)", "(size 1 1)")],
                "style.symbol.text_size",
            ),
            (
                &[("(offset 0.508)", "(offset 2.54)")],
                "style.symbol.name_offset",
            ),
            (
                &[("(at 5.08 0 180)", "(at -5.08 0 0)"), ("\"VCC\"", "\"IN\"")],
                "style.symbol.stack",
            ),
            (&[("(extends \"U\")", "(extends \"W\")")], "symbol.extends"),
        ];
        for (edits, kind) in defects {
            let source = edits.iter().fold(CLEAN.to_string(), |source, (from, to)| {
                source.replacen(from, to, 1)
            });
            let name = if *kind == "symbol.extends" { "D" } else { "U" };
            assert_eq!(kinds(&source, name), [*kind], "{edits:?}");
        }
    }

    #[test]
    fn contact_names_and_no_connect_stacks_are_not_findings() {
        // `NC` beside `NO` is a relay's normally-closed contact.
        let relay = CLEAN
            .replacen("\"IN\"", "\"NC\"", 1)
            .replacen("\"VCC\"", "\"NO\"", 1)
            .replacen("pin power_in", "pin passive", 1);
        assert!(kinds(&relay, "U").is_empty());
        let do_not_use = relay.replacen("\"NC\"", "\"DNU\"", 1);
        assert_eq!(kinds(&do_not_use, "U"), ["symbol.pin.nc_type"]);

        let stack = CLEAN
            .replacen("pin input", "pin no_connect", 1)
            .replacen("pin power_in", "pin no_connect", 1)
            .replacen("\"IN\"", "\"NC_1\"", 1)
            .replacen("\"VCC\"", "\"NC_2\"", 1)
            .replacen("(at 5.08 0 180)", "(at -5.08 0 0)", 1);
        assert!(kinds(&stack, "U").is_empty());
    }

    #[test]
    fn pins_with_one_cause_share_a_warning() {
        let source = CLEAN
            .replacen("pin input", "pin passive", 1)
            .replacen("pin power_in", "pin passive", 1)
            .replacen("\"IN\"", "\"NC\"", 1)
            .replacen("\"VCC\"", "\"NC\"", 1);
        let messages: Vec<String> = check(&source, "U", None)
            .into_iter()
            .map(|issue| issue.message)
            .collect();
        assert_eq!(
            messages,
            ["U: pins \"1\", \"2\" (NC) are `passive`, not `no_connect`"]
        );

        let source = CLEAN
            .replacen("(at -5.08 0 0)", "(at -5.08 1.27 0)", 1)
            .replacen("(at 5.08 0 180)", "(at 5.08 1.27 180)", 1)
            .replacen("(length 2.54)", "(length 2)", 1);
        let issues = check(&source, "U", None);
        let messages: Vec<&str> = issues.iter().map(|issue| &*issue.message).collect();
        assert_eq!(
            messages,
            [
                "U: off the 2.54 mm grid: pins \"1\" (IN), \"2\" (VCC)",
                "U: length 2 mm is not a multiple of 1.27 mm: pin \"1\" (IN)",
            ]
        );
        let span = issues[0].span;
        assert_eq!(&source[span.start..span.end], "(at -5.08 1.27 0)");
    }

    #[test]
    fn pin_findings_of_a_parent_reach_the_symbols_that_extend_it() {
        let source = CLEAN.replacen("(number \"2\"", "(number \"1\"", 1);
        let issues = check(&source, "D", None);
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].message,
            "U: pin \"1\" is \"IN\" at (-5.08, 0) and \"VCC\" at (5.08, 0)"
        );
        let span = issues[0].span;
        assert_eq!(&source[span.start..span.end], "\"1\"");
        assert!(span.start > source.find("power_in").unwrap());

        // A parent in the middle of the chain is held to the same forms.
        let middle = "(symbol \"M\" (extends \"U\") (offset 0) (property \"Value\" \"M\"))";
        let source = CLEAN
            .replacen("(extends \"U\")", "(extends \"M\")", 1)
            .replacen("(symbol \"D\"", &format!("{middle} (symbol \"D\""), 1);
        let issues = check(&source, "D", None);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].message.starts_with("M: `offset`"));
    }

    #[test]
    fn pins_and_pads_are_compared_in_both_directions() {
        let messages = |source: &str, pads: &[&str]| -> Vec<String> {
            let numbers = pads.iter().map(|pad| pad.to_string()).collect();
            let footprint = FootprintPads {
                name: "U.kicad_mod",
                numbers: &numbers,
            };
            check(source, "U", Some(footprint))
                .into_iter()
                .filter(|issue| matches!(issue.kind, "symbol.pin.no_pad" | "symbol.pad.no_pin"))
                .map(|issue| issue.message)
                .collect()
        };
        assert!(messages(CLEAN, &["1", "2"]).is_empty());
        // An outline footprint and a pinless symbol have no pinout to compare.
        assert!(messages(CLEAN, &[]).is_empty());
        let pinless = r#"(kicad_symbol_lib (version 20251024) (symbol "U"
            (property "Reference" "U") (property "Value" "U")))"#;
        assert!(messages(pinless, &["1"]).is_empty());
        assert!(messages(CLEAN, &["", "1", "2"]).is_empty());
        assert_eq!(
            messages(CLEAN, &[""]),
            [r#"U: pins "1", "2" have no pad in U.kicad_mod"#]
        );
        assert_eq!(
            messages(CLEAN, &["1"]),
            ["U: pin \"2\" has no pad in U.kicad_mod"]
        );
        assert_eq!(
            messages(CLEAN, &["1", "2", "3", "EP"]),
            ["U: pads \"3\", \"EP\" of U.kicad_mod have no symbol pin"]
        );

        // Loading keeps one body style per unit; the other adds no pins.
        let alternate = CLEAN.replacen(
            "(symbol \"U_1_1\"",
            "(symbol \"U_1_2\" (pin passive line (at 0 5.08 270) (length 2.54) (name \"~\") (number \"3\")))
             (symbol \"U_1_1\"",
            1,
        );
        assert!(messages(&alternate, &["1", "2"]).is_empty());

        // A native stack is the pins it expands to.
        let stacked = CLEAN.replacen("(number \"2\"", "(number \"[2,3]\"", 1);
        assert!(messages(&stacked, &["1", "2", "3"]).is_empty());
        assert_eq!(
            messages(&stacked, &["1", "2"]),
            ["U: pin \"3\" has no pad in U.kicad_mod"]
        );
    }
}
