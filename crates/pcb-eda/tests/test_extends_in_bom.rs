use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;

#[test]
fn extends_in_bom_overrides_or_inherits() {
    for (parent, child, expected) in [
        ("", "(in_bom no)", false),
        ("(in_bom no)", "(in_bom yes)", true),
        ("", "", true),
        ("(in_bom no)", "", false),
    ] {
        let content = format!(
            r#"(kicad_symbol_lib
                (symbol "Base" {parent})
                (symbol "Child" (extends "Base") {child})
                (symbol "Grandchild" (extends "Child"))
            )"#
        );
        let lib = KicadSymbolLibrary::from_string(&content).unwrap();
        for name in ["Child", "Grandchild"] {
            let symbol = lib.get_symbol_lazy_as_eda(name).unwrap().unwrap();
            assert_eq!(symbol.in_bom, expected, "{name}: {content}");
            let raw = symbol.raw_sexp().unwrap().find_list("in_bom");
            assert_eq!(
                raw.map(|items| items[1].as_atom() == Some("yes"))
                    .unwrap_or(true),
                expected,
                "raw {name}: {content}"
            );
        }
    }
}
