mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    FieldHorizontalJustify, FieldJustify, FieldVerticalJustify, Label, LabelKind, Point,
    SchDocument, SchItem, SchPage, Symbol, SymbolDefinition,
    analysis::{SchematicIssue, SchematicIssueKey, inspect_schematic},
    connectivity::{PhysicalConnectivity, PinVisibility},
    deterministic_uuid,
    reconcile::{plan_reconciliation, plan_repairs},
};
use pcb_sch::{AttributeValue, Schematic};

fn connected_by_wires(page: &SchPage, start: Point, end: Point) -> bool {
    let mut pending = vec![start];
    let mut visited = Vec::new();
    while let Some(point) = pending.pop() {
        if point == end {
            return true;
        }
        if visited.contains(&point) {
            continue;
        }
        visited.push(point);
        for wire in page.items.iter().filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire),
            _ => None,
        }) {
            if wire.a == point {
                pending.push(wire.b);
            } else if wire.b == point {
                pending.push(wire.a);
            }
        }
    }
    false
}

#[test]
fn editor_core_plans_applies_analyzes_and_reopens_in_memory() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let plan = plan_reconciliation(None, &netlist, "Editor.kicad_sch").unwrap();

    assert!(!plan.is_empty());
    assert!(plan.inspection_after().analysis.is_equivalent());
    let document = plan.apply(None).unwrap();
    let physical = PhysicalConnectivity::from_kicad(&document, PinVisibility::VisibleOnly).unwrap();
    assert!(!physical.graph.components.is_empty());
    assert!(!physical.islands.is_empty());
    assert!(
        physical
            .islands
            .values()
            .any(|island| !island.items.is_empty())
    );

    let page = &document.pages[0];
    let symbol = page
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        })
        .unwrap();
    let definition = &page.library.definitions[&symbol.lib_id];
    assert!(!definition.placed_pins(symbol).unwrap().is_empty());
    assert!(symbol.visual_bounds(definition).unwrap().is_some());

    let unchanged = plan_reconciliation(Some(&document), &netlist, "Editor.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
    assert_eq!(unchanged.apply(Some(&document)).unwrap(), document);

    let mut reorganized = document.clone();
    let label = reorganized.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        })
        .unwrap();
    label.spin = match label.spin {
        pcb_kicad_sch::LabelSpin::Right => pcb_kicad_sch::LabelSpin::Left,
        _ => pcb_kicad_sch::LabelSpin::Right,
    };
    let preserved = plan_reconciliation(Some(&reorganized), &netlist, "Editor.kicad_sch").unwrap();
    assert!(preserved.is_empty(), "{:#?}", preserved.edits());
    assert_eq!(preserved.apply(Some(&reorganized)).unwrap(), reorganized);
}

#[test]
fn reconciliation_drives_each_physical_pin_of_a_logical_terminal() {
    let netlist = common::compile_fixture("multi_pad", "root.zen");
    let document = plan_reconciliation(None, &netlist, "MultiPad.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection.analysis.is_equivalent(),
        "{:#?}",
        inspection.analysis.issues()
    );

    let unchanged = plan_reconciliation(Some(&document), &netlist, "MultiPad.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
}

#[test]
fn parallel_capacitors_form_one_regular_wired_bank() {
    let netlist = common::compile_fixture("analysis", "capacitor_bank.zen");
    let document = plan_reconciliation(None, &netlist, "CapacitorBank.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    let page = &document.pages[0];
    let mut bank = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol)
                if matches!(symbol.field_value("Path"), Some("C1.C" | "C2.C" | "C3.C")) =>
            {
                Some((symbol.field_value("Path").unwrap(), symbol.at))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    bank.sort_by_key(|(path, _)| *path);
    assert_eq!(bank.len(), 3);
    assert_eq!(bank[0].1.y, bank[1].1.y);
    assert_eq!(bank[1].1.y, bank[2].1.y);
    let first_spacing = bank[1].1.x - bank[0].1.x;
    let second_spacing = bank[2].1.x - bank[1].1.x;
    assert!((first_spacing - second_spacing).abs() <= 1.0e-9);

    for (net_name, expected) in [("VCC", 1), ("GROUND", 2)] {
        assert_eq!(
            page.items
                .iter()
                .filter(|item| matches!(
                    item,
                    SchItem::Symbol(symbol)
                        if symbol.field_value("Path").is_none()
                            && symbol.field_value("Value") == Some(net_name)
                ))
                .count(),
            expected,
            "the shared bank rail should need one {net_name} symbol; C4 needs its own GROUND"
        );
    }

    let unchanged =
        plan_reconciliation(Some(&document), &netlist, "CapacitorBank.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
}

#[test]
fn generated_net_symbols_share_staircase_channels_across_orientations() {
    let netlist = common::compile_fixture("net_symbol_staircase", "root.zen");
    let document = plan_reconciliation(None, &netlist, "NetSymbolStaircase.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    let page = &document.pages[0];
    let shared_ground_count = page
        .items
        .iter()
        .filter(|item| {
            matches!(
                item,
                SchItem::Symbol(symbol)
                    if symbol.field_value("Value") == Some("GROUND_SHARED")
            )
        })
        .count();
    assert_eq!(shared_ground_count, 1);
    let mut net_symbols = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol)
                if matches!(symbol.field_value("Value"), Some("POWER_A" | "GROUND_Z")) =>
            {
                let definition = &page.library.definitions[&symbol.lib_id];
                Some((
                    symbol.field_value("Value").unwrap(),
                    definition.placed_pins(symbol).unwrap()[0].point,
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    net_symbols.sort_by(|left, right| left.1.x.total_cmp(&right.1.x));
    assert_eq!(net_symbols.len(), 2);
    let connection_points = [net_symbols[0].1, net_symbols[1].1];
    assert_ne!(connection_points[0], connection_points[1]);
    assert_ne!(connection_points[0].x, connection_points[1].x);
    let wire_count = page
        .items
        .iter()
        .filter(|item| matches!(item, SchItem::Wire(_)))
        .count();
    assert!((5..=10).contains(&wire_count), "{wire_count}");
    assert!(page.items.iter().all(|item| match item {
        SchItem::Wire(wire) => wire.a.x == wire.b.x || wire.a.y == wire.b.y,
        _ => true,
    }));
    assert!(connection_points.iter().all(|point| {
        page.items.iter().any(|item| match item {
            SchItem::Wire(wire) => wire.a == *point || wire.b == *point,
            _ => false,
        })
    }));
    let managed_pin_points = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(
                page.library.definitions[&symbol.lib_id]
                    .placed_pins(symbol)
                    .unwrap(),
            ),
            _ => None,
        })
        .flatten()
        .map(|pin| pin.point)
        .collect::<Vec<_>>();
    assert!(
        connection_points
            .iter()
            .all(|connection| !managed_pin_points.contains(connection))
    );

    let unchanged =
        plan_reconciliation(Some(&document), &netlist, "NetSymbolStaircase.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
}

#[test]
fn driven_pin_splits_missing_net_symbol_runs_into_distinct_stairs() {
    let netlist = common::compile_fixture("net_symbol_staircase", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "NetSymbolStaircase.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let component = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some("STAIR") => Some(symbol),
            _ => None,
        })
        .unwrap();
    let pins = document.pages[0].library.definitions[&component.lib_id]
        .placed_pins(component)
        .unwrap();
    let pin_point = |number: &str| pins.iter().find(|pin| pin.number == number).unwrap().point;
    let missing = [pin_point("3"), pin_point("5")];
    let driven = pin_point("4");
    let incident_wires = |document: &SchDocument, point| {
        document.pages[0]
            .items
            .iter()
            .filter(
                |item| matches!(item, SchItem::Wire(wire) if wire.a == point || wire.b == point),
            )
            .count()
    };
    let items_before = document.pages[0].items.len();
    document.pages[0].items.retain(|item| {
        !matches!(item, SchItem::Wire(wire) if missing.contains(&wire.a) || missing.contains(&wire.b))
    });
    assert!(document.pages[0].items.len() < items_before);
    let driven_before = incident_wires(&document, driven);

    let repaired = plan_reconciliation(Some(&document), &netlist, "NetSymbolStaircase.kicad_sch")
        .unwrap()
        .apply(Some(&document))
        .unwrap();

    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    assert_eq!(incident_wires(&repaired, driven), driven_before);
    let mut connection_points = repaired.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("GROUND_SHARED") => {
                let definition = &repaired.pages[0].library.definitions[&symbol.lib_id];
                Some(definition.placed_pins(symbol).unwrap()[0].point)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    connection_points.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
    });
    connection_points.dedup();
    assert_eq!(connection_points.len(), 3);
}

#[test]
fn root_interfaces_use_hierarchical_labels_directly_on_component_pins() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let page = &document.pages[0];
    let labels = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels
            .iter()
            .map(|label| label.text.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["INPUT", "OUTPUT"])
    );
    assert!(
        labels
            .iter()
            .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. }))
    );

    let pin_points = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) => Some(
                page.library.definitions[&symbol.lib_id]
                    .placed_pins(symbol)
                    .unwrap(),
            ),
            _ => None,
        })
        .flatten()
        .map(|pin| pin.point)
        .collect::<Vec<_>>();
    assert!(
        labels.iter().all(|label| pin_points.contains(&label.at)),
        "labels={labels:#?} pins={pin_points:#?}"
    );
    assert!(
        page.items
            .iter()
            .all(|item| !matches!(item, SchItem::Wire(_)))
    );
}

#[test]
fn unused_interface_labels_are_grouped_and_required() {
    let netlist = common::compile_fixture("hierarchy", "unused_root_interface.zen");
    let document = plan_reconciliation(None, &netlist, "UnusedRootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut labels = document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();
    labels.sort_by(|left, right| left.text.cmp(&right.text));

    assert_eq!(labels.len(), 6);
    assert!(
        labels
            .iter()
            .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. }))
    );
    assert!(labels.iter().all(|label| label.at.x == labels[0].at.x));
    let row_spacing = labels[1].at.y - labels[0].at.y;
    assert!(row_spacing > 0.0);
    assert!(
        labels
            .windows(2)
            .all(|pair| ((pair[1].at.y - pair[0].at.y) - row_spacing).abs() < 1e-9)
    );
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let mut missing_label = document;
    missing_label.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Label(label) if label.text == "UNUSED_3"));

    let inspection = inspect_schematic(&missing_label, &netlist).unwrap();
    assert!(matches!(
        inspection.analysis.issues(),
        [SchematicIssue::MissingPort { net_name, ports, .. }]
            if net_name == "UNUSED_3" && ports == &["UNUSED_3".to_string()]
    ));
}

#[test]
fn generated_hierarchy_connects_sheet_ports_with_orthogonal_routes() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = plan_reconciliation(None, &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    let hierarchy_wires = document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!hierarchy_wires.is_empty());
    assert!(
        hierarchy_wires
            .iter()
            .all(|wire| wire.a.x == wire.b.x || wire.a.y == wire.b.y)
    );
    let root = document
        .pages
        .iter()
        .find(|page| document.root_page_ids.contains(&page.id))
        .unwrap();
    for sheet in root.items.iter().filter_map(|item| match item {
        SchItem::Sheet(sheet) => Some(sheet),
        _ => None,
    }) {
        assert_eq!(sheet.size, Some(Point::new(38.1, 15.24)));
        assert_eq!(
            sheet.name.as_ref().unwrap().justify,
            Some(FieldJustify::new(
                Some(FieldHorizontalJustify::Left),
                Some(FieldVerticalJustify::Bottom),
            ))
        );
        assert_eq!(
            sheet.file.justify,
            Some(FieldJustify::new(
                Some(FieldHorizontalJustify::Left),
                Some(FieldVerticalJustify::Top),
            ))
        );
    }
    for page in &document.pages {
        let labels = page
            .items
            .iter()
            .filter_map(|item| match item {
                SchItem::Label(label) => Some(label),
                _ => None,
            })
            .collect::<Vec<_>>();
        if document.root_page_ids.contains(&page.id) {
            assert!(labels.iter().all(|label| label.kind == LabelKind::Local));
        } else {
            assert!(
                labels
                    .iter()
                    .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. })),
                "page={} labels={:#?}",
                page.id,
                labels
            );
        }
        assert!(!labels.is_empty());
        assert!(labels.iter().all(|label| page.items.iter().any(|item| {
            matches!(item, SchItem::Wire(wire) if wire.a == label.at || wire.b == label.at)
        })));
    }
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn complete_reconciliation_prefers_hierarchy_for_cross_page_interface_nets() {
    let netlist = common::compile_fixture("hierarchy", "root_multi_island_interface.zen");
    let document = plan_reconciliation(None, &netlist, "MultiIslandInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    let temp_labels = document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            SchItem::Label(label) if label.text == "TEMP" => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(temp_labels.len() >= 2, "labels={temp_labels:#?}");
    assert!(
        temp_labels
            .iter()
            .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. })),
        "labels={temp_labels:#?}"
    );
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn hierarchy_net_symbols_share_staircase_channels_across_orientations() {
    let netlist = common::compile_fixture("hierarchy", "root_power_hierarchy.zen");
    let document = plan_reconciliation(None, &netlist, "PowerHierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root = document
        .pages
        .iter()
        .find(|page| document.root_page_ids.contains(&page.id))
        .unwrap();
    let sheet = root
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        })
        .expect("generated child sheet exists");
    let mut connections = Vec::new();
    for (port_name, net_name) in [("INPUT", "POWER"), ("OUTPUT", "GROUND")] {
        let sheet_pin = sheet
            .pins
            .iter()
            .find(|pin| pin.name == port_name)
            .unwrap()
            .at;
        let connection = root
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Symbol(symbol) if symbol.field_value("Value") == Some(net_name) => {
                    let definition = &root.library.definitions[&symbol.lib_id];
                    let connection = definition.placed_pins(symbol).unwrap()[0].point;
                    connected_by_wires(root, sheet_pin, connection).then_some(connection)
                }
                _ => None,
            })
            .expect("sheet pin gets an offset net symbol");
        connections.push(connection);
    }
    assert_ne!(connections[0].x, connections[1].x);
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn missing_symbols_use_attached_net_symbol_orientation() {
    let mut netlist = common::compile_fixture("analysis", "initial_orientation.zen");
    netlist.net_mut("VCC_LIKE_NAME").unwrap().properties.insert(
        "__symbol_value".into(),
        AttributeValue::String(
            r#"(symbol "Custom:Side" (power global)
                  (symbol "Side_1_1"
                    (pin power_in line (at 0 0 180) (length 0)
                      (name "") (number "1"))))"#
                .into(),
        ),
    );
    let document = plan_reconciliation(None, &netlist, "InitialOrientation.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let rotation = |path: &str| {
        document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => {
                    Some(symbol.rotation)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing managed symbol {path}"))
    };

    // The custom symbol's visible pin points right, so resistor pin 1 points
    // left even though the net has a VCC-like name.
    assert_eq!(rotation("R_SINGLE.R"), pcb_kicad_sch::Rotation::Deg90);
    // Default GND and VCC symbols on opposite ends agree on this rotation.
    assert_eq!(rotation("R_BOTH.R"), pcb_kicad_sch::Rotation::Deg180);
    // Two equal GND constraints conflict, while ordinary nets do not constrain
    // orientation; both cases retain the deterministic default.
    assert_eq!(rotation("R_TIED.R"), pcb_kicad_sch::Rotation::Deg0);
    assert_eq!(rotation("R_NONE.R"), pcb_kicad_sch::Rotation::Deg0);
    // Larger symbols retain the orientation authored by their library even
    // when every attached net symbol would favor the same rotation.
    assert_eq!(rotation("U_LARGE"), pcb_kicad_sch::Rotation::Deg0);
}

#[test]
fn replacing_a_modules_only_component_reuses_its_existing_sheet() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = plan_reconciliation(None, &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut stale = document.clone();
    let filter_a_page = stale
        .pages
        .iter()
        .position(|page| page.file_name.as_deref() == Some("FILTER_A.kicad_sch"))
        .unwrap();
    let symbol = stale.pages[filter_a_page]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .unwrap();
    let original_path = symbol.field_value("Path").unwrap().to_string();
    symbol.fields.get_mut("Path").unwrap().value = "FILTER_A.REMOVED".to_string();

    let repaired = plan_reconciliation(Some(&stale), &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(Some(&stale))
        .unwrap();

    assert_eq!(repaired.pages.len(), document.pages.len());
    assert_eq!(
        repaired
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .filter(|item| {
                matches!(item, SchItem::Sheet(sheet) if sheet.file_name() == "FILTER_A.kicad_sch")
            })
            .count(),
        1
    );
    assert!(repaired.pages[filter_a_page].items.iter().any(|item| {
        matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(original_path.as_str()))
    }));
}

#[test]
fn initializes_and_semantically_adopts_net_symbols() {
    let (netlist, document) = net_symbol_fixture();

    let ground = net_symbol(&document, "GROUND");
    assert!(ground.field_value("Path").is_none());
    assert!(
        !document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "GROUND"))
    );

    let unchanged = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());

    let mut customized = document.clone();
    let ground = net_symbol_mut(&mut customized, "GROUND");
    let old_lib_id = ground.lib_id.clone();
    ground.lib_id = "Custom:Ground".to_string();
    let custom_definition = SymbolDefinition::from_kicad_symbol_sexpr(
        r#"(symbol "Custom:Ground" (power global)
          (symbol "Ground_1_1"
            (pin power_in line (at 0 0 0) (length 0)
              (name "") (number "1"))))"#,
    )
    .unwrap();
    customized.pages[0].library.definitions.remove(&old_lib_id);
    customized.pages[0]
        .library
        .definitions
        .insert(custom_definition.lib_id.clone(), custom_definition);

    let adopted = plan_reconciliation(Some(&customized), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(adopted.is_empty(), "{:#?}", adopted.edits());
}

#[test]
fn local_label_does_not_replace_a_symbol_backed_root_endpoint() {
    let (netlist, mut document) = net_symbol_fixture();
    let ground_index = document.pages[0]
        .items
        .iter()
        .position(|item| {
            matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("GROUND"))
        })
        .unwrap();
    let SchItem::Symbol(ground) = document.pages[0].items.remove(ground_index) else {
        unreachable!("selected a symbol")
    };
    let definition = document.pages[0]
        .library
        .definitions
        .remove(&ground.lib_id)
        .unwrap();
    let ground_pin = definition
        .placed_pins(&ground)
        .unwrap()
        .into_iter()
        .find(|pin| pin.is_power_input())
        .unwrap();
    document.pages[0].items.push(SchItem::Label(Label::new(
        deterministic_uuid("equivalent-ground-label"),
        "GROUND",
        ground_pin.point,
    )));
    assert!(matches!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .issues(),
        [SchematicIssue::MissingPort { net_name, .. }] if net_name == "GROUND"
    ));
}

#[test]
fn moved_net_symbol_does_not_block_a_missing_driver() {
    let (netlist, mut document) = net_symbol_fixture();
    let original = net_symbol(&document, "GROUND").clone();
    let moved_at = Point::new(original.at.x + 100.0, original.at.y);
    move_symbol(net_symbol_mut(&mut document, "GROUND"), moved_at);

    let plan = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();
    let grounds = repaired.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("GROUND") => {
                Some(symbol)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(grounds.len(), 2);
    assert!(
        grounds
            .iter()
            .any(|symbol| symbol.id == original.id && symbol.at == moved_at)
    );
    assert!(
        grounds
            .iter()
            .any(|symbol| symbol.id != original.id && symbol.at == original.at)
    );
    let unchanged = plan_reconciliation(Some(&repaired), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
}

#[test]
fn preserves_non_naming_power_symbols() {
    let (netlist, mut document) = net_symbol_fixture();
    let mut power_flag = net_symbol(&document, "GROUND").clone();
    power_flag.id = "00000000-0000-0000-0000-000000000097".to_string();
    power_flag.lib_id = "power:PWR_FLAG".to_string();
    power_flag.fields.get_mut("Value").unwrap().value = "PWR_FLAG".to_string();
    power_flag.at.x += 100.0;
    for field in power_flag.fields.values_mut() {
        field.at.x += 100.0;
    }
    document.pages[0].items.push(SchItem::Symbol(power_flag));
    let power_flag_definition = SymbolDefinition::from_kicad_symbol_sexpr(
        r#"(symbol "power:PWR_FLAG" (power global)
          (symbol "PWR_FLAG_1_1"
            (pin power_out line (at 0 0 0) (length 0)
              (name "") (number "1"))))"#,
    )
    .unwrap();
    document.pages[0]
        .library
        .definitions
        .insert(power_flag_definition.lib_id.clone(), power_flag_definition);

    let preserved = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(preserved.is_empty(), "{:#?}", preserved.edits());
}

#[test]
fn reconciliation_removes_only_an_unmatched_net_symbol() {
    let (netlist, document) = net_symbol_fixture();
    let mut with_stray = document.clone();
    let mut stray = net_symbol(&with_stray, "GROUND").clone();
    stray.id = "00000000-0000-0000-0000-000000000099".to_string();
    stray.at.x += 100.0;
    for field in stray.fields.values_mut() {
        field.at.x += 100.0;
    }
    stray.fields.get_mut("Value").unwrap().value = "STRAY".to_string();
    with_stray.pages[0].items.push(SchItem::Symbol(stray));

    assert_eq!(
        with_stray.pages[0].items.len(),
        document.pages[0].items.len() + 1
    );

    let repaired = plan_reconciliation(Some(&with_stray), &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(Some(&with_stray))
        .unwrap();
    assert_eq!(repaired, document);
}

#[test]
fn reconciliation_removes_a_matching_net_symbol_from_the_wrong_island() {
    let (netlist, document) = net_symbol_fixture();
    let mut shorted = document.clone();
    let signal_at = shorted.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "SIGNAL" => Some(label.at),
            _ => None,
        })
        .unwrap();
    let mut misplaced = net_symbol(&shorted, "GROUND").clone();
    misplaced.id = "00000000-0000-0000-0000-000000000098".to_string();
    move_symbol(&mut misplaced, signal_at);
    shorted.pages[0].items.push(SchItem::Symbol(misplaced));

    let repaired = plan_reconciliation(Some(&shorted), &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(Some(&shorted))
        .unwrap();
    assert_eq!(repaired, document);
}

fn net_symbol_fixture() -> (Schematic, SchDocument) {
    let netlist = common::compile_fixture("analysis", "net_symbols.zen");
    let document = plan_reconciliation(None, &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    (netlist, document)
}

fn net_symbol<'a>(document: &'a SchDocument, value: &str) -> &'a Symbol {
    document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some(value) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing net symbol {value}"))
}

fn net_symbol_mut<'a>(document: &'a mut SchDocument, value: &str) -> &'a mut Symbol {
    document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some(value) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing net symbol {value}"))
}

fn move_symbol(symbol: &mut Symbol, at: Point) {
    let dx = at.x - symbol.at.x;
    let dy = at.y - symbol.at.y;
    symbol.at = at;
    for field in symbol.fields.values_mut() {
        field.at.x += dx;
        field.at.y += dy;
    }
}

#[test]
fn connected_net_missing_its_port_label_reports_missing_port() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Demote the INPUT port's hierarchical label to a local label: every pin
    // stays connected, but the module no longer exposes the port.
    for page in &mut document.pages {
        for item in &mut page.items {
            if let SchItem::Label(label) = item
                && label.text == "INPUT"
            {
                label.kind = LabelKind::Local;
            }
        }
    }

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        matches!(
            inspection.analysis.issues(),
            [SchematicIssue::MissingPort { net_name, ports, .. }]
                if net_name == "INPUT" && ports == &["INPUT".to_string()]
        ),
        "{:#?}",
        inspection.analysis.issues()
    );
}

#[test]
fn symbol_backed_root_port_accepts_symbol_or_hierarchical_label_and_repairs_neither() {
    let netlist = common::compile_fixture("hierarchy", "root_symbol_interface.zen");
    let symbol_only = plan_reconciliation(None, &netlist, "RootSymbolInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let power_symbol = net_symbol(&symbol_only, "POWER");
    let power_connection = symbol_only.pages[0].library.definitions[&power_symbol.lib_id]
        .placed_pins(power_symbol)
        .unwrap()[0]
        .point;
    assert!(
        inspect_schematic(&symbol_only, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let mut neither = symbol_only.clone();
    neither.pages[0].items.retain(|item| {
        !matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("POWER"))
    });
    let inspection = inspect_schematic(&neither, &netlist).unwrap();
    assert!(matches!(
        inspection.analysis.issues(),
        [SchematicIssue::MissingPort { net_name, ports, .. }]
            if net_name == "POWER" && ports == &["POWER".to_string()]
    ));

    let mut label_only = neither.clone();
    let mut label = Label::new("power-port", "POWER", power_connection);
    label.kind = LabelKind::Hierarchical {
        shape: pcb_kicad_sch::LabelShape::Bidirectional,
    };
    label_only.pages[0].items.push(SchItem::Label(label));
    let label_inspection = inspect_schematic(&label_only, &netlist).unwrap();
    assert!(
        label_inspection.analysis.is_equivalent(),
        "{:#?}",
        label_inspection.analysis.issues()
    );

    let mut dangling_label = neither.clone();
    let mut label = Label::new("dangling-power-port", "POWER", Point::new(200.0, 200.0));
    label.kind = LabelKind::Hierarchical {
        shape: pcb_kicad_sch::LabelShape::Bidirectional,
    };
    dangling_label.pages[0].items.push(SchItem::Label(label));
    assert!(
        !inspect_schematic(&dangling_label, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let repaired = plan_repairs(
        &neither,
        &netlist,
        &inspection,
        BTreeSet::from([SchematicIssueKey::MissingPort("POWER".to_string())]),
    )
    .unwrap()
    .apply(Some(&neither))
    .unwrap();
    assert!(repaired.pages[0].items.iter().any(|item| {
        matches!(item, SchItem::Symbol(symbol)
            if symbol.field_value("Value") == Some("POWER"))
    }));
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

/// The composer mints deterministic label ids. When the editor has since
/// moved that label to another page (or the id is otherwise taken), a repair
/// must mint a fresh id instead of stealing the existing item back.
#[test]
fn repairing_a_missing_port_never_steals_a_label_from_another_page() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root_page_id = document.pages[0].id.clone();
    // Delete the composed INPUT label outright: the deterministic id the
    // repair would mint must exist only on the other page.
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Label(label) if label.text == "INPUT"));
    // Another page owns the id the repair would mint for the root's INPUT
    // context endpoint.
    let taken_id = deterministic_uuid(format!("zener:context-endpoint:{root_page_id}:INPUT:INPUT"));
    let mut extra = SchPage::new("extra");
    extra.file_name = Some("Extra.kicad_sch".to_string());
    extra.items.push(SchItem::Label(Label::new(
        &taken_id,
        "OTHER",
        Point::new(10.0, 10.0),
    )));
    document.pages.push(extra);
    document.root_page_ids.push("extra".to_string());

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| issue.key == SchematicIssueKey::MissingPort("INPUT".to_string())),
        "{:#?}",
        inspection.analysis.issues()
    );
    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        std::collections::BTreeSet::from([SchematicIssueKey::MissingPort("INPUT".to_string())]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();

    // The foreign label stays exactly where the user left it.
    let extra_page = repaired
        .pages
        .iter()
        .find(|page| page.id == "extra")
        .unwrap();
    assert!(extra_page.items.iter().any(|item| matches!(
        item,
        SchItem::Label(label) if label.id == taken_id && label.text == "OTHER"
    )));
    // The root regained its INPUT port under a fresh id.
    assert!(repaired.pages[0].items.iter().any(|item| matches!(
        item,
        SchItem::Label(label) if label.text == "INPUT"
            && matches!(label.kind, LabelKind::Hierarchical { .. })
            && label.id != taken_id
    )));
}
