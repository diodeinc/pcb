use super::props::{best_properties, find_property_ci};
use super::*;
use anyhow::{Context, Result};
use log::debug;
use pcb_component_gen as component_gen;
use pcb_sexpr::find_child_list;
use pcb_sexpr::Sexpr;
use pcb_sexpr::{board as sexpr_board, PatchSet, Span};
use pcb_zen_core::lang::stackup as zen_stackup;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use uuid::Uuid;

pub(super) fn generate(
    materialized: &MaterializedBoard,
    board_name: &str,
    ir: &ImportIr,
) -> Result<()> {
    let port_to_net = build_port_to_net_map(&ir.nets)?;
    let not_connected_nets = build_not_connected_nets(&ir.nets);
    let net_decls = build_net_decls(&ir.nets, &not_connected_nets);
    let reserved_idents: BTreeSet<String> =
        net_decls.decls.iter().map(|d| d.ident.clone()).collect();

    let refdes_instance_names = build_refdes_instance_name_map(&ir.components);

    let component_modules = generate_imported_components(
        &materialized.board_dir,
        &ir.components,
        &reserved_idents,
        &ir.schematic_lib_symbols,
        &ir.semantic.passives.by_component,
    )?;

    let sheet_modules = generate_sheet_modules(GenerateSheetModulesArgs {
        board_dir: &materialized.board_dir,
        board_name,
        ir,
        port_to_net: &port_to_net,
        refdes_instance_names: &refdes_instance_names,
        net_decls: &net_decls,
        components: &component_modules,
        not_connected_nets: &not_connected_nets,
    })?;

    write_imported_board_zen(ImportedBoardZenArgs {
        board_zen: &materialized.board_zen,
        board_name,
        layout_kicad_pcb: &materialized.layout_kicad_pcb,
        port_to_net: &port_to_net,
        refdes_instance_names: &refdes_instance_names,
        components: &ir.components,
        hierarchy_plan: &ir.hierarchy_plan,
        schematic_sheet_tree: &ir.schematic_sheet_tree,
        net_decls: &net_decls,
        component_modules: &component_modules,
        sheet_modules: &sheet_modules,
        not_connected_nets: &not_connected_nets,
    })?;

    Ok(())
}

struct ImportedBoardZenArgs<'a> {
    board_zen: &'a Path,
    board_name: &'a str,
    layout_kicad_pcb: &'a Path,
    port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    refdes_instance_names: &'a BTreeMap<KiCadRefDes, String>,
    components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    hierarchy_plan: &'a ImportHierarchyPlan,
    schematic_sheet_tree: &'a ImportSheetTree,
    net_decls: &'a ImportedNetDecls,
    component_modules: &'a GeneratedComponents,
    sheet_modules: &'a GeneratedSheetModules,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

fn write_imported_board_zen(args: ImportedBoardZenArgs<'_>) -> Result<()> {
    let pcb_text = fs::read_to_string(args.layout_kicad_pcb).with_context(|| {
        format!(
            "Failed to read KiCad PCB for stackup extraction: {}",
            args.layout_kicad_pcb.display()
        )
    })?;

    let (copper_layers, stackup) = match try_extract_stackup(&pcb_text, args.layout_kicad_pcb) {
        Ok(v) => v,
        Err(e) => {
            debug!("{e:#}");
            (4, None)
        }
    };

    prepatch_imported_layout_kicad_pcb(
        args.layout_kicad_pcb,
        &pcb_text,
        args.components,
        args.refdes_instance_names,
        &args.net_decls.zener_name_by_kicad_name,
        args.component_modules,
        args.sheet_modules,
    )
    .context("Failed to pre-patch imported KiCad PCB for sync hooks")?;

    let root_sheet = KiCadSheetPath::root();
    let root_plan = args
        .hierarchy_plan
        .modules
        .get(&root_sheet)
        .cloned()
        .unwrap_or_default();

    let root_net_set: BTreeSet<KiCadNetName> = root_plan.nets_defined_here;
    let root_net_idents = args.net_decls.ident_map_for_set(&root_net_set);

    let root_anchors: Vec<(&KiCadUuidPathKey, &ImportComponentData)> = args
        .components
        .iter()
        .filter(|(a, c)| {
            c.layout.is_some()
                && KiCadSheetPath::from_sheetpath_tstamps(&a.sheetpath_tstamps).as_str() == "/"
        })
        .collect();

    let root_component_calls = build_imported_instance_calls_for_instances(
        root_anchors,
        args.port_to_net,
        args.refdes_instance_names,
        &root_net_idents,
        args.component_modules,
        args.not_connected_nets,
    )?;

    let (root_sheet_module_decls, root_sheet_module_calls) = build_root_sheet_module_calls(
        args.schematic_sheet_tree,
        args.sheet_modules,
        args.hierarchy_plan,
        args.net_decls,
        &root_net_set,
        &root_component_calls,
    );

    let mut instance_calls: Vec<crate::codegen::board::ImportedInstanceCall> = Vec::new();
    instance_calls.extend(root_sheet_module_calls);
    instance_calls.extend(root_component_calls);

    let root_net_decls = args.net_decls.decls_for_set(&root_net_set);

    let used_module_idents: BTreeSet<String> = instance_calls
        .iter()
        .map(|c| c.module_ident.clone())
        .collect();
    let mut module_decls: BTreeMap<String, String> = BTreeMap::new();
    for (ident, path) in args
        .component_modules
        .module_decls
        .iter()
        .chain(root_sheet_module_decls.iter())
    {
        if used_module_idents.contains(ident) {
            module_decls.insert(ident.clone(), path.clone());
        }
    }
    let module_decls: Vec<(String, String)> = module_decls.into_iter().collect();

    let uses_not_connected = instance_calls_use_not_connected(&instance_calls);

    let board_zen_content = crate::codegen::board::render_imported_board(
        args.board_name,
        copper_layers,
        stackup.as_ref(),
        uses_not_connected,
        &root_net_decls,
        &module_decls,
        &instance_calls,
    );
    crate::codegen::zen::write_zen_formatted(args.board_zen, &board_zen_content)
        .with_context(|| format!("Failed to write {}", args.board_zen.display()))?;

    Ok(())
}

fn prepatch_imported_layout_kicad_pcb(
    layout_kicad_pcb: &Path,
    pcb_text: &str,
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    refdes_instance_names: &BTreeMap<KiCadRefDes, String>,
    net_ident_by_kicad_name: &BTreeMap<KiCadNetName, String>,
    generated_components: &GeneratedComponents,
    sheet_modules: &GeneratedSheetModules,
) -> Result<()> {
    let board = pcb_sexpr::parse(pcb_text).map_err(|e| anyhow::anyhow!(e))?;

    let net_renames: std::collections::HashMap<String, String> = net_ident_by_kicad_name
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.clone()))
        .collect();
    let (net_patches, _applied) = pcb_layout::compute_net_renames_patches(&board, &net_renames);

    let path_patches = compute_import_footprint_path_property_patches(
        &board,
        pcb_text,
        components,
        refdes_instance_names,
        generated_components,
        sheet_modules,
    )?;

    let mut patches = PatchSet::default();
    patches.extend(net_patches);
    patches.extend(path_patches);

    if patches.is_empty() {
        return Ok(());
    }

    let mut out: Vec<u8> = Vec::new();
    patches
        .write_to(pcb_text, &mut out)
        .with_context(|| format!("Failed to apply patches to {}", layout_kicad_pcb.display()))?;
    fs::write(layout_kicad_pcb, out)
        .with_context(|| format!("Failed to write patched {}", layout_kicad_pcb.display()))?;

    Ok(())
}

fn compute_import_footprint_path_property_patches(
    board: &Sexpr,
    pcb_text: &str,
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    refdes_instance_names: &BTreeMap<KiCadRefDes, String>,
    generated_components: &GeneratedComponents,
    sheet_modules: &GeneratedSheetModules,
) -> Result<PatchSet> {
    let mut desired_by_refdes: BTreeMap<KiCadRefDes, String> = BTreeMap::new();
    for (anchor, component) in components {
        if component.layout.is_none() {
            continue;
        }
        let Some(component_name) = generated_components.anchor_to_component_name.get(anchor) else {
            continue;
        };
        let refdes = &component.netlist.refdes;
        let instance_name = refdes_instance_names
            .get(refdes)
            .cloned()
            .unwrap_or_else(|| refdes.as_str().to_string());
        let prefix = sheet_modules
            .anchor_to_entity_prefix
            .get(anchor)
            .cloned()
            .unwrap_or_default();
        if prefix.is_empty() {
            desired_by_refdes.insert(refdes.clone(), format!("{instance_name}.{component_name}"));
        } else {
            desired_by_refdes.insert(
                refdes.clone(),
                format!("{prefix}.{instance_name}.{component_name}"),
            );
        }
    }

    compute_set_footprint_sync_hook_patches_by_refdes(board, pcb_text, &desired_by_refdes)
}

fn compute_set_footprint_sync_hook_patches_by_refdes(
    board: &Sexpr,
    pcb_text: &str,
    desired_by_refdes: &BTreeMap<KiCadRefDes, String>,
) -> std::result::Result<PatchSet, anyhow::Error> {
    const UUID_NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8); // uuid.NAMESPACE_URL

    let root_list = board
        .as_list()
        .ok_or_else(|| anyhow::anyhow!("KiCad PCB root is not a list"))?;

    let mut patches = PatchSet::default();

    for node in root_list.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("footprint") {
            continue;
        }

        let mut refdes: Option<KiCadRefDes> = None;
        let mut path_spans: Vec<Span> = Vec::new();
        let mut existing_path_span: Option<Span> = None;

        for child in items.iter().skip(1) {
            let Some(list) = child.as_list() else {
                continue;
            };
            match list.first().and_then(Sexpr::as_sym) {
                Some("path") => {
                    let Some(value_node) = list.get(1) else {
                        continue;
                    };
                    if value_node.as_str().is_some() {
                        path_spans.push(value_node.span);
                    }
                }
                Some("property") => {
                    let prop_name = list.get(1).and_then(Sexpr::as_str);
                    if prop_name == Some("Reference") && refdes.is_none() {
                        if let Some(value) = list.get(2).and_then(Sexpr::as_str) {
                            refdes = Some(KiCadRefDes::from(value.to_string()));
                        }
                    }
                    if prop_name != Some("Path") {
                        continue;
                    }
                    if let Some(value) = list.get(2) {
                        existing_path_span = Some(value.span);
                    }
                }
                _ => {}
            }
        }

        let Some(refdes) = refdes else {
            continue;
        };
        let Some(desired) = desired_by_refdes.get(&refdes) else {
            continue;
        };

        // Ensure KiCad internal KIID path matches what sync expects for this footprint path.
        //
        // Note: This overwrites KiCad's schematic association path. That's intentional: once a
        // KiCad project is adopted into Zener, Zener becomes the source of truth and the layout
        // sync pipeline relies on this deterministic KIID path.
        let uuid = Uuid::new_v5(&UUID_NAMESPACE_URL, desired.as_bytes()).to_string();
        for span in path_spans {
            patches.replace_string(span, &format!("/{uuid}/{uuid}"));
        }

        if let Some(span) = existing_path_span {
            patches.replace_string(span, desired);
        } else {
            // Insert a new (property "Path" "...") block before the footprint's closing paren.
            let insert_at = footprint_closing_line_start(pcb_text, node.span);
            let property_text = format!(
                "\t\t(property \"Path\" \"{}\"\n\t\t\t(at 0 0 0)\n\t\t\t(layer \"F.SilkS\")\n\t\t\t(hide yes)\n\t\t)\n",
                desired
            );
            patches.replace_raw(
                Span {
                    start: insert_at,
                    end: insert_at,
                },
                property_text,
            );
        }
    }

    Ok(patches)
}

fn footprint_closing_line_start(pcb_text: &str, footprint_span: Span) -> usize {
    let start = footprint_span.start.min(pcb_text.len());
    let end = footprint_span.end.min(pcb_text.len());
    let slice = &pcb_text[start..end];

    if let Some(last_nl) = slice.rfind('\n') {
        return start + last_nl + 1;
    }

    // Fallback: insert before the closing ')' if no newline exists.
    end.saturating_sub(1)
}

fn try_extract_stackup(
    pcb_text: &str,
    layout_kicad_pcb: &Path,
) -> Result<(usize, Option<zen_stackup::Stackup>)> {
    let fallback_layers = infer_copper_layers_from_layers_section(pcb_text)?;

    let stackup = match zen_stackup::Stackup::from_kicad_pcb(pcb_text) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok((fallback_layers, None));
        }
        Err(e) => {
            debug!(
                "Skipping stackup extraction (failed to parse stackup from {}): {}",
                layout_kicad_pcb.display(),
                e
            );
            return Ok((fallback_layers, None));
        }
    };

    let Some(layers) = stackup.layers.as_deref() else {
        return Ok((fallback_layers, None));
    };
    if layers.is_empty() {
        return Ok((fallback_layers, None));
    }

    let copper_layers = stackup.copper_layer_count();
    if !matches!(copper_layers, 2 | 4 | 6 | 8 | 10) {
        debug!(
            "Skipping stackup extraction (unexpected copper layer count {copper_layers} in {}); using layer count inferred from (layers ...) section ({fallback_layers}).",
            layout_kicad_pcb.display()
        );
        return Ok((fallback_layers, None));
    }

    Ok((copper_layers, Some(stackup)))
}

fn infer_copper_layers_from_layers_section(pcb_text: &str) -> Result<usize> {
    let root = pcb_sexpr::parse(pcb_text).map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let root_items = root
        .as_list()
        .ok_or_else(|| anyhow::anyhow!("Expected KiCad PCB root to be a list"))?;
    let layers = find_child_list(root_items, "layers")
        .ok_or_else(|| anyhow::anyhow!("KiCad PCB missing (layers ...) section"))?;

    let mut copper_layer_names: BTreeSet<&str> = BTreeSet::new();
    for item in layers.iter().skip(1) {
        let Some(list) = item.as_list() else {
            continue;
        };
        let Some(name) = list.get(1).and_then(Sexpr::as_str) else {
            continue;
        };
        if name.ends_with(".Cu") {
            copper_layer_names.insert(name);
        }
    }

    let count = copper_layer_names.len();
    if !matches!(count, 2 | 4 | 6 | 8 | 10) {
        anyhow::bail!(
            "Unsupported copper layer count inferred from KiCad (layers ...) section: {count}"
        );
    }
    Ok(count)
}

#[cfg(test)]
mod stackup_fallback_tests {
    use super::*;

    #[test]
    fn layer_count_falls_back_to_layers_section_when_stackup_missing() {
        let pcb_text = r#"
        (kicad_pcb
          (layers
            (0 "F.Cu" mixed)
            (4 "In1.Cu" power)
            (6 "In2.Cu" signal)
            (2 "B.Cu" mixed)
            (9 "F.Adhes" user "F.Adhesive")
          )
        )
        "#;

        let (layers, stackup) =
            try_extract_stackup(pcb_text, Path::new("dummy.kicad_pcb")).unwrap();
        assert_eq!(layers, 4);
        assert!(stackup.is_none());
    }

    #[test]
    fn errors_when_layers_section_is_missing() {
        let pcb_text = r#"(kicad_pcb (version 20241229) (generator "pcbnew"))"#;
        let err = try_extract_stackup(pcb_text, Path::new("dummy.kicad_pcb"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing (layers"));
    }
}

fn build_net_decls(
    netlist_nets: &BTreeMap<KiCadNetName, ImportNetData>,
    not_connected_nets: &BTreeSet<KiCadNetName>,
) -> ImportedNetDecls {
    let mut used_idents: BTreeSet<String> = BTreeSet::new();
    let mut used_net_names: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
    let mut var_ident_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();
    let mut zener_name_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();

    for net_name in netlist_nets.keys() {
        if not_connected_nets.contains(net_name) {
            continue;
        }
        let ident_base = sanitize_screaming_snake_identifier(net_name.as_str(), "NET");
        let ident = alloc_unique_ident(&ident_base, &mut used_idents);

        let name_base = sanitize_kicad_name_for_zener(net_name.as_str(), "NET");
        let name = alloc_unique_ident(&name_base, &mut used_net_names);

        out.push(crate::codegen::board::ImportedNetDecl {
            ident: ident.clone(),
            name: name.clone(),
        });
        var_ident_by_kicad_name.insert(net_name.clone(), ident);
        zener_name_by_kicad_name.insert(net_name.clone(), name);
    }

    ImportedNetDecls {
        decls: out,
        var_ident_by_kicad_name,
        zener_name_by_kicad_name,
    }
}

fn build_not_connected_nets(
    netlist_nets: &BTreeMap<KiCadNetName, ImportNetData>,
) -> BTreeSet<KiCadNetName> {
    netlist_nets
        .iter()
        .filter(|(name, net)| name.as_str().starts_with("unconnected-(") && net.ports.len() == 1)
        .map(|(name, _)| name.clone())
        .collect()
}

fn instance_calls_use_not_connected(
    instance_calls: &[crate::codegen::board::ImportedInstanceCall],
) -> bool {
    instance_calls.iter().any(|call| {
        call.io_nets
            .values()
            .any(|expr| expr.trim_start().starts_with("NotConnected("))
    })
}

impl ImportedNetDecls {
    fn decls_for_set(
        &self,
        net_set: &BTreeSet<KiCadNetName>,
    ) -> Vec<crate::codegen::board::ImportedNetDecl> {
        let mut out: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
        for net_name in net_set {
            let Some(ident) = self.var_ident_by_kicad_name.get(net_name).cloned() else {
                continue;
            };
            let Some(name) = self.zener_name_by_kicad_name.get(net_name).cloned() else {
                continue;
            };
            out.push(crate::codegen::board::ImportedNetDecl { ident, name });
        }
        out
    }

    fn ident_map_for_set(
        &self,
        net_set: &BTreeSet<KiCadNetName>,
    ) -> BTreeMap<KiCadNetName, String> {
        let mut out: BTreeMap<KiCadNetName, String> = BTreeMap::new();
        for net_name in net_set {
            if let Some(ident) = self.var_ident_by_kicad_name.get(net_name).cloned() {
                out.insert(net_name.clone(), ident);
            }
        }
        out
    }
}

fn build_port_to_net_map(
    netlist_nets: &BTreeMap<KiCadNetName, ImportNetData>,
) -> Result<BTreeMap<ImportNetPort, KiCadNetName>> {
    let mut port_to_net: BTreeMap<ImportNetPort, KiCadNetName> = BTreeMap::new();
    for (net_name, net) in netlist_nets {
        for port in &net.ports {
            if port_to_net.insert(port.clone(), net_name.clone()).is_some() {
                anyhow::bail!(
                    "KiCad netlist produced duplicate connectivity for port {}:{}",
                    port.component.pcb_path(),
                    port.pin.as_str()
                );
            }
        }
    }
    Ok(port_to_net)
}

struct GenerateSheetModulesArgs<'a> {
    board_dir: &'a Path,
    board_name: &'a str,
    ir: &'a ImportIr,
    port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    refdes_instance_names: &'a BTreeMap<KiCadRefDes, String>,
    net_decls: &'a ImportedNetDecls,
    components: &'a GeneratedComponents,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

fn generate_sheet_modules(args: GenerateSheetModulesArgs<'_>) -> Result<GeneratedSheetModules> {
    let board_dir = args.board_dir;
    let board_name = args.board_name;
    let ir = args.ir;
    let port_to_net = args.port_to_net;
    let refdes_instance_names = args.refdes_instance_names;
    let net_decls = args.net_decls;
    let components = args.components;
    let not_connected_nets = args.not_connected_nets;
    let modules_root = board_dir.join("modules");
    fs::create_dir_all(&modules_root)
        .with_context(|| format!("Failed to create {}", modules_root.display()))?;

    let mut anchors_by_sheet: BTreeMap<KiCadSheetPath, Vec<KiCadUuidPathKey>> = BTreeMap::new();
    for (anchor, component) in &ir.components {
        if component.layout.is_none() {
            continue;
        }
        let sheet_path = KiCadSheetPath::from_sheetpath_tstamps(&anchor.sheetpath_tstamps);
        anchors_by_sheet
            .entry(sheet_path)
            .or_default()
            .push(anchor.clone());
    }

    let subtree_has_components =
        compute_subtree_has_components(&ir.schematic_sheet_tree, &anchors_by_sheet);

    // Track allocated module directory names in a case-insensitive way to avoid
    // collisions on case-insensitive filesystems (e.g. macOS default).
    let mut used_module_dirs_ci: BTreeSet<String> = BTreeSet::new();
    let mut module_dir_by_sheet: BTreeMap<KiCadSheetPath, String> = BTreeMap::new();
    for (sheet_path, node) in &ir.schematic_sheet_tree.nodes {
        if sheet_path.as_str() == "/" {
            continue;
        }
        if !subtree_has_components
            .get(sheet_path)
            .copied()
            .unwrap_or(false)
        {
            continue;
        }

        let sheet_name = node
            .sheet_name
            .clone()
            .or_else(|| sheet_path.last_uuid().map(|u| u.to_string()))
            .unwrap_or_else(|| "sheet".to_string());

        let mut base = component_gen::sanitize_mpn_for_path(&sheet_name);
        if base.is_empty() {
            base = "sheet".to_string();
        }
        let dir = alloc_unique_fs_segment(&base, &mut used_module_dirs_ci);
        module_dir_by_sheet.insert(sheet_path.clone(), dir);
    }

    let instance_name_by_sheet =
        assign_sheet_instance_names(&ir.schematic_sheet_tree, &subtree_has_components);
    let entity_prefix_by_sheet =
        build_sheet_entity_prefixes(&ir.schematic_sheet_tree, &instance_name_by_sheet);

    let mut anchor_to_entity_prefix: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
    for (anchor, component) in &ir.components {
        if component.layout.is_none() {
            continue;
        }
        let sheet_path = KiCadSheetPath::from_sheetpath_tstamps(&anchor.sheetpath_tstamps);
        let prefix = entity_prefix_by_sheet
            .get(&sheet_path)
            .cloned()
            .unwrap_or_default();
        anchor_to_entity_prefix.insert(anchor.clone(), prefix);
    }

    let mut module_paths: BTreeSet<(std::cmp::Reverse<usize>, KiCadSheetPath)> = BTreeSet::new();
    for sheet_path in module_dir_by_sheet.keys() {
        module_paths.insert((std::cmp::Reverse(sheet_path.depth()), sheet_path.clone()));
    }

    for (_, sheet_path) in module_paths {
        let Some(node) = ir.schematic_sheet_tree.nodes.get(&sheet_path) else {
            continue;
        };
        let Some(module_dir) = module_dir_by_sheet.get(&sheet_path).cloned() else {
            continue;
        };

        let sheet_name = node
            .sheet_name
            .clone()
            .or_else(|| sheet_path.last_uuid().map(|u| u.to_string()))
            .unwrap_or_else(|| "sheet".to_string());

        let module_plan = ir
            .hierarchy_plan
            .modules
            .get(&sheet_path)
            .cloned()
            .unwrap_or_default();

        let mut module_net_set: BTreeSet<KiCadNetName> = BTreeSet::new();
        module_net_set.extend(module_plan.nets_defined_here.iter().cloned());
        module_net_set.extend(module_plan.nets_io_here.iter().cloned());

        let module_net_ident_by_kicad = net_decls.ident_map_for_set(&module_net_set);

        let io_net_idents: Vec<String> = module_plan
            .nets_io_here
            .iter()
            .filter_map(|n| module_net_ident_by_kicad.get(n).cloned())
            .collect();

        let mut internal_net_decls: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
        for net_name in &module_plan.nets_defined_here {
            let Some(ident) = module_net_ident_by_kicad.get(net_name).cloned() else {
                continue;
            };
            let Some(name) = net_decls.zener_name_by_kicad_name.get(net_name).cloned() else {
                continue;
            };
            internal_net_decls.push(crate::codegen::board::ImportedNetDecl { ident, name });
        }

        let sheet_anchors = anchors_by_sheet
            .get(&sheet_path)
            .cloned()
            .unwrap_or_default();
        let sheet_instances: Vec<(&KiCadUuidPathKey, &ImportComponentData)> = sheet_anchors
            .iter()
            .filter_map(|a| ir.components.get_key_value(a))
            .collect();

        let component_instance_calls = build_imported_instance_calls_for_instances(
            sheet_instances,
            port_to_net,
            refdes_instance_names,
            &module_net_ident_by_kicad,
            components,
            not_connected_nets,
        )?;

        let used_component_modules: BTreeSet<String> = component_instance_calls
            .iter()
            .map(|c| c.module_ident.clone())
            .collect();
        let mut module_component_decls: BTreeMap<String, String> = BTreeMap::new();
        for (ident, path) in &components.module_decls {
            if !used_component_modules.contains(ident) {
                continue;
            }
            let module_path = if path.starts_with('@') {
                path.clone()
            } else {
                format!("../../{path}")
            };
            module_component_decls.insert(ident.clone(), module_path);
        }

        let mut used_idents: BTreeSet<String> = BTreeSet::new();
        used_idents.extend(io_net_idents.iter().cloned());
        used_idents.extend(internal_net_decls.iter().map(|d| d.ident.clone()));
        used_idents.extend(module_component_decls.keys().cloned());

        let mut child_module_decls: BTreeMap<String, String> = BTreeMap::new();
        let mut child_module_calls: BTreeMap<String, crate::codegen::board::ImportedInstanceCall> =
            BTreeMap::new();

        for child in &node.children {
            if !subtree_has_components.get(child).copied().unwrap_or(false) {
                continue;
            }
            let Some(child_dir) = module_dir_by_sheet.get(child).cloned() else {
                continue;
            };

            let module_path = format!("../{child_dir}/{child_dir}.zen");
            let module_ident_base = module_ident_from_component_dir(&child_dir);
            let module_ident = alloc_unique_ident(&module_ident_base, &mut used_idents);
            child_module_decls.insert(module_ident.clone(), module_path);

            let child_plan = ir
                .hierarchy_plan
                .modules
                .get(child)
                .cloned()
                .unwrap_or_default();

            let mut io_nets: BTreeMap<String, String> = BTreeMap::new();
            for net in &child_plan.nets_io_here {
                let Some(ident) = net_decls.var_ident_by_kicad_name.get(net).cloned() else {
                    continue;
                };
                io_nets.insert(ident.clone(), ident);
            }

            let instance_name = instance_name_by_sheet
                .get(child)
                .cloned()
                .unwrap_or_else(|| "sheet".to_string());

            child_module_calls.insert(
                instance_name.clone(),
                crate::codegen::board::ImportedInstanceCall {
                    module_ident,
                    refdes: instance_name,
                    dnp: false,
                    skip_bom: None,
                    skip_pos: None,
                    config_args: BTreeMap::new(),
                    io_nets,
                },
            );
        }

        let module_dir_abs = modules_root.join(&module_dir);
        fs::create_dir_all(&module_dir_abs)
            .with_context(|| format!("Failed to create {}", module_dir_abs.display()))?;
        let module_zen = module_dir_abs.join(format!("{module_dir}.zen"));

        let module_doc = format!(
            "{} sheet module: {} ({})",
            board_name,
            sheet_name,
            sheet_path.as_str()
        );

        let mut module_decls: BTreeMap<String, String> = BTreeMap::new();
        module_decls.extend(module_component_decls);
        module_decls.extend(child_module_decls);
        let module_decls: Vec<(String, String)> = module_decls.into_iter().collect();

        let mut instance_calls: Vec<crate::codegen::board::ImportedInstanceCall> = Vec::new();
        instance_calls.extend(child_module_calls.into_values());
        instance_calls.extend(component_instance_calls);

        let uses_not_connected = instance_calls_use_not_connected(&instance_calls);
        let module_zen_content = crate::codegen::board::render_imported_sheet_module(
            &module_doc,
            &io_net_idents,
            &internal_net_decls,
            &module_decls,
            &instance_calls,
            uses_not_connected,
        );
        crate::codegen::zen::write_zen_formatted(&module_zen, &module_zen_content)
            .with_context(|| format!("Failed to write {}", module_zen.display()))?;
    }

    Ok(GeneratedSheetModules {
        module_dir_by_sheet,
        instance_name_by_sheet,
        anchor_to_entity_prefix,
        subtree_has_components,
    })
}

fn compute_subtree_has_components(
    tree: &ImportSheetTree,
    anchors_by_sheet: &BTreeMap<KiCadSheetPath, Vec<KiCadUuidPathKey>>,
) -> BTreeMap<KiCadSheetPath, bool> {
    let mut paths: BTreeSet<(std::cmp::Reverse<usize>, KiCadSheetPath)> = BTreeSet::new();
    for path in tree.nodes.keys() {
        paths.insert((std::cmp::Reverse(path.depth()), path.clone()));
    }

    let mut subtree_has_components: BTreeMap<KiCadSheetPath, bool> = BTreeMap::new();
    for (_, path) in paths {
        let has_here = anchors_by_sheet.get(&path).is_some_and(|v| !v.is_empty());
        let has_child = tree
            .nodes
            .get(&path)
            .map(|n| {
                n.children
                    .iter()
                    .any(|c| subtree_has_components.get(c).copied().unwrap_or(false))
            })
            .unwrap_or(false);
        subtree_has_components.insert(path.clone(), has_here || has_child);
    }
    subtree_has_components
}

fn assign_sheet_instance_names(
    tree: &ImportSheetTree,
    subtree_has_components: &BTreeMap<KiCadSheetPath, bool>,
) -> BTreeMap<KiCadSheetPath, String> {
    let mut out: BTreeMap<KiCadSheetPath, String> = BTreeMap::new();

    let mut parents: BTreeSet<(usize, KiCadSheetPath)> = BTreeSet::new();
    for path in tree.nodes.keys() {
        parents.insert((path.depth(), path.clone()));
    }

    for (_, parent_path) in parents {
        let Some(parent) = tree.nodes.get(&parent_path) else {
            continue;
        };
        let mut used: BTreeSet<String> = BTreeSet::new();

        for child_path in &parent.children {
            if child_path.as_str() == "/" {
                continue;
            }
            if !subtree_has_components
                .get(child_path)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let child_node = tree.nodes.get(child_path);
            let name = child_node
                .and_then(|n| n.sheet_name.clone())
                .or_else(|| child_path.last_uuid().map(|u| u.to_string()))
                .unwrap_or_else(|| "sheet".to_string());

            let base = sanitize_screaming_snake_identifier(&name, "SHEET");
            let inst = alloc_unique_ident(&base, &mut used);
            out.insert(child_path.clone(), inst);
        }
    }

    out
}

fn build_sheet_entity_prefixes(
    tree: &ImportSheetTree,
    instance_name_by_sheet: &BTreeMap<KiCadSheetPath, String>,
) -> BTreeMap<KiCadSheetPath, String> {
    let mut out: BTreeMap<KiCadSheetPath, String> = BTreeMap::new();
    out.insert(KiCadSheetPath::root(), String::new());

    let mut paths: BTreeSet<(usize, KiCadSheetPath)> = BTreeSet::new();
    for path in tree.nodes.keys() {
        paths.insert((path.depth(), path.clone()));
    }

    for (_, path) in paths {
        if path.as_str() == "/" {
            continue;
        }
        let Some(inst) = instance_name_by_sheet.get(&path).cloned() else {
            continue;
        };
        let parent = path.parent().unwrap_or_else(KiCadSheetPath::root);
        let parent_prefix = out.get(&parent).cloned().unwrap_or_default();
        let prefix = if parent_prefix.is_empty() {
            inst
        } else {
            format!("{parent_prefix}.{inst}")
        };
        out.insert(path, prefix);
    }

    out
}

fn build_root_sheet_module_calls(
    tree: &ImportSheetTree,
    sheet_modules: &GeneratedSheetModules,
    hierarchy_plan: &ImportHierarchyPlan,
    net_decls: &ImportedNetDecls,
    root_net_set: &BTreeSet<KiCadNetName>,
    root_component_calls: &[crate::codegen::board::ImportedInstanceCall],
) -> (
    Vec<(String, String)>,
    Vec<crate::codegen::board::ImportedInstanceCall>,
) {
    let root = KiCadSheetPath::root();
    let Some(root_node) = tree.nodes.get(&root) else {
        return (Vec::new(), Vec::new());
    };

    let mut used_idents: BTreeSet<String> = BTreeSet::new();
    for net in root_net_set {
        if let Some(ident) = net_decls.var_ident_by_kicad_name.get(net).cloned() {
            used_idents.insert(ident);
        }
    }
    for call in root_component_calls {
        used_idents.insert(call.module_ident.clone());
    }

    let mut module_decls: BTreeMap<String, String> = BTreeMap::new();
    let mut module_calls: BTreeMap<String, crate::codegen::board::ImportedInstanceCall> =
        BTreeMap::new();

    for child in &root_node.children {
        if !sheet_modules
            .subtree_has_components
            .get(child)
            .copied()
            .unwrap_or(false)
        {
            continue;
        }

        let Some(child_dir) = sheet_modules.module_dir_by_sheet.get(child).cloned() else {
            continue;
        };
        let module_path = format!("modules/{child_dir}/{child_dir}.zen");

        let module_ident_base = module_ident_from_component_dir(&child_dir);
        let module_ident = alloc_unique_ident(&module_ident_base, &mut used_idents);
        module_decls.insert(module_ident.clone(), module_path);

        let child_plan = hierarchy_plan
            .modules
            .get(child)
            .cloned()
            .unwrap_or_default();

        let mut io_nets: BTreeMap<String, String> = BTreeMap::new();
        for net in &child_plan.nets_io_here {
            let Some(ident) = net_decls.var_ident_by_kicad_name.get(net).cloned() else {
                continue;
            };
            io_nets.insert(ident.clone(), ident);
        }

        let instance_name = sheet_modules
            .instance_name_by_sheet
            .get(child)
            .cloned()
            .unwrap_or_else(|| "SHEET".to_string());

        module_calls.insert(
            instance_name.clone(),
            crate::codegen::board::ImportedInstanceCall {
                module_ident,
                refdes: instance_name,
                dnp: false,
                skip_bom: None,
                skip_pos: None,
                config_args: BTreeMap::new(),
                io_nets,
            },
        );
    }

    (
        module_decls.into_iter().collect(),
        module_calls.into_values().collect(),
    )
}

struct ImportedNetDecls {
    decls: Vec<crate::codegen::board::ImportedNetDecl>,
    var_ident_by_kicad_name: BTreeMap<KiCadNetName, String>,
    zener_name_by_kicad_name: BTreeMap<KiCadNetName, String>,
}

#[derive(Debug, Default)]
struct GeneratedSheetModules {
    module_dir_by_sheet: BTreeMap<KiCadSheetPath, String>,
    instance_name_by_sheet: BTreeMap<KiCadSheetPath, String>,
    anchor_to_entity_prefix: BTreeMap<KiCadUuidPathKey, String>,
    subtree_has_components: BTreeMap<KiCadSheetPath, bool>,
}

fn sanitize_kicad_name_for_zener(raw: &str, fallback: &str) -> String {
    // Keep KiCad net names intact as much as possible.
    //
    // Zener identifier rules are intentionally permissive (paths, punctuation, etc.) but forbid:
    // - `.`
    // - whitespace
    // - `@`
    // - non-ASCII
    //
    // Apply the minimal substitutions required for Zener acceptance while preserving case and
    // most punctuation.
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_underscore = false;

    for c in trimmed.chars() {
        let mapped = match c {
            '.' => '_',
            '@' => '_',
            c if c.is_whitespace() => '_',
            c if !c.is_ascii() => '_',
            c => c,
        };
        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        out.push(mapped);
    }

    let cleaned = out.trim_matches('_');
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned.to_string()
    }
}

fn sanitize_screaming_snake_identifier(raw: &str, prefix: &str) -> String {
    let mut out = sanitize_screaming_snake_fragment(raw);
    if out.is_empty() {
        out = prefix.to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out = format!("{prefix}_{out}");
    }
    out
}

fn sanitize_screaming_snake_fragment(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::new();
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ImportPartKey {
    mpn: Option<String>,
    footprint: Option<String>,
    lib_id: Option<KiCadLibId>,
    value: Option<String>,
}

struct GeneratedComponents {
    module_decls: Vec<(String, String)>,
    anchor_to_module_ident: BTreeMap<KiCadUuidPathKey, String>,
    /// Per-instance component name (the `Component(name=...)` inside the generated per-part module).
    ///
    /// Used to pre-patch KiCad footprints with a stable sync `Path` hook:
    /// `<refdes>.<component_name>`.
    anchor_to_component_name: BTreeMap<KiCadUuidPathKey, String>,
    /// Per-instance module config kwargs to pass when instantiating the module.
    ///
    /// Only used for stdlib-generated components (e.g. promoted passives).
    anchor_to_config_args: BTreeMap<KiCadUuidPathKey, BTreeMap<String, String>>,
    module_io_pins: BTreeMap<String, BTreeMap<String, BTreeSet<KiCadPinNumber>>>,
    module_skip_defaults: BTreeMap<String, ModuleSkipDefaults>,
}

#[derive(Debug, Clone, Copy)]
struct ModuleSkipDefaults {
    include_skip_bom: bool,
    skip_bom_default: bool,
    include_skip_pos: bool,
    skip_pos_default: bool,
}

#[derive(Debug, Clone, Copy)]
struct ImportPartFlags {
    any_skip_bom: bool,
    any_skip_pos: bool,
    all_skip_bom: bool,
    all_skip_pos: bool,
}

impl Default for ImportPartFlags {
    fn default() -> Self {
        Self {
            any_skip_bom: false,
            any_skip_pos: false,
            all_skip_bom: true,
            all_skip_pos: true,
        }
    }
}

fn generate_imported_components(
    board_dir: &Path,
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    reserved_idents: &BTreeSet<String>,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
    passive_by_component: &BTreeMap<KiCadUuidPathKey, ImportPassiveClassification>,
) -> Result<GeneratedComponents> {
    let components_root = board_dir.join("components");
    fs::create_dir_all(&components_root).with_context(|| {
        format!(
            "Failed to create components output directory {}",
            components_root.display()
        )
    })?;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PromotedPassiveKind {
        Resistor,
        Capacitor,
    }

    #[derive(Debug, Clone)]
    struct PromotedPassive {
        kind: PromotedPassiveKind,
        config_args: BTreeMap<String, String>,
    }

    fn alloc_unique_module_ident(base: &str, used: &mut BTreeSet<String>) -> String {
        if used.insert(base.to_string()) {
            return base.to_string();
        }
        let underscored = format!("_{base}");
        if used.insert(underscored.clone()) {
            return underscored;
        }
        alloc_unique_ident(base, used)
    }

    fn canonical_dielectric(raw: &str) -> Option<&'static str> {
        let s = raw.trim().to_ascii_uppercase();
        match s.as_str() {
            "C0G" | "COG" => Some("C0G"),
            "NP0" | "NPO" => Some("NP0"),
            "X5R" => Some("X5R"),
            "X7R" => Some("X7R"),
            "X7S" => Some("X7S"),
            "X7T" => Some("X7T"),
            "Y5V" => Some("Y5V"),
            "Z5U" => Some("Z5U"),
            _ => None,
        }
    }

    fn canonical_voltage(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut s = trimmed.replace(' ', "");
        s = s.replace('µ', "u");

        if !(s.ends_with('V') || s.ends_with('v')) {
            return None;
        }
        let core = &s[..s.len() - 1];
        if core.is_empty() {
            return None;
        }

        let (num, prefix) = match core.chars().last() {
            Some(c) if matches!(c, 'm' | 'u' | 'k' | 'M' | 'K' | 'U') => {
                (&core[..core.len() - 1], Some(c))
            }
            _ => (core, None),
        };

        let num = num.trim();
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        if num.chars().filter(|&c| c == '.').count() > 1 {
            return None;
        }

        let mut out = num.to_string();
        if let Some(p) = prefix {
            let canonical = match p {
                'U' => 'u',
                'K' => 'k',
                c => c,
            };
            out.push(canonical);
        }
        out.push('V');
        Some(out)
    }

    fn promotable_passive_kind(
        anchor: &KiCadUuidPathKey,
        component: &ImportComponentData,
        passive_by_component: &BTreeMap<KiCadUuidPathKey, ImportPassiveClassification>,
    ) -> Option<PromotedPassive> {
        let class = passive_by_component.get(anchor)?;

        component.layout.as_ref()?;
        if class.pad_count != Some(2) {
            return None;
        }
        if class.confidence != Some(ImportPassiveConfidence::High) {
            return None;
        }
        let kind = match class.kind? {
            ImportPassiveKind::Resistor => PromotedPassiveKind::Resistor,
            ImportPassiveKind::Capacitor => PromotedPassiveKind::Capacitor,
        };
        let value = class.parsed_value.as_deref()?;
        let package = class.package?;

        // Note: stdlib passives support `skip_bom` and `dnp`. We intentionally do not
        // plumb `skip_pos` for promoted passives.

        let mut config_args: BTreeMap<String, String> = BTreeMap::new();
        config_args.insert("value".to_string(), value.to_string());
        config_args.insert("package".to_string(), package.as_str().to_string());

        if let Some(v) = class.mpn.as_deref() {
            config_args.insert("mpn".to_string(), v.to_string());
        }
        if let Some(v) = class.manufacturer.as_deref() {
            config_args.insert("manufacturer".to_string(), v.to_string());
        }
        if kind == PromotedPassiveKind::Capacitor {
            if let Some(v) = class.voltage.as_deref() {
                if let Some(v) = canonical_voltage(v) {
                    config_args.insert("voltage".to_string(), v);
                }
            }
            if let Some(v) = class.dielectric.as_deref() {
                if let Some(d) = canonical_dielectric(v) {
                    config_args.insert("dielectric".to_string(), d.to_string());
                }
            }
        }

        Some(PromotedPassive { kind, config_args })
    }

    // Compute promoted-passive candidates per-instance.
    let mut candidate_by_anchor: BTreeMap<KiCadUuidPathKey, PromotedPassive> = BTreeMap::new();
    for (anchor, component) in components {
        if let Some(p) = promotable_passive_kind(anchor, component, passive_by_component) {
            candidate_by_anchor.insert(anchor.clone(), p);
        }
    }

    // Ensure promotion is consistent within a per-part group: either all instances of a part
    // are promoted, or none are (avoids mixing stdlib generics with generated component modules).
    let mut anchors_by_part_key: BTreeMap<ImportPartKey, Vec<KiCadUuidPathKey>> = BTreeMap::new();
    for (anchor, c) in components {
        if c.layout.is_none() {
            continue;
        }
        anchors_by_part_key
            .entry(derive_part_key(c))
            .or_default()
            .push(anchor.clone());
    }

    let mut promoted: BTreeMap<KiCadUuidPathKey, PromotedPassive> = BTreeMap::new();
    for (_part_key, anchors) in anchors_by_part_key {
        let Some(first) = anchors.first() else {
            continue;
        };
        let Some(first_candidate) = candidate_by_anchor.get(first) else {
            continue;
        };

        let kind = first_candidate.kind;
        let config_args = &first_candidate.config_args;

        let all_match = anchors.iter().all(|a| {
            candidate_by_anchor
                .get(a)
                .is_some_and(|c| c.kind == kind && &c.config_args == config_args)
        });
        if !all_match {
            continue;
        }

        for a in anchors {
            if let Some(c) = candidate_by_anchor.get(&a).cloned() {
                promoted.insert(a, c);
            }
        }
    }

    let mut part_to_instances: BTreeMap<ImportPartKey, Vec<KiCadUuidPathKey>> = BTreeMap::new();
    let mut part_flags: BTreeMap<ImportPartKey, ImportPartFlags> = BTreeMap::new();
    for (anchor, c) in components {
        if c.layout.is_none() {
            // Only generate component packages for footprints that exist on the PCB.
            continue;
        }
        if promoted.contains_key(anchor) {
            // Promoted passives use stdlib generics and don't produce component packages.
            continue;
        }
        let key = derive_part_key(c);
        part_to_instances
            .entry(key.clone())
            .or_default()
            .push(anchor.clone());

        let (_dnp, skip_bom, skip_pos) = derive_import_instance_flags(c);
        let flags = part_flags.entry(key).or_default();
        flags.any_skip_bom |= skip_bom;
        flags.any_skip_pos |= skip_pos;
        flags.all_skip_bom &= skip_bom;
        flags.all_skip_pos &= skip_pos;
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct ImportPartDir {
        manufacturer_dir: Option<String>,
        component_dir: String,
    }

    #[derive(Debug, Clone)]
    struct ImportPartDirCandidate {
        part_key: ImportPartKey,
        manufacturer_dir_candidate: Option<String>,
        component_dir_base: String,
        footprint_name: String,
    }

    let mut candidates: Vec<ImportPartDirCandidate> = Vec::new();
    let mut manufacturer_canonical: BTreeMap<String, String> = BTreeMap::new();

    for (part_key, instances) in &part_to_instances {
        let Some(first_anchor) = instances.first() else {
            continue;
        };
        let Some(component) = components.get(first_anchor) else {
            continue;
        };

        let manufacturer_dir_candidate =
            find_property_ci(best_properties(component), &["manufacturer", "mfr", "mfg"])
                .map(sanitize_component_dir_name);
        if let Some(mfr) = &manufacturer_dir_candidate {
            let key = mfr.to_ascii_lowercase();
            manufacturer_canonical
                .entry(key)
                .and_modify(|cur| {
                    if mfr < cur {
                        *cur = mfr.clone();
                    }
                })
                .or_insert(mfr.clone());
        }

        let footprint_name = part_key
            .footprint
            .as_deref()
            .map(footprint_name_from_fpid)
            .unwrap_or_else(|| "footprint".to_string());

        candidates.push(ImportPartDirCandidate {
            part_key: part_key.clone(),
            manufacturer_dir_candidate,
            component_dir_base: derive_part_name(part_key, component),
            footprint_name,
        });
    }

    // Allocate final filesystem directory names in a case-insensitive way to avoid
    // collisions on case-insensitive filesystems (e.g. macOS default).
    let mut used_component_dirs_ci: BTreeMap<Option<String>, BTreeSet<String>> = BTreeMap::new();
    let mut part_dir_by_key: BTreeMap<ImportPartKey, ImportPartDir> = BTreeMap::new();

    for candidate in candidates {
        let manufacturer_dir = candidate.manufacturer_dir_candidate.as_ref().map(|mfr| {
            manufacturer_canonical
                .get(&mfr.to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| mfr.clone())
        });

        let used = used_component_dirs_ci
            .entry(manufacturer_dir.clone())
            .or_default();

        let mut desired = candidate.component_dir_base.clone();
        if used.contains(&desired.to_ascii_lowercase()) {
            desired = format!("{desired}__{}", candidate.footprint_name);
        }
        let component_dir = alloc_unique_fs_segment(&desired, used);

        part_dir_by_key.insert(
            candidate.part_key,
            ImportPartDir {
                manufacturer_dir,
                component_dir,
            },
        );
    }

    let mut module_decls: BTreeMap<String, String> = BTreeMap::new();
    let mut used_module_idents: BTreeSet<String> = reserved_idents.iter().cloned().collect();
    let mut anchor_to_module_ident: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
    let mut anchor_to_component_name: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
    let mut anchor_to_config_args: BTreeMap<KiCadUuidPathKey, BTreeMap<String, String>> =
        BTreeMap::new();
    let mut module_io_pins: BTreeMap<String, BTreeMap<String, BTreeSet<KiCadPinNumber>>> =
        BTreeMap::new();
    let mut module_skip_defaults: BTreeMap<String, ModuleSkipDefaults> = BTreeMap::new();

    for (part_key, part_dir) in part_dir_by_key {
        let instances = part_to_instances
            .get(&part_key)
            .cloned()
            .unwrap_or_default();
        let Some(first_anchor) = instances.first() else {
            continue;
        };
        let Some(component) = components.get(first_anchor) else {
            continue;
        };

        let out_dir = match &part_dir.manufacturer_dir {
            Some(mfr) => components_root.join(mfr).join(&part_dir.component_dir),
            None => components_root.join(&part_dir.component_dir),
        };
        fs::create_dir_all(&out_dir)
            .with_context(|| format!("Failed to create {}", out_dir.display()))?;

        let flags = part_flags.get(&part_key).copied().unwrap_or_default();

        let symbol = write_component_symbol(
            &out_dir,
            &part_dir.component_dir,
            component,
            schematic_lib_symbols,
        )?;
        let footprint_filename = write_component_footprint(&out_dir, component)?;
        let io_pins = write_component_zen(
            &out_dir,
            &part_dir.component_dir,
            component,
            symbol.as_ref(),
            footprint_filename.as_deref(),
            flags,
        )?;

        if let Some(io_pins) = io_pins {
            let ident_base = module_ident_from_component_dir(&part_dir.component_dir);
            let ident = alloc_unique_ident(&ident_base, &mut used_module_idents);

            let module_path = match &part_dir.manufacturer_dir {
                Some(mfr) => format!(
                    "components/{mfr}/{name}/{name}.zen",
                    name = part_dir.component_dir
                ),
                None => format!(
                    "components/{name}/{name}.zen",
                    name = part_dir.component_dir
                ),
            };

            if module_io_pins.insert(ident.clone(), io_pins).is_some() {
                anyhow::bail!("Duplicate module IO mapping for {ident}");
            }
            if module_skip_defaults
                .insert(
                    ident.clone(),
                    ModuleSkipDefaults {
                        include_skip_bom: flags.any_skip_bom,
                        skip_bom_default: flags.all_skip_bom,
                        include_skip_pos: flags.any_skip_pos,
                        skip_pos_default: flags.all_skip_pos,
                    },
                )
                .is_some()
            {
                anyhow::bail!("Duplicate module skip defaults for {ident}");
            }

            for anchor in &instances {
                if anchor_to_module_ident
                    .insert(anchor.clone(), ident.clone())
                    .is_some()
                {
                    anyhow::bail!(
                        "Duplicate component instance mapping for {}",
                        anchor.pcb_path()
                    );
                }
                // Component name inside the module uses the same sanitizer as the directory name
                // generation and should be stable across runs.
                let component_name = component_gen::sanitize_mpn_for_path(&part_dir.component_dir);
                if anchor_to_component_name
                    .insert(anchor.clone(), component_name)
                    .is_some()
                {
                    anyhow::bail!(
                        "Duplicate component instance name mapping for {}",
                        anchor.pcb_path()
                    );
                }
            }

            if module_decls.insert(ident, module_path).is_some() {
                anyhow::bail!("Duplicate module declaration generated");
            }
        }
    }

    let resistor_module_ident = if promoted
        .values()
        .any(|p| p.kind == PromotedPassiveKind::Resistor)
    {
        Some(alloc_unique_module_ident(
            "Resistor",
            &mut used_module_idents,
        ))
    } else {
        None
    };
    let capacitor_module_ident = if promoted
        .values()
        .any(|p| p.kind == PromotedPassiveKind::Capacitor)
    {
        Some(alloc_unique_module_ident(
            "Capacitor",
            &mut used_module_idents,
        ))
    } else {
        None
    };

    if let Some(ident) = resistor_module_ident.as_ref() {
        if module_decls
            .insert(ident.clone(), "@stdlib/generics/Resistor.zen".to_string())
            .is_some()
        {
            anyhow::bail!("Duplicate module declaration generated for {ident}");
        }
        module_io_pins.insert(
            ident.clone(),
            BTreeMap::from([
                (
                    "P1".to_string(),
                    BTreeSet::from([KiCadPinNumber::from("1".to_string())]),
                ),
                (
                    "P2".to_string(),
                    BTreeSet::from([KiCadPinNumber::from("2".to_string())]),
                ),
            ]),
        );
        module_skip_defaults.insert(
            ident.clone(),
            ModuleSkipDefaults {
                include_skip_bom: true,
                skip_bom_default: false,
                include_skip_pos: false,
                skip_pos_default: false,
            },
        );
    }
    if let Some(ident) = capacitor_module_ident.as_ref() {
        if module_decls
            .insert(ident.clone(), "@stdlib/generics/Capacitor.zen".to_string())
            .is_some()
        {
            anyhow::bail!("Duplicate module declaration generated for {ident}");
        }
        module_io_pins.insert(
            ident.clone(),
            BTreeMap::from([
                (
                    "P1".to_string(),
                    BTreeSet::from([KiCadPinNumber::from("1".to_string())]),
                ),
                (
                    "P2".to_string(),
                    BTreeSet::from([KiCadPinNumber::from("2".to_string())]),
                ),
            ]),
        );
        module_skip_defaults.insert(
            ident.clone(),
            ModuleSkipDefaults {
                include_skip_bom: true,
                skip_bom_default: false,
                include_skip_pos: false,
                skip_pos_default: false,
            },
        );
    }

    for (anchor, passive) in promoted {
        let module_ident = match passive.kind {
            PromotedPassiveKind::Resistor => resistor_module_ident.as_ref(),
            PromotedPassiveKind::Capacitor => capacitor_module_ident.as_ref(),
        }
        .cloned()
        .context("Missing promoted passive module ident")?;

        if anchor_to_module_ident
            .insert(anchor.clone(), module_ident)
            .is_some()
        {
            anyhow::bail!(
                "Duplicate component instance mapping for {}",
                anchor.pcb_path()
            );
        }
        let component_name = match passive.kind {
            PromotedPassiveKind::Resistor => "R",
            PromotedPassiveKind::Capacitor => "C",
        };
        if anchor_to_component_name
            .insert(anchor.clone(), component_name.to_string())
            .is_some()
        {
            anyhow::bail!(
                "Duplicate component instance name mapping for {}",
                anchor.pcb_path()
            );
        }
        anchor_to_config_args.insert(anchor, passive.config_args);
    }

    Ok(GeneratedComponents {
        module_decls: module_decls.into_iter().collect(),
        anchor_to_module_ident,
        anchor_to_component_name,
        anchor_to_config_args,
        module_io_pins,
        module_skip_defaults,
    })
}

fn module_ident_from_component_dir(dir_name: &str) -> String {
    let frag = sanitize_screaming_snake_fragment(dir_name);
    if frag.is_empty() {
        return "_COMPONENT".to_string();
    }
    if frag.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return format!("_{frag}");
    }
    frag
}

fn derive_part_key(component: &ImportComponentData) -> ImportPartKey {
    let props = best_properties(component);
    let mpn = find_property_ci(
        props,
        &[
            "mpn",
            "manufacturer_part_number",
            "manufacturer part number",
        ],
    )
    .or_else(|| {
        find_property_ci(
            props,
            &["mfr part number", "manufacturer_pn", "part number"],
        )
    })
    .map(|s| s.to_string());

    let footprint = component
        .netlist
        .footprint
        .clone()
        .or_else(|| component.layout.as_ref().and_then(|l| l.fpid.clone()));

    let lib_id = component
        .schematic
        .as_ref()
        .and_then(|s| s.units.values().find_map(|u| u.lib_id.clone()));

    let value = component
        .netlist
        .value
        .clone()
        .or_else(|| props.get("Value").cloned())
        .or_else(|| props.get("Val").cloned());

    ImportPartKey {
        mpn,
        footprint,
        lib_id,
        value,
    }
}

fn derive_part_name(part_key: &ImportPartKey, component: &ImportComponentData) -> String {
    let raw = part_key
        .mpn
        .as_deref()
        .or(part_key.value.as_deref())
        .unwrap_or(component.netlist.refdes.as_str());
    sanitize_component_dir_name(raw)
}

fn sanitize_component_dir_name(raw: &str) -> String {
    // Reuse the strict, shared sanitizer used by `pcb search` component generation.
    // This keeps import outputs consistent and ensures names are compatible with
    // Zener `Component(name=...)` validation rules.
    let mut out = component_gen::sanitize_mpn_for_path(raw);
    if out.len() > 100 {
        out.truncate(100);
    }
    out
}

fn footprint_name_from_fpid(fpid: &str) -> String {
    fpid.rsplit_once(':')
        .map(|(_, name)| name)
        .unwrap_or(fpid)
        .trim()
        .to_string()
}

fn write_component_symbol(
    out_dir: &Path,
    component_name: &str,
    component: &ImportComponentData,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
) -> Result<Option<pcb_eda::Symbol>> {
    let unit = component
        .schematic
        .as_ref()
        .and_then(|s| s.units.values().next());

    let lib_id = unit.and_then(|u| {
        u.lib_name
            .as_deref()
            .map(|n| KiCadLibId::from(n.to_string()))
    });
    let lib_id = lib_id
        .filter(|k| schematic_lib_symbols.contains_key(k))
        .or_else(|| unit.and_then(|u| u.lib_id.clone()));

    let Some(lib_id) = lib_id else {
        debug!(
            "Skipping symbol output for {} (no schematic lib_id)",
            component.netlist.refdes.as_str()
        );
        return Ok(None);
    };

    let Some(sym) = schematic_lib_symbols.get(&lib_id) else {
        debug!(
            "Skipping symbol output for {} (missing embedded lib_symbol for {})",
            component.netlist.refdes.as_str(),
            lib_id.as_str()
        );
        return Ok(None);
    };

    let out = pcb_eda::kicad::symbol_library::wrap_symbol_as_library(sym, "pcb import");
    let parsed = pcb_eda::SymbolLibrary::from_string(&out, "kicad_sym")
        .context("Failed to parse embedded KiCad symbol as a symbol library")?;
    let symbol = parsed
        .first_symbol()
        .context("Embedded symbol library contained no symbols")?
        .clone();

    let path = out_dir.join(format!("{component_name}.kicad_sym"));
    fs::write(&path, out).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(Some(symbol))
}

fn write_component_footprint(
    out_dir: &Path,
    component: &ImportComponentData,
) -> Result<Option<String>> {
    let Some(layout) = &component.layout else {
        debug!(
            "Skipping footprint output for {} (no layout footprint)",
            component.netlist.refdes.as_str()
        );
        return Ok(None);
    };

    let fpid = layout
        .fpid
        .as_deref()
        .or(component.netlist.footprint.as_deref())
        .unwrap_or("footprint");
    let fp_name = sanitize_component_dir_name(&footprint_name_from_fpid(fpid));
    let filename = format!("{fp_name}.kicad_mod");
    let path = out_dir.join(&filename);

    let mod_text =
        sexpr_board::transform_board_instance_footprint_to_standalone(&layout.footprint_sexpr)
            .map_err(|e| anyhow::anyhow!(e))
            .with_context(|| {
                format!(
                    "Failed to transform footprint {} for {}",
                    fpid,
                    component.netlist.refdes.as_str()
                )
            })?;

    fs::write(&path, mod_text).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(Some(filename))
}

fn write_component_zen(
    out_dir: &Path,
    component_name: &str,
    component: &ImportComponentData,
    symbol: Option<&pcb_eda::Symbol>,
    footprint_filename: Option<&str>,
    flags: ImportPartFlags,
) -> Result<Option<BTreeMap<String, BTreeSet<KiCadPinNumber>>>> {
    let symbol_filename = format!("{component_name}.kicad_sym");
    let Some(symbol) = symbol else {
        debug!(
            "Skipping .zen generation for {} (missing embedded lib_symbol)",
            component_name
        );
        return Ok(None);
    };

    let mut io_pins: BTreeMap<String, BTreeSet<KiCadPinNumber>> = BTreeMap::new();
    for pin in &symbol.pins {
        let pin_number = KiCadPinNumber::from(pin.number.clone());
        let io_name = component_gen::sanitize_pin_name(pin.signal_name());
        io_pins.entry(io_name).or_default().insert(pin_number);
    }

    let props = best_properties(component);
    let mpn = find_property_ci(props, &["mpn"])
        .or_else(|| find_property_ci(props, &["manufacturer_part_number"]))
        .or_else(|| find_property_ci(props, &["manufacturer part number"]))
        .map(|s| s.to_string())
        .unwrap_or_else(|| component_name.to_string());
    let manufacturer =
        find_property_ci(props, &["manufacturer", "mfr", "mfg"]).map(|s| s.to_string());

    let zen_content =
        component_gen::generate_component_zen(component_gen::GenerateComponentZenArgs {
            mpn: &mpn,
            component_name,
            symbol,
            symbol_filename: &symbol_filename,
            footprint_filename,
            datasheet_filename: None,
            manufacturer: manufacturer.as_deref(),
            generated_by: "pcb import",
            include_skip_bom: flags.any_skip_bom,
            include_skip_pos: flags.any_skip_pos,
            skip_bom_default: flags.all_skip_bom,
            skip_pos_default: flags.all_skip_pos,
        })
        .context("Failed to generate component .zen")?;

    let zen_path = out_dir.join(format!("{component_name}.zen"));
    crate::codegen::zen::write_zen_formatted(&zen_path, &zen_content)
        .with_context(|| format!("Failed to write {}", zen_path.display()))?;
    Ok(Some(io_pins))
}

fn build_imported_instance_calls_for_instances(
    mut instances: Vec<(&KiCadUuidPathKey, &ImportComponentData)>,
    port_to_net: &BTreeMap<ImportNetPort, KiCadNetName>,
    refdes_instance_names: &BTreeMap<KiCadRefDes, String>,
    net_ident_by_kicad_name: &BTreeMap<KiCadNetName, String>,
    generated_components: &GeneratedComponents,
    not_connected_nets: &BTreeSet<KiCadNetName>,
) -> Result<Vec<crate::codegen::board::ImportedInstanceCall>> {
    instances.sort_by(|a, b| a.1.netlist.refdes.cmp(&b.1.netlist.refdes));

    let mut instance_calls: Vec<crate::codegen::board::ImportedInstanceCall> = Vec::new();

    for (anchor, component) in instances {
        let Some(module_ident) = generated_components.anchor_to_module_ident.get(anchor) else {
            continue;
        };
        let Some(io_pins) = generated_components.module_io_pins.get(module_ident) else {
            continue;
        };
        let skip_defaults = generated_components
            .module_skip_defaults
            .get(module_ident)
            .with_context(|| format!("Missing module defaults for {module_ident}"))?;

        let refdes = component.netlist.refdes.clone();
        let instance_name = refdes_instance_names
            .get(&refdes)
            .cloned()
            .unwrap_or_else(|| refdes.as_str().to_string());
        let (dnp, skip_bom, skip_pos) = derive_import_instance_flags(component);
        let skip_bom_override =
            if skip_defaults.include_skip_bom && skip_bom != skip_defaults.skip_bom_default {
                Some(skip_bom)
            } else {
                None
            };
        let skip_pos_override =
            if skip_defaults.include_skip_pos && skip_pos != skip_defaults.skip_pos_default {
                Some(skip_pos)
            } else {
                None
            };
        let mut io_nets: BTreeMap<String, String> = BTreeMap::new();

        for (io_name, pin_numbers) in io_pins {
            let mut connected: BTreeSet<KiCadNetName> = BTreeSet::new();
            for pin in pin_numbers {
                for key in &component.netlist.unit_pcb_paths {
                    let port = ImportNetPort {
                        component: key.clone(),
                        pin: pin.clone(),
                    };
                    if let Some(net_name) = port_to_net.get(&port) {
                        connected.insert(net_name.clone());
                        break;
                    }
                }
            }

            let net_ident = if connected.is_empty() {
                anyhow::bail!(
                    "Missing KiCad connectivity for component {} IO {} (pins {}). This is likely an import bug.",
                    refdes,
                    io_name,
                    pin_numbers
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                let chosen = connected
                    .iter()
                    .find(|n| !not_connected_nets.contains(*n))
                    .unwrap_or_else(|| connected.iter().next().unwrap());
                if connected.len() > 1 {
                    debug!(
                        "Component {} IO {} spans multiple KiCad nets ({}); using {}",
                        refdes,
                        io_name,
                        connected
                            .iter()
                            .map(|n| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        chosen.as_str()
                    );
                }
                if not_connected_nets.contains(chosen) {
                    "NotConnected()".to_string()
                } else {
                    net_ident_by_kicad_name
                        .get(chosen)
                        .cloned()
                        .with_context(|| {
                            format!("Missing net identifier for KiCad net {}", chosen.as_str())
                        })?
                }
            };

            io_nets.insert(io_name.clone(), net_ident);
        }

        instance_calls.push(crate::codegen::board::ImportedInstanceCall {
            module_ident: module_ident.clone(),
            refdes: instance_name,
            dnp,
            skip_bom: skip_bom_override,
            skip_pos: skip_pos_override,
            config_args: generated_components
                .anchor_to_config_args
                .get(anchor)
                .cloned()
                .unwrap_or_default(),
            io_nets,
        });
    }

    Ok(instance_calls)
}

fn build_refdes_instance_name_map(
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> BTreeMap<KiCadRefDes, String> {
    let refdeses: BTreeSet<KiCadRefDes> = components
        .values()
        .map(|c| c.netlist.refdes.clone())
        .collect();

    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut out: BTreeMap<KiCadRefDes, String> = BTreeMap::new();

    for refdes in refdeses {
        let base = sanitize_kicad_name_for_zener(refdes.as_str(), "REF");
        let name = alloc_unique_ident(&base, &mut used);
        out.insert(refdes, name);
    }

    out
}

fn derive_import_instance_flags(component: &ImportComponentData) -> (bool, bool, bool) {
    let mut dnp = false;
    let mut skip_bom = false;
    let mut skip_pos = false;

    if let Some(schematic) = component.schematic.as_ref() {
        for unit in schematic.units.values() {
            dnp |= unit.dnp.unwrap_or(false);
            skip_bom |= unit.in_bom == Some(false);
            skip_pos |= unit.on_board == Some(false);
        }
    }

    if let Some(layout) = component.layout.as_ref() {
        let has_attr = |needle: &str| layout.attrs.iter().any(|a| a == needle);
        dnp |= has_attr("dnp");
        skip_bom |= has_attr("exclude_from_bom");
        skip_pos |= has_attr("exclude_from_pos_files");
    }

    (dnp, skip_bom, skip_pos)
}

fn alloc_unique_ident(base: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut n: usize = 2;
    loop {
        let candidate = format!("{base}_{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn alloc_unique_fs_segment(base: &str, used_ci: &mut BTreeSet<String>) -> String {
    // Allocate unique path segments while treating collisions case-insensitively.
    //
    // The importer sanitizers only emit ASCII path segments; ASCII casefolding is
    // sufficient and matches common case-insensitive filesystem behavior.
    let mut candidate = base.to_string();
    let mut n: usize = 2;
    loop {
        let key = candidate.to_ascii_lowercase();
        if used_ci.insert(key) {
            return candidate;
        }
        candidate = format!("{base}_{n}");
        n += 1;
    }
}
