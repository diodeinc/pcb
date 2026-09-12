mod common;

use std::collections::BTreeSet;

use common::kicad_builder::{KicadBuilder, TestPin};
use pcb_kicad_sch::{
    SchDocument, SchItem, Symbol, SymbolDefinition, connectivity::ConnectivityGraph,
    patch_page_source, reconcile::plan_reconciliation,
};
use pcb_sch::AttributeValue;
use pcb_sexpr::{
    Sexpr,
    formatter::{FormatMode, format_tree},
};

#[test]
fn cache_alias_lookup_is_distinct_from_library_identity() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:Part", &[TestPin::passive("1", (0.0, 0.0))])
        .define_symbol("Part_1", &[TestPin::passive("7", (5.0, 0.0))])
        .component("Test:Part", Some("BASE"), (0.0, 0.0))
        .local_label("BASE_NET", (0.0, 0.0))
        .component("Test:Part", Some("ALIAS"), (10.0, 0.0))
        .local_label("ALIAS_NET", (15.0, 0.0));
    let mut document = builder.build();
    managed_mut(&mut document, "ALIAS").lib_name = Some("Part_1".into());
    let source = document.to_kicad_sch().unwrap();
    let mut reopened = SchDocument::from_kicad_sch(&source).unwrap();
    let alias = managed(&reopened, "ALIAS");
    assert_eq!(alias.lib_id, "Test:Part");
    assert_eq!(alias.library_key(), "Part_1");
    let graph = ConnectivityGraph::from_kicad(&reopened).unwrap();
    for (name, number) in [("BASE_NET", "1"), ("ALIAS_NET", "7")] {
        let group = graph
            .groups
            .iter()
            .find(|group| group.names.contains(name))
            .unwrap();
        assert_eq!(group.terminals.len(), 1);
        assert!(group.terminals.iter().all(|terminal| matches!(terminal,
            pcb_kicad_sch::connectivity::Terminal::ComponentPin { pin_numbers, .. }
                if pin_numbers == &BTreeSet::from([number.to_owned()]))));
    }
    reopened.pages[0].library.definitions.remove("Part_1");
    assert!(
        ConnectivityGraph::from_kicad(&reopened)
            .unwrap_err()
            .to_string()
            .contains("no cached definition Part_1"),
        "must not fall back to the base definition"
    );
}

#[test]
fn save_apply_reopen_preserves_distinct_native_alias_presentation() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = plan_reconciliation(None, &netlist, "Alias.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    for keep_base in [true, false] {
        let mut saved = document.clone();
        let base_id = managed(&saved, "R1.R").lib_id.clone();
        let base = saved.pages[0].library.definitions[&base_id].clone();
        let alias = with_name(&base, "Native_1");
        let mut alias = alias;
        let section = alias
            .sexpr
            .as_list_mut()
            .unwrap()
            .iter_mut()
            .filter_map(Sexpr::as_list_mut)
            .find(|items| items.first().and_then(Sexpr::as_sym) == Some("symbol"))
            .unwrap();
        section.push(
            pcb_sexpr::parse(
                r#"(circle (center 0 0) (radius 3.81)
            (stroke (width 0.254) (type default)) (fill (type none)))"#,
            )
            .unwrap(),
        );
        managed_mut(&mut saved, "R1.R").lib_name = Some(alias.lib_id.clone());
        if !keep_base {
            managed_mut(&mut saved, "R2.R").lib_name = Some(alias.lib_id.clone());
            saved.pages[0].library.definitions.remove(&base_id);
        }
        saved.pages[0]
            .library
            .definitions
            .insert(alias.lib_id.clone(), alias.clone());
        let source = saved.to_kicad_sch().unwrap();
        let saved = SchDocument::from_kicad_sch(&source).unwrap();
        let plan = plan_reconciliation(Some(&saved), &netlist, "Alias.kicad_sch").unwrap();
        assert!(plan.inspection_after().analysis.is_equivalent());
        let applied = plan.apply(Some(&saved)).unwrap();
        assert_eq!(managed(&applied, "R1.R").lib_id, base_id);
        assert_eq!(applied.pages[0].library.definitions["Native_1"], alias);
        if keep_base {
            assert_eq!(applied.pages[0].library.definitions[&base_id], base);
        }
        let output = patch_page_source(&source, &applied.pages[0])
            .unwrap()
            .unwrap_or(source);
        let reopened = SchDocument::from_kicad_sch(&output).unwrap();
        let second = plan_reconciliation(Some(&reopened), &netlist, "Alias.kicad_sch").unwrap();
        assert!(second.is_empty(), "{:#?}", second.edits());
        assert!(
            patch_page_source(&output, &second.apply(Some(&reopened)).unwrap().pages[0])
                .unwrap()
                .is_none(),
            "second apply must be byte unchanged"
        );
    }
}

#[test]
fn refreshing_one_shared_alias_does_not_change_the_other_instance() {
    for use_alias in [false, true] {
        let mut netlist = common::compile_fixture("analysis", "simple.zen");
        let mut saved = plan_reconciliation(None, &netlist, "Alias.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        let base_id = managed(&saved, "R1.R").lib_id.clone();
        let base = saved.pages[0].library.definitions[&base_id].clone();
        let alias = with_name(&base, if use_alias { "Native_1" } else { &base_id });
        for path in ["R1.R", "R2.R"] {
            managed_mut(&mut saved, path).lib_name = use_alias.then(|| alias.lib_id.clone());
        }
        saved.pages[0].library.definitions.clear();
        saved.pages[0]
            .library
            .definitions
            .insert(alias.lib_id.clone(), alias.clone());

        // Same library identity, but an authoritative replacement changes a pin's name.
        // Equal physical numbers alone must not preserve the old electrical interface.
        let raw = format_tree(&base.sexpr, FormatMode::Normal);
        let old_name = alias.placed_pins(managed(&saved, "R1.R")).unwrap()[0]
            .name
            .clone();
        let replacement = raw.replace(&format!("(name \"{old_name}\""), "(name \"CHANGED\"");
        assert_ne!(raw, replacement);
        let instance = netlist
            .instances
            .values_mut()
            .find(|instance| instance.reference_designator.as_deref() == Some("R1"))
            .unwrap();
        instance
            .attributes
            .insert("__symbol_value".into(), AttributeValue::String(replacement));
        let plan = plan_reconciliation(Some(&saved), &netlist, "Alias.kicad_sch").unwrap();
        let applied = plan.apply(Some(&saved)).unwrap();
        let first = managed(&applied, "R1.R");
        let second = managed(&applied, "R2.R");
        assert_eq!(first.lib_id, base_id);
        assert_ne!(first.library_key(), second.library_key());
        assert_eq!(
            applied.pages[0].library.definitions[second.library_key()],
            alias
        );
        assert!(
            applied.pages[0].library.definitions[first.library_key()]
                .placed_pins(first)
                .unwrap()
                .iter()
                .any(|pin| pin.name == "CHANGED")
        );
        let source = applied.to_kicad_sch().unwrap();
        let reopened = SchDocument::from_kicad_sch(&source).unwrap();
        let second = plan_reconciliation(Some(&reopened), &netlist, "Alias.kicad_sch").unwrap();
        assert!(second.is_empty(), "{:#?}", second.edits());
    }
}

#[test]
fn stale_alias_pin_or_unit_interfaces_are_refreshed() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = plan_reconciliation(None, &netlist, "Alias.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let base_id = managed(&document, "R1.R").lib_id.clone();
    let base = document.pages[0].library.definitions[&base_id].clone();
    for extra_unit in [false, true] {
        let mut saved = document.clone();
        let mut alias = with_name(&base, "Native_1");
        if extra_unit {
            alias.sexpr.as_list_mut().unwrap().push(
                pcb_sexpr::parse(
                    r#"(symbol "Native_1_2_1" (pin passive line (at 0 0 0)
                    (length 0) (name "BAD") (number "99")))"#,
                )
                .unwrap(),
            );
        } else {
            let raw = format_tree(&alias.sexpr, FormatMode::Normal);
            let changed = raw.replace("(number \"1\"", "(number \"99\"");
            assert_ne!(raw, changed);
            alias = SymbolDefinition::from_kicad_symbol_sexpr(&changed).unwrap();
        }
        managed_mut(&mut saved, "R1.R").lib_name = Some(alias.lib_id.clone());
        saved.pages[0]
            .library
            .definitions
            .insert(alias.lib_id.clone(), alias);
        let plan = plan_reconciliation(Some(&saved), &netlist, "Alias.kicad_sch").unwrap();
        let applied = plan.apply(Some(&saved)).unwrap();
        let symbol = managed(&applied, "R1.R");
        assert_eq!(symbol.lib_id, base_id);
        assert_eq!(
            applied.pages[0].library.definitions[symbol.library_key()],
            base
        );
        assert!(
            !applied.pages[0]
                .library
                .definitions
                .contains_key("Native_1")
        );
        assert!(
            plan_reconciliation(Some(&applied), &netlist, "Alias.kicad_sch")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn explicit_library_replacement_is_not_mistaken_for_a_cache_rename() {
    let mut netlist = common::compile_fixture("analysis", "simple.zen");
    let mut saved = plan_reconciliation(None, &netlist, "Alias.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let base_id = managed(&saved, "R1.R").lib_id.clone();
    let base = saved.pages[0].library.definitions[&base_id].clone();
    let alias = with_name(&base, "Native_1");
    managed_mut(&mut saved, "R1.R").lib_name = Some(alias.lib_id.clone());
    saved.pages[0]
        .library
        .definitions
        .insert(alias.lib_id.clone(), alias);
    let replacement = with_name(&base, "Other:Resistor");
    netlist
        .instances
        .values_mut()
        .find(|instance| instance.reference_designator.as_deref() == Some("R1"))
        .unwrap()
        .attributes
        .insert(
            "__symbol_value".into(),
            AttributeValue::String(format_tree(&replacement.sexpr, FormatMode::Normal)),
        );
    let applied = plan_reconciliation(Some(&saved), &netlist, "Alias.kicad_sch")
        .unwrap()
        .apply(Some(&saved))
        .unwrap();
    assert_eq!(managed(&applied, "R1.R").lib_id, "Other:Resistor");
    assert_eq!(managed(&applied, "R2.R").lib_id, base_id);
    assert!(
        plan_reconciliation(Some(&applied), &netlist, "Alias.kicad_sch")
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "requires KiCad 10 CLI"]
fn native_netlist_export_preserves_alias_pin_partitions_after_two_applies() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "Alias.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let base_id = managed(&document, "R1.R").lib_id.clone();
    let alias = with_name(&document.pages[0].library.definitions[&base_id], "Native_1");
    managed_mut(&mut document, "R1.R").lib_name = Some(alias.lib_id.clone());
    document.pages[0]
        .library
        .definitions
        .insert(alias.lib_id.clone(), alias);
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("Alias.kicad_sch");
    let output = directory.path().join("Alias.net");
    let expected: BTreeSet<BTreeSet<(String, String)>> = [
        vec![("R1", "1")],
        vec![("R1", "2"), ("R2", "1")],
        vec![("R2", "2")],
    ]
    .into_iter()
    .map(|pins| {
        pins.into_iter()
            .map(|(reference, pin)| (reference.to_owned(), pin.to_owned()))
            .collect()
    })
    .collect();
    let mut source = document.to_kicad_sch().unwrap();
    for apply in 0..3 {
        std::fs::write(&file, &source).unwrap();
        let exported = std::process::Command::new("kicad-cli")
            .args([
                "sch",
                "export",
                "netlist",
                "--format",
                "kicadsexpr",
                "--output",
            ])
            .arg(&output)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let native = pcb_sexpr::parse(&std::fs::read_to_string(&output).unwrap()).unwrap();
        let partitions = native
            .find_list("nets")
            .unwrap()
            .iter()
            .filter_map(Sexpr::as_list)
            .filter(|items| items.first().and_then(Sexpr::as_sym) == Some("net"))
            .map(|net| {
                net.iter()
                    .filter_map(Sexpr::as_list)
                    .filter(|items| items.first().and_then(Sexpr::as_sym) == Some("node"))
                    .map(|node| {
                        (
                            pcb_sexpr::kicad::string_prop(node, "ref").unwrap(),
                            pcb_sexpr::kicad::string_prop(node, "pin").unwrap(),
                        )
                    })
                    .collect::<BTreeSet<_>>()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(partitions, expected, "native export after {apply} applies");
        let reopened = SchDocument::from_kicad_sch(&source).unwrap();
        let applied = plan_reconciliation(Some(&reopened), &netlist, "Alias.kicad_sch")
            .unwrap()
            .apply(Some(&reopened))
            .unwrap();
        let patch = patch_page_source(&source, &applied.pages[0]).unwrap();
        if apply > 0 {
            assert!(patch.is_none(), "second apply changes source bytes");
        }
        source = patch.unwrap_or(source);
    }
}

fn managed<'a>(document: &'a SchDocument, path: &str) -> &'a Symbol {
    document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        })
        .unwrap()
}

fn managed_mut<'a>(document: &'a mut SchDocument, path: &str) -> &'a mut Symbol {
    document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        })
        .unwrap()
}

fn with_name(definition: &SymbolDefinition, name: &str) -> SymbolDefinition {
    let stem = definition.lib_id.rsplit(':').next().unwrap();
    let source = format_tree(&definition.sexpr, FormatMode::Normal)
        .replace(
            &format!("\"{}\"", definition.lib_id),
            &format!("\"{name}\""),
        )
        .replace(&format!("\"{stem}_"), &format!("\"{name}_"));
    SymbolDefinition::from_kicad_symbol_sexpr(&source).unwrap()
}
