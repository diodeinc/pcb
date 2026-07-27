mod schematic_comments;
mod schematic_placement;
mod schematic_types;

use self::schematic_comments::{
    append_schematic_position_comments, build_flat_component_schematic_positions,
    build_net_symbol_positions_for_sheet,
};
use super::*;
use anyhow::{Context, Result};
use log::debug;
use pcb_component_gen as component_gen;
use pcb_sexpr::Sexpr;
use pcb_sexpr::find_child_list;
use pcb_sexpr::formatter::{FormatMode, format_tree};
use pcb_sexpr::kicad::symbol::{
    kicad_symbol_lib_items_mut, rewrite_symbol_properties, symbol_names, symbol_properties,
};
use pcb_sexpr::{PatchSet, Span, board as sexpr_board};
use pcb_zen_core::lang::stackup as zen_stackup;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(super) struct GenerationResult {
    pub(super) registry_reused_entrypoints: Vec<PathBuf>,
    /// Refdes -> number of competing compatible cached registry entrypoints, for components that
    /// fell back because the choice was ambiguous rather than because nothing matched.
    pub(super) registry_ambiguous_by_refdes: BTreeMap<KiCadRefDes, usize>,
    pub(super) sourcing_by_refdes: BTreeMap<KiCadRefDes, ImportGeneratedSourcingStatus>,
    pub(super) expected_pins_by_refdes: BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    /// The Zener instance name generated for each source reference designator.
    ///
    /// Exposed because validation has to map a *built* component back to its source refdes, and the
    /// instance name is sanitized on the way out — `TP_3.3v1` becomes `TP_3_3v1`. Matching path
    /// segments against raw refdeses therefore fails for any refdes the sanitizer had to change, and
    /// the component then cannot be mapped at all.
    pub(super) instance_name_by_refdes: BTreeMap<KiCadRefDes, String>,
}

pub(super) fn generate(
    materialized: &MaterializedBoard,
    board_name: &str,
    ir: &ImportIr,
    allow_registry_reuse: bool,
    writer: &mut output::ImportWriter,
) -> Result<GenerationResult> {
    let port_to_net = build_port_to_net_map(&ir.nets)?;
    let not_connected_nets = build_not_connected_nets(&ir.nets);
    let net_decls = build_net_decls(&ir.nets, &not_connected_nets, &ir.semantic.net_kinds.by_net);
    let reserved_idents: BTreeSet<String> =
        net_decls.decls.iter().map(|d| d.ident.clone()).collect();

    let refdes_instance_names = build_refdes_instance_name_map(&ir.components);

    let component_modules = generate_imported_components(
        GenerateImportedComponentsArgs {
            board_dir: &materialized.board_dir,
            components: &ir.components,
            reserved_idents: &reserved_idents,
            schematic_lib_symbols: &ir.schematic_lib_symbols,
            passive_by_component: &ir.semantic.passives.by_component,
            registry_lookup: &ir.semantic.registry_mpn_lookup,
            allow_registry_reuse,
            port_to_net: &port_to_net,
            not_connected_nets: &not_connected_nets,
        },
        writer,
    )?;

    let expected_pins_by_refdes = component_modules
        .expected_pins_by_anchor
        .iter()
        .filter_map(|(anchor, pins)| {
            let component = ir.components.get(anchor)?;
            Some((component.netlist.refdes.clone(), pins.clone()))
        })
        .collect();

    let sheet_modules = generate_sheet_modules(
        GenerateSheetModulesArgs {
            board_dir: &materialized.board_dir,
            board_name,
            ir,
            port_to_net: &port_to_net,
            refdes_instance_names: &refdes_instance_names,
            net_decls: &net_decls,
            components: &component_modules,
            not_connected_nets: &not_connected_nets,
        },
        writer,
    )?;

    write_imported_board_zen(
        writer,
        ImportedBoardZenArgs {
            board_zen: &materialized.board_zen,
            board_name,
            layout_kicad_pro: materialized.layout_kicad_pro.as_deref(),
            layout_kicad_pcb: materialized.layout_kicad_pcb.as_deref(),
            port_to_net: &port_to_net,
            refdes_instance_names: &refdes_instance_names,
            components: &ir.components,
            hierarchy_plan: &ir.hierarchy_plan,
            schematic_sheet_tree: &ir.schematic_sheet_tree,
            schematic_lib_symbols: &ir.schematic_lib_symbols,
            schematic_power_symbol_decls: &ir.schematic_power_symbol_decls,
            net_kinds_by_net: &ir.semantic.net_kinds.by_net,
            net_decls: &net_decls,
            component_modules: &component_modules,
            sheet_modules: &sheet_modules,
            not_connected_nets: &not_connected_nets,
        },
    )?;

    Ok(GenerationResult {
        registry_reused_entrypoints: component_modules.registry_reused_entrypoints,
        registry_ambiguous_by_refdes: component_modules.registry_ambiguous_by_refdes,
        sourcing_by_refdes: component_modules.sourcing_by_refdes,
        expected_pins_by_refdes,
        instance_name_by_refdes: refdes_instance_names,
    })
}

struct ImportedBoardZenArgs<'a> {
    board_zen: &'a Path,
    board_name: &'a str,
    layout_kicad_pro: Option<&'a Path>,
    layout_kicad_pcb: Option<&'a Path>,
    port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    refdes_instance_names: &'a BTreeMap<KiCadRefDes, String>,
    components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    hierarchy_plan: &'a ImportHierarchyPlan,
    schematic_sheet_tree: &'a ImportSheetTree,
    schematic_lib_symbols: &'a BTreeMap<KiCadLibId, String>,
    schematic_power_symbol_decls: &'a [ImportSchematicPowerSymbolDecl],
    net_kinds_by_net: &'a BTreeMap<KiCadNetName, ImportNetKindClassification>,
    net_decls: &'a ImportedNetDecls,
    component_modules: &'a GeneratedComponents,
    sheet_modules: &'a GeneratedSheetModules,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

fn write_imported_board_zen(
    writer: &mut output::ImportWriter,
    args: ImportedBoardZenArgs<'_>,
) -> Result<()> {
    let mut copper_layers = 4;
    let mut stackup = None;
    let mut design_rules = None;

    if let Some(layout_kicad_pcb) = args.layout_kicad_pcb {
        let pcb_text = fs::read_to_string(layout_kicad_pcb).with_context(|| {
            format!(
                "Failed to read KiCad PCB for stackup extraction: {}",
                layout_kicad_pcb.display()
            )
        })?;
        match try_extract_stackup(&pcb_text, layout_kicad_pcb) {
            Ok((layers, extracted_stackup)) => {
                copper_layers = layers;
                stackup = extracted_stackup;
            }
            Err(e) => debug!("{e:#}"),
        }
        if let Some(layout_kicad_pro) = args.layout_kicad_pro {
            design_rules = pcb_layout::extract_design_rules_from_kicad_pro(layout_kicad_pro)
                .ok()
                .flatten();
        }

        prepatch_imported_layout_kicad_pcb(
            writer,
            LayoutPrepatchArgs {
                layout_kicad_pcb,
                pcb_text: &pcb_text,
                components: args.components,
                refdes_instance_names: args.refdes_instance_names,
                net_ident_by_kicad_name: &args.net_decls.zener_name_by_kicad_name,
                generated_components: args.component_modules,
                sheet_modules: args.sheet_modules,
            },
        )
        .context("Failed to pre-patch imported KiCad PCB for sync hooks")?;
    }

    let root_sheet = KiCadSheetPath::root();
    let root_plan = args
        .hierarchy_plan
        .modules
        .get(&root_sheet)
        .cloned()
        .unwrap_or_default();

    let root_net_set: BTreeSet<KiCadNetName> = root_plan.nets_defined_here.clone();
    let root_net_idents = args.net_decls.ident_map_for_set(&root_net_set);

    let root_anchors: Vec<(&KiCadUuidPathKey, &ImportComponentData)> = args
        .components
        .iter()
        .filter(|(a, c)| {
            c.layout.is_some()
                && KiCadSheetPath::from_sheetpath_tstamps(&a.sheetpath_tstamps).as_str() == "/"
        })
        .collect();
    let root_schematic_positions = build_flat_component_schematic_positions(
        &root_anchors,
        args.refdes_instance_names,
        args.component_modules,
    );

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
    let mut root_schematic_positions = if root_sheet_module_calls.is_empty() {
        root_schematic_positions
    } else {
        BTreeMap::new()
    };
    if root_sheet_module_calls.is_empty() {
        root_schematic_positions.extend(build_net_symbol_positions_for_sheet(
            &root_sheet,
            &root_plan,
            args.net_decls,
            args.net_kinds_by_net,
            args.schematic_power_symbol_decls,
        ));
    }

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

    let board_zen_content = crate::codegen::board::render_imported_board(
        crate::codegen::board::RenderImportedBoardArgs {
            board_name: args.board_name,
            copper_layers,
            design_rules: design_rules.as_ref(),
            stackup: stackup.as_ref(),
            net_decls: &root_net_decls,
            module_decls: &module_decls,
            instance_calls: &instance_calls,
        },
    );
    let board_zen_content = append_schematic_position_comments(
        board_zen_content,
        &root_schematic_positions,
        args.schematic_lib_symbols,
    );
    writer
        .write_zen(args.board_zen, &board_zen_content)
        .with_context(|| format!("Failed to write {}", args.board_zen.display()))?;

    Ok(())
}

struct LayoutPrepatchArgs<'a> {
    layout_kicad_pcb: &'a Path,
    pcb_text: &'a str,
    components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    refdes_instance_names: &'a BTreeMap<KiCadRefDes, String>,
    net_ident_by_kicad_name: &'a BTreeMap<KiCadNetName, String>,
    generated_components: &'a GeneratedComponents,
    sheet_modules: &'a GeneratedSheetModules,
}

fn prepatch_imported_layout_kicad_pcb(
    writer: &mut output::ImportWriter,
    args: LayoutPrepatchArgs<'_>,
) -> Result<()> {
    let LayoutPrepatchArgs {
        layout_kicad_pcb,
        pcb_text,
        components,
        refdes_instance_names,
        net_ident_by_kicad_name,
        generated_components,
        sheet_modules,
    } = args;
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
    writer
        .write(layout_kicad_pcb, &out)
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
                    if prop_name == Some("Reference")
                        && refdes.is_none()
                        && let Some(value) = list.get(2).and_then(Sexpr::as_str)
                    {
                        refdes = Some(KiCadRefDes::from(value.to_string()));
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
    net_kinds: &BTreeMap<KiCadNetName, ImportNetKindClassification>,
) -> ImportedNetDecls {
    let mut used_idents: BTreeSet<String> = BTreeSet::new();
    let mut used_net_names: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
    let mut var_ident_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();
    let mut zener_name_by_kicad_name: BTreeMap<KiCadNetName, String> = BTreeMap::new();
    let mut kind_by_kicad_name: BTreeMap<KiCadNetName, crate::codegen::board::ImportedNetKind> =
        BTreeMap::new();

    for net_name in netlist_nets.keys() {
        if not_connected_nets.contains(net_name) {
            continue;
        }
        let ident_base = sanitize_screaming_snake_identifier(net_name.as_str(), "NET");
        let ident = alloc_unique_ident(&ident_base, "_", &mut used_idents);

        let name_base = sanitize_kicad_name_for_zener(net_name.as_str(), "NET");
        let name = alloc_unique_ident(&name_base, "_", &mut used_net_names);

        let kind = net_kinds
            .get(net_name)
            .map(|k| k.kind)
            .unwrap_or(ImportNetKind::Net);

        let imported_kind = match kind {
            ImportNetKind::Net => crate::codegen::board::ImportedNetKind::Net,
            ImportNetKind::Power => crate::codegen::board::ImportedNetKind::Power,
            ImportNetKind::Ground => crate::codegen::board::ImportedNetKind::Ground,
        };

        out.push(crate::codegen::board::ImportedNetDecl {
            ident: ident.clone(),
            name: name.clone(),
            kind: imported_kind,
        });
        var_ident_by_kicad_name.insert(net_name.clone(), ident);
        zener_name_by_kicad_name.insert(net_name.clone(), name);
        kind_by_kicad_name.insert(net_name.clone(), imported_kind);
    }

    ImportedNetDecls {
        decls: out,
        var_ident_by_kicad_name,
        zener_name_by_kicad_name,
        kind_by_kicad_name,
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
            let kind = self
                .kind_by_kicad_name
                .get(net_name)
                .copied()
                .unwrap_or(crate::codegen::board::ImportedNetKind::Net);
            out.push(crate::codegen::board::ImportedNetDecl { ident, name, kind });
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

fn generate_sheet_modules(
    args: GenerateSheetModulesArgs<'_>,
    writer: &mut output::ImportWriter,
) -> Result<GeneratedSheetModules> {
    let board_dir = args.board_dir;
    let board_name = args.board_name;
    let ir = args.ir;
    let port_to_net = args.port_to_net;
    let refdes_instance_names = args.refdes_instance_names;
    let net_decls = args.net_decls;
    let components = args.components;
    let not_connected_nets = args.not_connected_nets;
    let modules_root = board_dir.join("modules");

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

        let io_nets: Vec<crate::codegen::board::ImportedIoNetDecl> = module_plan
            .nets_io_here
            .iter()
            .filter_map(|net_name| {
                let ident = module_net_ident_by_kicad.get(net_name).cloned()?;
                let kind = ir
                    .semantic
                    .net_kinds
                    .by_net
                    .get(net_name)
                    .map(|k| k.kind)
                    .unwrap_or(ImportNetKind::Net);
                Some(crate::codegen::board::ImportedIoNetDecl {
                    ident,
                    kind: match kind {
                        ImportNetKind::Net => crate::codegen::board::ImportedNetKind::Net,
                        ImportNetKind::Power => crate::codegen::board::ImportedNetKind::Power,
                        ImportNetKind::Ground => crate::codegen::board::ImportedNetKind::Ground,
                    },
                })
            })
            .collect();

        let mut internal_net_decls: Vec<crate::codegen::board::ImportedNetDecl> = Vec::new();
        for net_name in &module_plan.nets_defined_here {
            let Some(ident) = module_net_ident_by_kicad.get(net_name).cloned() else {
                continue;
            };
            let Some(name) = net_decls.zener_name_by_kicad_name.get(net_name).cloned() else {
                continue;
            };
            let kind = ir
                .semantic
                .net_kinds
                .by_net
                .get(net_name)
                .map(|k| k.kind)
                .unwrap_or(ImportNetKind::Net);
            internal_net_decls.push(crate::codegen::board::ImportedNetDecl {
                ident,
                name,
                kind: match kind {
                    ImportNetKind::Net => crate::codegen::board::ImportedNetKind::Net,
                    ImportNetKind::Power => crate::codegen::board::ImportedNetKind::Power,
                    ImportNetKind::Ground => crate::codegen::board::ImportedNetKind::Ground,
                },
            });
        }

        let sheet_anchors = anchors_by_sheet
            .get(&sheet_path)
            .cloned()
            .unwrap_or_default();
        let sheet_instances: Vec<(&KiCadUuidPathKey, &ImportComponentData)> = sheet_anchors
            .iter()
            .filter_map(|a| ir.components.get_key_value(a))
            .collect();
        let mut module_schematic_positions = build_flat_component_schematic_positions(
            &sheet_instances,
            refdes_instance_names,
            components,
        );
        module_schematic_positions.extend(build_net_symbol_positions_for_sheet(
            &sheet_path,
            &module_plan,
            net_decls,
            &ir.semantic.net_kinds.by_net,
            &ir.schematic_power_symbol_decls,
        ));

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
        used_idents.extend(io_nets.iter().map(|n| n.ident.clone()));
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
            let module_ident = alloc_unique_ident(&module_ident_base, "_", &mut used_idents);
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
        let is_flat_component_only_module = child_module_calls.is_empty();
        instance_calls.extend(child_module_calls.into_values());
        instance_calls.extend(component_instance_calls);

        let mut module_zen_content = crate::codegen::board::render_imported_sheet_module(
            &module_doc,
            &io_nets,
            &internal_net_decls,
            &module_decls,
            &instance_calls,
        );
        if is_flat_component_only_module {
            module_zen_content = append_schematic_position_comments(
                module_zen_content,
                &module_schematic_positions,
                &ir.schematic_lib_symbols,
            );
        }
        writer
            .write_zen(&module_zen, &module_zen_content)
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
            let inst = alloc_unique_ident(&base, "_", &mut used);
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
        let module_ident = alloc_unique_ident(&module_ident_base, "_", &mut used_idents);
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
    kind_by_kicad_name: BTreeMap<KiCadNetName, crate::codegen::board::ImportedNetKind>,
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
    manufacturer: Option<String>,
    footprint: Option<String>,
    lib_id: Option<KiCadLibId>,
    lib_name: Option<KiCadLibId>,
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
    expected_pins_by_anchor: BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadPinNumber>>,
    registry_reused_entrypoints: Vec<PathBuf>,
    registry_ambiguous_by_refdes: BTreeMap<KiCadRefDes, usize>,
    sourcing_by_refdes: BTreeMap<KiCadRefDes, ImportGeneratedSourcingStatus>,
}

#[derive(Debug, Clone, Copy)]
struct ModuleSkipDefaults {
    include_skip_bom: bool,
    skip_bom_default: bool,
    include_skip_pos: bool,
    skip_pos_default: bool,
}

impl From<ImportPartFlags> for ModuleSkipDefaults {
    fn from(flags: ImportPartFlags) -> Self {
        Self {
            include_skip_bom: flags.any_skip_bom,
            skip_bom_default: flags.all_skip_bom,
            include_skip_pos: flags.any_skip_pos,
            skip_pos_default: flags.all_skip_pos,
        }
    }
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

fn has_standard_promoted_passive_pins(symbol: &pcb_eda::Symbol) -> bool {
    symbol
        .canonical_pins()
        .map(|pin| pin.number.as_str())
        .collect::<BTreeSet<_>>()
        == BTreeSet::from(["1", "2"])
}

struct GenerateImportedComponentsArgs<'a> {
    board_dir: &'a Path,
    components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    reserved_idents: &'a BTreeSet<String>,
    schematic_lib_symbols: &'a BTreeMap<KiCadLibId, String>,
    passive_by_component: &'a BTreeMap<KiCadUuidPathKey, ImportPassiveClassification>,
    registry_lookup: &'a ImportRegistryMpnLookup,
    allow_registry_reuse: bool,
    port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

/// Record an ambiguous registry-reuse count for every instance in a part group.
///
/// Reuse is decided once per part group using one representative instance, but the report keys
/// ambiguity by reference designator, so twenty resistors sharing an ambiguous part each need an
/// entry rather than only the representative's.
fn record_ambiguous_entrypoints(
    registry_ambiguous_by_refdes: &mut BTreeMap<KiCadRefDes, usize>,
    instances: &[KiCadUuidPathKey],
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    count: usize,
) {
    for instance in instances.iter().filter_map(|anchor| components.get(anchor)) {
        registry_ambiguous_by_refdes.insert(instance.netlist.refdes.clone(), count);
    }
}

fn generate_imported_components(
    args: GenerateImportedComponentsArgs<'_>,
    writer: &mut output::ImportWriter,
) -> Result<GeneratedComponents> {
    let GenerateImportedComponentsArgs {
        board_dir,
        components,
        reserved_idents,
        schematic_lib_symbols,
        passive_by_component,
        registry_lookup,
        allow_registry_reuse,
        port_to_net,
        not_connected_nets,
    } = args;
    let components_root = board_dir.join("components");

    // One evaluation context for the whole run: candidates resolve against the staged board's
    // workspace once, instead of each cached package becoming its own workspace root.
    let mut registry_context = registry_reuse::RegistryReuseContext::new(board_dir);

    let mut endpoint_pins_by_component: BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadPinNumber>> =
        BTreeMap::new();
    for port in port_to_net.keys() {
        endpoint_pins_by_component
            .entry(port.component.clone())
            .or_default()
            .insert(port.pin.clone());
    }

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
        alloc_unique_ident(base, "_", used)
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
            if let Some(v) = class.voltage.as_deref()
                && let Some(v) = canonical_voltage(v)
            {
                config_args.insert("voltage".to_string(), v);
            }
            if let Some(v) = class.dielectric.as_deref()
                && let Some(d) = canonical_dielectric(v)
            {
                config_args.insert("dielectric".to_string(), d.to_string());
            }
        }

        Some(PromotedPassive { kind, config_args })
    }

    // Compute promoted-passive candidates per-instance. The stdlib modules hardcode pins 1 and
    // 2, so verify the schematic symbol before promotion. This check must remain paired with the
    // missing-endpoint NotConnected() fallback in instance generation; otherwise a custom A/K
    // passive could be silently disconnected.
    let mut candidate_by_anchor: BTreeMap<KiCadUuidPathKey, PromotedPassive> = BTreeMap::new();
    for (anchor, component) in components {
        let Some(candidate) = promotable_passive_kind(anchor, component, passive_by_component)
        else {
            continue;
        };
        let rendered =
            render_component_symbol("passive-pin-check", component, schematic_lib_symbols)
                .with_context(|| {
                    format!(
                        "Failed to verify passive pin numbers for {}",
                        component.netlist.refdes
                    )
                })?;
        if has_standard_promoted_passive_pins(&rendered.symbol) {
            candidate_by_anchor.insert(anchor.clone(), candidate);
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

        let manufacturer_dir_candidate = part_key
            .manufacturer
            .as_deref()
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
            .map(sexpr_board::footprint_name_from_fpid)
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
            desired = footprint_qualified_component_dir_name(&desired, &candidate.footprint_name);
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
    let mut expected_pins_by_anchor = BTreeMap::new();
    let mut registry_reused_entrypoints = Vec::new();
    let mut registry_ambiguous_by_refdes = BTreeMap::new();
    let mut sourcing_by_refdes = BTreeMap::new();

    for (part_key, part_dir) in part_dir_by_key {
        let Some(instances) = part_to_instances.get(&part_key) else {
            continue;
        };
        // ImportPartKey includes both library and instance-local symbol identities, so instances
        // in this group are expected to share one embedded symbol definition. Use one
        // representative for package pin metadata, then audit
        // every instance's source endpoints against its canonical physical pins below. A real net
        // on any instance also forces an electrical no_connect pin to remain externally exposed.
        let Some(component) = instances
            .iter()
            .filter_map(|a| components.get(a))
            .find(|c| c.schematic.is_some())
        else {
            anyhow::bail!(
                "Part group {} has PCB footprints but no schematic symbol instances",
                part_dir.component_dir
            );
        };

        let out_dir = match &part_dir.manufacturer_dir {
            Some(mfr) => components_root.join(mfr).join(&part_dir.component_dir),
            None => components_root.join(&part_dir.component_dir),
        };

        let flags = *part_flags
            .get(&part_key)
            .context("Internal error: missing per-part flags")?;

        // Render all artifacts first; only touch the filesystem if we can produce a complete
        // component package.
        let mut symbol =
            render_component_symbol(&part_dir.component_dir, component, schematic_lib_symbols)
                .with_context(|| format!("Failed to render symbol for {}", out_dir.display()))?;
        let footprint = render_component_footprint(component)
            .with_context(|| format!("Failed to render footprint for {}", out_dir.display()))?;

        // File-backed and board-instance geometry uses a colocated footprint stem;
        // bundled stdlib geometry keeps its resolvable KiCad `<lib>:<footprint>` ID.
        symbol.library_text =
            patch_symbol_footprint_property(&symbol.library_text, &footprint.symbol_property)
                .with_context(|| {
                    format!("Failed to patch symbol Footprint for {}", out_dir.display())
                })?;

        let pin_plan = build_physical_pin_plan(
            &symbol.symbol,
            instances,
            components,
            port_to_net,
            not_connected_nets,
            &endpoint_pins_by_component,
        )
        .with_context(|| {
            format!(
                "Failed to preserve physical pin connectivity for {}",
                out_dir.display()
            )
        })?;
        let ident_base = module_ident_from_component_dir(&part_dir.component_dir);
        let ident = alloc_unique_ident(&ident_base, "_", &mut used_module_idents);
        let expected_pins = pin_plan
            .bindings
            .iter()
            .map(|binding| binding.pad_number.clone())
            .collect::<BTreeSet<_>>();
        for anchor in instances {
            if expected_pins_by_anchor
                .insert(anchor.clone(), expected_pins.clone())
                .is_some()
            {
                anyhow::bail!(
                    "Duplicate expected physical-pin mapping for {}",
                    anchor.pcb_path()
                );
            }
        }

        let sourcing = derive_imported_sourcing(component, registry_lookup);
        let registry_plan = if !allow_registry_reuse || flags.any_skip_bom || flags.any_skip_pos {
            None
        } else {
            match registry_reuse::try_reuse_registry_component(
                registry_reuse::RegistryReuseRequest {
                    component,
                    instance_anchors: instances,
                    components,
                    port_to_net,
                    expected_pins: &expected_pins,
                    lookup: registry_lookup,
                },
                &mut registry_context,
                writer,
            )? {
                registry_reuse::RegistryReuseOutcome::Reused(plan) => Some(plan),
                registry_reuse::RegistryReuseOutcome::Ambiguous(count) => {
                    record_ambiguous_entrypoints(
                        &mut registry_ambiguous_by_refdes,
                        instances,
                        components,
                        count,
                    );
                    None
                }
                registry_reuse::RegistryReuseOutcome::NoMatch => None,
            }
        };

        let registry_was_reused = registry_plan.is_some();
        let (module_path, io_pins, skip_defaults, component_name) =
            if let Some(plan) = registry_plan {
                registry_reused_entrypoints.push(plan.staged_entrypoint);
                (
                    plan.module_path,
                    plan.io_pins,
                    ModuleSkipDefaults {
                        include_skip_bom: false,
                        skip_bom_default: false,
                        include_skip_pos: false,
                        skip_pos_default: false,
                    },
                    plan.component_name,
                )
            } else {
                let unresolved_footprint = component.layout.as_ref().and_then(|layout| {
                    layout.unresolved_footprint.as_ref().map(|unresolved| {
                        unresolved
                            .source_id
                            .as_deref()
                            .unwrap_or("<missing footprint>")
                    })
                });
                let zen = render_component_zen(
                    &part_dir.component_dir,
                    &symbol.filename,
                    flags,
                    &pin_plan,
                    &sourcing,
                    unresolved_footprint,
                )
                .with_context(|| format!("Failed to render .zen for {}", out_dir.display()))?;

                let sym_path = out_dir.join(&symbol.filename);
                writer
                    .write(&sym_path, symbol.library_text.as_bytes())
                    .with_context(|| format!("Failed to write {}", sym_path.display()))?;
                if let Some((filename, mod_text)) = &footprint.local_file {
                    let fp_path = out_dir.join(filename);
                    writer
                        .write(&fp_path, mod_text.as_bytes())
                        .with_context(|| format!("Failed to write {}", fp_path.display()))?;
                    if writer.was_kept(&fp_path) {
                        warn_kept_footprint_geometry(&fp_path, mod_text);
                    }
                }
                let zen_path = out_dir.join(&zen.filename);
                writer
                    .write_zen(&zen_path, &zen.zen_text)
                    .with_context(|| format!("Failed to write {}", zen_path.display()))?;

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
                let io_pins = pin_plan
                    .io_pins
                    .iter()
                    .map(|(io, pin)| (io.clone(), BTreeSet::from([pin.clone()])))
                    .collect();
                (
                    module_path,
                    io_pins,
                    ModuleSkipDefaults::from(flags),
                    component_gen::sanitize_mpn_for_path(&part_dir.component_dir),
                )
            };

        if module_io_pins.insert(ident.clone(), io_pins).is_some() {
            anyhow::bail!("Duplicate module IO mapping for {ident}");
        }
        if module_skip_defaults
            .insert(ident.clone(), skip_defaults)
            .is_some()
        {
            anyhow::bail!("Duplicate module skip defaults for {ident}");
        }

        for anchor in instances {
            let Some(instance) = components.get(anchor) else {
                continue;
            };
            let (_, skip_bom, _) = derive_import_instance_flags(instance);
            let sourcing_status = if skip_bom {
                ImportGeneratedSourcingStatus::Excluded
            } else if registry_was_reused {
                ImportGeneratedSourcingStatus::RegistryModuleReused
            } else if sourcing.mpn.is_some() && sourcing.manufacturer.is_some() {
                if sourcing.manufacturer_from_registry {
                    ImportGeneratedSourcingStatus::RegistryMetadataEnriched
                } else {
                    ImportGeneratedSourcingStatus::SourcePart
                }
            } else {
                ImportGeneratedSourcingStatus::Incomplete
            };
            sourcing_by_refdes.insert(instance.netlist.refdes.clone(), sourcing_status);

            if anchor_to_module_ident
                .insert(anchor.clone(), ident.clone())
                .is_some()
            {
                anyhow::bail!(
                    "Duplicate component instance mapping for {}",
                    anchor.pcb_path()
                );
            }
            if anchor_to_component_name
                .insert(anchor.clone(), component_name.clone())
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
        if let Some(component) = components.get(&anchor) {
            let (_, skip_bom, _) = derive_import_instance_flags(component);
            sourcing_by_refdes.insert(
                component.netlist.refdes.clone(),
                if skip_bom {
                    ImportGeneratedSourcingStatus::Excluded
                } else {
                    ImportGeneratedSourcingStatus::Parametric
                },
            );
        }
        expected_pins_by_anchor.insert(
            anchor.clone(),
            BTreeSet::from([
                KiCadPinNumber::from("1".to_string()),
                KiCadPinNumber::from("2".to_string()),
            ]),
        );
        anchor_to_config_args.insert(anchor, passive.config_args);
    }

    Ok(GeneratedComponents {
        module_decls: module_decls.into_iter().collect(),
        anchor_to_module_ident,
        anchor_to_component_name,
        anchor_to_config_args,
        module_io_pins,
        module_skip_defaults,
        expected_pins_by_anchor,
        registry_reused_entrypoints,
        registry_ambiguous_by_refdes,
        sourcing_by_refdes,
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
    let props = component.best_properties();

    let mpn = registry_lookup::explicit_mpn(component).map(str::to_string);
    let manufacturer = registry_lookup::explicit_manufacturer(component).map(str::to_string);

    let footprint = component
        .netlist
        .footprint
        .clone()
        .or_else(|| component.layout.as_ref().and_then(|l| l.fpid.clone()));

    let lib_id = component
        .schematic
        .as_ref()
        .and_then(|s| s.units.values().find_map(|u| u.lib_id.clone()));
    let lib_name = component.schematic.as_ref().and_then(|schematic| {
        schematic.units.values().find_map(|unit| {
            unit.lib_name
                .as_ref()
                .map(|name| KiCadLibId::from(name.clone()))
        })
    });

    let value = component
        .netlist
        .value
        .clone()
        .or_else(|| props.and_then(|p| p.get("Value")).cloned())
        .or_else(|| props.and_then(|p| p.get("Val")).cloned());

    ImportPartKey {
        mpn,
        manufacturer,
        footprint,
        lib_id,
        lib_name,
        value,
    }
}

/// Say so when a kept footprint's copper differs from what the KiCad source now describes.
///
/// A kept `.kicad_mod` is reported like any other kept file, but that report says only "your version was
/// kept" — which is unremarkable for a `.zen` and much less so here, because the board will be laid out
/// against the land pattern in the kept file rather than the one in the schematic. Reuse validation
/// cannot catch it: it loads `.zen` entrypoints and checks connectivity, and copper is neither.
///
/// A difference is warned about rather than refused. Hand-tuning a footprint's copper is legitimate, so
/// the useful thing is to name the file whose geometry no longer matches the source, not to block the
/// import. Geometry that cannot be compared is passed over silently — refusing to warn is better than
/// warning on every footprint carrying a custom pad.
fn warn_kept_footprint_geometry(path: &Path, generated: &str) {
    let Ok(existing) = fs::read_to_string(path) else {
        return;
    };
    if footprint_identity::same_land_pattern(&existing, generated).unwrap_or(true) {
        return;
    }
    eprintln!(
        "Kept footprint {} has a different land pattern than the KiCad source; the board will be laid out against the kept geometry. Re-import with --force to replace it",
        path.display()
    );
}

fn derive_part_name(part_key: &ImportPartKey, component: &ImportComponentData) -> String {
    let raw = part_key
        .mpn
        .as_deref()
        .or(part_key.value.as_deref())
        .unwrap_or(component.netlist.refdes.as_str());
    sanitize_component_dir_name(raw)
}

/// Disambiguate two parts that share a component directory name by appending their footprint.
///
/// The result must remain a valid sanitized component name, because it becomes the directory name,
/// the generated `.zen` filename, and the `Component(name=...)` value.
fn footprint_qualified_component_dir_name(base: &str, footprint_name: &str) -> String {
    sanitize_component_dir_name(&format!("{base}__{footprint_name}"))
}

pub(super) fn sanitize_component_dir_name(raw: &str) -> String {
    // Reuse the strict, shared sanitizer used by `pcb search` component generation.
    // This keeps import outputs consistent and ensures names are compatible with
    // Zener `Component(name=...)` validation rules.
    let mut out = component_gen::sanitize_mpn_for_path(raw);
    if out.len() > 100 {
        out.truncate(100);
    }
    out
}

#[derive(Debug, Clone)]
struct RenderedComponentSymbol {
    filename: String,
    library_text: String,
    symbol: pcb_eda::Symbol,
}

fn render_component_symbol(
    component_name: &str,
    component: &ImportComponentData,
    schematic_lib_symbols: &BTreeMap<KiCadLibId, String>,
) -> Result<RenderedComponentSymbol> {
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
        anyhow::bail!(
            "Missing schematic lib_id/lib_name for {}",
            component.netlist.refdes.as_str()
        );
    };

    let Some(sym) = schematic_lib_symbols.get(&lib_id) else {
        anyhow::bail!(
            "Missing embedded lib_symbol {} for {}",
            lib_id.as_str(),
            component.netlist.refdes.as_str()
        );
    };

    let library_text = pcb_eda::kicad::symbol_library::wrap_symbol_as_library(sym, "pcb import");
    let parsed = pcb_eda::SymbolLibrary::from_string(&library_text, "kicad_sym")
        .context("Failed to parse embedded KiCad symbol as a symbol library")?;
    let symbol = parsed
        .first_symbol()
        .context("Embedded symbol library contained no symbols")?
        .clone();

    Ok(RenderedComponentSymbol {
        filename: format!("{component_name}.kicad_sym"),
        library_text,
        symbol,
    })
}

fn patch_symbol_footprint_property(library_text: &str, footprint_stem: &str) -> Result<String> {
    let mut parsed = pcb_sexpr::parse(library_text).map_err(|e| anyhow::anyhow!(e))?;
    let root = kicad_symbol_lib_items_mut(&mut parsed).context("Not a KiCad symbol library")?;
    let names = symbol_names(root);
    anyhow::ensure!(!names.is_empty(), "Symbol library contains no symbols");
    let idx =
        pcb_sexpr::kicad::symbol::find_symbol_index(root, &names[0]).context("Symbol not found")?;
    let symbol_items = root[idx]
        .as_list_mut()
        .context("Invalid symbol structure")?;
    let mut props = symbol_properties(symbol_items);
    props.insert("Footprint".to_string(), footprint_stem.to_string());
    rewrite_symbol_properties(symbol_items, &props);
    Ok(format_tree(&parsed, FormatMode::Normal))
}

#[derive(Debug, Clone)]
struct RenderedComponentFootprint {
    symbol_property: String,
    local_file: Option<(String, String)>,
}

fn render_component_footprint(
    component: &ImportComponentData,
) -> Result<RenderedComponentFootprint> {
    let Some(layout) = &component.layout else {
        anyhow::bail!(
            "Missing resolved footprint for {}",
            component.netlist.refdes.as_str()
        );
    };

    if matches!(
        &layout.footprint_geometry,
        ImportFootprintGeometry::Unresolved
    ) {
        return Ok(RenderedComponentFootprint {
            // Remove the unresolved KiCad library reference from the copied symbol so the
            // generated Zener remains portable and evaluable without inventing geometry.
            symbol_property: String::new(),
            local_file: None,
        });
    }

    let fpid = layout
        .fpid
        .as_deref()
        .or(component.netlist.footprint.as_deref())
        .context("Resolved footprint is missing its KiCad footprint ID")?;

    if matches!(
        &layout.footprint_geometry,
        ImportFootprintGeometry::StandardLibrary
    ) {
        return Ok(RenderedComponentFootprint {
            symbol_property: fpid.to_string(),
            local_file: None,
        });
    }

    let fp_name = sanitize_component_dir_name(&sexpr_board::footprint_name_from_fpid(fpid));
    let filename = format!("{fp_name}.kicad_mod");
    let mod_text = match &layout.footprint_geometry {
        ImportFootprintGeometry::BoardInstance(sexpr) => {
            sexpr_board::transform_board_instance_footprint_to_standalone(sexpr)
                .map_err(|e| anyhow::anyhow!(e))
                .with_context(|| {
                    format!(
                        "Failed to transform footprint {} for {}",
                        fpid,
                        component.netlist.refdes.as_str()
                    )
                })?
        }
        ImportFootprintGeometry::LibraryFile(sexpr) => sexpr.clone(),
        ImportFootprintGeometry::StandardLibrary | ImportFootprintGeometry::Unresolved => {
            unreachable!()
        }
    };

    Ok(RenderedComponentFootprint {
        symbol_property: fp_name,
        local_file: Some((filename, mod_text)),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportComponentPinBinding {
    logical_name: String,
    pad_number: KiCadPinNumber,
    io_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalPinPlan {
    bindings: Vec<ImportComponentPinBinding>,
    io_pins: BTreeMap<String, KiCadPinNumber>,
}

#[derive(Debug, Default)]
struct PhysicalPinTypeMetadata {
    saw_pin_type: bool,
    saw_non_no_connect: bool,
}

fn update_physical_pin_type_metadata(metadata: &mut PhysicalPinTypeMetadata, pin: &pcb_eda::Pin) {
    for pin_type in pin.electrical_type.as_deref().into_iter().chain(
        pin.alternates
            .iter()
            .filter_map(|alternate| alternate.electrical_type.as_deref()),
    ) {
        metadata.saw_pin_type = true;
        metadata.saw_non_no_connect |= pin_type != "no_connect";
    }
}

fn sanitize_pin_number_suffix(raw: &str) -> String {
    let mut out = String::new();
    let mut previous_underscore = false;
    for c in raw.trim().chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if previous_underscore {
                continue;
            }
            previous_underscore = true;
        } else {
            previous_underscore = false;
        }
        out.push(mapped);
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "PIN".to_string()
    } else {
        out.to_string()
    }
}

fn component_port_keys(
    anchor: &KiCadUuidPathKey,
    component: &ImportComponentData,
) -> BTreeSet<KiCadUuidPathKey> {
    std::iter::once(anchor.clone())
        .chain(component.netlist.unit_pcb_paths.iter().cloned())
        .collect()
}

pub(super) fn resolve_instance_pin_net(
    anchor: &KiCadUuidPathKey,
    component: &ImportComponentData,
    pin: &KiCadPinNumber,
    port_to_net: &BTreeMap<ImportNetPort, KiCadNetName>,
) -> Result<Option<KiCadNetName>> {
    let mut nets = BTreeSet::new();
    for key in component_port_keys(anchor, component) {
        let port = ImportNetPort {
            component: key,
            pin: pin.clone(),
        };
        if let Some(net) = port_to_net.get(&port) {
            nets.insert(net.clone());
        }
    }
    if nets.len() > 1 {
        anyhow::bail!(
            "KiCad component {} pin {} resolves to multiple nets: {}",
            component.netlist.refdes,
            pin,
            nets.iter()
                .map(KiCadNetName::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(nets.into_iter().next())
}

fn build_physical_pin_plan(
    symbol: &pcb_eda::Symbol,
    instances: &[KiCadUuidPathKey],
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    port_to_net: &BTreeMap<ImportNetPort, KiCadNetName>,
    not_connected_nets: &BTreeSet<KiCadNetName>,
    endpoint_pins_by_component: &BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadPinNumber>>,
) -> Result<PhysicalPinPlan> {
    #[derive(Debug)]
    struct PinInfo {
        number: KiCadPinNumber,
        raw_name: String,
        exposed: bool,
    }

    let mut pin_types: BTreeMap<KiCadPinNumber, PhysicalPinTypeMetadata> = BTreeMap::new();
    for pin in &symbol.pins {
        update_physical_pin_type_metadata(
            pin_types
                .entry(KiCadPinNumber::from(pin.number.clone()))
                .or_default(),
            pin,
        );
    }

    let mut canonical_pins: Vec<&pcb_eda::Pin> = symbol.canonical_pins().collect();
    canonical_pins.sort_by(|left, right| {
        left.number
            .cmp(&right.number)
            .then_with(|| left.signal_name().cmp(right.signal_name()))
    });

    let mut raw_name_counts: BTreeMap<String, usize> = BTreeMap::new();
    for pin in &canonical_pins {
        *raw_name_counts
            .entry(pin.signal_name().to_string())
            .or_default() += 1;
    }

    let mut pins = Vec::with_capacity(canonical_pins.len());
    for pin in canonical_pins {
        let number = KiCadPinNumber::from(pin.number.clone());
        let metadata = pin_types.get(&number);
        let only_no_connect =
            metadata.is_some_and(|metadata| metadata.saw_pin_type && !metadata.saw_non_no_connect);
        let mut has_real_connection = false;
        for anchor in instances {
            let component = components.get(anchor).with_context(|| {
                format!(
                    "Missing component instance {} while planning pins",
                    anchor.pcb_path()
                )
            })?;
            if let Some(net) = resolve_instance_pin_net(anchor, component, &number, port_to_net)?
                && !not_connected_nets.contains(&net)
            {
                has_real_connection = true;
            }
        }
        pins.push(PinInfo {
            number,
            raw_name: pin.signal_name().to_string(),
            exposed: !only_no_connect || has_real_connection,
        });
    }

    // Reserve names originating from unique KiCad signal names before allocating names derived
    // from duplicate signals. This keeps a generated suffix from stealing a real source name.
    let mut used_logical_names: BTreeSet<String> = raw_name_counts
        .iter()
        .filter(|(_, count)| **count == 1)
        .map(|(name, _)| name.clone())
        .collect();
    let mut logical_names: BTreeMap<KiCadPinNumber, String> = BTreeMap::new();
    for pin in &pins {
        let logical_name = if raw_name_counts.get(&pin.raw_name) == Some(&1) {
            pin.raw_name.clone()
        } else {
            alloc_unique_ident(
                &format!("{}__{}", pin.raw_name, pin.number.as_str()),
                "__",
                &mut used_logical_names,
            )
        };
        logical_names.insert(pin.number.clone(), logical_name);
    }

    // Allocate unique-pin IO names first, then duplicate-derived names in the same shared
    // namespace. The pin-number suffix deliberately has no leading-digit guard: it follows an
    // already-valid identifier, so D+ pin 3 becomes D_POS_3 rather than D_POS_P3.
    let mut used_io_names = BTreeSet::new();
    let mut io_names: BTreeMap<KiCadPinNumber, String> = BTreeMap::new();
    for duplicate_group in [false, true] {
        for pin in &pins {
            if !pin.exposed {
                continue;
            }
            let is_duplicate = raw_name_counts.get(&pin.raw_name).copied().unwrap_or(0) > 1;
            if is_duplicate != duplicate_group {
                continue;
            }
            let mut base = component_gen::sanitize_pin_name(&pin.raw_name);
            if base.is_empty() {
                base = "PIN".to_string();
            }
            if is_duplicate {
                base.push('_');
                base.push_str(&sanitize_pin_number_suffix(pin.number.as_str()));
            }
            io_names.insert(
                pin.number.clone(),
                alloc_unique_ident(&base, "_", &mut used_io_names),
            );
        }
    }

    let mut bindings = Vec::with_capacity(pins.len());
    let mut io_pins = BTreeMap::new();
    let mut all_pins = BTreeSet::new();
    for pin in pins {
        anyhow::ensure!(
            all_pins.insert(pin.number.clone()),
            "Duplicate canonical physical pin {}",
            pin.number
        );
        let io_name = io_names.get(&pin.number).cloned();
        if let Some(io_name) = &io_name {
            anyhow::ensure!(
                io_pins
                    .insert(io_name.clone(), pin.number.clone())
                    .is_none(),
                "Duplicate generated IO name {io_name}"
            );
        }
        let logical_name = logical_names
            .remove(&pin.number)
            .context("Missing generated logical pin name")?;
        bindings.push(ImportComponentPinBinding {
            logical_name,
            pad_number: pin.number,
            io_name,
        });
    }

    // Every source endpoint for every instance in the package must refer to a physical pin in
    // the representative symbol. This catches unit or library divergence before files are written.
    for anchor in instances {
        let component = components.get(anchor).with_context(|| {
            format!(
                "Missing component instance {} while auditing pins",
                anchor.pcb_path()
            )
        })?;
        for key in component_port_keys(anchor, component) {
            let Some(endpoint_pins) = endpoint_pins_by_component.get(&key) else {
                continue;
            };
            for pin in endpoint_pins {
                if !all_pins.contains(pin) {
                    anyhow::bail!(
                        "KiCad component {} has netlist endpoint on pin {}, but its embedded symbol does not define that pin",
                        component.netlist.refdes,
                        pin
                    );
                }
            }
        }
    }

    Ok(PhysicalPinPlan { bindings, io_pins })
}

#[derive(Debug, Clone)]
/// The rendered component package files. The IO-name-to-pad mapping is *not* here: the caller owns
/// the [`PhysicalPinPlan`] that decided it, and rendering only consumes that decision.
struct RenderedComponentZen {
    filename: String,
    zen_text: String,
}

#[derive(Debug, Clone)]
struct ImportedSourcing {
    mpn: Option<String>,
    manufacturer: Option<String>,
    manufacturer_from_registry: bool,
}

fn derive_imported_sourcing(
    component: &ImportComponentData,
    registry_lookup: &ImportRegistryMpnLookup,
) -> ImportedSourcing {
    let mpn = registry_lookup::explicit_mpn(component).map(str::to_string);
    let explicit_manufacturer =
        registry_lookup::explicit_manufacturer(component).map(str::to_string);
    if explicit_manufacturer.is_some() || mpn.is_none() {
        return ImportedSourcing {
            mpn,
            manufacturer: explicit_manufacturer,
            manufacturer_from_registry: false,
        };
    }

    // An exact MPN is already source design intent. It is safe to fill its missing manufacturer
    // only when every cached exact-MPN record that names one agrees. Module compatibility is not
    // required for this metadata-only enrichment.
    let normalized = pcb_diode_api::normalize_mpn_for_lookup(mpn.as_deref().unwrap_or_default());
    // Keyed by folding key so spelling variants of one manufacturer agree; the mapped value is the
    // first-seen original spelling and is the only form ever written to generated output.
    let mut spelling_by_manufacturer_key: BTreeMap<String, String> = BTreeMap::new();
    for candidate in registry_lookup
        .candidates_by_mpn
        .iter()
        .filter(|(candidate_mpn, _)| {
            pcb_diode_api::normalize_mpn_for_lookup(candidate_mpn) == normalized
        })
        .flat_map(|(_, candidates)| candidates)
    {
        let key = registry_lookup::manufacturer_key(&candidate.manufacturer);
        if key.is_empty() {
            continue;
        }
        spelling_by_manufacturer_key
            .entry(key)
            .or_insert_with(|| candidate.manufacturer.trim().to_string());
    }
    let manufacturer = (spelling_by_manufacturer_key.len() == 1)
        .then(|| spelling_by_manufacturer_key.into_values().next())
        .flatten();

    ImportedSourcing {
        mpn,
        manufacturer_from_registry: manufacturer.is_some(),
        manufacturer,
    }
}

fn render_component_zen(
    component_name: &str,
    symbol_filename: &str,
    flags: ImportPartFlags,
    pin_plan: &PhysicalPinPlan,
    sourcing: &ImportedSourcing,
    unresolved_footprint: Option<&str>,
) -> Result<RenderedComponentZen> {
    // `component_name` is the already-sanitized, collision-resolved filesystem segment. Preserve
    // it exactly so the rendered filename matches the module path emitted by the caller.
    let string = crate::codegen::starlark::string;
    let mut zen =
        format!("\"\"\"\n{component_name}\n\nAuto-generated using `pcb import`.\n\"\"\"\n\n");
    if flags.any_skip_bom {
        zen.push_str(&format!(
            "skip_bom = config(bool, default = {})\n",
            crate::codegen::starlark::bool(flags.all_skip_bom)
        ));
    }
    if flags.any_skip_pos {
        zen.push_str(&format!(
            "skip_pos = config(bool, default = {})\n",
            crate::codegen::starlark::bool(flags.all_skip_pos)
        ));
    }
    if flags.any_skip_bom || flags.any_skip_pos {
        zen.push('\n');
    }
    for binding in &pin_plan.bindings {
        if let Some(io_name) = &binding.io_name {
            zen.push_str(&format!("{io_name} = io(Net)\n"));
        }
    }
    zen.push_str("Component(\n");
    zen.push_str(&format!("    name = {},\n", string(component_name)));
    match (&sourcing.mpn, &sourcing.manufacturer) {
        (Some(mpn), Some(manufacturer)) => {
            zen.push_str(&format!(
                "    part = Part(mpn = {}, manufacturer = {}),\n",
                string(mpn),
                string(manufacturer)
            ));
            if sourcing.manufacturer_from_registry {
                zen.push_str("    # Manufacturer enriched from unanimous cached exact-MPN registry metadata.\n");
            }
        }
        (Some(mpn), None) => {
            zen.push_str(&format!("    mpn = {},\n", string(mpn)));
        }
        (None, _) => {}
    }
    let sourcing_is_incomplete =
        !flags.all_skip_bom && (sourcing.mpn.is_none() || sourcing.manufacturer.is_none());
    if sourcing_is_incomplete {
        zen.push_str("    # Source schematic did not provide complete sourcing; preserve this for agent follow-up.\n");
    }
    if unresolved_footprint.is_some() {
        zen.push_str("    # Source footprint geometry was unavailable; retain its provenance for agent follow-up.\n");
    }
    if sourcing_is_incomplete || unresolved_footprint.is_some() {
        zen.push_str("    properties = {\n");
        if sourcing_is_incomplete {
            zen.push_str("        \"__imported_bom_source_incomplete\": True,\n");
        }
        if let Some(footprint) = unresolved_footprint {
            zen.push_str(&format!(
                "        {}: {},\n",
                string(IMPORT_UNRESOLVED_FOOTPRINT_PROPERTY),
                string(footprint)
            ));
        }
        zen.push_str("    },\n");
    }
    if flags.any_skip_bom {
        zen.push_str("    skip_bom = skip_bom,\n");
    }
    if flags.any_skip_pos {
        zen.push_str("    skip_pos = skip_pos,\n");
    }
    if let Some(footprint) = unresolved_footprint {
        // An explicit unresolved KiCad ID bypasses local geometry inference while keeping the
        // original design intent visible. Layout remains intentionally incomplete until replaced.
        zen.push_str(&format!("    footprint = {},\n", string(footprint)));
    }
    zen.push_str(&format!(
        "    symbol = Symbol(library = {}),\n",
        string(symbol_filename)
    ));
    zen.push_str("    pin_defs = {\n");
    for binding in &pin_plan.bindings {
        zen.push_str(&format!(
            "        {}: {},\n",
            string(&binding.logical_name),
            string(binding.pad_number.as_str())
        ));
    }
    zen.push_str("    },\n    pins = {\n");
    for binding in &pin_plan.bindings {
        if let Some(io_name) = &binding.io_name {
            zen.push_str(&format!(
                "        {}: {io_name},\n",
                string(&binding.logical_name)
            ));
        }
    }
    zen.push_str("    },\n)\n");

    Ok(RenderedComponentZen {
        filename: format!("{component_name}.zen"),
        zen_text: zen,
    })
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

        for (io_name, pins) in io_pins {
            let resolved = pins
                .iter()
                .map(|pin| resolve_instance_pin_net(anchor, component, pin, port_to_net))
                .collect::<Result<Vec<_>>>()?;
            let connected_nets = resolved.iter().flatten().cloned().collect::<BTreeSet<_>>();
            if connected_nets.len() > 1
                || (pins.len() > 1
                    && !connected_nets.is_empty()
                    && resolved.iter().any(Option::is_none))
            {
                anyhow::bail!(
                    "Registry module IO {io_name} groups physical pins with different KiCad connectivity on {}",
                    component.netlist.refdes.as_str()
                );
            }
            let connected = connected_nets.into_iter().next();
            let net_ident = match connected {
                Some(net) if !not_connected_nets.contains(&net) => net_ident_by_kicad_name
                    .get(&net)
                    .cloned()
                    .with_context(|| {
                        format!("Missing net identifier for KiCad net {}", net.as_str())
                    })?,
                // KiCad omits pins from unplaced symbol units. They still exist on the physical
                // package and must remain independent open terminals in generated Zener.
                Some(_) | None => "NotConnected()".to_string(),
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
        let name = alloc_unique_ident(&base, "_", &mut used);
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

/// `base` if it is unused, otherwise `base` with `separator` and the lowest free ordinal appended.
///
/// `separator` distinguishes the two namespaces this allocates in: Zener identifiers disambiguate
/// with `_`, while logical signal names use `__` so a suffix cannot collide with a pin name that
/// already contains an underscore.
fn alloc_unique_ident(base: &str, separator: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut ordinal = 2usize;
    loop {
        let candidate = format!("{base}{separator}{ordinal}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        ordinal += 1;
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

#[cfg(test)]
mod tests {
    use super::schematic_types::{ImportSchematicPositionComment, ImportSchematicTargetKind};
    use super::*;
    use std::path::PathBuf;

    fn make_anchor(symbol_uuid: &str) -> KiCadUuidPathKey {
        KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: symbol_uuid.to_string(),
        }
    }

    fn make_unit(unit: Option<i64>, at: Option<ImportSchematicAt>) -> ImportSchematicUnit {
        ImportSchematicUnit {
            lib_name: None,
            lib_id: None,
            unit,
            at,
            mirror: None,
            in_bom: None,
            on_board: None,
            dnp: None,
            exclude_from_sim: None,
            instance_path: None,
            properties: BTreeMap::new(),
            pins: None,
        }
    }

    fn make_component(
        refdes: &str,
        units: BTreeMap<KiCadUuidPathKey, ImportSchematicUnit>,
    ) -> ImportComponentData {
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from(refdes.to_string()),
                value: None,
                footprint: None,
                sheetpath_names: None,
                unit_pcb_paths: Vec::new(),
            },
            schematic: Some(ImportSchematicComponent { units }),
            layout: None,
        }
    }

    fn make_pin(name: &str, number: &str, electrical_type: Option<&str>) -> pcb_eda::Pin {
        pcb_eda::Pin {
            name: name.to_string(),
            number: number.to_string(),
            electrical_type: electrical_type.map(str::to_string),
            ..Default::default()
        }
    }

    fn make_generated_components(
        anchor_to_component_name: BTreeMap<KiCadUuidPathKey, String>,
    ) -> GeneratedComponents {
        GeneratedComponents {
            module_decls: Vec::new(),
            anchor_to_module_ident: BTreeMap::new(),
            anchor_to_component_name,
            anchor_to_config_args: BTreeMap::new(),
            module_io_pins: BTreeMap::new(),
            module_skip_defaults: BTreeMap::new(),
            expected_pins_by_anchor: BTreeMap::new(),
            registry_reused_entrypoints: Vec::new(),
            registry_ambiguous_by_refdes: BTreeMap::new(),
            sourcing_by_refdes: BTreeMap::new(),
        }
    }

    /// Ambiguity is discovered once per part group, but the report is per reference designator: every
    /// instance sharing the ambiguous part must get an entry, not just the representative.
    #[test]
    fn ambiguous_entrypoints_are_recorded_for_every_instance() {
        let instances = [make_anchor("r1"), make_anchor("r2"), make_anchor("missing")];
        let components = BTreeMap::from([
            (instances[0].clone(), make_component("R1", BTreeMap::new())),
            (instances[1].clone(), make_component("R2", BTreeMap::new())),
        ]);

        let mut ambiguous = BTreeMap::new();
        record_ambiguous_entrypoints(&mut ambiguous, &instances, &components, 2);

        assert_eq!(
            ambiguous,
            BTreeMap::from([
                (KiCadRefDes::from("R1".to_string()), 2),
                (KiCadRefDes::from("R2".to_string()), 2),
            ])
        );
    }

    #[test]
    fn physical_pin_plan_keeps_duplicate_display_names_independent() {
        let symbol = pcb_eda::Symbol {
            pins: vec![
                make_pin("D+", "3", Some("bidirectional")),
                make_pin("D+", "4", Some("bidirectional")),
                make_pin("D_POS_3", "5", Some("input")),
                make_pin("A+B", "6", Some("input")),
                make_pin("A-B", "7", Some("input")),
                make_pin("NC", "10", Some("no_connect")),
                make_pin("NC", "11", Some("no_connect")),
            ],
            ..Default::default()
        };

        let plan = build_physical_pin_plan(
            &symbol,
            &[],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        let by_pad: BTreeMap<_, _> = plan
            .bindings
            .iter()
            .map(|binding| (binding.pad_number.as_str(), binding))
            .collect();

        assert_eq!(by_pad["3"].logical_name, "D+__3");
        assert_eq!(by_pad["4"].logical_name, "D+__4");
        assert_eq!(by_pad["3"].io_name.as_deref(), Some("D_POS_3_2"));
        assert_eq!(by_pad["4"].io_name.as_deref(), Some("D_POS_4"));
        assert_eq!(by_pad["5"].io_name.as_deref(), Some("D_POS_3"));
        assert_eq!(by_pad["6"].io_name.as_deref(), Some("A_B"));
        assert_eq!(by_pad["7"].io_name.as_deref(), Some("A_B_2"));
        assert_eq!(by_pad["10"].logical_name, "NC__10");
        assert_eq!(by_pad["11"].logical_name, "NC__11");
        assert_eq!(by_pad["10"].io_name, None);
        assert_eq!(by_pad["11"].io_name, None);
    }
    #[test]
    fn connected_electrical_no_connect_pin_is_exposed() {
        let anchor = make_anchor("u1");
        let component = make_component("U1", BTreeMap::new());
        let components = BTreeMap::from([(anchor.clone(), component)]);
        let port_to_net = BTreeMap::from([(
            ImportNetPort {
                component: anchor.clone(),
                pin: KiCadPinNumber::from("10".to_string()),
            },
            KiCadNetName::from("REAL".to_string()),
        )]);
        let symbol = pcb_eda::Symbol {
            pins: vec![make_pin("NC", "10", Some("no_connect"))],
            ..Default::default()
        };

        let endpoint_pins = BTreeMap::from([(
            anchor.clone(),
            BTreeSet::from([KiCadPinNumber::from("10".to_string())]),
        )]);
        let plan = build_physical_pin_plan(
            &symbol,
            std::slice::from_ref(&anchor),
            &components,
            &port_to_net,
            &BTreeSet::new(),
            &endpoint_pins,
        )
        .unwrap();
        assert_eq!(plan.bindings[0].io_name.as_deref(), Some("NC"));
        assert_eq!(plan.io_pins["NC"].as_str(), "10");
    }

    #[test]
    fn imported_instance_maps_each_physical_pin_and_leaves_absent_pins_open() {
        let anchor = make_anchor("u1");
        let mut component = make_component("U1", BTreeMap::new());
        component.netlist.unit_pcb_paths = vec![anchor.clone()];
        let components = BTreeMap::from([(anchor.clone(), component)]);
        let port_to_net = BTreeMap::from([
            (
                ImportNetPort {
                    component: anchor.clone(),
                    pin: KiCadPinNumber::from("3".to_string()),
                },
                KiCadNetName::from("A".to_string()),
            ),
            (
                ImportNetPort {
                    component: anchor.clone(),
                    pin: KiCadPinNumber::from("4".to_string()),
                },
                KiCadNetName::from("A".to_string()),
            ),
        ]);
        let symbol = pcb_eda::Symbol {
            pins: vec![
                make_pin("D+", "3", Some("bidirectional")),
                make_pin("D+", "4", Some("bidirectional")),
                make_pin("UNPLACED", "5", Some("input")),
            ],
            ..Default::default()
        };
        let plan = build_physical_pin_plan(
            &symbol,
            std::slice::from_ref(&anchor),
            &components,
            &port_to_net,
            &BTreeSet::new(),
            &BTreeMap::from([(
                anchor.clone(),
                BTreeSet::from([
                    KiCadPinNumber::from("3".to_string()),
                    KiCadPinNumber::from("4".to_string()),
                ]),
            )]),
        )
        .unwrap();
        let mut generated = make_generated_components(BTreeMap::new());
        generated
            .anchor_to_module_ident
            .insert(anchor.clone(), "DEVICE".to_string());
        generated.module_io_pins.insert(
            "DEVICE".to_string(),
            plan.io_pins
                .into_iter()
                .map(|(io, pin)| (io, BTreeSet::from([pin])))
                .collect(),
        );
        generated.module_skip_defaults.insert(
            "DEVICE".to_string(),
            ModuleSkipDefaults::from(ImportPartFlags::default()),
        );
        let refs = BTreeMap::from([(KiCadRefDes::from("U1".to_string()), "U1".to_string())]);
        let net_idents =
            BTreeMap::from([(KiCadNetName::from("A".to_string()), "NET_A".to_string())]);

        let calls = build_imported_instance_calls_for_instances(
            vec![(&anchor, &components[&anchor])],
            &port_to_net,
            &refs,
            &net_idents,
            &generated,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].io_nets["D_POS_3"], "NET_A");
        assert_eq!(calls[0].io_nets["D_POS_4"], "NET_A");
        assert_eq!(calls[0].io_nets["UNPLACED"], "NotConnected()");
    }

    #[test]
    fn source_endpoint_missing_from_symbol_is_an_error() {
        let anchor = make_anchor("u1");
        let component = make_component("U1", BTreeMap::new());
        let components = BTreeMap::from([(anchor.clone(), component)]);
        let port_to_net = BTreeMap::from([(
            ImportNetPort {
                component: anchor.clone(),
                pin: KiCadPinNumber::from("9".to_string()),
            },
            KiCadNetName::from("A".to_string()),
        )]);
        let symbol = pcb_eda::Symbol {
            pins: vec![make_pin("IN", "1", Some("input"))],
            ..Default::default()
        };
        let endpoint_pins = BTreeMap::from([(
            anchor.clone(),
            BTreeSet::from([KiCadPinNumber::from("9".to_string())]),
        )]);
        let error = build_physical_pin_plan(
            &symbol,
            std::slice::from_ref(&anchor),
            &components,
            &port_to_net,
            &BTreeSet::new(),
            &endpoint_pins,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not define that pin"));
    }

    #[test]
    fn inconsistent_multi_anchor_pin_connectivity_is_an_error() {
        let anchor = make_anchor("anchor");
        let unit = make_anchor("unit");
        let mut component = make_component("U1", BTreeMap::new());
        component.netlist.unit_pcb_paths = vec![unit.clone()];
        let pin = KiCadPinNumber::from("1".to_string());
        let port_to_net = BTreeMap::from([
            (
                ImportNetPort {
                    component: anchor.clone(),
                    pin: pin.clone(),
                },
                KiCadNetName::from("A".to_string()),
            ),
            (
                ImportNetPort {
                    component: unit,
                    pin: pin.clone(),
                },
                KiCadNetName::from("B".to_string()),
            ),
        ]);
        let error = resolve_instance_pin_net(&anchor, &component, &pin, &port_to_net)
            .unwrap_err()
            .to_string();
        assert!(error.contains("resolves to multiple nets"));
    }

    #[test]
    fn renderer_emits_explicit_pin_defs_and_escapes_starlark_strings() {
        let plan = PhysicalPinPlan {
            bindings: vec![
                ImportComponentPinBinding {
                    logical_name: "D+__3".to_string(),
                    pad_number: KiCadPinNumber::from("3".to_string()),
                    io_name: Some("D_POS_3".to_string()),
                },
                ImportComponentPinBinding {
                    logical_name: "NC\"\\é".to_string(),
                    pad_number: KiCadPinNumber::from("10".to_string()),
                    io_name: None,
                },
            ],
            io_pins: BTreeMap::from([(
                "D_POS_3".to_string(),
                KiCadPinNumber::from("3".to_string()),
            )]),
        };
        let rendered = render_component_zen(
            "USB_DEVICE__VARIANT",
            "USB_DEVICE.kicad_sym",
            ImportPartFlags {
                all_skip_bom: false,
                ..ImportPartFlags::default()
            },
            &plan,
            &ImportedSourcing {
                mpn: None,
                manufacturer: None,
                manufacturer_from_registry: false,
            },
            None,
        )
        .unwrap();
        assert_eq!(rendered.filename, "USB_DEVICE__VARIANT.zen");
        assert!(rendered.zen_text.contains("D_POS_3 = io(Net)"));
        assert!(rendered.zen_text.contains("\"D+__3\": \"3\""));
        assert!(rendered.zen_text.contains("\"NC\\\"\\\\é\": \"10\""));
        assert!(!rendered.zen_text.contains("NC = io(Net)"));
        assert!(
            rendered
                .zen_text
                .contains("__imported_bom_source_incomplete")
        );
    }
    #[test]
    fn renderer_preserves_unresolved_footprint_without_geometry() {
        let rendered = render_component_zen(
            "DEVICE",
            "DEVICE.kicad_sym",
            ImportPartFlags {
                all_skip_bom: false,
                ..ImportPartFlags::default()
            },
            &PhysicalPinPlan {
                bindings: Vec::new(),
                io_pins: BTreeMap::new(),
            },
            &ImportedSourcing {
                mpn: Some("ABC-123".to_string()),
                manufacturer: Some("Acme".to_string()),
                manufacturer_from_registry: false,
            },
            Some("Missing:Footprint"),
        )
        .unwrap();

        assert!(
            rendered
                .zen_text
                .contains("footprint = \"Missing:Footprint\"")
        );
        assert!(
            rendered
                .zen_text
                .contains("\"__imported_unresolved_footprint\": \"Missing:Footprint\"")
        );
        assert!(
            !rendered
                .zen_text
                .contains("__imported_bom_source_incomplete")
        );
    }

    #[test]
    fn exact_registry_metadata_can_complete_explicit_source_mpn() {
        let mut unit = make_unit(Some(1), None);
        unit.properties.insert(
            "Manufacturer_Part_Number".to_string(),
            "ABC-123".to_string(),
        );
        let component = make_component("U1", BTreeMap::from([(make_anchor("u1"), unit)]));
        let lookup = ImportRegistryMpnLookup {
            candidates_by_mpn: BTreeMap::from([(
                "ABC-123".to_string(),
                vec![ImportRegistryMpnCandidate {
                    registry_id: "registry".to_string(),
                    registry_mpn: "ABC-123".to_string(),
                    manufacturer: "Acme".to_string(),
                    footprint: "FP".to_string(),
                    module_url: "example/component".to_string(),
                    module_version: "1.0.0".to_string(),
                    entrypoints: vec![],
                    symbol_preferred: false,
                    module_preferred: false,
                }],
            )]),
            ..ImportRegistryMpnLookup::default()
        };
        let sourcing = derive_imported_sourcing(&component, &lookup);
        assert_eq!(sourcing.mpn.as_deref(), Some("ABC-123"));
        assert_eq!(sourcing.manufacturer.as_deref(), Some("Acme"));
        assert!(sourcing.manufacturer_from_registry);
    }

    #[test]
    fn registry_manufacturer_enrichment_folds_case_and_emits_an_original_spelling() {
        let mut unit = make_unit(Some(1), None);
        unit.properties.insert(
            "Manufacturer_Part_Number".to_string(),
            "ABC-123".to_string(),
        );
        let component = make_component("U1", BTreeMap::from([(make_anchor("u1"), unit)]));
        let record = |manufacturer: &str| ImportRegistryMpnCandidate {
            registry_id: "registry".to_string(),
            registry_mpn: "ABC-123".to_string(),
            manufacturer: manufacturer.to_string(),
            footprint: "FP".to_string(),
            module_url: "example/component".to_string(),
            module_version: "1.0.0".to_string(),
            entrypoints: vec![],
            symbol_preferred: false,
            module_preferred: false,
        };
        let lookup_for = |records: Vec<ImportRegistryMpnCandidate>| ImportRegistryMpnLookup {
            candidates_by_mpn: BTreeMap::from([("ABC-123".to_string(), records)]),
            ..ImportRegistryMpnLookup::default()
        };

        // Records that disagree only by case name one manufacturer, and the enriched value must be
        // an original spelling: emitting the folded key would write "acme" into generated .zen.
        let sourcing = derive_imported_sourcing(
            &component,
            &lookup_for(vec![record(" Acme "), record("ACME")]),
        );
        assert_eq!(sourcing.manufacturer.as_deref(), Some("Acme"));
        assert!(sourcing.manufacturer_from_registry);

        // Genuinely different manufacturers still block enrichment.
        let sourcing = derive_imported_sourcing(
            &component,
            &lookup_for(vec![record("Acme"), record("Other")]),
        );
        assert_eq!(sourcing.manufacturer, None);
        assert!(!sourcing.manufacturer_from_registry);
    }

    #[test]
    fn passive_promotion_requires_physical_pins_one_and_two() {
        let standard = pcb_eda::Symbol {
            pins: vec![make_pin("~", "1", None), make_pin("~", "2", None)],
            ..Default::default()
        };
        let custom = pcb_eda::Symbol {
            pins: vec![make_pin("~", "A", None), make_pin("~", "K", None)],
            ..Default::default()
        };
        assert!(has_standard_promoted_passive_pins(&standard));
        assert!(!has_standard_promoted_passive_pins(&custom));
    }

    fn make_position_comment(
        x: f64,
        y: f64,
        rot: f64,
        unit: Option<i64>,
        lib_id: Option<&str>,
        mirror: Option<&str>,
        target_kind: ImportSchematicTargetKind,
    ) -> ImportSchematicPositionComment {
        ImportSchematicPositionComment {
            at: ImportSchematicAt {
                x,
                y,
                rot: Some(rot),
            },
            unit,
            mirror: mirror.map(|m| m.to_string()),
            lib_name: None,
            lib_id: lib_id.map(|id| KiCadLibId::from(id.to_string())),
            target_kind,
        }
    }

    #[test]
    fn flat_positions_emit_per_unit_keys_for_multi_unit_components() {
        let anchor = make_anchor("anchor");
        let other = make_anchor("other");

        let mut units = BTreeMap::new();
        units.insert(
            other,
            make_unit(
                Some(5),
                Some(ImportSchematicAt {
                    x: 10.0,
                    y: 20.0,
                    rot: Some(90.0),
                }),
            ),
        );
        units.insert(
            anchor.clone(),
            make_unit(
                Some(6),
                Some(ImportSchematicAt {
                    x: 30.0,
                    y: 40.0,
                    rot: Some(180.0),
                }),
            ),
        );

        let mut component = make_component("J15", units);
        component.netlist.unit_pcb_paths = vec![make_anchor("u5"), make_anchor("u6")];

        let refs = BTreeMap::from([(KiCadRefDes::from("J15".to_string()), "J15".to_string())]);
        let generated =
            make_generated_components(BTreeMap::from([(anchor.clone(), "2309413_1".to_string())]));

        let positions =
            build_flat_component_schematic_positions(&[(&anchor, &component)], &refs, &generated);
        let pos_u5 = positions
            .get("J15.2309413_1@U5")
            .expect("missing unit-5 position");
        assert_eq!(pos_u5.at.x, 10.0);
        assert_eq!(pos_u5.at.y, 20.0);
        assert_eq!(pos_u5.at.rot, Some(90.0));

        let pos_u6 = positions
            .get("J15.2309413_1@U6")
            .expect("missing unit-6 position");
        assert_eq!(pos_u6.at.x, 30.0);
        assert_eq!(pos_u6.at.y, 40.0);
        assert_eq!(pos_u6.at.rot, Some(180.0));
    }

    #[test]
    fn flat_positions_keep_unsuffixed_key_for_single_unit_components() {
        let anchor = make_anchor("anchor");

        let mut units = BTreeMap::new();
        units.insert(
            anchor.clone(),
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 30.0,
                    y: 40.0,
                    rot: Some(180.0),
                }),
            ),
        );

        let component = make_component("R1", units);
        let refs = BTreeMap::from([(KiCadRefDes::from("R1".to_string()), "R1".to_string())]);
        let generated =
            make_generated_components(BTreeMap::from([(anchor.clone(), "R".to_string())]));

        let positions =
            build_flat_component_schematic_positions(&[(&anchor, &component)], &refs, &generated);
        assert!(positions.contains_key("R1.R"));
        assert!(!positions.contains_key("R1.R@U1"));
    }

    #[test]
    fn flat_positions_emit_power_net_symbols_with_monotonic_counters() {
        let sheet_path = KiCadSheetPath::root();

        let module_plan = ImportModuleBoundaryNets {
            sheet_name: None,
            nets_defined_here: BTreeSet::from([KiCadNetName::from("GND".to_string())]),
            nets_io_here: BTreeSet::from([KiCadNetName::from("+1V8".to_string())]),
        };

        let net_decls = ImportedNetDecls {
            decls: Vec::new(),
            var_ident_by_kicad_name: BTreeMap::from([(
                KiCadNetName::from("+1V8".to_string()),
                "NET_1V8".to_string(),
            )]),
            zener_name_by_kicad_name: BTreeMap::from([
                (KiCadNetName::from("GND".to_string()), "GND".to_string()),
                (KiCadNetName::from("+1V8".to_string()), "+1V8".to_string()),
            ]),
            kind_by_kicad_name: BTreeMap::new(),
        };

        let net_kinds_by_net = BTreeMap::from([
            (
                KiCadNetName::from("GND".to_string()),
                ImportNetKindClassification {
                    kind: ImportNetKind::Ground,
                    reasons: BTreeSet::new(),
                },
            ),
            (
                KiCadNetName::from("+1V8".to_string()),
                ImportNetKindClassification {
                    kind: ImportNetKind::Power,
                    reasons: BTreeSet::new(),
                },
            ),
            (
                KiCadNetName::from("SIG".to_string()),
                ImportNetKindClassification {
                    kind: ImportNetKind::Net,
                    reasons: BTreeSet::new(),
                },
            ),
        ]);

        let power_symbol_decls = vec![
            ImportSchematicPowerSymbolDecl {
                schematic_file: PathBuf::from("root.kicad_sch"),
                sheet_path: sheet_path.clone(),
                symbol_uuid: Some("a".to_string()),
                at: Some(ImportSchematicAt {
                    x: 1.0,
                    y: 2.0,
                    rot: Some(90.0),
                }),
                mirror: Some("x".to_string()),
                reference: Some("#PWR01".to_string()),
                lib_id: Some(KiCadLibId::from("power:GND".to_string())),
                value: Some("GND".to_string()),
            },
            ImportSchematicPowerSymbolDecl {
                schematic_file: PathBuf::from("root.kicad_sch"),
                sheet_path: sheet_path.clone(),
                symbol_uuid: Some("b".to_string()),
                at: Some(ImportSchematicAt {
                    x: 3.0,
                    y: 4.0,
                    rot: Some(0.0),
                }),
                mirror: None,
                reference: Some("#PWR02".to_string()),
                lib_id: Some(KiCadLibId::from("power:GND".to_string())),
                value: Some("GND".to_string()),
            },
            ImportSchematicPowerSymbolDecl {
                schematic_file: PathBuf::from("root.kicad_sch"),
                sheet_path: sheet_path.clone(),
                symbol_uuid: Some("c".to_string()),
                at: Some(ImportSchematicAt {
                    x: 5.0,
                    y: 6.0,
                    rot: Some(180.0),
                }),
                mirror: None,
                reference: Some("#PWR03".to_string()),
                lib_id: Some(KiCadLibId::from("power:+1V8".to_string())),
                value: Some("+1V8".to_string()),
            },
            // Non power/ground net should not be emitted.
            ImportSchematicPowerSymbolDecl {
                schematic_file: PathBuf::from("root.kicad_sch"),
                sheet_path: sheet_path.clone(),
                symbol_uuid: Some("d".to_string()),
                at: Some(ImportSchematicAt {
                    x: 7.0,
                    y: 8.0,
                    rot: Some(0.0),
                }),
                mirror: None,
                reference: Some("#PWR04".to_string()),
                lib_id: Some(KiCadLibId::from("power:SIG".to_string())),
                value: Some("SIG".to_string()),
            },
        ];

        let positions = build_net_symbol_positions_for_sheet(
            &sheet_path,
            &module_plan,
            &net_decls,
            &net_kinds_by_net,
            &power_symbol_decls,
        );

        let out = append_schematic_position_comments(
            "load(\"dummy\")\n".to_string(),
            &positions,
            &BTreeMap::new(),
        );

        let gnd0 = out
            .lines()
            .find(|line| line.starts_with("# pcb:sch GND.0 "))
            .expect("missing GND.0 comment");
        assert!(gnd0.contains(" x=") && gnd0.contains(" y="));
        assert!(gnd0.contains(" rot=270"));
        assert!(gnd0.contains(" mirror=x"));

        let gnd1 = out
            .lines()
            .find(|line| line.starts_with("# pcb:sch GND.1 "))
            .expect("missing GND.1 comment");
        assert!(gnd1.contains(" x=") && gnd1.contains(" y="));
        assert!(gnd1.contains(" rot=0"));
        assert!(!gnd1.contains(" mirror="));

        let net_1v8_0 = out
            .lines()
            .find(|line| line.starts_with("# pcb:sch NET_1V8.0 "))
            .expect("missing NET_1V8.0 comment");
        assert!(net_1v8_0.contains(" x=") && net_1v8_0.contains(" y="));
        assert!(net_1v8_0.contains(" rot=180"));

        assert!(!out.contains("# pcb:sch SIG.0 "));
    }

    #[test]
    fn flat_positions_mark_promoted_resistor_target_kind() {
        let anchor = make_anchor("anchor");

        let mut units = BTreeMap::new();
        units.insert(
            anchor.clone(),
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 10.0,
                    y: 20.0,
                    rot: Some(90.0),
                }),
            ),
        );

        let component = make_component("R1", units);
        let refs = BTreeMap::from([(KiCadRefDes::from("R1".to_string()), "R1".to_string())]);
        let mut generated =
            make_generated_components(BTreeMap::from([(anchor.clone(), "R".to_string())]));
        generated
            .anchor_to_module_ident
            .insert(anchor.clone(), "Resistor".to_string());
        generated.module_decls.push((
            "Resistor".to_string(),
            "@stdlib/generics/Resistor.zen".to_string(),
        ));

        let positions =
            build_flat_component_schematic_positions(&[(&anchor, &component)], &refs, &generated);
        assert_eq!(
            positions.get("R1.R").map(|p| p.target_kind),
            Some(ImportSchematicTargetKind::GenericResistor)
        );
    }

    #[test]
    fn flat_positions_prefer_anchor_unit_when_unit_numbers_collide() {
        let anchor = make_anchor("anchor");
        let other = make_anchor("other");

        let mut units = BTreeMap::new();
        units.insert(
            other,
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 10.0,
                    y: 20.0,
                    rot: Some(90.0),
                }),
            ),
        );
        units.insert(
            anchor.clone(),
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 30.0,
                    y: 40.0,
                    rot: Some(180.0),
                }),
            ),
        );

        let mut component = make_component("U1", units);
        component.netlist.unit_pcb_paths = vec![make_anchor("u1"), make_anchor("u2")];
        let refs = BTreeMap::from([(KiCadRefDes::from("U1".to_string()), "U1".to_string())]);
        let generated =
            make_generated_components(BTreeMap::from([(anchor.clone(), "IC".to_string())]));

        let positions =
            build_flat_component_schematic_positions(&[(&anchor, &component)], &refs, &generated);
        let pos = positions.get("U1.IC@U1").expect("missing position");
        assert_eq!(pos.at.x, 30.0);
        assert_eq!(pos.at.y, 40.0);
        assert_eq!(pos.at.rot, Some(180.0));
    }

    #[test]
    fn flat_positions_keep_unsuffixed_key_when_only_one_unit_number_exists() {
        let anchor = make_anchor("anchor");
        let other = make_anchor("other");

        let mut units = BTreeMap::new();
        units.insert(
            other,
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 10.0,
                    y: 20.0,
                    rot: Some(90.0),
                }),
            ),
        );
        units.insert(
            anchor.clone(),
            make_unit(
                Some(1),
                Some(ImportSchematicAt {
                    x: 30.0,
                    y: 40.0,
                    rot: Some(180.0),
                }),
            ),
        );

        let component = make_component("U1", units);
        let refs = BTreeMap::from([(KiCadRefDes::from("U1".to_string()), "U1".to_string())]);
        let generated =
            make_generated_components(BTreeMap::from([(anchor.clone(), "IC".to_string())]));

        let positions =
            build_flat_component_schematic_positions(&[(&anchor, &component)], &refs, &generated);
        let pos = positions.get("U1.IC").expect("missing position");
        assert_eq!(pos.at.x, 30.0);
        assert_eq!(pos.at.y, 40.0);
        assert_eq!(pos.at.rot, Some(180.0));
        assert!(!positions.contains_key("U1.IC@U1"));
    }

    #[test]
    fn appends_pcb_sch_comments_block() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "R1.R".to_string(),
            make_position_comment(
                10.0,
                20.0,
                90.0,
                None,
                None,
                None,
                ImportSchematicTargetKind::Other,
            ),
        )]);

        let out = append_schematic_position_comments(content, &positions, &BTreeMap::new());
        assert!(out.contains("\n\n# pcb:sch R1.R x=100.0000 y=200.0000 rot=270\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_include_mirror_axis() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "U1.IC".to_string(),
            make_position_comment(
                10.0,
                20.0,
                90.0,
                None,
                None,
                Some("x"),
                ImportSchematicTargetKind::Other,
            ),
        )]);

        let out = append_schematic_position_comments(content, &positions, &BTreeMap::new());
        assert!(out.contains("\n\n# pcb:sch U1.IC x=100.0000 y=200.0000 rot=270 mirror=x\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_ignore_invalid_mirror_axis() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "U1.IC".to_string(),
            make_position_comment(
                10.0,
                20.0,
                90.0,
                None,
                None,
                Some("z"),
                ImportSchematicTargetKind::Other,
            ),
        )]);

        let out = append_schematic_position_comments(content, &positions, &BTreeMap::new());
        assert!(out.contains("\n\n# pcb:sch U1.IC x=100.0000 y=200.0000 rot=270\n"));
        assert!(!out.contains(" mirror=z"));
    }

    #[test]
    fn appends_pcb_sch_comments_use_symbol_bbox_top_left() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "U1.IC".to_string(),
            make_position_comment(
                10.0,
                20.0,
                0.0,
                Some(1),
                Some("Demo:TestSymbol"),
                None,
                ImportSchematicTargetKind::Other,
            ),
        )]);

        let schematic_lib_symbols = BTreeMap::from([(
            KiCadLibId::from("Demo:TestSymbol".to_string()),
            r#"(symbol "Demo:TestSymbol"
  (symbol "TestSymbol_0_1"
    (rectangle (start -1 -2) (end 3 4))
  )
)"#
            .to_string(),
        )]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch U1.IC x=89.0000 y=159.0000 rot=0\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_use_unrotated_symbol_offset_with_rotated_symbol() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "U1.IC".to_string(),
            make_position_comment(
                50.0,
                75.0,
                90.0,
                Some(1),
                Some("Demo:RotSymbol"),
                None,
                ImportSchematicTargetKind::Other,
            ),
        )]);

        let schematic_lib_symbols = BTreeMap::from([(
            KiCadLibId::from("Demo:RotSymbol".to_string()),
            r#"(symbol "Demo:RotSymbol"
  (symbol "RotSymbol_0_1"
    (rectangle (start -10 -5) (end 10 5))
  )
)"#
            .to_string(),
        )]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch U1.IC x=399.0000 y=699.0000 rot=270\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_compensate_promoted_resistor_symbol_axis() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "R166.R".to_string(),
            make_position_comment(
                26.67,
                135.89,
                90.0,
                Some(1),
                Some("Demo:R0402"),
                None,
                ImportSchematicTargetKind::GenericResistor,
            ),
        )]);
        let schematic_lib_symbols = BTreeMap::from([
            (
                KiCadLibId::from("Demo:R0402".to_string()),
                r#"(symbol "Demo:R0402"
  (symbol "R0402_1_1"
    (pin passive line (at 0 0 0) (length 0.635) (name "~") (number "1"))
    (pin passive line (at 5.08 0 180) (length 0.635) (name "~") (number "2"))
  )
)"#
                .to_string(),
            ),
            (
                KiCadLibId::from("Device:R".to_string()),
                r#"(symbol "Device:R"
  (symbol "R_0_1"
    (rectangle (start -1 -2) (end 3 4))
  )
)"#
                .to_string(),
            ),
        ]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch R166.R x=285.7000 y=1287.1000 rot=180\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_no_passive_axis_compensation_when_already_aligned() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "R1.R".to_string(),
            make_position_comment(
                10.0,
                20.0,
                90.0,
                Some(1),
                Some("Demo:VertRes"),
                None,
                ImportSchematicTargetKind::GenericResistor,
            ),
        )]);
        let schematic_lib_symbols = BTreeMap::from([(
            KiCadLibId::from("Demo:VertRes".to_string()),
            r#"(symbol "Demo:VertRes"
  (symbol "VertRes_1_1"
    (pin passive line (at 0 3.81 270) (length 1.27) (name "~") (number "1"))
    (pin passive line (at 0 -3.81 90) (length 1.27) (name "~") (number "2"))
  )
)"#
            .to_string(),
        )]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch R1.R "));
        assert!(out.contains(" rot=270\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_compensate_promoted_resistor_pin_order() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "R2.R".to_string(),
            make_position_comment(
                26.67,
                135.89,
                90.0,
                Some(1),
                Some("Demo:R0402Reversed"),
                None,
                ImportSchematicTargetKind::GenericResistor,
            ),
        )]);
        let schematic_lib_symbols = BTreeMap::from([
            (
                KiCadLibId::from("Demo:R0402Reversed".to_string()),
                r#"(symbol "Demo:R0402Reversed"
  (symbol "R0402Reversed_1_1"
    (pin passive line (at 5.08 0 180) (length 0.635) (name "~") (number "1"))
    (pin passive line (at 0 0 0) (length 0.635) (name "~") (number "2"))
  )
)"#
                .to_string(),
            ),
            (
                KiCadLibId::from("Device:R".to_string()),
                r#"(symbol "Device:R"
  (symbol "R_0_1"
    (rectangle (start -1 -2) (end 3 4))
  )
)"#
                .to_string(),
            ),
        ]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch R2.R x=265.7000 y=1307.1000 rot=0\n"));
    }

    #[test]
    fn appends_pcb_sch_comments_compensate_promoted_resistor_with_mirror() {
        let content = "Board(\n    name = \"Demo\",\n)\n".to_string();
        let positions = BTreeMap::from([(
            "R162.R".to_string(),
            make_position_comment(
                40.64,
                63.5,
                0.0,
                Some(1),
                Some("Demo:R0402"),
                Some("y"),
                ImportSchematicTargetKind::GenericResistor,
            ),
        )]);
        let schematic_lib_symbols = BTreeMap::from([
            (
                KiCadLibId::from("Demo:R0402".to_string()),
                r#"(symbol "Demo:R0402"
  (symbol "R0402_1_1"
    (pin passive line (at 0 0 0) (length 0.635) (name "~") (number "1"))
    (pin passive line (at 5.08 0 180) (length 0.635) (name "~") (number "2"))
  )
)"#
                .to_string(),
            ),
            (
                KiCadLibId::from("Device:R".to_string()),
                r#"(symbol "Device:R"
  (symbol "R_0_1"
    (rectangle (start -1 -2) (end 3 4))
  )
)"#
                .to_string(),
            ),
        ]);

        let out = append_schematic_position_comments(content, &positions, &schematic_lib_symbols);
        assert!(out.contains("\n\n# pcb:sch R162.R x=364.6000 y=624.0000 rot=270 mirror=y\n"));
    }

    /// A kept `.kicad_mod` decides the board's copper, and nothing else checks it: reuse validation
    /// loads `.zen` entrypoints and compares connectivity, not geometry. So the difference has to be
    /// noticed here, and only when the land pattern really differs.
    #[test]
    fn a_kept_footprint_with_different_copper_is_noticed() {
        let pad = |size: &str| {
            format!(
                r#"(footprint "F" (layer "F.Cu")
        (pad "1" smd rect (at 0 0) (size {size}) (layers "F.Cu")))"#
            )
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("F.kicad_mod");

        // Same copper written the same way: nothing to say.
        fs::write(&path, pad("1 2")).unwrap();
        assert!(footprint_identity::same_land_pattern(&pad("1 2"), &pad("1 2")).unwrap());

        // Different pad size is a different land pattern, which is what the warning exists for.
        assert!(!footprint_identity::same_land_pattern(&pad("1 2"), &pad("3 2")).unwrap());

        // Both paths must be reachable without panicking, including unreadable and unparseable input.
        warn_kept_footprint_geometry(&path, &pad("3 2"));
        warn_kept_footprint_geometry(&path, &pad("1 2"));
        warn_kept_footprint_geometry(&dir.path().join("missing.kicad_mod"), &pad("1 2"));
        fs::write(&path, "(footprint unterminated").unwrap();
        warn_kept_footprint_geometry(&path, &pad("1 2"));
    }
}
