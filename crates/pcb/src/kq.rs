//! `pcb kq` queries KiCad-style S-expressions directly.
//!
//! The query program is passed as an argument, and the input sexpr comes from
//! stdin by default:
//!
//! ```bash
//! cat Interface_USB.kicad_sym | pcb kq '(query (select (symbol "ADUM4160" _ ...)) (emit $match))'
//! ```
//!
//! An optional file path can be provided instead of stdin:
//!
//! ```bash
//! pcb kq '(query (select (symbol "ADUM4160" _ ...)) (emit $match))' Interface_USB.kicad_sym
//! ```
use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_sexpr::formatter::{FormatMode, format_tree};
use pcb_sexpr::{Sexpr, SexprKind};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Query KiCad-style S-expressions with native kq syntax")]
pub struct KqArgs {
    /// kq query program
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// KiCad-style S-expression file to inspect
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct QueryProgram {
    selects: Vec<Pattern>,
    sort_by: Option<String>,
    emit: Sexpr,
}

#[derive(Debug, Clone)]
enum Pattern {
    Any,
    Capture(String),
    Literal(Sexpr),
    List(Vec<PatternPart>),
}

#[derive(Debug, Clone)]
struct PatternPart {
    pattern: Pattern,
    repeat: bool,
}

#[derive(Debug, Clone)]
struct MatchState {
    node: Sexpr,
    bindings: BTreeMap<String, Sexpr>,
}

pub fn execute(args: KqArgs) -> Result<()> {
    let source = load_query_input(args.path.as_deref())?;
    let root = pcb_sexpr::parse(&source)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to parse kq input")?;

    let program = parse_query_program(&args.query)?;
    let outputs = evaluate_query(&root, &program)?;

    for output in outputs {
        print_output(&output);
    }

    Ok(())
}

fn load_query_input(path: Option<&std::path::Path>) -> Result<String> {
    if let Some(path) = path {
        return fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()));
    }

    let mut source = String::new();
    std::io::stdin()
        .read_to_string(&mut source)
        .context("Failed to read kq input from stdin")?;

    if source.trim().is_empty() {
        bail!("`pcb kq` expects input sexpr on stdin when no FILE is provided");
    }

    Ok(source)
}

fn parse_query_program(source: &str) -> Result<QueryProgram> {
    let sexpr = pcb_sexpr::parse(source)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to parse kq query program")?;
    let items = sexpr
        .as_list()
        .ok_or_else(|| anyhow::anyhow!("kq query must be a `(query ...)` list"))?;

    if items.first().and_then(Sexpr::as_sym) != Some("query") {
        bail!("kq query must start with `(query ...)`");
    }

    let mut selects = Vec::new();
    let mut sort_by = None;
    let mut emit = None;

    for form in items.iter().skip(1) {
        let form_items = form
            .as_list()
            .ok_or_else(|| anyhow::anyhow!("query forms must be lists"))?;
        match form_items.first().and_then(Sexpr::as_sym) {
            Some("select") => {
                if form_items.len() != 2 {
                    bail!("`(select ...)` expects exactly one pattern");
                }
                selects.push(parse_pattern(&form_items[1])?);
            }
            Some("emit") => {
                if form_items.len() != 2 {
                    bail!("`(emit ...)` expects exactly one template");
                }
                if emit.is_some() {
                    bail!("kq query may contain only one `(emit ...)` form");
                }
                emit = Some(form_items[1].clone());
            }
            Some("sort-by") => {
                if form_items.len() != 2 {
                    bail!("`(sort-by ...)` expects exactly one capture");
                }
                if sort_by.is_some() {
                    bail!("kq query may contain only one `(sort-by ...)` form");
                }
                let capture = form_items[1]
                    .as_sym()
                    .and_then(|sym| sym.strip_prefix('$'))
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("`(sort-by ...)` expects a capture like `$name`"))?;
                sort_by = Some(capture.to_string());
            }
            Some(other) => bail!("Unsupported kq form `{other}`"),
            None => bail!("query forms must start with a symbol"),
        }
    }

    if selects.is_empty() {
        bail!("kq query must contain at least one `(select ...)` form");
    }

    let emit = emit.ok_or_else(|| anyhow::anyhow!("kq query must contain an `(emit ...)` form"))?;

    Ok(QueryProgram {
        selects,
        sort_by,
        emit,
    })
}

fn parse_pattern(node: &Sexpr) -> Result<Pattern> {
    if let Some(sym) = node.as_sym() {
        if sym == "_" {
            return Ok(Pattern::Any);
        }
        if let Some(name) = sym.strip_prefix('$') {
            if name.is_empty() {
                bail!("Capture names may not be empty");
            }
            return Ok(Pattern::Capture(name.to_string()));
        }
        if sym == "..." {
            bail!("`...` must follow a pattern inside a list");
        }
    }

    if let Some(items) = node.as_list() {
        let mut parts: Vec<PatternPart> = Vec::new();
        for item in items {
            if item.as_sym() == Some("...") {
                let Some(last) = parts.last_mut() else {
                    bail!("`...` may not appear first in a list pattern");
                };
                if last.repeat {
                    bail!("`...` may only be applied once to the same pattern");
                }
                last.repeat = true;
                continue;
            }

            parts.push(PatternPart {
                pattern: parse_pattern(item)?,
                repeat: false,
            });
        }

        return Ok(Pattern::List(parts));
    }

    Ok(Pattern::Literal(node.clone()))
}

fn evaluate_query(root: &Sexpr, program: &QueryProgram) -> Result<Vec<Sexpr>> {
    let mut states = vec![MatchState {
        node: root.clone(),
        bindings: BTreeMap::new(),
    }];

    for pattern in &program.selects {
        states = apply_select(&states, pattern)?;
    }

    if let Some(capture) = &program.sort_by {
        sort_states_by_capture(&mut states, capture)?;
    }

    let mut out = Vec::with_capacity(states.len());
    for state in states {
        out.push(render_emit(&program.emit, &state.bindings)?);
    }

    Ok(out)
}

fn sort_states_by_capture(states: &mut [MatchState], capture: &str) -> Result<()> {
    for state in states.iter() {
        if !state.bindings.contains_key(capture) {
            bail!("Unknown capture `${capture}` in `sort-by`");
        }
    }

    states.sort_by(|a, b| compare_sort_nodes(&a.bindings[capture], &b.bindings[capture]));
    Ok(())
}

fn compare_sort_nodes(a: &Sexpr, b: &Sexpr) -> Ordering {
    match (&a.kind, &b.kind) {
        (SexprKind::Int(ai), SexprKind::Int(bi)) => ai.cmp(bi),
        (SexprKind::F64(af), SexprKind::F64(bf)) => af.partial_cmp(bf).unwrap_or(Ordering::Equal),
        (SexprKind::Int(ai), SexprKind::F64(bf)) => {
            (*ai as f64).partial_cmp(bf).unwrap_or(Ordering::Equal)
        }
        (SexprKind::F64(af), SexprKind::Int(bi)) => {
            af.partial_cmp(&(*bi as f64)).unwrap_or(Ordering::Equal)
        }
        _ => natord::compare(&sort_key_text(a), &sort_key_text(b)),
    }
}

fn sort_key_text(node: &Sexpr) -> String {
    match &node.kind {
        SexprKind::Symbol(s) | SexprKind::String(s) => s.clone(),
        SexprKind::Int(n) => n.to_string(),
        SexprKind::F64(f) => node
            .raw_atom
            .clone()
            .unwrap_or_else(|| {
                let mut s = f.to_string();
                if s.contains('.') {
                    while s.ends_with('0') {
                        s.pop();
                    }
                    if s.ends_with('.') {
                        s.pop();
                    }
                }
                s
            }),
        SexprKind::List(_) => format_tree(node, FormatMode::Dense).trim_end().to_string(),
    }
}

fn apply_select(states: &[MatchState], pattern: &Pattern) -> Result<Vec<MatchState>> {
    let mut out = Vec::new();

    for state in states {
        visit_nodes(&state.node, &mut |candidate| {
            let mut bindings = state.bindings.clone();
            bindings.remove("match");
            if match_pattern(candidate, pattern, &mut bindings)
                && merge_binding(&mut bindings, "match", candidate.clone()).is_ok()
            {
                out.push(MatchState {
                    node: candidate.clone(),
                    bindings,
                });
            }
        });
    }

    Ok(out)
}

fn visit_nodes(node: &Sexpr, visitor: &mut impl FnMut(&Sexpr)) {
    visitor(node);
    if let Some(items) = node.as_list() {
        for item in items {
            visit_nodes(item, visitor);
        }
    }
}

fn match_pattern(node: &Sexpr, pattern: &Pattern, bindings: &mut BTreeMap<String, Sexpr>) -> bool {
    match pattern {
        Pattern::Any => true,
        Pattern::Capture(name) => merge_binding(bindings, name, node.clone()).is_ok(),
        Pattern::Literal(literal) => node == literal,
        Pattern::List(parts) => {
            let Some(items) = node.as_list() else {
                return false;
            };
            if let Some(next) = match_sequence(items, parts, 0, 0, bindings) {
                *bindings = next;
                true
            } else {
                false
            }
        }
    }
}

fn match_sequence(
    items: &[Sexpr],
    parts: &[PatternPart],
    item_idx: usize,
    part_idx: usize,
    bindings: &BTreeMap<String, Sexpr>,
) -> Option<BTreeMap<String, Sexpr>> {
    if part_idx == parts.len() {
        return (item_idx == items.len()).then(|| bindings.clone());
    }

    let part = &parts[part_idx];
    if part.repeat {
        let mut states = vec![(item_idx, bindings.clone())];
        let mut cursor = item_idx;
        let mut current = bindings.clone();

        while cursor < items.len() {
            let mut attempt = current.clone();
            if !match_pattern(&items[cursor], &part.pattern, &mut attempt) {
                break;
            }
            cursor += 1;
            current = attempt;
            states.push((cursor, current.clone()));
        }

        for (next_idx, next_bindings) in states.into_iter().rev() {
            if let Some(done) = match_sequence(items, parts, next_idx, part_idx + 1, &next_bindings)
            {
                return Some(done);
            }
        }

        None
    } else {
        let item = items.get(item_idx)?;
        let mut next_bindings = bindings.clone();
        if !match_pattern(item, &part.pattern, &mut next_bindings) {
            return None;
        }
        match_sequence(items, parts, item_idx + 1, part_idx + 1, &next_bindings)
    }
}

fn merge_binding(bindings: &mut BTreeMap<String, Sexpr>, name: &str, value: Sexpr) -> Result<()> {
    match bindings.get(name) {
        Some(existing) if existing != &value => {
            bail!("Capture `${name}` matched multiple different nodes")
        }
        Some(_) => Ok(()),
        None => {
            bindings.insert(name.to_string(), value);
            Ok(())
        }
    }
}

fn render_emit(template: &Sexpr, bindings: &BTreeMap<String, Sexpr>) -> Result<Sexpr> {
    if template.as_sym() == Some("match") {
        return bindings
            .get("match")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("`match` is not available in this context"));
    }

    if let Some(sym) = template.as_sym()
        && let Some(name) = sym.strip_prefix('$')
    {
        return bindings
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown capture `${name}`"));
    }

    if let Some(items) = template.as_list() {
        let mut rendered = Vec::with_capacity(items.len());
        for item in items {
            rendered.push(render_emit(item, bindings)?);
        }
        return Ok(Sexpr::list(rendered));
    }

    Ok(template.clone())
}

fn print_output(output: &Sexpr) {
    print!("{}", format_tree(output, FormatMode::Dense));
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use tempfile::TempDir;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        args: KqArgs,
    }

    struct TempKicadSym {
        _dir: TempDir,
        path: std::path::PathBuf,
        contents: String,
    }

    fn temp_kicad_sym(contents: &str) -> TempKicadSym {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.kicad_sym");
        std::fs::write(&path, contents).expect("write symbol lib");
        TempKicadSym {
            _dir: dir,
            path,
            contents: contents.to_string(),
        }
    }

    fn run_query(contents: &str, query: &str) -> Vec<String> {
        let root = pcb_sexpr::parse(contents).expect("parse source");
        let program = parse_query_program(query).expect("parse query");
        let outputs = evaluate_query(&root, &program).expect("evaluate query");
        render_outputs(contents, &outputs)
    }

    fn render_outputs(_contents: &str, outputs: &[Sexpr]) -> Vec<String> {
        outputs
            .iter()
            .map(|output| format_tree(output, FormatMode::Dense).trim_end().to_string())
            .collect()
    }

    #[test]
    fn query_can_list_direct_properties_for_one_symbol() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "Base"
    (property "Reference" "U" (at 0 0 0))
    (property "Value" "Base" (at 0 0 0))
  )
  (symbol "Child"
    (extends "Base")
    (property "Reference" "U" (at 0 0 0))
    (property "Value" "Child" (at 0 0 0))
    (property "Datasheet" "https://child.example.com" (at 0 0 0))
  )
)"#,
        );

        let query = r#"(query
  (select (symbol "Child" _ ...))
  (select (property $name $value _ ...))
  (emit $match))"#;

        let outputs = run_query(&temp.contents, query);
        assert_eq!(outputs.len(), 3);
        assert_eq!(outputs[0], r#"(property "Reference" "U" (at 0 0 0))"#);
        assert_eq!(outputs[1], r#"(property "Value" "Child" (at 0 0 0))"#);
        assert_eq!(
            outputs[2],
            r#"(property "Datasheet" "https://child.example.com" (at 0 0 0))"#
        );
        assert!(!outputs.iter().any(|line| line.contains("Base")));
        assert!(temp.path.exists());
    }

    #[test]
    fn query_can_filter_down_to_a_single_symbol() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "A"
    (property "Reference" "U" (at 0 0 0))
  )
  (symbol "B"
    (property "Reference" "R" (at 0 0 0))
    (property "Value" "B" (at 0 0 0))
  )
)"#,
        );

        let query = r#"(query
  (select (symbol "B" _ ...))
  (emit $match))"#;

        let outputs = run_query(&temp.contents, query);
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].starts_with("(symbol \"B\""));
        assert!(outputs[0].contains(r#"(property "Reference" "R""#));
        assert!(outputs[0].contains(r#"(property "Value" "B""#));
        assert!(!outputs[0].contains("(symbol \"A\""));
    }

    #[test]
    fn query_can_emit_pin_info_without_electrical_type_or_graphic_style() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "Base"
    (property "Reference" "U" (at 0 0 0))
    (property "Value" "Base" (at 0 0 0))
    (symbol "Base_1_1"
      (pin passive line (at 0 0 0) (length 2.54)
        (name "~" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 1 2 90) (length 2.54)
        (name "VIN" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#,
        );

        let query = r#"(query
  (select (symbol "Base" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (emit (pin_info (name $name) (number $number))))"#;

        let outputs = run_query(&temp.contents, query);
        let parsed: Vec<Sexpr> = outputs
            .iter()
            .map(|output| pcb_sexpr::parse(output).expect("parse emitted sexpr"))
            .collect();
        let expected = vec![
            pcb_sexpr::parse(r#"(pin_info (name "~") (number "1"))"#).unwrap(),
            pcb_sexpr::parse(r#"(pin_info (name "VIN") (number "2"))"#).unwrap(),
        ];
        assert_eq!(parsed, expected);
    }

    #[test]
    fn query_can_sort_by_capture_naturally() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (symbol "Base"
    (pin passive line (at 0 0 0) (length 2.54)
      (name "TEN" (effects (font (size 1.27 1.27))))
      (number "10" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 0 0) (length 2.54)
      (name "ONE" (effects (font (size 1.27 1.27))))
      (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 0 0) (length 2.54)
      (name "TWO" (effects (font (size 1.27 1.27))))
      (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
        );

        let query = r#"(query
  (select (symbol "Base" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_info (name $name) (number $number))))"#;

        let outputs = run_query(&temp.contents, query);
        assert_eq!(
            outputs,
            vec![
                r#"(pin_info (name "ONE") (number "1"))"#.to_string(),
                r#"(pin_info (name "TWO") (number "2"))"#.to_string(),
                r#"(pin_info (name "TEN") (number "10"))"#.to_string(),
            ]
        );
    }

    #[test]
    fn output_formats_matches_and_synthetic_results_on_one_line() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (symbol "B"
    (property "Reference" "R" (at 0 0 0))
    (pin input line (at 1 2 90) (length 2.54)
      (name "VIN" (effects (font (size 1.27 1.27))))
      (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
        );

        let symbol_query = r#"(query
  (select (symbol "B" _ ...))
  (emit $match))"#;
        let pin_query = r#"(query
  (select (symbol "B" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (emit (pin_info (name $name) (number $number))))"#;

        let symbol_output = run_query(&temp.contents, symbol_query);
        let pin_output = run_query(&temp.contents, pin_query);

        assert_eq!(
            symbol_output,
            vec![r#"(symbol "B" (property "Reference" "R" (at 0 0 0)) (pin input line (at 1 2 90) (length 2.54) (name "VIN" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27))))))"#.to_string()]
        );
        assert_eq!(
            pin_output,
            vec![r#"(pin_info (name "VIN") (number "2"))"#.to_string()]
        );
        assert!(temp.path.exists());
    }

    #[test]
    fn query_requires_query_root_and_emit() {
        assert!(parse_query_program("(select _)").is_err());
        assert!(parse_query_program("(query (select _))").is_err());
    }

    #[test]
    fn cli_accepts_query_and_optional_file() {
        let parsed = TestCli::try_parse_from(["pcb", "(query (select _) (emit $match))"]).unwrap();
        assert_eq!(parsed.args.query, "(query (select _) (emit $match))");
        assert!(parsed.args.path.is_none());

        let parsed =
            TestCli::try_parse_from(["pcb", "(query (select _) (emit $match))", "foo.kicad_sym"])
                .unwrap();
        assert_eq!(parsed.args.path, Some(PathBuf::from("foo.kicad_sym")));
    }
}
