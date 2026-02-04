use super::*;
use anyhow::{Context, Result};
use log::debug;
use pcb_component_gen as component_gen;
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
    let net_decls = build_net_decls(&ir.nets);
    let reserved_idents: BTreeSet<String> =
        net_decls.decls.iter().map(|d| d.ident.clone()).collect();

    let refdes_instance_names = build_refdes_instance_name_map(&ir.components);

    let component_modules = generate_imported_components(
        &materialized.board_dir,
        &ir.components,
        &reserved_idents,
        &ir.schematic_lib_symbols,
    )?;

    let leaf_modules = generate_leaf_sheet_modules(
        &materialized.board_dir,
        board_name,
        ir,
        &port_to_net,
        &refdes_instance_names,
        &net_decls,
        &component_modules,
    )?;

    write_imported_board_zen(ImportedBoardZenArgs {
        board_zen: &materialized.board_zen,
        board_name,
        layout_kicad_pcb: &materialized.layout_kicad_pcb,
        port_to_net: &port_to_net,
        refdes_instance_names: &refdes_instance_names,
        components: &ir.components,
        net_decls: &net_decls,
        component_modules: &component_modules,
        leaf_modules: &leaf_modules,
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
    net_decls: &'a ImportedNetDecls,
    component_modules: &'a GeneratedComponents,
    leaf_modules: &'a GeneratedLeafModules,
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
        args.leaf_modules,
    )
    .context("Failed to pre-patch imported KiCad PCB for sync hooks")?;

    let root_net_idents = args
        .net_decls
        .ident_map_for_set(&args.leaf_modules.root_net_set);

    let mut instance_calls = args.leaf_modules.module_instance_calls.clone();
    instance_calls.extend(build_imported_instance_calls_for_instances(
        args.components
            .iter()
            .filter(|(a, _)| !args.leaf_modules.anchors_in_leaf.contains(*a))
            .collect(),
        args.port_to_net,
        args.refdes_instance_names,
        &root_net_idents,
        args.component_modules,
    )?);

    let root_net_decls = args
        .net_decls
        .decls_for_set(&args.leaf_modules.root_net_set);

    let used_module_idents: BTreeSet<String> = instance_calls
        .iter()
        .map(|c| c.module_ident.clone())
        .collect();
    let module_decls: Vec<(String, String)> = args
        .component_modules
        .module_decls
        .iter()
        .chain(args.leaf_modules.module_decls.iter())
        .filter(|(ident, _)| used_module_idents.contains(ident))
        .cloned()
        .collect();

    let board_zen_content = crate::codegen::board::render_imported_board(
        args.board_name,
        copper_layers,
        stackup.as_ref(),
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
    leaf_modules: &GeneratedLeafModules,
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
        leaf_modules,
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
    leaf_modules: &GeneratedLeafModules,
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
        let prefix = leaf_modules
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
    let stackup = match zen_stackup::Stackup::from_kicad_pcb(pcb_text) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok((4, None));
        }
        Err(e) => {
            anyhow::bail!(
                "Skipping stackup extraction (failed to parse stackup from {}): {}",
                layout_kicad_pcb.display(),
                e
            );
        }
    };

    let Some(layers) = stackup.layers.as_deref() else {
        return Ok((4, None));
    };
    if layers.is_empty() {
        return Ok((4, None));
    }

    let copper_layers = stackup.copper_layer_count();
    if !matches!(copper_layers, 2 | 4 | 6 | 8 | 10) {
        return Ok((4, None));
    }

    Ok((copper_layers, Some(stackup)))
}

fn build_net_decls(netlist_nets: &BTreeMap<KiCadNetName, ImportNetData>) -> ImportedNetDecls {
    let mut used_idents: BTreeSet<String> = BTreeSet::new();
    let mut used_net_names: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
    let mut var_ident_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();
    let mut zener_name_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();

    for net_name in netlist_nets.keys() {
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

fn generate_leaf_sheet_modules(
    board_dir: &Path,
    board_name: &str,
    ir: &ImportIr,
    port_to_net: &BTreeMap<ImportNetPort, KiCadNetName>,
    refdes_instance_names: &BTreeMap<KiCadRefDes, String>,
    net_decls: &ImportedNetDecls,
    components: &GeneratedComponents,
) -> Result<GeneratedLeafModules> {
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

    let mut paths: Vec<KiCadSheetPath> = ir.schematic_sheet_tree.nodes.keys().cloned().collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.depth()));

    let mut subtree_has_components: BTreeMap<KiCadSheetPath, bool> = BTreeMap::new();
    for path in &paths {
        let has_here = anchors_by_sheet.get(path).is_some_and(|v| !v.is_empty());
        let has_child = ir
            .schematic_sheet_tree
            .nodes
            .get(path)
            .map(|n| {
                n.children
                    .iter()
                    .any(|c| subtree_has_components.get(c).copied().unwrap_or(false))
            })
            .unwrap_or(false);
        subtree_has_components.insert(path.clone(), has_here || has_child);
    }

    let mut leaf_paths: Vec<KiCadSheetPath> = Vec::new();
    for path in ir.schematic_sheet_tree.nodes.keys() {
        if path.as_str() == "/" {
            continue;
        }
        let has_here = anchors_by_sheet.get(path).is_some_and(|v| !v.is_empty());
        if !has_here {
            continue;
        }
        let has_child_subtree = ir
            .schematic_sheet_tree
            .nodes
            .get(path)
            .map(|n| {
                n.children
                    .iter()
                    .any(|c| subtree_has_components.get(c).copied().unwrap_or(false))
            })
            .unwrap_or(false);
        if !has_child_subtree {
            leaf_paths.push(path.clone());
        }
    }

    leaf_paths.sort();

    let mut used_module_dirs: BTreeSet<String> = BTreeSet::new();
    let mut used_module_instance_names: BTreeSet<String> = BTreeSet::new();

    let mut used_root_idents: BTreeSet<String> = BTreeSet::new();
    used_root_idents.extend(net_decls.var_ident_by_kicad_name.values().cloned());
    used_root_idents.extend(components.module_decls.iter().map(|(i, _)| i.clone()));

    let mut module_decls: Vec<(String, String)> = Vec::new();
    let mut module_instance_calls: Vec<crate::codegen::board::ImportedInstanceCall> = Vec::new();
    let mut anchors_in_leaf: BTreeSet<KiCadUuidPathKey> = BTreeSet::new();
    let mut anchor_to_entity_prefix: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
    let mut leaf_owned_nets: BTreeSet<KiCadNetName> = BTreeSet::new();

    for leaf in &leaf_paths {
        let Some(node) = ir.schematic_sheet_tree.nodes.get(leaf) else {
            continue;
        };
        let sheet_name = node
            .sheet_name
            .clone()
            .or_else(|| leaf.last_uuid().map(|u| u.to_string()))
            .unwrap_or_else(|| "sheet".to_string());

        let mut module_dir = component_gen::sanitize_mpn_for_path(&sheet_name);
        if module_dir.is_empty() {
            module_dir = "sheet".to_string();
        }
        if !used_module_dirs.insert(module_dir.clone()) {
            let suffix = leaf
                .last_uuid()
                .map(|u| u.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "sheet".to_string());
            module_dir =
                alloc_unique_ident(&format!("{module_dir}_{suffix}"), &mut used_module_dirs);
        }

        let module_ident_base = module_ident_from_component_dir(&module_dir);
        let module_ident = alloc_unique_ident(&module_ident_base, &mut used_root_idents);

        let instance_name_base = sanitize_screaming_snake_identifier(&sheet_name, "SHEET");
        let instance_name =
            alloc_unique_ident(&instance_name_base, &mut used_module_instance_names);

        let module_rel_path = format!("modules/{module_dir}/{module_dir}.zen");
        module_decls.push((module_ident.clone(), module_rel_path.clone()));

        let module_plan = ir
            .hierarchy_plan
            .modules
            .get(leaf)
            .cloned()
            .unwrap_or_default();

        let mut module_net_set: BTreeSet<KiCadNetName> = BTreeSet::new();
        module_net_set.extend(module_plan.nets_defined_here.iter().cloned());
        module_net_set.extend(module_plan.nets_io_here.iter().cloned());

        leaf_owned_nets.extend(module_plan.nets_defined_here.iter().cloned());

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

        let leaf_anchors = anchors_by_sheet.get(leaf).cloned().unwrap_or_default();
        for a in &leaf_anchors {
            anchors_in_leaf.insert(a.clone());
            anchor_to_entity_prefix.insert(a.clone(), instance_name.clone());
        }

        let leaf_instances: Vec<(&KiCadUuidPathKey, &ImportComponentData)> = leaf_anchors
            .iter()
            .filter_map(|a| ir.components.get_key_value(a))
            .collect();

        let component_instance_calls = build_imported_instance_calls_for_instances(
            leaf_instances,
            port_to_net,
            refdes_instance_names,
            &module_net_ident_by_kicad,
            components,
        )?;

        let used_component_modules: BTreeSet<String> = component_instance_calls
            .iter()
            .map(|c| c.module_ident.clone())
            .collect();
        let mut module_component_decls: Vec<(String, String)> = components
            .module_decls
            .iter()
            .filter(|(ident, _)| used_component_modules.contains(ident))
            .map(|(ident, path)| (ident.clone(), format!("../../{path}")))
            .collect();
        module_component_decls.sort();

        let module_dir_abs = modules_root.join(&module_dir);
        fs::create_dir_all(&module_dir_abs)
            .with_context(|| format!("Failed to create {}", module_dir_abs.display()))?;
        let module_zen = module_dir_abs.join(format!("{module_dir}.zen"));

        let module_doc = format!(
            "{} sheet module: {} ({})",
            board_name,
            sheet_name,
            leaf.as_str()
        );
        let module_zen_content = crate::codegen::board::render_imported_sheet_module(
            &module_doc,
            &io_net_idents,
            &internal_net_decls,
            &module_component_decls,
            &component_instance_calls,
        );
        crate::codegen::zen::write_zen_formatted(&module_zen, &module_zen_content)
            .with_context(|| format!("Failed to write {}", module_zen.display()))?;

        let io_nets: BTreeMap<String, String> = io_net_idents
            .iter()
            .map(|ident| (ident.clone(), ident.clone()))
            .collect();

        module_instance_calls.push(crate::codegen::board::ImportedInstanceCall {
            module_ident: module_ident.clone(),
            refdes: instance_name,
            dnp: false,
            skip_bom: None,
            skip_pos: None,
            io_nets,
        });
    }

    let mut root_net_set: BTreeSet<KiCadNetName> =
        net_decls.var_ident_by_kicad_name.keys().cloned().collect();
    for n in leaf_owned_nets {
        root_net_set.remove(&n);
    }

    Ok(GeneratedLeafModules {
        module_decls,
        module_instance_calls,
        anchors_in_leaf,
        anchor_to_entity_prefix,
        root_net_set,
    })
}

struct ImportedNetDecls {
    decls: Vec<crate::codegen::board::ImportedNetDecl>,
    var_ident_by_kicad_name: BTreeMap<KiCadNetName, String>,
    zener_name_by_kicad_name: BTreeMap<KiCadNetName, String>,
}

#[derive(Debug, Default)]
struct GeneratedLeafModules {
    /// Module() declarations for leaf sheet modules, to be included in the root board file.
    module_decls: Vec<(String, String)>,
    /// Instantiation calls for leaf sheet modules, to be included in the root board file.
    module_instance_calls: Vec<crate::codegen::board::ImportedInstanceCall>,
    /// Component anchors that are instantiated inside leaf sheet modules (not in the root board).
    anchors_in_leaf: BTreeSet<KiCadUuidPathKey>,
    /// Component anchor -> Zener entity path prefix (module instance name), used for footprint sync hook prepatching.
    anchor_to_entity_prefix: BTreeMap<KiCadUuidPathKey, String>,
    /// Net set declared in the root board file (everything except leaf-owned internal nets).
    root_net_set: BTreeSet<KiCadNetName>,
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

#[derive(Debug, Default, Clone, Copy)]
struct ImportPartFlags {
    any_skip_bom: bool,
    any_skip_pos: bool,
    all_skip_bom: bool,
    all_skip_pos: bool,
}

fn generate_imported_components(
    board_dir: &Path,
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    reserved_idents: &BTreeSet<String>,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
) -> Result<GeneratedComponents> {
    let components_root = board_dir.join("components");
    fs::create_dir_all(&components_root).with_context(|| {
        format!(
            "Failed to create components output directory {}",
            components_root.display()
        )
    })?;

    let mut part_to_instances: BTreeMap<ImportPartKey, Vec<KiCadUuidPathKey>> = BTreeMap::new();
    let mut part_flags: BTreeMap<ImportPartKey, ImportPartFlags> = BTreeMap::new();
    for (anchor, c) in components {
        if c.layout.is_none() {
            // Only generate component packages for footprints that exist on the PCB.
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

    let mut used_names: BTreeMap<(Option<String>, String), usize> = BTreeMap::new();
    let mut part_dir_by_key: BTreeMap<ImportPartKey, ImportPartDir> = BTreeMap::new();

    for (part_key, instances) in &part_to_instances {
        let Some(first_anchor) = instances.first() else {
            continue;
        };
        let Some(component) = components.get(first_anchor) else {
            continue;
        };

        let manufacturer_dir =
            component_manufacturer(component).map(|m| sanitize_component_dir_name(&m));
        let base_name = derive_part_name(part_key, component);
        let mut name = base_name.clone();
        if used_names.contains_key(&(manufacturer_dir.clone(), name.clone())) {
            let fp_name = part_key
                .footprint
                .as_deref()
                .map(footprint_name_from_fpid)
                .unwrap_or_else(|| "footprint".to_string());
            name = format!("{base_name}__{fp_name}");
        }
        let count = used_names
            .entry((manufacturer_dir.clone(), name.clone()))
            .or_insert(0);
        *count += 1;
        if *count > 1 {
            name = format!("{name}_{}", *count);
        }

        part_dir_by_key.insert(
            part_key.clone(),
            ImportPartDir {
                manufacturer_dir,
                component_dir: name,
            },
        );
    }

    let mut module_decls: Vec<(String, String)> = Vec::new();
    let mut used_module_idents: BTreeMap<String, usize> = reserved_idents
        .iter()
        .map(|i| (i.clone(), 1usize))
        .collect();
    let mut anchor_to_module_ident: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
    let mut anchor_to_component_name: BTreeMap<KiCadUuidPathKey, String> = BTreeMap::new();
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
            let mut ident = module_ident_from_component_dir(&part_dir.component_dir);
            match used_module_idents.get_mut(&ident) {
                None => {
                    used_module_idents.insert(ident.clone(), 1);
                }
                Some(n) => {
                    *n += 1;
                    ident = format!("{ident}_{}", *n);
                }
            }

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

            module_decls.push((ident, module_path));
        }
    }

    Ok(GeneratedComponents {
        module_decls,
        anchor_to_module_ident,
        anchor_to_component_name,
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
    let props = component_best_properties(component);
    let mpn = find_property_ci(
        &props,
        &[
            "mpn",
            "manufacturer_part_number",
            "manufacturer part number",
        ],
    )
    .or_else(|| {
        find_property_ci(
            &props,
            &["mfr part number", "manufacturer_pn", "part number"],
        )
    });

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

fn component_best_properties(component: &ImportComponentData) -> BTreeMap<String, String> {
    if let Some(sch) = &component.schematic {
        if let Some(unit) = sch.units.values().next() {
            return unit.properties.clone();
        }
    }
    component
        .layout
        .as_ref()
        .map(|l| l.properties.clone())
        .unwrap_or_default()
}

fn component_manufacturer(component: &ImportComponentData) -> Option<String> {
    let props = component_best_properties(component);
    find_property_ci(&props, &["manufacturer"])
}

fn find_property_ci(props: &BTreeMap<String, String>, keys: &[&str]) -> Option<String> {
    for want in keys {
        let want_lc = want.to_ascii_lowercase();
        for (k, v) in props {
            if k.to_ascii_lowercase() == want_lc {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

fn derive_part_name(part_key: &ImportPartKey, component: &ImportComponentData) -> String {
    let raw = part_key
        .mpn
        .as_deref()
        .or(part_key.value.as_deref())
        .or(Some(component.netlist.refdes.as_str()))
        .unwrap_or("component");
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

    let props = component_best_properties(component);
    let mpn = find_property_ci(&props, &["mpn"])
        .or_else(|| find_property_ci(&props, &["manufacturer_part_number"]))
        .or_else(|| find_property_ci(&props, &["manufacturer part number"]))
        .unwrap_or_else(|| component_name.to_string());
    let manufacturer = component_manufacturer(component);

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
                let chosen = connected.iter().next().unwrap();
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
                net_ident_by_kicad_name
                    .get(chosen)
                    .cloned()
                    .with_context(|| {
                        format!("Missing net identifier for KiCad net {}", chosen.as_str())
                    })?
            };

            io_nets.insert(io_name.clone(), net_ident);
        }

        instance_calls.push(crate::codegen::board::ImportedInstanceCall {
            module_ident: module_ident.clone(),
            refdes: instance_name,
            dnp,
            skip_bom: skip_bom_override,
            skip_pos: skip_pos_override,
            io_nets,
        });
    }

    Ok(instance_calls)
}

fn build_refdes_instance_name_map(
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> BTreeMap<KiCadRefDes, String> {
    let mut refdeses: Vec<KiCadRefDes> = components
        .values()
        .map(|c| c.netlist.refdes.clone())
        .collect();
    refdeses.sort();
    refdeses.dedup();

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
