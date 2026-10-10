//! Adopt the original KiCad document; never reconstruct its wiring or placement.

use super::*;
use anyhow::{Context, Result, ensure};
use pcb_kicad_sch::connectivity::{
    ConnectionOrigin, ConnectivityGraph, ConnectivityItemRef, IslandRef, PhysicalConnectivity,
    PhysicalIsland, PinVisibility,
};
use pcb_kicad_sch::{
    SymbolSlotKey,
    analysis::{ConnectivityInspection, NetAnalysis, SchematicIssue, inspect_schematic},
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
    let mut bindings = BTreeMap::new();
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
            let file = ir
                .schematic_sheet_tree
                .unit_file(&component.netlist.refdes, key)?;
            let slot = SymbolSlotKey::new(path.clone(), u32::try_from(unit.unit.unwrap_or(1))?)
                .context("Imported symbol has an invalid unit")?;
            ensure!(
                bindings
                    .insert((file.to_path_buf(), key.symbol_uuid.clone()), slot)
                    .is_none(),
                "Persistent schematics do not support managed components on reused sheet files: {}",
                file.display()
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
            let path = items
                .iter()
                .filter_map(Sexpr::as_list)
                .find(|items| {
                    items.first().and_then(Sexpr::as_sym) == Some("property")
                        && items.get(1).and_then(Sexpr::as_str) == Some("Path")
                })
                .and_then(|items| items.get(2));
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

/// Name what each piece of a split net is called, so the label or alias that splits it is visible.
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
    match inspection.analysis.nets.get(net_name) {
        Some(net) if missing_terminals.is_empty() && net.connected_islands.len() > 1 => format!(
            "{summary} ({}); connect them or rename the label so every piece uses one net name",
            split_net_pieces(net, &inspection.physical.islands, document)
        ),
        _ => summary,
    }
}

fn split_net_pieces(
    net: &NetAnalysis,
    islands: &BTreeMap<IslandRef, PhysicalIsland>,
    document: &pcb_kicad_sch::SchDocument,
) -> String {
    net.connected_islands
        .iter()
        .map(|piece| {
            let names = piece
                .iter()
                .filter_map(|island| islands.get(island))
                .flat_map(|island| &island.names)
                .map(|name| format!("'{name}'"))
                .collect::<BTreeSet<_>>();
            let files = piece
                .iter()
                .filter_map(|island| {
                    let page = document
                        .pages
                        .iter()
                        .find(|page| page.id == island.page_id)?;
                    page.file_name.clone()
                })
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
        .collect::<Vec<_>>()
        .join(" | ")
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
                if let Some(existing) = items
                    .iter()
                    .filter_map(Sexpr::as_list)
                    .find(|items| {
                        items.first().and_then(Sexpr::as_sym) == Some("property")
                            && items.get(1).and_then(Sexpr::as_str) == Some(name)
                    })
                    .and_then(|items| items.get(2))
                {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_net_names_each_piece_and_its_sheet() {
        let island = |page_id: &str, index| IslandRef {
            page_id: page_id.into(),
            index,
        };
        let named = |names: &[&str]| {
            let mut island = PhysicalIsland::default();
            island.names = names.iter().map(|name| name.to_string()).collect();
            island
        };
        let page = |id: &str, file: &str| pcb_kicad_sch::SchPage {
            file_name: Some(file.into()),
            ..pcb_kicad_sch::SchPage::new(id)
        };
        let net = NetAnalysis {
            name: "VCC".into(),
            expected_terminals: Vec::new(),
            missing_terminals: Vec::new(),
            islands: vec![island("root", 0), island("power", 0)],
            connected_islands: vec![vec![island("root", 0)], vec![island("power", 0)]],
        };
        let islands = BTreeMap::from([
            (island("root", 0), named(&["VCC"])),
            (island("power", 0), named(&["/power/VCC_ALIAS"])),
        ]);
        let document = pcb_kicad_sch::SchDocument {
            pages: vec![
                page("root", "root.kicad_sch"),
                page("power", "power.kicad_sch"),
            ],
            ..Default::default()
        };
        assert_eq!(
            split_net_pieces(&net, &islands, &document),
            "'VCC' in root.kicad_sch | '/power/VCC_ALIAS' in power.kicad_sch"
        );
    }
}
