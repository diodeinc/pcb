//! Regression tests for `extends` symbol resolution combined with the `in_bom`
//! flag. The structured `merge_symbols` path previously treated a child's
//! default-`true` `in_bom` as "explicitly set", so a child that explicitly
//! declared `(in_bom no)` was ignored, and a child that omitted `in_bom`
//! clobbered a parent's explicit `(in_bom no)`. The raw S-expression merge
//! (`merge_symbol_sexprs`) already handled this correctly, which produced an
//! observable divergence between the structured field and the persisted
//! `raw_sexp`. These tests assert the structured field now matches the
//! reference raw-merge behavior.

use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;

fn resolved_child_in_bom(content: &str, child: &str) -> bool {
    KicadSymbolLibrary::from_string(content)
        .unwrap()
        .get_symbol_lazy_as_eda(child)
        .unwrap()
        .unwrap()
        .in_bom
}

// Child explicitly says (in_bom no); parent omits in_bom (default true).
#[test]
fn extends_in_bom_child_explicit_no_overrides_parent_default() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (property "Reference" "U" (at 0 0 0)))
        (symbol "Child" (extends "Base") (in_bom no) (property "Reference" "U" (at 0 0 0)))
    )"#;
    assert!(
        !resolved_child_in_bom(content, "Child"),
        "child explicit (in_bom no) should override parent default true"
    );
}

// Child explicitly says (in_bom yes); parent explicitly says (in_bom no).
#[test]
fn extends_in_bom_child_explicit_yes_overrides_parent_explicit_no() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (in_bom no) (property "Reference" "U" (at 0 0 0)))
        (symbol "Child" (extends "Base") (in_bom yes) (property "Reference" "U" (at 0 0 0)))
    )"#;
    assert!(
        resolved_child_in_bom(content, "Child"),
        "child explicit (in_bom yes) should override parent explicit (in_bom no)"
    );
}

// Both child and parent omit in_bom; KiCad default of true wins.
#[test]
fn extends_in_bom_child_omits_inherits_parent_omits_default_true() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (property "Reference" "U" (at 0 0 0)))
        (symbol "Child" (extends "Base") (property "Reference" "U" (at 0 0 0)))
    )"#;
    assert!(
        resolved_child_in_bom(content, "Child"),
        "default in_bom should be true when neither parent nor child declares it"
    );
}

// Child omits in_bom; parent explicitly says (in_bom no). Child inherits.
#[test]
fn extends_in_bom_child_omits_inherits_parent_explicit_no() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (in_bom no) (property "Reference" "U" (at 0 0 0)))
        (symbol "Child" (extends "Base") (property "Reference" "U" (at 0 0 0)))
    )"#;
    assert!(
        !resolved_child_in_bom(content, "Child"),
        "child should inherit parent's explicit (in_bom no)"
    );
}

// Multi-level chain: Base (in_bom no) -> Middle (omits) -> Final (omits).
// The resolved Middle carries the parent's explicit (in_bom no); Final must
// inherit it through the already-resolved parent.
#[test]
fn extends_in_bom_chain_inherits_explicit_no_through_resolved_parent() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (in_bom no) (property "Reference" "U" (at 0 0 0)))
        (symbol "Middle" (extends "Base") (property "Reference" "U" (at 0 0 0)))
        (symbol "Final" (extends "Middle") (property "Reference" "U" (at 0 0 0)))
    )"#;
    assert!(
        !resolved_child_in_bom(content, "Middle"),
        "Middle should inherit Base's explicit (in_bom no)"
    );
    assert!(
        !resolved_child_in_bom(content, "Final"),
        "Final should inherit the resolved (in_bom no) through Middle"
    );
}

// The structured `in_bom` field must agree with the merged `raw_sexp` written
// into the resolved symbol. Pre-fix, `raw_sexp` carried the correct `(in_bom
// no)` while the structured field stayed `true`, so a saved symbol/netlist
// could disagree with the BOM decision.
#[test]
fn extends_in_bom_structured_field_matches_raw_sexp() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base" (property "Reference" "U" (at 0 0 0)))
        (symbol "Child" (extends "Base") (in_bom no) (property "Reference" "U" (at 0 0 0)))
    )"#;
    let sym = KicadSymbolLibrary::from_string(content)
        .unwrap()
        .get_symbol_lazy_as_eda("Child")
        .unwrap()
        .unwrap();
    let raw_str = format!(
        "{}",
        sym.raw_sexp().expect("resolved symbol carries raw_sexp")
    );
    assert!(
        !sym.in_bom,
        "structured in_bom must be false (raw-merge reference says (in_bom no))"
    );
    assert!(
        raw_str.contains("(in_bom no)"),
        "raw_sexp must contain (in_bom no), got: {raw_str}"
    );
}
