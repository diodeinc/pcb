//! A known-error preflight, not a replacement for KiCad's rule compiler.
//!
//! The lexical/schema contract follows KiCad 10.0.6's `common/dsnlexer.cpp`
//! and `pcbnew/drc/drc_rule_parser.cpp`. Numeric arithmetic follows
//! `common/libeval_compiler/{grammar.lemon,libeval_compiler.cpp}`. Conditions,
//! board-dependent layer names, and text-variable expansion remain KiCad's
//! responsibility. In particular, passing this check does not prove that
//! KiCad compiled or applied every rule.

use anyhow::{Context, Result, anyhow, bail};
use pcb_sexpr::{Sexpr, SexprKind, Span};
use std::collections::HashSet;
use std::path::Path;

pub(super) fn preflight(board: &Path) -> Result<()> {
    let path = board.with_extension("kicad_dru");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let warnings = check(&path, &source)?;
    for warning in warnings {
        log::warn!("Custom-rule preflight incomplete: {warning}");
    }
    Ok(())
}

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0)
}

struct Checker<'a> {
    path: &'a Path,
    source: &'a str,
    rule: Option<String>,
    warnings: Vec<String>,
}

impl Checker<'_> {
    fn message(&self, offset: usize, message: impl std::fmt::Display) -> String {
        let prefix = &self.source[..offset];
        let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
        let column = prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1;
        let rule = self
            .rule
            .as_ref()
            .map_or(String::new(), |name| format!("rule {name:?}: "));
        format!("{}:{line}:{column}: {rule}{message}", self.path.display())
    }

    fn error(&self, offset: usize, message: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("{}\nKiCad DRC was not run.", self.message(offset, message))
    }

    fn defer(&mut self, offset: usize, message: impl std::fmt::Display) {
        self.warnings.push(self.message(offset, message));
    }

    fn atom<'a>(&self, node: &'a Sexpr) -> Result<&'a str> {
        node.as_sym()
            .or_else(|| node.as_str())
            .ok_or_else(|| self.error(node.span.start, "expected a value, not a list"))
    }

    fn form<'a>(&self, node: &'a Sexpr) -> Result<(&'a str, &'a [Sexpr])> {
        let items = node
            .as_list()
            .ok_or_else(|| self.error(node.span.start, "expected '(keyword ...)'"))?;
        let keyword = items
            .first()
            .and_then(Sexpr::as_sym)
            .ok_or_else(|| self.error(node.span.start, "missing keyword"))?;
        Ok((keyword, &items[1..]))
    }

    fn one<'a>(&self, node: &Sexpr, values: &'a [Sexpr]) -> Result<&'a Sexpr> {
        if values.len() != 1 {
            return Err(self.error(node.span.start, "expected exactly one value"));
        }
        Ok(&values[0])
    }

    fn constraint(
        &mut self,
        node: &Sexpr,
        values: &[Sexpr],
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let Some(kind) = values.first().and_then(Sexpr::as_sym) else {
            return Err(self.error(node.span.start, "missing constraint type"));
        };
        let kind = match kind {
            "mechanical_clearance" => "physical_clearance",
            "mechanical_hole_clearance" => "physical_hole_clearance",
            "hole" => "hole_size",
            other => other,
        };
        if !seen.insert(kind.to_owned()) {
            return Err(self.error(node.span.start, format!("duplicate {kind} constraint")));
        }
        let values = &values[1..];
        match kind {
            "assertion" => {
                self.atom(self.one(node, values)?)?;
                self.defer(
                    node.span.start,
                    "assertion expression compilation is left to KiCad",
                );
            }
            "min_resolved_spokes" => {
                let value = self.one(node, values)?;
                if !value
                    .as_sym()
                    .is_some_and(|value| value.parse::<f64>().is_ok())
                {
                    return Err(
                        self.error(node.span.start, "min_resolved_spokes requires a number")
                    );
                }
            }
            "disallow" => {
                for value in values {
                    if !matches!(
                        self.atom(value)?,
                        "track"
                            | "via"
                            | "through_via"
                            | "blind_via"
                            | "buried_via"
                            | "micro_via"
                            | "pad"
                            | "zone"
                            | "text"
                            | "graphic"
                            | "hole"
                            | "footprint"
                    ) {
                        return Err(self.error(value.span.start, "invalid disallow item type"));
                    }
                }
            }
            "zone_connection" => {
                if !matches!(
                    self.atom(self.one(node, values)?)?,
                    "solid" | "thermal_reliefs" | "none"
                ) {
                    return Err(self.error(
                        node.span.start,
                        "zone_connection requires solid, thermal_reliefs, or none",
                    ));
                }
            }
            "clearance"
            | "creepage"
            | "hole_clearance"
            | "edge_clearance"
            | "hole_size"
            | "hole_to_hole"
            | "courtyard_clearance"
            | "silk_clearance"
            | "text_height"
            | "text_thickness"
            | "track_width"
            | "track_angle"
            | "track_segment_length"
            | "connection_width"
            | "annular_width"
            | "via_diameter"
            | "via_dangling"
            | "thermal_relief_gap"
            | "thermal_spoke_width"
            | "solder_mask_expansion"
            | "solder_mask_sliver"
            | "solder_paste_abs_margin"
            | "solder_paste_rel_margin"
            | "length"
            | "skew"
            | "via_count"
            | "diff_pair_gap"
            | "diff_pair_uncoupled"
            | "physical_clearance"
            | "physical_hole_clearance"
            | "bridged_mask" => {
                let unitless = matches!(
                    kind,
                    "via_count" | "track_angle" | "via_dangling" | "bridged_mask"
                );
                let time_allowed = matches!(kind, "length" | "skew");
                let mut domain = None;
                for bound in values {
                    let (name, expr) = self.form(bound)?;
                    if name == "within_diff_pairs" && kind == "skew" && expr.is_empty() {
                        continue;
                    }
                    if !matches!(name, "min" | "max" | "opt") {
                        return Err(
                            self.error(bound.span.start, format!("invalid {kind} option {name:?}"))
                        );
                    }
                    if expr.is_empty() {
                        return Err(self
                            .error(bound.span.start, format!("missing {name} value for {kind}")));
                    }
                    let expression = expr
                        .iter()
                        .map(expression_text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    match arithmetic(&expression, unitless) {
                        Ok(Some(units)) => {
                            if units == Domain::Time && !time_allowed {
                                return Err(self.error(bound.span.start, format!("time units are not allowed for {kind}")));
                            }
                            if domain.is_some_and(|previous| previous != units) {
                                return Err(self.error(bound.span.start, format!("mixed unit domains in {kind} bounds")));
                            }
                            domain = Some(units);
                        }
                        Ok(None) => self.defer(bound.span.start, format!("{kind} {name} expression {expression:?} is outside the arithmetic preflight subset")),
                        Err(error) => return Err(self.error(bound.span.start, format!("{kind} {name}: {error}"))),
                    }
                }
            }
            _ => self.defer(
                node.span.start,
                format!("unrecognized constraint {kind:?}; left to KiCad"),
            ),
        }
        Ok(())
    }

    fn rules(&mut self, roots: &[Sexpr]) -> Result<()> {
        let mut version = false;
        for root in roots {
            let (name, values) = self.form(root)?;
            match name {
                "version" => {
                    let node = self.one(root, values)?;
                    let value = self.atom(node)?;
                    if node.as_sym().is_none() || value.parse::<f64>().is_err() {
                        return Err(self.error(root.span.start, "version requires a number"));
                    }
                    if value != "1" {
                        self.defer(
                            root.span.start,
                            format!("rule-file version {value:?} is not the supported version 1"),
                        );
                        return Ok(());
                    }
                    version = true;
                }
                "rule" => {
                    if !version {
                        return Err(
                            self.error(root.span.start, "missing version statement before rules")
                        );
                    }
                    let Some(name) = values.first() else {
                        return Err(self.error(root.span.start, "missing rule name"));
                    };
                    let name = self.atom(name)?;
                    if values[0].as_sym().is_some() && name.parse::<f64>().is_ok() {
                        return Err(self.error(
                            root.span.start,
                            "rule name must be a symbol or quoted string",
                        ));
                    }
                    self.rule = Some(name.to_owned());
                    let mut seen = HashSet::new();
                    let mut layer = false;
                    for clause in &values[1..] {
                        let (keyword, args) = self.form(clause)?;
                        match keyword {
                            "constraint" => self.constraint(clause, args, &mut seen)?,
                            "condition" => {
                                self.atom(self.one(clause, args)?)?;
                                self.defer(
                                    clause.span.start,
                                    "condition expression compilation is left to KiCad",
                                );
                            }
                            "layer" => {
                                self.atom(self.one(clause, args)?)?;
                                self.defer(
                                    clause.span.start,
                                    "layer names require KiCad's board-dependent layer resolution",
                                );
                                if layer {
                                    self.defer(clause.span.start, "repeated layer clause needs KiCad's board-dependent layer resolution");
                                }
                                layer = true;
                            }
                            "severity" => {
                                let severity = self.one(clause, args)?;
                                if !matches!(
                                    severity.as_sym(),
                                    Some("ignore" | "warning" | "error" | "exclusion")
                                ) {
                                    return Err(self.error(
                                        severity.span.start,
                                        "severity requires ignore, warning, error, or exclusion",
                                    ));
                                }
                            }
                            _ => self.defer(
                                clause.span.start,
                                format!("unrecognized rule clause {keyword:?}; left to KiCad"),
                            ),
                        }
                    }
                    self.rule = None;
                }
                _ => self.defer(
                    root.span.start,
                    format!("unrecognized top-level form {name:?}; left to KiCad"),
                ),
            }
        }
        Ok(())
    }
}

fn check(path: &Path, source: &str) -> Result<Vec<String>> {
    let mut checker = Checker {
        path,
        source,
        rule: None,
        warnings: Vec::new(),
    };
    // KiCad expands source before lexing, using board/footprint/project context.
    // A replacement can even contain rule forms. A partial JSON resolver would
    // therefore both miss errors and incorrectly reject valid source.
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        if !line
            .trim_start_matches([' ', '\t', '\r', '\0'])
            .starts_with('#')
        {
            for (offset, _) in line.match_indices("${") {
                let rest = &line[offset + 2..];
                if !rest
                    .get(..6)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Class:"))
                {
                    checker.defer(start + offset, "text-variable expansion requires board context; this rule file was not preflighted");
                    return Ok(checker.warnings);
                }
            }
        }
        start += line.len();
    }
    let mut parser = Parser {
        checker: &mut checker,
        position: 0,
    };
    let mut roots = Vec::new();
    while parser.skip_space() {
        roots.push(parser.node(0)?);
    }
    checker.rules(&roots)?;
    Ok(checker.warnings)
}

struct Parser<'a, 's> {
    checker: &'a mut Checker<'s>,
    position: usize,
}

impl Parser<'_, '_> {
    fn skip_space(&mut self) -> bool {
        let bytes = self.checker.source.as_bytes();
        loop {
            while self.position < bytes.len() && space(bytes[self.position]) {
                self.position += 1;
            }
            if bytes.get(self.position) != Some(&b'#') {
                break;
            }
            let line_start = self.checker.source[..self.position]
                .rfind('\n')
                .map_or(0, |p| p + 1);
            if !bytes[line_start..self.position].iter().all(|&b| space(b)) {
                break;
            }
            while self.position < bytes.len() && bytes[self.position] != b'\n' {
                self.position += 1;
            }
        }
        self.position < bytes.len()
    }

    fn node(&mut self, depth: usize) -> Result<Sexpr> {
        let start = self.position;
        if depth > 256 {
            return Err(self
                .checker
                .error(start, "rule nesting exceeds preflight limit (256)"));
        }
        let bytes = self.checker.source.as_bytes();
        let kind = match bytes[start] {
            b'(' => {
                self.position += 1;
                let mut children = Vec::new();
                loop {
                    if !self.skip_space() {
                        return Err(self.checker.error(start, "unclosed '(' at end of file"));
                    }
                    if bytes[self.position] == b')' {
                        self.position += 1;
                        break;
                    }
                    children.push(self.node(depth + 1)?);
                    if depth == 0 && children.len() == 2 && children[0].as_sym() == Some("rule") {
                        self.checker.rule = children[1]
                            .as_sym()
                            .or_else(|| children[1].as_str())
                            .map(str::to_owned);
                    }
                }
                if depth == 0 {
                    self.checker.rule = None;
                }
                SexprKind::List(children)
            }
            b')' => return Err(self.checker.error(start, "unexpected ')'")),
            b'"' => {
                self.position += 1;
                let mut decoded = Vec::new();
                loop {
                    let Some(&byte) = bytes.get(self.position) else {
                        return Err(self.checker.error(start, "unterminated quoted string"));
                    };
                    if byte == b'\n' {
                        return Err(self
                            .checker
                            .error(start, "unterminated quoted string at end of line"));
                    }
                    self.position += 1;
                    match byte {
                        b'"' => break,
                        b'\\' => {
                            let Some(&escape) = bytes.get(self.position) else {
                                return Err(self
                                    .checker
                                    .error(start, "unterminated string escape"));
                            };
                            if escape == b'\n' {
                                return Err(self
                                    .checker
                                    .error(start, "unterminated quoted string at end of line"));
                            }
                            let value = match escape {
                                b'"' | b'\\' => {
                                    self.position += 1;
                                    escape
                                }
                                b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' => {
                                    self.position += 1;
                                    match escape {
                                        b'a' => 7,
                                        b'b' => 8,
                                        b'f' => 12,
                                        b'n' => b'\n',
                                        b'r' => b'\r',
                                        b't' => b'\t',
                                        _ => 11,
                                    }
                                }
                                _ => {
                                    let (radix, limit) = if escape == b'x' {
                                        self.position += 1;
                                        (16, 2)
                                    } else {
                                        (8, 3)
                                    };
                                    let mut value = 0u32;
                                    let mut digits = 0;
                                    while digits < limit {
                                        let Some(digit) = bytes
                                            .get(self.position)
                                            .and_then(|&b| (b as char).to_digit(radix))
                                        else {
                                            break;
                                        };
                                        value = value * radix + digit;
                                        self.position += 1;
                                        digits += 1;
                                    }
                                    if digits == 0 {
                                        if radix == 16 { b'x' } else { b'\\' }
                                    } else {
                                        value as u8
                                    }
                                }
                            };
                            decoded.push(value);
                        }
                        _ => decoded.push(byte),
                    }
                }
                let text = String::from_utf8(decoded).map_err(|_| {
                    self.checker
                        .error(start, "string escapes do not produce UTF-8")
                })?;
                SexprKind::String(text)
            }
            b'|' => {
                self.position += 1;
                SexprKind::Symbol("|".to_owned())
            }
            _ => {
                while self.position < bytes.len()
                    && !space(bytes[self.position])
                    && !matches!(bytes[self.position], b'(' | b')' | b'|')
                {
                    self.position += 1;
                }
                SexprKind::Symbol(self.checker.source[start..self.position].to_owned())
            }
        };
        Ok(Sexpr::with_span(kind, Span::new(start, self.position)))
    }
}

fn expression_text(node: &Sexpr) -> String {
    match &node.kind {
        SexprKind::List(children) => format!(
            "({})",
            children
                .iter()
                .map(expression_text)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        SexprKind::Symbol(text) | SexprKind::String(text) => text.clone(),
        _ => unreachable!("rule tokenizer preserves numeric atoms as symbols"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Domain {
    Unitless,
    Length,
    Angle,
    Time,
}

#[derive(Clone, Copy, Debug)]
enum NumericToken {
    Number(Domain),
    Operator(u8),
    Left,
    Right,
}

fn arithmetic(source: &str, unitless: bool) -> Result<Option<Domain>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0;
    let mut numbers = 0;
    let mut missing_units = None;
    let mut unexpected_units = false;
    while position < bytes.len() {
        match bytes[position] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                position += 1;
            }
            b'+' | b'-' | b'*' | b'/' => {
                tokens.push(NumericToken::Operator(bytes[position]));
                position += 1;
            }
            b'(' => {
                tokens.push(NumericToken::Left);
                position += 1;
            }
            b')' => {
                tokens.push(NumericToken::Right);
                position += 1;
            }
            b'0'..=b'9' => {
                let start = position;
                while position < bytes.len() && bytes[position].is_ascii_digit() {
                    position += 1;
                }
                if bytes.get(position).is_some_and(|&b| b == b'.' || b == b',') {
                    position += 1;
                    while position < bytes.len() && bytes[position].is_ascii_digit() {
                        position += 1;
                    }
                }
                let literal = &source[start..position];
                while bytes.get(position) == Some(&b' ') {
                    position += 1;
                }
                let unit_start = position;
                while bytes.get(position).is_some_and(u8::is_ascii_alphabetic) {
                    position += 1;
                }
                let domain = match &source[unit_start..position] {
                    "" => {
                        missing_units = Some(literal);
                        Domain::Unitless
                    }
                    "mm" | "mil" | "in" => Domain::Length,
                    "deg" => Domain::Angle,
                    "fs" | "ps" => Domain::Time,
                    // Identifiers/functions are outside this preflight's scope.
                    _ => return Ok(None),
                };
                unexpected_units |= unitless && domain != Domain::Unitless;
                tokens.push(NumericToken::Number(domain));
                numbers += 1;
            }
            // Conditions, functions, property lookups, comparisons, scientific
            // notation, etc. require KiCad's compiler, not a guessed evaluator.
            _ => return Ok(None),
        }
    }
    let mut parser = Arithmetic {
        tokens: &tokens,
        position: 0,
        implicit_numbers: 0,
    };
    let domain = parser.expression(0, 0)?;
    if parser.position != tokens.len() {
        bail!("invalid arithmetic expression {source:?}");
    }
    if unexpected_units {
        bail!("unexpected units in a unitless constraint");
    }
    // KiCad checks missing units only if there is exactly one compiled numeric
    // node. Unary minus introduces an implicit zero; numeric factors in compound
    // arithmetic are intentionally allowed to be unitless.
    if !unitless
        && numbers + parser.implicit_numbers == 1
        && let Some(literal) = missing_units
    {
        bail!("missing units for {literal:?}; use mm, in, mil, deg, fs, or ps");
    }
    Ok(Some(domain))
}

struct Arithmetic<'a> {
    tokens: &'a [NumericToken],
    position: usize,
    implicit_numbers: usize,
}

impl Arithmetic<'_> {
    fn expression(&mut self, precedence: u8, depth: usize) -> Result<Domain> {
        if depth > 256 {
            bail!("arithmetic nesting exceeds preflight limit (256)");
        }
        let mut left = match self.tokens.get(self.position) {
            Some(NumericToken::Number(domain)) => {
                self.position += 1;
                *domain
            }
            Some(NumericToken::Operator(op @ (b'+' | b'-'))) => {
                self.position += 1;
                if *op == b'-' {
                    self.implicit_numbers += 1;
                }
                self.expression(3, depth + 1)?
            }
            Some(NumericToken::Left) => {
                self.position += 1;
                let inner = self.expression(0, depth + 1)?;
                if !matches!(self.tokens.get(self.position), Some(NumericToken::Right)) {
                    bail!("missing ')' in arithmetic expression");
                }
                self.position += 1;
                inner
            }
            _ => bail!("expected a number or parenthesized arithmetic expression"),
        };
        while let Some(NumericToken::Operator(op)) = self.tokens.get(self.position) {
            let next = if matches!(op, b'*' | b'/') { 2 } else { 1 };
            if next < precedence {
                break;
            }
            self.position += 1;
            let right = self.expression(next + 1, depth + 1)?;
            // Match KiCad's actual unit propagation (not dimensional analysis):
            // a non-unitless RHS wins, otherwise preserve the LHS's domain.
            if right != Domain::Unitless {
                left = right;
            }
        }
        Ok(left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(body: &str) -> String {
        format!("(version 1)\n(rule \"isolation\" {body})")
    }

    fn checked(source: &str) -> Result<Vec<String>> {
        check(Path::new("layout.kicad_dru"), source)
    }

    #[test]
    fn missing_units_names_the_rule_and_source_location() {
        let error =
            checked("(version 1)\n(rule \"PoE isolation\"\n  (constraint clearance (min 0.11)))")
                .unwrap_err()
                .to_string();
        assert_eq!(
            error,
            "layout.kicad_dru:3:25: rule \"PoE isolation\": clearance min: missing units for \"0.11\"; use mm, in, mil, deg, fs, or ps\nKiCad DRC was not run."
        );
    }

    #[test]
    fn malformed_source_is_not_partially_accepted() {
        for (source, expected) in [
            (
                "(version 1)\n(rule \"broken\" (constraint clearance (min 1mm))",
                "rule \"broken\": unclosed '('",
            ),
            (
                "(version 1)\n(rule \"broken\" (condition \"A.Type == 'Pad'\n\"))",
                "rule \"broken\": unterminated quoted string at end of line",
            ),
            ("(version 1))", "unexpected ')'"),
            (
                "(version 1) ; not a KiCad comment",
                "expected '(keyword ...)'",
            ),
            (
                "(version 1) # not an inline comment",
                "expected '(keyword ...)'",
            ),
            ("(rule named)", "missing version statement"),
        ] {
            let error = checked(source).unwrap_err().to_string();
            assert!(error.contains(expected), "{source:?}: {error}");
        }
    }

    #[test]
    fn kicad_comments_and_string_escapes_preserve_content() {
        let source = "  # ${COMMENT} (( \" ignored\r\n(version 1)\r\n(rule \"résistor # ; \\x41\\101 \\\" \\\\ \\q\"\r\n (condition \"A.NetName == '${cLaSs:Power}'\")\r\n (constraint clearance (min \"2 * (1mm + 1.5mm)\")))\r\n";
        let warnings = checked(source).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("condition expression compilation"));
        // The rule name is decoded with DSN escapes, not Lisp escapes.
        let bad = source.replace("2 * (1mm + 1.5mm)", "0.11");
        let error = checked(&bad).unwrap_err().to_string();
        assert!(error.contains("résistor # ; AA"), "{error}");
        assert!(error.contains("missing units"), "{error}");
    }

    #[test]
    fn arithmetic_matches_kicad_not_strict_dimensional_analysis() {
        for (expr, expected) in [
            ("2 * (1mm + 1.5mm)", Domain::Length),
            ("(1mm + 2mm) / 2", Domain::Length),
            ("5 mil", Domain::Length),
            ("0,11mm", Domain::Length),
            ("1mm / 1mm", Domain::Length),
            ("1mm + 2ps", Domain::Time),
            ("2ps + 1mm", Domain::Length),
            ("+0.11mm", Domain::Length),
            ("-0.11", Domain::Unitless),
            ("1 + 2", Domain::Unitless),
        ] {
            assert_eq!(arithmetic(expr, false).unwrap(), Some(expected), "{expr}");
        }
        assert!(
            arithmetic("+0.11", false)
                .unwrap_err()
                .to_string()
                .contains("missing units")
        );
        for invalid in ["1mm +", "1mm 2mm", "(1mm + 2mm", "1mm ** 2"] {
            assert!(arithmetic(invalid, false).is_err(), "{invalid}");
        }
        assert!(
            arithmetic("3mm", true)
                .unwrap_err()
                .to_string()
                .contains("unexpected units")
        );
        assert_eq!(arithmetic("3", true).unwrap(), Some(Domain::Unitless));
    }

    #[test]
    fn constraint_schemas_units_and_aliases() {
        let valid = rules(
            "(constraint min_resolved_spokes 2)\n (constraint zone_connection \"thermal_reliefs\")\n (constraint disallow track \"via\")\n (constraint via_count (max 4))\n (constraint track_angle (opt 45))\n (constraint skew (min 10ps) (max 2000fs) (within_diff_pairs))\n (constraint mechanical_clearance (min 1mm))\n (constraint assertion \"A.Type == 'Pad'\")",
        );
        let warnings = checked(&valid).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("assertion expression compilation"));
        for (body, expected) in [
            ("(constraint clearance (min))", "missing min value"),
            (
                "(constraint clearance (min 1ps))",
                "time units are not allowed",
            ),
            (
                "(constraint length (min 1mm) (max 2ps))",
                "mixed unit domains",
            ),
            (
                "(constraint clearance (within_diff_pairs))",
                "invalid clearance option",
            ),
            (
                "(constraint min_resolved_spokes (min 2))",
                "min_resolved_spokes requires a number",
            ),
            (
                "(constraint zone_connection bogus)",
                "zone_connection requires",
            ),
            ("(constraint disallow balloon)", "invalid disallow"),
            (
                "(constraint hole (min 1mm)) (constraint hole_size (max 2mm))",
                "duplicate hole_size",
            ),
            ("(severity fatal)", "severity requires"),
        ] {
            let error = checked(&rules(body)).unwrap_err().to_string();
            assert!(error.contains(expected), "{body}: {error}");
        }
    }

    #[test]
    fn unsupported_constructs_and_variables_are_explicitly_deferred() {
        for source in [
            "${WHOLE_RULE_FILE}",
            "(version 1)\n(rule name (constraint clearance (min ${GAP})))",
            "(version 1)\n(rule name (condition \"A.Reference == '${REF}'\")",
        ] {
            let warnings = checked(source).unwrap();
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("this rule file was not preflighted"));
        }
        let warnings = checked(&rules("(constraint future_constraint (min 1)) (future_clause x) (constraint length (min fn(2ps)))")).unwrap();
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings
                .iter()
                .all(|message| message.contains("rule \"isolation\""))
        );
        assert!(checked("").unwrap().is_empty());
        assert!(checked(" # comment-only\n").unwrap().is_empty());
        // A future version can change the known schema: do not reject it using
        // version-1 assumptions. Likewise layer masks require board context.
        assert_eq!(
            checked("(version 2) (rule name (constraint clearance (future_option x)))")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            checked(&rules("(layer \"*\") (layer \"*\")"))
                .unwrap()
                .len(),
            3
        );
        // A syntactically valid string can still fail KiCad's expression
        // compiler. Never imply this expression was validated by the guard.
        let warnings = checked(&rules("(condition \"A.Type ==\")")).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("condition expression compilation"));
    }

    #[test]
    fn deeply_nested_quoted_arithmetic_fails_without_overflow() {
        let expression = format!("{}1mm{}", "(".repeat(300), ")".repeat(300));
        assert!(
            arithmetic(&expression, false)
                .unwrap_err()
                .to_string()
                .contains("nesting exceeds")
        );
    }
}
