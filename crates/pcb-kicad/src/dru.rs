//! A known-error preflight, not a replacement for KiCad's rule compiler.
//!
//! The lexical/schema contract follows KiCad 10.0.6's `common/dsnlexer.cpp`
//! and `pcbnew/drc/drc_rule_parser.cpp`; expressions are covered by [`expr`].
//! Names in conditions, board-dependent layer names, and text-variable
//! expansion remain KiCad's responsibility. In particular, passing this check
//! does not prove that KiCad compiled or applied every rule.

use anyhow::{Context, Result, anyhow};
use pcb_sexpr::{Sexpr, SexprKind, Span};
use pcb_zen_core::diagnostics::{Diagnostic, DiagnosticError, Diagnostics};
use starlark::codemap::{CodeMap, Pos, Span as CodeSpan};
use starlark::errors::EvalSeverity;
use std::collections::HashSet;
use std::path::Path;

mod expr;
use expr::{Domain, arithmetic};

pub(super) fn preflight(
    board: &Path,
    display_board: &Path,
    diagnostics: &mut Diagnostics,
) -> Result<bool> {
    let path = board.with_extension("kicad_dru");
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    match check(&display_board.with_extension("kicad_dru"), &source) {
        Ok(warnings) => {
            diagnostics.extend(warnings);
            Ok(true)
        }
        Err(error) => {
            diagnostics.push(error.downcast::<DiagnosticError>()?.0);
            Ok(false)
        }
    }
}

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0)
}

struct Checker<'a> {
    path: &'a Path,
    source: &'a str,
    rule: Option<String>,
    warnings: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn diagnostic(
        &self,
        offset: usize,
        message: impl std::fmt::Display,
        severity: EvalSeverity,
    ) -> Diagnostic {
        let rule = self
            .rule
            .as_ref()
            .map_or(String::new(), |name| format!("rule {name:?}: "));
        let kind = if severity == EvalSeverity::Error {
            "layout.drc.rules.invalid"
        } else {
            "layout.drc.rules.incomplete"
        };
        let path = self.path.to_string_lossy();
        let codemap = CodeMap::new(path.to_string(), self.source.to_owned());
        let position = Pos::new(offset as u32);
        Diagnostic::categorized(&path, &format!("{rule}{message}"), kind, severity).with_span(
            codemap
                .file_span(CodeSpan::new(position, position))
                .resolve_span(),
        )
    }

    fn error(&self, offset: usize, message: impl std::fmt::Display) -> anyhow::Error {
        anyhow!(DiagnosticError(self.diagnostic(
            offset,
            message,
            EvalSeverity::Error
        )))
    }

    fn defer(&mut self, offset: usize, message: impl std::fmt::Display) {
        let diagnostic = self.diagnostic(offset, message, EvalSeverity::Warning);
        // Repeated clauses in one rule need only one warning per reason.
        if !self
            .warnings
            .iter()
            .any(|existing| existing.body == diagnostic.body)
        {
            self.warnings.push(diagnostic);
        }
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
                    let expression = expression_text(expr);
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
                    let mut layer = None;
                    for clause in &values[1..] {
                        let (keyword, args) = self.form(clause)?;
                        match keyword {
                            "constraint" => self.constraint(clause, args, &mut seen)?,
                            "condition" => {
                                let value = self.one(clause, args)?;
                                self.atom(value)?;
                                // Unquoted text is split by KiCad's own lexer.
                                if let Some(Err(error)) = value.as_str().map(expr::condition) {
                                    return Err(self
                                        .error(clause.span.start, format!("condition: {error}")));
                                }
                            }
                            "layer" => {
                                let value = self.atom(self.one(clause, args)?)?;
                                // KiCad allows another only while every layer is
                                // still selected, which takes a pattern of wildcards alone.
                                if layer.replace(value).is_some_and(|previous: &str| {
                                    previous.contains(|c| !matches!(c, '*' | '?'))
                                }) {
                                    return Err(self.error(
                                        clause.span.start,
                                        "a rule can have only one layer clause",
                                    ));
                                }
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

fn check(path: &Path, source: &str) -> Result<Vec<Diagnostic>> {
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

// The text KiCad compiles: tokens are joined by the whitespace between them,
// so only tokens that touch in the source stay adjacent.
fn expression_text(nodes: &[Sexpr]) -> String {
    let mut text = String::new();
    let mut end = None;
    for node in nodes {
        if end.is_some_and(|end| end != node.span.start) {
            text.push(' ');
        }
        match &node.kind {
            SexprKind::List(children) => {
                text.push('(');
                text += &expression_text(children);
                text.push(')');
            }
            SexprKind::Symbol(atom) | SexprKind::String(atom) => text += atom,
            _ => unreachable!("rule tokenizer preserves numeric atoms as symbols"),
        }
        end = Some(node.span.end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(body: &str) -> String {
        format!("(version 1)\n(rule \"isolation\" {body})")
    }

    fn checked(source: &str) -> Result<Vec<String>> {
        check(Path::new("layout.kicad_dru"), source).map(|warnings| {
            warnings
                .into_iter()
                .map(|diagnostic| diagnostic.body)
                .collect()
        })
    }

    #[test]
    fn missing_units_names_the_rule_and_source_location() {
        let error =
            checked("(version 1)\n(rule \"PoE isolation\"\n  (constraint clearance (min 0.11)))")
                .unwrap_err()
                .downcast::<DiagnosticError>()
                .unwrap()
                .0;
        let report = pcb_zen_core::diagnostics::DiagnosticReport::from_diagnostic(&error);
        assert_eq!(report.location, "layout.kicad_dru:3:25");
        assert_eq!(report.kind.as_deref(), Some("layout.drc.rules.invalid"));
        assert_eq!(report.severity, EvalSeverity::Error);
        assert_eq!(
            report.body,
            "rule \"PoE isolation\": clearance min: missing units for \"0.11\"; use mm, in, mil, deg, fs, or ps"
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
        assert!(checked(source).unwrap().is_empty());
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
        assert!(checked(&valid).unwrap().is_empty());
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
        let warnings = checked(&rules("(constraint future_constraint (min 1)) (future_clause x) (constraint length (min A.Length))")).unwrap();
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings
                .iter()
                .all(|message| message.contains("rule \"isolation\""))
        );
        assert!(checked("").unwrap().is_empty());
        assert!(checked(" # comment-only\n").unwrap().is_empty());
        // A future version can change the known schema: do not reject it using
        // version-1 assumptions.
        assert_eq!(
            checked("(version 2) (rule name (constraint clearance (future_option x)))")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn conditions_and_layers_report_only_what_kicad_rejects() {
        for valid in [
            "(layer outer) (condition \"A.Parent == 'H11' && B.Parent == 'H11'\")",
            "(condition \"A.Width > 0.2mm && !A.isPlated() || A.Net != B.Net\")",
            // Names are KiCad's to judge, as is anything outside ASCII.
            "(condition \"C.Typo == nope && A.missing('x')\")",
            "(condition \"A.NetName == 'µ' &&\")",
            "(layer \"*\") (layer \"F.Cu\")",
            // KiCad loads a rule file whose assertion does not compile.
            "(constraint assertion \"A.Type ==\")",
        ] {
            assert!(checked(&rules(valid)).unwrap().is_empty(), "{valid}");
        }
        for (body, expected) in [
            ("(condition \"A.Type = 'Pad'\")", "unexpected character '='"),
            (
                "(condition \"A.Type == 'Pad' and B.Type == 'Via'\")",
                "unexpected \"and\"",
            ),
            ("(condition \"A.Type ==\")", "unexpected end"),
            ("(condition \"A.Width < 1mm < 2mm\")", "cannot be chained"),
            ("(condition \"isPlated()\")", "needs an item"),
            ("(condition \"A.Width > 1\")", "missing units for \"1\""),
            ("(constraint length (min fn(2ps)))", "needs an item"),
            ("(layer \"*.Cu\") (layer \"F.SilkS\")", "only one layer"),
        ] {
            let error = checked(&rules(body)).unwrap_err().to_string();
            assert!(error.contains(expected), "{body}: {error}");
        }
    }

    #[test]
    fn repeated_warnings_are_structured_and_deduplicated() {
        let source = rules("(future_clause x) (future_clause y) (other_clause x) (other_clause y)");
        let warnings = check(Path::new("layout.kicad_dru"), &source).unwrap();
        assert_eq!(warnings.len(), 2);
        for warning in warnings {
            assert_eq!(warning.severity, EvalSeverity::Warning);
            assert_eq!(
                pcb_zen_core::diagnostics::diagnostic_kind(&warning).as_deref(),
                Some("layout.drc.rules.incomplete")
            );
            assert_eq!(warning.path, "layout.kicad_dru");
            assert!(warning.span.is_some());
        }
    }

    // Every sequence of up to `max` atoms.
    fn sequences(atoms: &[&str], max: usize) -> Vec<String> {
        (0..max)
            .scan(vec![String::new()], |longest, _| {
                *longest = longest
                    .iter()
                    .flat_map(|prefix| atoms.iter().map(move |atom| format!("{prefix}{atom} ")))
                    .collect();
                Some(longest.clone())
            })
            .flatten()
            .collect()
    }

    #[test]
    #[ignore = "requires KiCad's Python; run explicitly for KiCad compatibility"]
    fn real_kicad_rejects_whatever_the_preflight_rejects() {
        let conditions = sequences(
            &[
                "A.Type",
                "'Pad'",
                "1",
                "1mm",
                "==",
                "<",
                "&&",
                "||",
                "!",
                "-",
                "(",
                ")",
                ",",
                ".",
                "A.isPlated()",
                "isPlated()",
                "=",
                "and",
                "x",
                "\t",
            ],
            3,
        );
        let values = sequences(
            &[
                "1", "1mm", "5ps", "+", "*", "(", ")", "-", "A.Width", "fn(1mm)", "x", "mm",
            ],
            3,
        );
        let layers = sequences(
            &[
                "(layer outer)",
                "(layer \"F.Cu\")",
                "(layer \"*.Cu\")",
                "(layer \"?*\")",
            ],
            3,
        );
        let cases: Vec<String> = conditions
            .iter()
            .map(|text| format!("(condition \"{text}\") (constraint clearance (min 1mm))"))
            .chain(
                ["clearance (min", "via_count (max"]
                    .iter()
                    .flat_map(|kind| {
                        values
                            .iter()
                            .map(move |text| format!("(constraint {kind} \"{text}\"))"))
                    }),
            )
            .chain(
                layers
                    .iter()
                    .map(|clauses| format!("{clauses}(constraint clearance (min 1mm))")),
            )
            .map(|body| rules(&body))
            .collect();

        let root = tempfile::tempdir().unwrap();
        let [board, input, output] =
            ["layout.kicad_pcb", "cases.json", "accepted.json"].map(|name| root.path().join(name));
        std::fs::write(
            &board,
            include_str!("../../pcb-layout/tests/resources/graphics/module/layout.kicad_pcb"),
        )
        .unwrap();
        std::fs::write(&input, serde_json::to_string(&cases).unwrap()).unwrap();
        crate::PythonScriptBuilder::new(include_str!("dru/oracle.py"))
            .args([&board, &input, &output].map(|path| path.to_string_lossy()))
            .run()
            .unwrap();
        let accepted: Vec<bool> =
            serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(accepted.len(), cases.len());
        let rejected = cases
            .iter()
            .zip(accepted)
            .filter_map(|(rules, accepted)| Some((rules, accepted, checked(rules).err()?)))
            .inspect(|(rules, accepted, error)| assert!(!accepted, "{rules}: {error}"))
            .count();
        eprintln!(
            "KiCad rejects all {rejected} of {} files the preflight rejects",
            cases.len()
        );
    }

    #[test]
    fn deep_nesting_is_left_to_kicad_without_overflow() {
        let expression = format!("{}1mm{}", "(".repeat(300), ")".repeat(300));
        assert_eq!(arithmetic(&expression, false).unwrap(), None);
    }
}
