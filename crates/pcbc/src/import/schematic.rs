//! Adopt the original KiCad document; never reconstruct its wiring or placement.

use super::*;
use anyhow::{Context, Result, ensure};
use pcb_kicad_sch::connectivity::{
    ConnectionOrigin, ConnectivityGraph, ConnectivityItemRef, PhysicalConnectivity, PinVisibility,
};
use pcb_kicad_sch::{
    SymbolSlotKey,
    analysis::{ConnectivityInspection, SchematicIssue, inspect_schematic},
    canonical_component_path,
};
use pcb_sch::{InstanceKind, Schematic};
use pcb_sexpr::{PatchSet, Sexpr, Span, find_child_list, formatter::quote_string};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

pub(super) fn bind_imported_schematic(
    board: &MaterializedBoard,
    ir: &ImportIr,
    netlist: &Schematic,
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Result<()> {
    let paths_by_refdes = netlist
        .instances
        .iter()
        .filter(|(_, instance)| instance.kind == InstanceKind::Component)
        .map(|(instance_ref, _)| {
            let refdes = generated_validate::source_refdes_for_instance(
                instance_ref,
                refdes_by_instance_name,
            )
            .context("Imported component has no source identity")?;
            let path = canonical_component_path(&instance_ref.instance_path)
                .context("Imported component has no canonical path")?;
            Ok((refdes, path))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let sheet_tree = split_reused_sheet_files(&board.layout_dir, &ir.schematic_sheet_tree)?;
    let mut bindings = BTreeMap::new();
    let mut bound_slots = BTreeSet::new();
    for component in ir
        .components
        .values()
        .filter(|component| component.layout.is_some())
    {
        let path = paths_by_refdes
            .get(component.netlist.refdes.as_str())
            .context("Imported component was not generated")?;
        for (key, unit) in &component
            .schematic
            .as_ref()
            .with_context(|| {
                format!(
                    "Component {} has no schematic symbol",
                    component.netlist.refdes.as_str()
                )
            })?
            .units
        {
            let file = sheet_tree.unit_file(&component.netlist.refdes, key)?;
            let slot = SymbolSlotKey::new(path.clone(), u32::try_from(unit.unit.unwrap_or(1))?)
                .context("Imported symbol has an invalid unit")?;
            ensure!(
                bound_slots.insert(slot.clone()),
                "{} places unit {} of {} more than once; re-annotate it in KiCad so each unit is placed once \
                 (`kicad-cli sch upgrade` of a pre-KiCad 7 schematic can reset every unit to 1)",
                file.display(),
                slot.unit(),
                component.netlist.refdes.as_str()
            );
            ensure!(
                bindings
                    .insert((file.to_path_buf(), key.symbol_uuid.clone()), slot)
                    .is_none(),
                "{} places symbol {} more than once",
                file.display(),
                key.symbol_uuid
            );
        }
    }

    let project = pcbc::kicad_schematic::KicadProject::load(&board.layout_kicad_pro)?;
    for file in &project.schematic_files {
        let relative = file.strip_prefix(&board.layout_dir)?;
        let source = fs::read_to_string(file)?;
        let root = pcb_sexpr::parse(&source)?;
        let mut patches = PatchSet::new();
        for node in root.as_list().context("Invalid schematic root")? {
            let Some(items) = node.as_list() else {
                continue;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("symbol") {
                continue;
            }
            let Some(uuid) = find_child_list(items, "uuid").and_then(|items| items.get(1)) else {
                continue;
            };
            let Some(slot) = uuid
                .as_str()
                .and_then(|uuid| bindings.remove(&(relative.to_path_buf(), uuid.to_string())))
            else {
                continue;
            };
            patches.replace_string(uuid.span, &slot.symbol_id());
            let path = property_value(items, "Path");
            if let Some(path) = path {
                patches.replace_string(path.span, slot.component_path());
            } else {
                let at = node.span.end - 1;
                patches.replace_raw(Span::new(at, at), format!(
                    "\n(property \"Path\" {} (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))\n",
                    quote_string(slot.component_path())));
            }
        }
        let mut output = Vec::new();
        patches.write_to(&source, &mut output)?;
        fs::write(file, output)?;
    }
    ensure!(
        bindings.is_empty(),
        "Some imported symbol identities were not found in the copied schematic"
    );
    bind_net_names(&board.layout_kicad_pro, netlist)?;
    let project = pcbc::kicad_schematic::KicadProject::load(&board.layout_kicad_pro)?;
    let inspection = inspect_schematic(&project.document, netlist)?;
    ensure!(
        inspection.analysis.issues().is_empty(),
        "Imported schematic does not agree with generated Zener: {}",
        inspection
            .analysis
            .issues()
            .iter()
            .map(|issue| issue_summary(issue, &inspection, &project.document))
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(())
}

fn issue_summary(
    issue: &SchematicIssue,
    inspection: &ConnectivityInspection,
    document: &pcb_kicad_sch::SchDocument,
) -> String {
    let summary = issue.summary();
    let SchematicIssue::DisconnectedNet {
        net_name,
        missing_terminals,
        ..
    } = issue
    else {
        return summary;
    };
    if !missing_terminals.is_empty() {
        return summary;
    }
    // Island names are the bound `pcb:net` names, so show what the schematic itself says.
    let items = document
        .pages
        .iter()
        .flat_map(|page| page.items.iter().map(move |item| (page, item)))
        .filter_map(|(page, item)| {
            let name = match item {
                pcb_kicad_sch::SchItem::Label(label) => Some(format!("'{}'", label.text)),
                pcb_kicad_sch::SchItem::Symbol(symbol) => symbol
                    .field_value("Value")
                    .map(|value| format!("'{value}'")),
                _ => None,
            };
            Some((
                item.id()?,
                (page.file_name.as_deref().unwrap_or_default(), name),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let id = |item: &ConnectivityItemRef| match item {
        ConnectivityItemRef::Symbol { id, .. }
        | ConnectivityItemRef::Wire { id, .. }
        | ConnectivityItemRef::Junction { id, .. }
        | ConnectivityItemRef::NoConnect { id, .. }
        | ConnectivityItemRef::Label { id, .. } => id.clone(),
        ConnectivityItemRef::SheetPin { sheet_id, .. } => sheet_id.clone(),
    };
    let pieces = inspection.analysis.nets[net_name]
        .connected_islands
        .iter()
        .map(|piece| {
            let islands = piece
                .iter()
                .map(|island| &inspection.physical.islands[island])
                .collect::<Vec<_>>();
            let names = islands
                .iter()
                .flat_map(|island| island.named_drivers.values().flatten())
                .filter_map(|driver| items.get(id(driver).as_str())?.1.clone())
                .collect::<BTreeSet<_>>();
            let files = islands
                .iter()
                .flat_map(|island| &island.items)
                .filter_map(|item| Some(items.get(id(item).as_str())?.0))
                .collect::<BTreeSet<_>>();
            let names = if names.is_empty() {
                "unlabelled wiring".to_string()
            } else {
                names.into_iter().collect::<Vec<_>>().join(", ")
            };
            format!(
                "{names} in {}",
                files.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>();
    format!("{summary}: {}", pieces.join(" | "))
}

fn bind_net_names(project_file: &Path, netlist: &Schematic) -> Result<()> {
    let project = pcbc::kicad_schematic::KicadProject::load(project_file)?;
    let mut unbound = project.document.clone();
    for item in unbound.pages.iter_mut().flat_map(|page| &mut page.items) {
        let fields = match item {
            pcb_kicad_sch::SchItem::Symbol(symbol) => &mut symbol.fields,
            pcb_kicad_sch::SchItem::Label(label) => &mut label.fields,
            _ => continue,
        };
        fields.retain(|name, _| name != "pcb:net" && !name.starts_with("pcb:net:"));
    }
    let physical = PhysicalConnectivity::from_kicad(&unbound, PinVisibility::IncludeHidden)?;
    let expected = ConnectivityGraph::from_zener(netlist)?;
    let mut fields: BTreeMap<(String, String), BTreeMap<String, String>> = BTreeMap::new();
    for group in &physical.graph.groups {
        let names = expected
            .groups
            .iter()
            .filter(|expected| {
                expected
                    .terminals
                    .iter()
                    .any(|terminal| group.terminals.iter().any(|other| terminal.matches(other)))
            })
            .flat_map(|group| &group.names)
            .collect::<BTreeSet<_>>();
        ensure!(
            names.len() <= 1,
            "Original schematic shorts generated nets: {names:?}"
        );
        let Some(name) = names.first() else { continue };
        for origin in &group.origins {
            let ConnectionOrigin::KiCadIsland(island) = origin else {
                continue;
            };
            for drivers in physical.islands[island].named_drivers.values() {
                for driver in drivers {
                    let (page_id, id) = match driver {
                        ConnectivityItemRef::Label { page_id, id }
                        | ConnectivityItemRef::Symbol { page_id, id } => (page_id, id),
                        _ => continue,
                    };
                    fields
                        .entry((page_id.clone(), id.clone()))
                        .or_default()
                        .insert("pcb:net".into(), (*name).clone());
                }
            }
            for (source_name, owners) in &physical.islands[island].implicit_power_drivers {
                for owner in owners {
                    fields
                        .entry((owner.page_id.clone(), owner.symbol_id.clone()))
                        .or_default()
                        .insert(format!("pcb:net:{source_name}"), (*name).clone());
                }
            }
        }
    }
    for (page, file) in project.document.pages.iter().zip(&project.schematic_files) {
        let source = fs::read_to_string(file)?;
        let root = pcb_sexpr::parse(&source)?;
        let mut patches = PatchSet::new();
        for node in root.as_list().context("Invalid schematic root")? {
            let Some(items) = node.as_list() else {
                continue;
            };
            let Some(id) = find_child_list(items, "uuid")
                .and_then(|items| items.get(1))
                .and_then(Sexpr::as_str)
            else {
                continue;
            };
            let Some(fields) = fields.get(&(page.id.clone(), id.to_string())) else {
                continue;
            };
            let mut added = String::new();
            for (name, value) in fields {
                if let Some(existing) = property_value(items, name) {
                    patches.replace_string(existing.span, value);
                    continue;
                }
                added.push_str(&format!(
                    "\n(property {} {} (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))\n",
                    quote_string(name),
                    quote_string(value)
                ));
            }
            patches.replace_raw(Span::new(node.span.end - 1, node.span.end - 1), added);
        }
        if !patches.is_empty() {
            let mut output = Vec::new();
            patches.write_to(&source, &mut output)?;
            fs::write(file, output)?;
        }
    }
    Ok(())
}

/// The persistent schematic keeps one page per module instance, so every further instance of a
/// reused sheet file gets its own copy, as `pcb apply schematic` creates for repeated modules.
/// Each file keeps only its own instance's annotations; copies get a new page UUID.
/// Returns the sheet tree with each instance's file.
fn split_reused_sheet_files(layout_dir: &Path, tree: &ImportSheetTree) -> Result<ImportSheetTree> {
    let mut instances = BTreeMap::<&Path, Vec<&KiCadSheetPath>>::new();
    for (sheet, node) in &tree.nodes {
        if let Some(file) = &node.schematic_file {
            instances.entry(file).or_default().push(sheet);
        }
    }
    let mut split = tree.clone();
    let mut used = instances
        .keys()
        .map(|file| file.to_path_buf())
        .collect::<BTreeSet<_>>();
    for (file, sheets) in &instances {
        let stem = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("Schematic file name is not UTF-8")?;
        for sheet in &sheets[1..] {
            let copy = (2..)
                .map(|n| file.with_file_name(format!("{stem}_{n}.kicad_sch")))
                .find(|copy| !used.contains(copy) && !layout_dir.join(copy).exists())
                .expect("unbounded candidates");
            used.insert(copy.clone());
            split.nodes.get_mut(*sheet).expect("sheet").schematic_file = Some(copy);
        }
    }
    if used.len() == instances.len() {
        return Ok(split);
    }

    let read = |file: &Path| {
        fs::read_to_string(layout_dir.join(file))
            .with_context(|| format!("Failed to read {}", file.display()))
    };
    let page_id = |source: &str| -> Result<String> {
        let root = pcb_sexpr::parse(source)?;
        find_child_list(root.as_list().context("Invalid schematic root")?, "uuid")
            .and_then(|items| items.get(1)?.as_str())
            .map(str::to_string)
            .context("Schematic has no uuid")
    };
    let root_id = page_id(&read(&tree.root_schematic)?)?;
    // KiCad instance paths start with the root page UUID: `/<root>/<sheet>/...`.
    let instance_path =
        |sheet: &KiCadSheetPath| format!("/{root_id}{}", sheet.as_str().trim_end_matches('/'));
    let sources = instances
        .keys()
        .map(|file| Ok((*file, read(file)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for (sheet, node) in &tree.nodes {
        let Some(source_file) = node.schematic_file.as_deref() else {
            continue;
        };
        let file = split.nodes[sheet]
            .schematic_file
            .as_deref()
            .expect("split keeps files");
        let others = instances[source_file]
            .iter()
            .filter(|other| **other != sheet)
            .map(|other| instance_path(other))
            .collect::<BTreeSet<_>>();
        let renamed = node
            .children
            .iter()
            .filter(|child| split.nodes[*child].schematic_file != tree.nodes[*child].schematic_file)
            .filter_map(|child| {
                let copy = split.nodes[child].schematic_file.as_deref()?.file_name()?;
                Some((tree.nodes[child].sheet_uuid.as_deref()?, copy.to_str()?))
            })
            .collect::<BTreeMap<_, _>>();
        if others.is_empty() && renamed.is_empty() {
            continue;
        }
        let source = &sources[source_file];
        let root = pcb_sexpr::parse(source)?;
        let items = root.as_list().context("Invalid schematic root")?;
        let mut patches = PatchSet::new();
        if file != source_file
            && let Some(id) = find_child_list(items, "uuid").and_then(|items| items.get(1))
        {
            let key = format!("{root_id}/{}", file.display());
            patches.replace_string(
                id.span,
                &uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string(),
            );
        }
        for item in items.iter().filter_map(Sexpr::as_list) {
            if !matches!(
                item.first().and_then(Sexpr::as_sym),
                Some("symbol" | "sheet")
            ) {
                continue;
            }
            let paths = find_child_list(item, "instances")
                .into_iter()
                .flatten()
                .filter_map(Sexpr::as_list)
                .flatten()
                .filter(|path| {
                    path.as_list().is_some_and(|path| {
                        path.first().and_then(Sexpr::as_sym) == Some("path")
                            && path
                                .get(1)
                                .and_then(Sexpr::as_str)
                                .is_some_and(|path| others.contains(path))
                    })
                });
            for path in paths {
                patches.replace_raw(path.span, String::new());
            }
            let sheet_file = find_child_list(item, "uuid")
                .and_then(|uuid| renamed.get(uuid.get(1)?.as_str()?))
                .zip(property_value(item, "Sheetfile"));
            if let Some((copy, value)) = sheet_file {
                let reference = value.as_str().context("Invalid Sheetfile")?;
                let directory = reference.rsplit_once('/').map(|(directory, _)| directory);
                let reference = directory.map_or(copy.to_string(), |dir| format!("{dir}/{copy}"));
                patches.replace_string(value.span, &reference);
            }
        }
        let mut output = Vec::new();
        patches.write_to(source, &mut output)?;
        fs::write(layout_dir.join(file), output)
            .with_context(|| format!("Failed to write {}", file.display()))?;
    }
    Ok(split)
}

/// The value node of a symbol's direct `(property "<name>" ...)`.
fn property_value<'a>(symbol: &'a [Sexpr], name: &str) -> Option<&'a Sexpr> {
    symbol.iter().filter_map(Sexpr::as_list).find_map(|items| {
        if items.first().and_then(Sexpr::as_sym) != Some("property") {
            return None;
        }
        let (key, value) = pcb_sexpr::kicad::symbol::property_name_value(items)?;
        (key.as_str() == Some(name)).then_some(value)
    })
}
