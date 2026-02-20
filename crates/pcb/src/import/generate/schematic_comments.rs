use super::schematic_placement::{SchematicPlacementMapper, format_pcb_sch_comment_line};
use super::schematic_types::{ImportSchematicPositionComment, ImportSchematicTargetKind};
use super::*;

pub(super) fn build_flat_component_schematic_positions(
    components: &[(&KiCadUuidPathKey, &ImportComponentData)],
    refdes_instance_names: &BTreeMap<KiCadRefDes, String>,
    generated_components: &GeneratedComponents,
) -> BTreeMap<String, ImportSchematicPositionComment> {
    let mut out: BTreeMap<String, ImportSchematicPositionComment> = BTreeMap::new();
    let module_target_kind: BTreeMap<&str, ImportSchematicTargetKind> = generated_components
        .module_decls
        .iter()
        .map(|(ident, path)| {
            (
                ident.as_str(),
                ImportSchematicTargetKind::from_module_path(path),
            )
        })
        .collect();

    for (anchor, component) in components {
        let Some(component_name) = generated_components.anchor_to_component_name.get(*anchor)
        else {
            continue;
        };
        let target_kind = generated_components
            .anchor_to_module_ident
            .get(*anchor)
            .and_then(|ident| module_target_kind.get(ident.as_str()))
            .copied()
            .unwrap_or(ImportSchematicTargetKind::Other);

        let refdes = component.netlist.refdes.clone();
        let instance_name = refdes_instance_names
            .get(&refdes)
            .cloned()
            .unwrap_or_else(|| refdes.as_str().to_string());
        let base_key = format!("{instance_name}.{component_name}");

        let Some(schematic) = component.schematic.as_ref() else {
            continue;
        };

        let mut unit_positions: BTreeMap<i64, (bool, ImportSchematicPositionComment)> =
            BTreeMap::new();
        for (unit_key, unit_data) in &schematic.units {
            let Some(at) = unit_data.at.as_ref() else {
                continue;
            };
            let Some(unit_number) = unit_data.unit else {
                continue;
            };

            let prefer_existing = unit_key == *anchor;
            let position = schematic_position_comment_from_unit(unit_data, at.clone(), target_kind);
            match unit_positions.entry(unit_number) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((prefer_existing, position));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if prefer_existing {
                        entry.insert((true, position));
                    }
                }
            }
        }

        let is_multi_unit = component.netlist.unit_pcb_paths.len() > 1 || unit_positions.len() > 1;
        if is_multi_unit && !unit_positions.is_empty() {
            for (unit_number, (_preferred, position)) in unit_positions {
                out.insert(format!("{base_key}@U{unit_number}"), position);
            }
            continue;
        }

        if let Some(unit) = extract_anchor_schematic_unit(anchor, component)
            && let Some(at) = unit.at.as_ref()
        {
            out.insert(
                base_key,
                schematic_position_comment_from_unit(unit, at.clone(), target_kind),
            );
        }
    }

    out
}

pub(super) fn build_net_symbol_positions_for_sheet(
    sheet_path: &KiCadSheetPath,
    module_plan: &ImportModuleBoundaryNets,
    net_decls: &ImportedNetDecls,
    net_kinds_by_net: &BTreeMap<KiCadNetName, ImportNetKindClassification>,
    power_symbol_decls: &[ImportSchematicPowerSymbolDecl],
) -> BTreeMap<String, ImportSchematicPositionComment> {
    let mut by_net_name: BTreeMap<String, Vec<(String, ImportSchematicPositionComment)>> =
        BTreeMap::new();

    for decl in power_symbol_decls {
        if &decl.sheet_path != sheet_path {
            continue;
        }
        let Some(at) = decl.at.clone() else {
            continue;
        };
        let Some(value) = decl
            .value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let kicad_net_name = KiCadNetName::from(value.to_string());

        let kind = net_kinds_by_net
            .get(&kicad_net_name)
            .map(|k| k.kind)
            .unwrap_or(ImportNetKind::Net);
        if kind == ImportNetKind::Net {
            continue;
        }

        let net_name_for_comment = if module_plan.nets_io_here.contains(&kicad_net_name) {
            let Some(ident) = net_decls
                .var_ident_by_kicad_name
                .get(&kicad_net_name)
                .cloned()
            else {
                continue;
            };
            ident
        } else if module_plan.nets_defined_here.contains(&kicad_net_name) {
            let Some(name) = net_decls
                .zener_name_by_kicad_name
                .get(&kicad_net_name)
                .cloned()
            else {
                continue;
            };
            name
        } else {
            continue;
        };

        let sort_key = decl
            .symbol_uuid
            .clone()
            .or_else(|| decl.reference.clone())
            .unwrap_or_default();

        by_net_name.entry(net_name_for_comment).or_default().push((
            sort_key,
            ImportSchematicPositionComment {
                at,
                unit: None,
                mirror: decl.mirror.clone(),
                lib_name: None,
                lib_id: decl.lib_id.clone(),
                target_kind: ImportSchematicTargetKind::Other,
            },
        ));
    }

    let mut out: BTreeMap<String, ImportSchematicPositionComment> = BTreeMap::new();
    for (net_name, mut items) in by_net_name {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        for (i, (_sort_key, position)) in items.into_iter().enumerate() {
            out.insert(format!("{net_name}.{i}"), position);
        }
    }

    out
}

pub(super) fn schematic_position_comment_from_unit(
    unit: &ImportSchematicUnit,
    at: ImportSchematicAt,
    target_kind: ImportSchematicTargetKind,
) -> ImportSchematicPositionComment {
    ImportSchematicPositionComment {
        at,
        unit: unit.unit,
        mirror: unit.mirror.clone(),
        lib_name: unit.lib_name.clone(),
        lib_id: unit.lib_id.clone(),
        target_kind,
    }
}

fn extract_anchor_schematic_unit<'a>(
    anchor: &KiCadUuidPathKey,
    component: &'a ImportComponentData,
) -> Option<&'a ImportSchematicUnit> {
    let schematic = component.schematic.as_ref()?;

    // Prefer the placement for the anchor unit, which is the one joined against layout.
    if let Some(unit) = schematic.units.get(anchor)
        && unit.at.is_some()
    {
        return Some(unit);
    }

    // Fallback for incomplete/misaligned unit mappings.
    schematic.units.values().find(|unit| unit.at.is_some())
}

pub(super) fn append_schematic_position_comments(
    mut content: String,
    positions: &BTreeMap<String, ImportSchematicPositionComment>,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
) -> String {
    if positions.is_empty() {
        return content;
    }

    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');

    let mut mapper = SchematicPlacementMapper::new(schematic_lib_symbols);

    for (element_id, position) in positions {
        let pos = mapper.editor_persisted_position(position);
        content.push_str(&format_pcb_sch_comment_line(element_id, &pos));
    }

    content
}
