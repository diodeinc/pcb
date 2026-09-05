use super::schematic_apply_common as common;

use std::{collections::BTreeSet, fs};

use pcb_kicad_sch::{
    Label, LabelKind, LabelShape, LabelSpin, PinInstance, Point, SchItem, SymbolDefinition, Wire,
    analysis::{SchematicIssue, inspect_schematic},
    reconcile::plan_repairs,
};
use pcb_sch::{ATTR_SCHEMATIC_PATH, ATTR_SYMBOL_FORMAT_VERSION, AttributeValue};
use pcb_sexpr::Sexpr;
use pcbc::kicad_schematic::{KicadProject, apply_linked_schematic};

const CONNECTION_GRID_MM: f64 = 1.27;

fn assert_on_connection_grid(point: Point) {
    for coordinate in [point.x, point.y] {
        let grid_units = coordinate / CONNECTION_GRID_MM;
        assert!(
            (grid_units - grid_units.round()).abs() < 1.0e-9,
            "coordinate {coordinate} is not on KiCad's 50 mil connection grid"
        );
    }
}

fn linked_fixture(path: &std::path::Path) -> pcb_sch::Schematic {
    linked_fixture_from(path, "simple.zen")
}

fn linked_fixture_from(path: &std::path::Path, entrypoint: &str) -> pcb_sch::Schematic {
    let mut netlist = common::compile_fixture("analysis", entrypoint);
    let root = netlist.root_ref.clone().unwrap();
    let package_root = path.parent().unwrap();
    netlist
        .package_roots
        .insert("apply-test".to_string(), package_root.to_path_buf());
    netlist.instances.get_mut(&root).unwrap().attributes.insert(
        ATTR_SCHEMATIC_PATH.to_string(),
        AttributeValue::String("package://apply-test/hardware".to_string()),
    );
    netlist
}

fn linked_hierarchy_fixture(path: &std::path::Path) -> pcb_sch::Schematic {
    let mut netlist = common::compile_fixture("hierarchy", "root.zen");
    let root = netlist.root_ref.clone().unwrap();
    netlist.package_roots.insert(
        "apply-test".to_string(),
        path.parent().unwrap().to_path_buf(),
    );
    netlist.instances.get_mut(&root).unwrap().attributes.insert(
        ATTR_SCHEMATIC_PATH.to_string(),
        AttributeValue::String("package://apply-test/hardware".to_string()),
    );
    netlist
}

#[test]
fn apply_restores_deleted_sheet_instance_without_recreating_child_files() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_hierarchy_fixture(&project_dir);
    let created = apply_linked_schematic(&netlist).unwrap().unwrap();
    let mut project = KicadProject::load(&project_dir).unwrap();
    let original = project.document.clone();
    let parent = project
        .document
        .pages
        .iter_mut()
        .find(|page| {
            page.items
                .iter()
                .any(|item| matches!(item, SchItem::Sheet(_)))
        })
        .unwrap();
    let parent_file = project_dir.join(parent.file_name.as_ref().unwrap());
    let originals = created
        .schematic_files
        .iter()
        .map(|path| (path.clone(), fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    let SchItem::Sheet(sheet) = parent
        .items
        .iter_mut()
        .find(|item| matches!(item, SchItem::Sheet(_)))
        .unwrap()
    else {
        unreachable!()
    };
    sheet.placed = false;
    let source = fs::read_to_string(&parent_file).unwrap();
    let deleted = pcb_kicad_sch::patch_page_source(&source, parent)
        .unwrap()
        .unwrap();
    fs::write(&parent_file, deleted).unwrap();

    let reopened = KicadProject::load(&project_dir).unwrap();
    assert_eq!(reopened.document.pages.len(), original.pages.len());
    let inspection = inspect_schematic(&reopened.document, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::MissingSheet { .. }))
    );
    assert!(
        !inspection
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::MissingSymbol { .. }))
    );
    assert!(apply_linked_schematic(&netlist).unwrap().unwrap().changed);
    let restored = KicadProject::load(&project_dir).unwrap().document;
    for page in &original.pages {
        let actual = restored
            .pages
            .iter()
            .find(|other| other.id == page.id)
            .unwrap();
        let mut expected_items = page.items.iter().collect::<Vec<_>>();
        let mut actual_items = actual.items.iter().collect::<Vec<_>>();
        expected_items.sort_by_key(|item| item.id());
        actual_items.sort_by_key(|item| item.id());
        assert_eq!(actual_items, expected_items);
    }
    for (path, contents) in originals {
        if path != parent_file {
            assert_eq!(
                fs::read(path).unwrap(),
                contents,
                "child file must be reused unchanged"
            );
        }
    }
    assert!(!apply_linked_schematic(&netlist).unwrap().unwrap().changed);
}

#[test]
fn apply_recreates_child_deleted_after_its_sheet_placement() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_hierarchy_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();
    let mut project = KicadProject::load(&project_dir).unwrap();
    let parent = project
        .document
        .pages
        .iter_mut()
        .find(|page| {
            page.items
                .iter()
                .any(|item| matches!(item, SchItem::Sheet(_)))
        })
        .unwrap();
    let parent_file = project_dir.join(parent.file_name.as_ref().unwrap());
    let child_file = parent
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) => {
                sheet.placed = false;
                Some(parent_file.parent().unwrap().join(sheet.file_name()))
            }
            _ => None,
        })
        .unwrap();
    let source = fs::read_to_string(&parent_file).unwrap();
    fs::write(
        &parent_file,
        pcb_kicad_sch::patch_page_source(&source, parent)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    fs::remove_file(&child_file).unwrap();

    KicadProject::load(&project_dir).expect("stale placement metadata must not block loading");
    assert!(apply_linked_schematic(&netlist).unwrap().unwrap().changed);
    assert!(
        child_file.is_file(),
        "netlist content recreates the missing child"
    );
    let repaired = KicadProject::load(&project_dir).unwrap();
    assert!(
        inspect_schematic(&repaired.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    assert!(!apply_linked_schematic(&netlist).unwrap().unwrap().changed);
}

fn expose_root_port(netlist: &mut pcb_sch::Schematic, port_name: &str, net_name: &str) {
    let net_id = netlist
        .nets
        .values()
        .find(|net| net.name == net_name)
        .unwrap_or_else(|| panic!("missing test net {net_name}"))
        .id;
    let root = netlist.root_ref.as_ref().unwrap();
    netlist.instances.get_mut(root).unwrap().attributes.insert(
        "__signature".to_string(),
        AttributeValue::Json(serde_json::json!({
            "parameters": [{
                "name": port_name,
                "is_config": false,
                "value": {
                    "Net": {
                        "id": net_id,
                        "name": net_name,
                        "properties": {}
                    }
                }
            }]
        })),
    );
}

fn hide_symbol_pin(node: &mut Sexpr, number: &str) -> bool {
    let is_target = node.as_list().is_some_and(|items| {
        items.first().and_then(Sexpr::as_sym) == Some("pin")
            && items.iter().filter_map(Sexpr::as_list).any(|child| {
                child.first().and_then(Sexpr::as_sym) == Some("number")
                    && child.get(1).and_then(Sexpr::as_atom) == Some(number)
            })
    });
    if is_target {
        node.as_list_mut().unwrap().push(Sexpr::list(vec![
            Sexpr::symbol("hide"),
            Sexpr::symbol("yes"),
        ]));
        return true;
    }
    node.as_list_mut()
        .is_some_and(|items| items.iter_mut().any(|child| hide_symbol_pin(child, number)))
}

#[test]
fn creates_a_verified_project_and_then_makes_no_changes() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);

    let created = apply_linked_schematic(&netlist).unwrap().unwrap();
    assert!(created.created);
    assert!(created.changed);
    assert_eq!(
        created.schematic_files[0].file_name().unwrap(),
        "Simple.kicad_sch"
    );
    let source = fs::read(&created.schematic_files[0]).unwrap();

    let unchanged = apply_linked_schematic(&netlist).unwrap().unwrap();
    assert!(!unchanged.created);
    assert!(!unchanged.changed);
    assert_eq!(fs::read(&unchanged.schematic_files[0]).unwrap(), source);

    let project = KicadProject::load(project_dir).unwrap();
    let analysis = inspect_schematic(&project.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());

    let managed_symbols = project.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!managed_symbols.is_empty());
    for symbol in managed_symbols {
        assert_on_connection_grid(symbol.at);
        let reference = symbol.field("Reference").unwrap();
        let value = symbol.field("Value").unwrap();
        assert_ne!(reference.at, symbol.at);
        assert_ne!(value.at, symbol.at);
        assert_ne!(reference.at, value.at);
    }
    let label_spins = project.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) if matches!(label.kind, LabelKind::Local) => {
                assert_on_connection_grid(label.at);
                Some(label.spin)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(label_spins.contains(&LabelSpin::Up));
    assert!(label_spins.contains(&LabelSpin::Bottom));
    assert!(
        label_spins
            .iter()
            .all(|spin| matches!(spin, LabelSpin::Up | LabelSpin::Bottom))
    );
    assert!(!project.document.pages[0].items.iter().any(
        |item| matches!(item, SchItem::Label(label) if matches!(label.kind, LabelKind::Global { .. }))
    ));
}

#[test]
fn creates_and_reloads_requested_net_symbols() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture_from(&project_dir, "net_symbols.zen");

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let project = KicadProject::load(&project_dir).unwrap();
    assert!(project.document.pages[0].items.iter().any(|item| {
        matches!(item, SchItem::Symbol(symbol)
            if symbol.field_value("Path").is_none()
                && symbol.field_value("Value") == Some("GROUND"))
    }));
    assert!(
        !project.document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "GROUND"))
    );
    assert!(
        inspect_schematic(&project.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let unchanged = apply_linked_schematic(&netlist).unwrap().unwrap();
    assert!(!unchanged.changed);
}

#[test]
fn initializes_each_linked_module_instance_as_its_own_child_sheet() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_hierarchy_fixture(&project_dir);

    let created = apply_linked_schematic(&netlist).unwrap().unwrap();

    assert_eq!(
        created
            .schematic_files
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Hierarchy.kicad_sch",
            "FILTER_A.kicad_sch",
            "FILTER_B.kicad_sch"
        ]
    );
    let project = KicadProject::load(&created.project_file).unwrap();
    let root = project
        .document
        .pages
        .iter()
        .find(|page| page.file_name.as_deref() == Some("Hierarchy.kicad_sch"))
        .unwrap();
    assert_eq!(
        root.items
            .iter()
            .filter_map(|item| match item {
                SchItem::Sheet(sheet) => Some(sheet.file_name()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["FILTER_A.kicad_sch", "FILTER_B.kicad_sch"]
    );
    for (file_name, component_path) in [
        ("FILTER_A.kicad_sch", "FILTER_A.R1"),
        ("FILTER_B.kicad_sch", "FILTER_B.R1"),
    ] {
        let page = project
            .document
            .pages
            .iter()
            .find(|page| page.file_name.as_deref() == Some(file_name))
            .unwrap();
        assert!(page.items.iter().any(|item| {
            matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path").is_some_and(|path| path.starts_with(component_path)))
        }));
    }
    assert!(
        inspect_schematic(&project.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let before = created
        .schematic_files
        .iter()
        .map(|path| (path.clone(), fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    let unchanged = apply_linked_schematic(&netlist).unwrap().unwrap();
    assert!(!unchanged.changed);
    for (path, source) in before {
        assert_eq!(fs::read(path).unwrap(), source);
    }

    // Hierarchy aliases are KiCad organization, not reconciliation metadata.
    // Change them to an equivalent non-generated convention and verify that
    // apply does not restore our initial naming policy.
    let mut edited = KicadProject::load(&created.project_file).unwrap();
    let mut aliases_by_file = std::collections::BTreeMap::new();
    for page in &mut edited.document.pages {
        for sheet in page.items.iter_mut().filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        }) {
            let aliases = sheet
                .pins
                .iter_mut()
                .map(|pin| {
                    let old = pin.name.clone();
                    pin.name = format!("CUSTOM_{old}");
                    (old, pin.name.clone())
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            aliases_by_file.insert(sheet.file_name().to_string(), aliases);
        }
    }
    for page in &mut edited.document.pages {
        let Some(aliases) = page
            .file_name
            .as_deref()
            .and_then(|file| aliases_by_file.get(file))
        else {
            continue;
        };
        for label in page.items.iter_mut().filter_map(|item| match item {
            SchItem::Label(label) if matches!(label.kind, LabelKind::Hierarchical { .. }) => {
                Some(label)
            }
            _ => None,
        }) {
            if let Some(alias) = aliases.get(&label.text) {
                label.text = alias.clone();
            }
        }
    }
    assert!(
        inspect_schematic(&edited.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    for file in edited.document.to_kicad_sch_files() {
        fs::write(edited.directory.join(file.file_name.unwrap()), file.content).unwrap();
    }
    let before = created
        .schematic_files
        .iter()
        .map(|path| (path.clone(), fs::read(path).unwrap()))
        .collect::<Vec<_>>();

    let unchanged = apply_linked_schematic(&netlist).unwrap().unwrap();

    // Source aliases are preserved; only their recovery metadata is refreshed.
    assert!(unchanged.changed);
    assert!(!apply_linked_schematic(&netlist).unwrap().unwrap().changed);
    for (path, source) in before {
        assert_eq!(fs::read(path).unwrap(), source);
    }
}

#[test]
fn adds_an_uninitialized_linked_module_to_an_existing_project() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_hierarchy_fixture(&project_dir);
    let created = apply_linked_schematic(&netlist).unwrap().unwrap();
    let missing_file = project_dir.join("FILTER_B.kicad_sch");

    let mut incomplete = KicadProject::load(&created.project_file).unwrap();
    incomplete
        .document
        .pages
        .retain(|page| page.file_name.as_deref() != Some("FILTER_B.kicad_sch"));
    for page in &mut incomplete.document.pages {
        page.items.retain(|item| {
            !matches!(item, SchItem::Sheet(sheet) if sheet.file_name() == "FILTER_B.kicad_sch")
        });
    }
    for file in incomplete.document.to_kicad_sch_files() {
        fs::write(
            incomplete.directory.join(file.file_name.unwrap()),
            file.content,
        )
        .unwrap();
    }
    pcb_kicad_sch::sync_sheet_placements(&mut incomplete.project, &incomplete.document).unwrap();
    fs::write(
        &created.project_file,
        serde_json::to_string_pretty(&incomplete.project).unwrap(),
    )
    .unwrap();
    fs::remove_file(&missing_file).unwrap();

    let repaired = apply_linked_schematic(&netlist).unwrap().unwrap();

    assert!(repaired.changed);
    assert!(!repaired.created);
    assert!(missing_file.is_file());
    let reloaded = KicadProject::load(&repaired.project_file).unwrap();
    assert!(
        inspect_schematic(&reloaded.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn rejects_pre_kicad_10_symbol_libraries() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let mut netlist = linked_fixture(&project_dir);
    let component = netlist
        .instances
        .values_mut()
        .find(|instance| instance.kind == pcb_sch::InstanceKind::Component)
        .expect("component");
    assert!(matches!(
        component.attributes.get("__symbol_value"),
        Some(AttributeValue::String(_))
    ));
    component.attributes.remove("symbol_path");
    component.attributes.insert(
        ATTR_SYMBOL_FORMAT_VERSION.to_string(),
        AttributeValue::Number(20211014.0),
    );

    let error = apply_linked_schematic(&netlist).unwrap_err();

    assert!(format!("{error:#}").contains(
        "KiCad symbol-library format version 20211014; pcb apply supports KiCad 10+ symbols"
    ));
    assert!(!project_dir.exists());
}

#[test]
fn preserves_user_symbol_and_equivalent_label_geometry() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let symbol = project.document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .expect("managed symbol");
    let delta = Point::new(50.8, 0.0);
    symbol.at.x += delta.x;
    for field in symbol.fields.values_mut() {
        field.at.x += delta.x;
    }
    symbol.fields.get_mut("Reference").unwrap().at.y -= 5.08;
    symbol.fields.get_mut("Value").unwrap().at.y += 7.62;
    symbol.fields.get_mut("Reference").unwrap().hidden = true;
    symbol.fields.get_mut("Value").unwrap().hidden = true;
    let expected_at = symbol.at;
    let expected_reference_at = symbol.field("Reference").unwrap().at;
    let expected_value_at = symbol.field("Value").unwrap().at;
    let label = project.document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Label(label) if matches!(label.kind, LabelKind::Local) => Some(label),
            _ => None,
        })
        .expect("equivalent label");
    let label_id = label.id.clone();
    label.spin = match label.spin {
        LabelSpin::Left => LabelSpin::Right,
        _ => LabelSpin::Left,
    };
    let expected_spin = label.spin;
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(&project_dir).unwrap();
    let symbol = repaired.document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .unwrap();
    assert_eq!(symbol.at, expected_at);
    assert_eq!(symbol.field("Reference").unwrap().at, expected_reference_at);
    assert_eq!(symbol.field("Value").unwrap().at, expected_value_at);
    assert!(symbol.field("Reference").unwrap().hidden);
    assert!(symbol.field("Value").unwrap().hidden);
    let label = repaired.document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.id == label_id => Some(label),
            _ => None,
        })
        .unwrap();
    assert_eq!(label.spin, expected_spin);
}

#[test]
fn accepts_an_isolated_not_connected_pin() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let mut netlist = linked_fixture(&project_dir);
    let mut not_connected = netlist.nets.remove("RIGHT").unwrap();
    not_connected.kind = "NotConnected".to_string();
    not_connected.name.clear();
    netlist.nets.insert(String::new(), not_connected);

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let project = KicadProject::load(project_dir).unwrap();
    let analysis = inspect_schematic(&project.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
    assert!(
        !project.document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "RIGHT"))
    );
}

#[test]
fn preserves_equivalent_user_connectivity_without_normalizing_labels() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let anchors = project.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label)
                if label.text == "MID" && matches!(label.kind, LabelKind::Local) =>
            {
                Some(label.at)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), 2);
    project.document.pages[0].items.push(SchItem::Wire(Wire {
        id: "mid-island-wire".to_string(),
        a: anchors[0],
        b: anchors[1],
        unsupported: Vec::new(),
    }));
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    assert!(
        repaired.document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Wire(wire) if wire.id == "mid-island-wire"))
    );
    assert_eq!(
        repaired.document.pages[0]
            .items
            .iter()
            .filter(|item| matches!(
                item,
                SchItem::Label(label)
                    if label.text == "MID" && matches!(label.kind, LabelKind::Local)
            ))
            .count(),
        2
    );
    let analysis = inspect_schematic(&repaired.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn omits_hidden_pins_from_generated_connectivity() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let mut netlist = linked_fixture(&project_dir);
    let components = netlist
        .instances
        .values_mut()
        .filter(|instance| matches!(instance.reference_designator.as_deref(), Some("R1" | "R2")))
        .collect::<Vec<_>>();
    assert_eq!(components.len(), 2);
    for component in components {
        let AttributeValue::String(symbol_source) = component
            .attributes
            .get_mut("__symbol_value")
            .expect("embedded resistor symbol")
        else {
            panic!("embedded resistor symbol must be a string");
        };
        let mut symbol = pcb_sexpr::parse(symbol_source).unwrap();
        assert!(hide_symbol_pin(&mut symbol, "1"));
        *symbol_source = symbol.to_string();
    }

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let project = KicadProject::load(project_dir).unwrap();
    assert!(
        !project.document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "LEFT"))
    );
    let analysis = inspect_schematic(&project.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn projects_the_exact_symbol_and_library_definition_set() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let mut extra = project.document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol.clone()),
            _ => None,
        })
        .expect("managed symbol");
    extra.id = "00000000-0000-0000-0000-000000000019".to_string();
    extra.fields.remove("Path");
    extra.fields.get_mut("Reference").unwrap().value = "R19".to_string();
    extra.at.x += 100.0;
    for (index, pin) in extra.pins.iter_mut().enumerate() {
        pin.id = format!("00000000-0000-0000-0001-{index:012}");
    }
    project.document.pages[0].items.push(SchItem::Symbol(extra));
    let unused_definition = SymbolDefinition::from_kicad_symbol_sexpr(
        r#"(symbol "Test:Unused" (symbol "Unused_1_1"))"#,
    )
    .unwrap();
    project.document.pages[0]
        .library
        .definitions
        .insert(unused_definition.lib_id.clone(), unused_definition);
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    let applied = apply_linked_schematic(&netlist).unwrap().unwrap();

    assert!(applied.changed);
    let repaired = KicadProject::load(project_dir).unwrap();
    assert!(!repaired.document.pages[0].items.iter().any(
        |item| matches!(item, SchItem::Symbol(symbol) if symbol.id == "00000000-0000-0000-0000-000000000019")
    ));
    assert!(
        !repaired.document.pages[0]
            .library
            .definitions
            .contains_key("Test:Unused")
    );
    assert!(
        inspect_schematic(&repaired.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn root_interface_aliases_drive_their_component_pins() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let mut netlist = linked_fixture(&project_dir);
    expose_root_port(&mut netlist, "INPUT", "LEFT");

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let project = KicadProject::load(&project_dir).unwrap();
    let page = &project.document.pages[0];
    let hierarchical = page
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "INPUT" => Some(label),
            _ => None,
        })
        .expect("root interface label");
    assert!(matches!(hierarchical.kind, LabelKind::Hierarchical { .. }));
    assert_on_connection_grid(hierarchical.at);
    // The interface label anchors directly on the component pin it drives.
    let on_pin = page.items.iter().any(|item| match item {
        SchItem::Symbol(symbol) => page
            .library
            .definitions
            .get(&symbol.lib_id)
            .and_then(|definition| definition.placed_pins(symbol).ok())
            .is_some_and(|pins| pins.iter().any(|pin| pin.point == hierarchical.at)),
        _ => false,
    });
    assert!(on_pin, "interface label must sit on a component pin");
    assert!(
        inspect_schematic(&project.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn initializes_the_missing_schematic_in_an_existing_layout_project() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    fs::create_dir(&project_dir).unwrap();
    fs::write(
        project_dir.join("layout.kicad_pro"),
        r#"{
  "board": { "preserve": true },
  "schematic": {
    "top_level_sheets": [
      {
        "filename": "layout.kicad_sch",
        "name": "layout",
        "uuid": "00000000-0000-0000-0000-000000000000"
      }
    ]
  }
}
"#,
    )
    .unwrap();
    let netlist = linked_fixture(&project_dir);

    let created = apply_linked_schematic(&netlist).unwrap().unwrap();

    assert!(created.created);
    assert_eq!(created.project_file, project_dir.join("layout.kicad_pro"));
    assert_eq!(
        created.schematic_files,
        [project_dir.join("Simple.kicad_sch")]
    );
    assert!(!project_dir.join("layout.kicad_sch").exists());
    let project_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&created.project_file).unwrap()).unwrap();
    assert_eq!(project_json["board"]["preserve"], true);
    assert_eq!(
        project_json["schematic"]["top_level_sheets"][0]["filename"],
        "Simple.kicad_sch"
    );
    let project = KicadProject::load(&created.project_file).unwrap();
    let analysis = inspect_schematic(&project.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn refuses_to_initialize_a_top_level_schematic_outside_the_project() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    fs::create_dir(&project_dir).unwrap();
    let project_file = project_dir.join("layout.kicad_pro");
    let project_source = r#"{
  "schematic": {
    "top_level_sheets": [
      { "filename": "../outside.kicad_sch" }
    ]
  }
}
"#;
    fs::write(&project_file, project_source).unwrap();
    let outside = workspace.path().join("outside.kicad_sch");
    let netlist = linked_fixture(&project_dir);

    let error = apply_linked_schematic(&netlist).unwrap_err();

    assert!(error.to_string().contains("escapes project directory"));
    assert_eq!(fs::read_to_string(project_file).unwrap(), project_source);
    assert!(!outside.exists());
}

#[test]
fn repairs_component_identity_without_rebuilding_connectivity() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    fs::create_dir(&project_dir).unwrap();
    let fixture_dir = common::test_data_dir().join("analysis/kicad");
    for name in ["simple.kicad_pro", "simple.kicad_sch"] {
        fs::copy(fixture_dir.join(name), project_dir.join(name)).unwrap();
    }
    let netlist = linked_fixture(&project_dir);
    let project = KicadProject::load(&project_dir).unwrap();
    let mut broken = project.document.clone();
    let wire_ids = broken.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let symbol = broken.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .expect("managed component symbol");
    symbol.id = "00000000-0000-0000-0000-000000000000".to_string();
    fs::write(
        project_dir.join("simple.kicad_sch"),
        broken.to_kicad_sch().unwrap(),
    )
    .unwrap();

    let applied = apply_linked_schematic(&netlist).unwrap().unwrap();
    assert!(applied.changed);
    assert!(!applied.created);
    let repaired = KicadProject::load(project_dir).unwrap();
    let analysis = inspect_schematic(&repaired.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
    let repaired_wire_ids = repaired.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(repaired_wire_ids, wire_ids);
}

#[test]
fn repairs_a_disconnected_net_without_removing_remaining_wires() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    fs::create_dir(&project_dir).unwrap();
    let fixture_dir = common::test_data_dir().join("analysis/kicad");
    for name in ["simple.kicad_pro", "simple.kicad_sch"] {
        fs::copy(fixture_dir.join(name), project_dir.join(name)).unwrap();
    }
    let netlist = linked_fixture(&project_dir);
    let project = KicadProject::load(&project_dir).unwrap();
    let mut broken = project.document.clone();
    broken.pages[0].items.retain(|item| {
        !matches!(item, SchItem::Wire(wire) if wire.id == "7a490608-016e-58f2-8e95-047dc37bfb71")
    });
    let remaining_wire_ids = broken.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    fs::write(
        project_dir.join("simple.kicad_sch"),
        broken.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    let repaired_wire_ids = repaired.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Wire(wire) => Some(wire.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(repaired_wire_ids, remaining_wire_ids);
    let analysis = inspect_schematic(&repaired.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn removes_only_the_driver_of_an_unexpected_net() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let anchor = project.document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) => Some(label.at),
            _ => None,
        })
        .expect("generated net label");
    let original_label_ids = project.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut unexpected = Label::new("unexpected-label", "EXTRA", anchor);
    unexpected.kind = LabelKind::Global {
        shape: LabelShape::Bidirectional,
    };
    project.document.pages[0]
        .items
        .push(SchItem::Label(unexpected));
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    let repaired_label_ids = repaired.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(repaired_label_ids, original_label_ids);
    let analysis = inspect_schematic(&repaired.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn removes_a_short_without_rebuilding_unaffected_nets() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let label = |name: &str| {
        project.document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.text == name => Some(label),
                _ => None,
            })
            .unwrap_or_else(|| panic!("generated {name} label"))
    };
    let left = label("LEFT").at;
    let mid = label("MID").at;
    let original_label_ids = project.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    project.document.pages[0].items.push(SchItem::Wire(Wire {
        id: "shorting-wire".to_string(),
        a: left,
        b: mid,
        unsupported: Vec::new(),
    }));
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    assert!(
        !repaired.document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Wire(wire) if wire.id == "shorting-wire"))
    );
    let repaired_label_ids = repaired.document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(repaired_label_ids, original_label_ids);
    let analysis = inspect_schematic(&repaired.document, &netlist)
        .unwrap()
        .analysis;
    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
}

#[test]
fn ambiguous_short_uses_the_same_repair_as_the_shared_issue_planner() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let label_point = |name: &str| {
        project.document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.text == name => Some(label.at),
                _ => None,
            })
            .unwrap_or_else(|| panic!("generated {name} label"))
    };
    let left = label_point("LEFT");
    let mid = label_point("MID");
    let bend = Point::new(left.x + 12.7, left.y + 12.7);
    for (id, a, b) in [("ambiguous-a", left, bend), ("ambiguous-b", bend, mid)] {
        project.document.pages[0].items.push(SchItem::Wire(Wire {
            id: id.to_string(),
            a,
            b,
            unsupported: Vec::new(),
        }));
    }

    let inspection = inspect_schematic(&project.document, &netlist).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .expect("ambiguous short");
    let expected = plan_repairs(
        &project.document,
        &netlist,
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .unwrap()
    .apply(Some(&project.document))
    .unwrap();
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    assert_eq!(repaired.document.root_page_ids, expected.root_page_ids);
    assert_eq!(repaired.document.pages.len(), expected.pages.len());
    for (repaired, expected) in repaired.document.pages.iter().zip(&expected.pages) {
        assert_eq!(repaired.id, expected.id);
        assert_eq!(repaired.file_name, expected.file_name);
        let mut repaired_items = repaired.items.iter().collect::<Vec<_>>();
        let mut expected_items = expected.items.iter().collect::<Vec<_>>();
        repaired_items.sort_by_key(|item| item.id());
        expected_items.sort_by_key(|item| item.id());
        assert_eq!(repaired_items, expected_items);
    }
}

#[test]
fn refreshes_placed_pin_instances_from_the_netlist_symbol_definition() {
    let workspace = tempfile::tempdir().unwrap();
    let project_dir = workspace.path().join("hardware");
    let netlist = linked_fixture(&project_dir);
    apply_linked_schematic(&netlist).unwrap().unwrap();

    let mut project = KicadProject::load(&project_dir).unwrap();
    let symbol = project.document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .expect("managed symbol");
    let expected_at = symbol.at;
    let preserved_pin = symbol.pins.first().cloned().expect("placed pin instance");
    symbol.pins.push(PinInstance {
        number: "STALE".to_string(),
        id: "stale-pin-instance".to_string(),
        alternate: Some("STALE_ALTERNATE".to_string()),
        unsupported: Vec::new(),
    });
    fs::write(
        &project.schematic_files[0],
        project.document.to_kicad_sch().unwrap(),
    )
    .unwrap();

    apply_linked_schematic(&netlist).unwrap().unwrap();

    let repaired = KicadProject::load(project_dir).unwrap();
    let symbol = repaired.document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .expect("managed symbol");
    assert_eq!(symbol.at, expected_at);
    assert!(!symbol.pins.iter().any(|pin| pin.number == "STALE"));
    assert!(
        symbol
            .pins
            .iter()
            .any(|pin| { pin.number == preserved_pin.number && pin.id == preserved_pin.id })
    );
    assert!(
        inspect_schematic(&repaired.document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}
