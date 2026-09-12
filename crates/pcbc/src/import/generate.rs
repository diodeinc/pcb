use super::*;
use anyhow::{Context, Result};
use pcb_component_gen as component_gen;
use pcb_sexpr::Sexpr;
use pcb_sexpr::find_child_list;
use pcb_sexpr::formatter::{FormatMode, format_tree, quote_string};
use pcb_sexpr::kicad::symbol::{
    kicad_symbol_lib_items_mut, rewrite_symbol_properties, symbol_names, symbol_properties,
};
use pcb_sexpr::{PatchSet, Span, board as sexpr_board};
use pcb_zen_core::lang::stackup as zen_stackup;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

pub(super) struct GenerationResult {
    pub(super) expected_pins_by_refdes: BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    pub(super) not_connected_nets: BTreeSet<KiCadNetName>,
    /// The Zener instance name generated for each source reference designator.
    ///
    /// Exposed because validation has to map a *built* component back to its source refdes, and the
    /// instance name is sanitized on the way out — `TP_3.3v1` becomes `TP_3_3v1`. Matching path
    /// segments against raw refdeses therefore fails for any refdes the sanitizer had to change, and
    /// the component then cannot be mapped at all.
    pub(super) instance_name_by_refdes: BTreeMap<KiCadRefDes, String>,
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory {}", parent.display()))?;
    }
    Ok(())
}

fn write_file(path: &Path, content: &[u8]) -> Result<()> {
    ensure_parent(path)?;
    let parent = path
        .parent()
        .context("Expected generated file path to have a parent directory")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".pcb.import.")
        .tempfile_in(parent)
        .with_context(|| format!("Failed to create temp file in {}", parent.display()))?;
    temp.write_all(content)
        .with_context(|| format!("Failed to write temp file for {}", path.display()))?;
    temp.flush()
        .with_context(|| format!("Failed to flush temp file for {}", path.display()))?;
    temp.persist(path)
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!(error))
        .with_context(|| format!("Failed to persist {}", path.display()))
}

fn write_zen(path: &Path, content: &str) -> Result<()> {
    ensure_parent(path)?;
    crate::codegen::zen::write_zen_formatted(path, content)
        .with_context(|| format!("Failed to write {}", path.display()))
}

pub(super) fn generate(
    materialized: &MaterializedBoard,
    board_name: &str,
    ir: &ImportIr,
) -> Result<GenerationResult> {
    // Use the same KiCad 9 -> 10 symbol normalization as the persistent editor.
    let project = pcbc::kicad_schematic::KicadProject::load(
        materialized
            .layout_kicad_pro
            .as_ref()
            .context("Import has no KiCad project")?,
    )?;
    let port_to_net = build_port_to_net_map(&ir.nets)?;
    let not_connected_nets = build_not_connected_nets(ir, &project.document)?;
    let net_decls = build_net_decls(&ir.nets, &not_connected_nets, &ir.semantic.net_kinds.by_net);
    let reserved_idents: BTreeSet<String> =
        net_decls.decls.iter().map(|d| d.ident.clone()).collect();
    let refdes_instance_names = build_refdes_instance_name_map(&ir.components);
    let component_modules = generate_imported_components(GenerateImportedComponentsArgs {
        board_dir: &materialized.board_dir,
        components: &ir.components,
        reserved_idents: &reserved_idents,
        schematic: &project.document,
        sheet_tree: &ir.schematic_sheet_tree,
        port_to_net: &port_to_net,
        not_connected_nets: &not_connected_nets,
    })?;

    let expected_pins_by_refdes = component_modules
        .expected_pins_by_anchor
        .iter()
        .filter_map(|(anchor, pins)| {
            let component = ir.components.get(anchor)?;
            Some((component.netlist.refdes.clone(), pins.clone()))
        })
        .collect();

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
        layout_kicad_pro: materialized.layout_kicad_pro.as_deref(),
        layout_kicad_pcb: materialized.layout_kicad_pcb.as_deref(),
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

    Ok(GenerationResult {
        expected_pins_by_refdes,
        not_connected_nets,
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
    net_decls: &'a ImportedNetDecls,
    component_modules: &'a GeneratedComponents,
    sheet_modules: &'a GeneratedSheetModules,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

fn write_imported_board_zen(args: ImportedBoardZenArgs<'_>) -> Result<()> {
    let (copper_layers, stackup, design_rules) =
        if let Some(layout_kicad_pcb) = args.layout_kicad_pcb {
            let pcb_text = fs::read_to_string(layout_kicad_pcb).with_context(|| {
                format!(
                    "Failed to read KiCad PCB for stackup extraction: {}",
                    layout_kicad_pcb.display()
                )
            })?;
            let (copper_layers, stackup) = try_extract_stackup(&pcb_text, layout_kicad_pcb)?;
            let design_rules = args.layout_kicad_pro.and_then(|layout_kicad_pro| {
                pcb_layout::extract_design_rules_from_kicad_pro(layout_kicad_pro)
                    .ok()
                    .flatten()
            });

            prepatch_imported_layout_kicad_pcb(LayoutPrepatchArgs {
                layout_kicad_pcb,
                pcb_text: &pcb_text,
                components: args.components,
                refdes_instance_names: args.refdes_instance_names,
                net_ident_by_kicad_name: &args.net_decls.zener_name_by_kicad_name,
                generated_components: args.component_modules,
                sheet_modules: args.sheet_modules,
            })
            .context("Failed to pre-patch imported KiCad PCB for sync hooks")?;

            (copper_layers, stackup, design_rules)
        } else {
            (4, None, None)
        };

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
    write_zen(args.board_zen, &board_zen_content)?;

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

fn prepatch_imported_layout_kicad_pcb(args: LayoutPrepatchArgs<'_>) -> Result<()> {
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

    let mut patches = PatchSet::default();
    let mut untouched_net_names = BTreeSet::new();
    // build_net_decls allocates unique final names. Apply that authoritative map
    // simultaneously, without layout repair's guard against existing target names.
    board.walk_strings(|value, span, ctx| {
        if sexpr_board::is_net_name(&ctx) || sexpr_board::is_zone_net_name(&ctx) {
            if let Some(name) = net_ident_by_kicad_name.get(&KiCadNetName::from(value.to_string()))
            {
                if name != value {
                    patches.replace_string(span, name);
                }
            } else {
                untouched_net_names.insert(value.to_string());
            }
        }
    });
    // PCB-only nets are absent from the allocator and must not be merged with
    // generated nets. Reject collisions before writing any layout patches.
    for name in net_ident_by_kicad_name.values() {
        anyhow::ensure!(
            !untouched_net_names.contains(name),
            "Generated net name {name:?} conflicts with an untouched PCB net"
        );
    }

    let path_patches = compute_import_footprint_path_property_patches(
        &board,
        pcb_text,
        components,
        refdes_instance_names,
        generated_components,
        sheet_modules,
    )?;

    patches.extend(path_patches);

    if patches.is_empty() {
        return Ok(());
    }

    let mut out: Vec<u8> = Vec::new();
    patches
        .write_to(pcb_text, &mut out)
        .with_context(|| format!("Failed to apply patches to {}", layout_kicad_pcb.display()))?;
    fs::write(layout_kicad_pcb, &out)
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
                "\t\t(property \"Path\" {}\n\t\t\t(at 0 0 0)\n\t\t\t(layer \"F.SilkS\")\n\t\t\t(hide yes)\n\t\t)\n",
                quote_string(desired)
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
    let source_copper_layers = infer_copper_layers_from_layers_section(pcb_text)?;
    let Some(stackup) = zen_stackup::Stackup::from_kicad_pcb(pcb_text).with_context(|| {
        format!(
            "Failed to parse stackup from {}",
            layout_kicad_pcb.display()
        )
    })?
    else {
        anyhow::ensure!(
            matches!(source_copper_layers, 2 | 4 | 6 | 8 | 10),
            "KiCad PCB {} has {source_copper_layers} copper layers but no explicit stackup; Zener has no default stackup for that layer count, so configure a stackup in KiCad before importing",
            layout_kicad_pcb.display()
        );
        return Ok((source_copper_layers, None));
    };

    stackup
        .validate()
        .with_context(|| format!("Invalid stackup in {}", layout_kicad_pcb.display()))?;

    let stackup_copper_layers = stackup.copper_layer_count();
    anyhow::ensure!(
        stackup_copper_layers == source_copper_layers,
        "KiCad PCB {} declares {source_copper_layers} copper layers in its layers section but its stackup contains {stackup_copper_layers}",
        layout_kicad_pcb.display()
    );

    Ok((source_copper_layers, Some(stackup)))
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

    Ok(copper_layer_names.len())
}

#[cfg(test)]
mod stackup_tests {
    use super::*;

    fn twelve_layer_pcb(with_stackup: bool) -> String {
        let copper_layers = [
            "F.Cu", "In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu", "In5.Cu", "In6.Cu", "In7.Cu", "In8.Cu",
            "In9.Cu", "In10.Cu", "B.Cu",
        ];
        let mut pcb = String::from("(kicad_pcb\n  (layers\n");
        for (index, name) in copper_layers.iter().enumerate() {
            pcb.push_str(&format!("    ({index} \"{name}\" signal)\n"));
        }
        pcb.push_str("  )\n  (setup\n");
        if with_stackup {
            pcb.push_str("    (stackup\n");
            for (index, name) in copper_layers.iter().enumerate() {
                pcb.push_str(&format!(
                    "      (layer \"{name}\" (type \"copper\") (thickness 0.035))\n"
                ));
                if index + 1 < copper_layers.len() {
                    pcb.push_str(&format!(
                        "      (layer \"dielectric {}\" (type \"core\") (thickness 0.1) (material \"FR4\"))\n",
                        index + 1
                    ));
                }
            }
            pcb.push_str("    )\n");
        }
        pcb.push_str("  )\n)\n");
        pcb
    }

    #[test]
    fn extracts_valid_twelve_layer_stackup() {
        let (layers, stackup) =
            try_extract_stackup(&twelve_layer_pcb(true), Path::new("twelve-layer.kicad_pcb"))
                .unwrap();

        assert_eq!(layers, 12);
        assert_eq!(stackup.unwrap().copper_layer_count(), 12);
    }

    #[test]
    fn unsupported_default_layer_count_requires_explicit_stackup() {
        let error = try_extract_stackup(
            &twelve_layer_pcb(false),
            Path::new("twelve-layer.kicad_pcb"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("12 copper layers but no explicit stackup"));
    }

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
          (setup)
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
    ir: &ImportIr,
    document: &pcb_kicad_sch::SchDocument,
) -> Result<BTreeSet<KiCadNetName>> {
    let marked = pcb_kicad_sch::analysis::marked_no_connect_targets(document)?;
    let files = document
        .pages
        .iter()
        .filter_map(|page| Some((page.id.as_str(), Path::new(page.file_name.as_deref()?))))
        .collect::<BTreeMap<_, _>>();
    let marked = marked
        .iter()
        .filter_map(|target| {
            Some((
                (
                    *files.get(target.page_id.as_str())?,
                    target.symbol_id.as_str(),
                    target.pin_number.as_str(),
                ),
                target,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    Ok(ir
        .nets
        .iter()
        .filter(|(_, net)| {
            let targets = net
                .ports
                .iter()
                .map(|port| {
                    ir.components
                        .get(&port.component)?
                        .schematic
                        .as_ref()?
                        .units
                        .keys()
                        .find_map(|key| {
                            let path =
                                KiCadSheetPath::from_sheetpath_tstamps(&key.sheetpath_tstamps);
                            let file = ir
                                .schematic_sheet_tree
                                .nodes
                                .get(&path)?
                                .schematic_file
                                .as_deref()?;
                            marked
                                .get(&(file, key.symbol_uuid.as_str(), port.pin.as_str()))
                                .copied()
                        })
                })
                .collect::<Option<Vec<_>>>();
            let Some(targets) = targets else {
                return false;
            };
            let Some(first) = targets.first() else {
                return false;
            };
            targets.iter().all(|target| {
                target.page_id == first.page_id
                    && target.symbol_id == first.symbol_id
                    && pcb_kicad_sch::connectivity::points_connect(target.at, first.at)
            })
        })
        .map(|(name, _)| name.clone())
        .collect())
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
        instance_calls.extend(child_module_calls.into_values());
        instance_calls.extend(component_instance_calls);

        let module_zen_content = crate::codegen::board::render_imported_sheet_module(
            &module_doc,
            &io_nets,
            &internal_net_decls,
            &module_decls,
            &instance_calls,
        );
        write_zen(&module_zen, &module_zen_content)?;
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
    symbol_definition: String,
    value: Option<String>,
    schematic_properties: BTreeMap<String, String>,
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
    expected_pins_by_anchor: BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadPinNumber>>,
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

struct GenerateImportedComponentsArgs<'a> {
    board_dir: &'a Path,
    components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    reserved_idents: &'a BTreeSet<String>,
    schematic: &'a pcb_kicad_sch::SchDocument,
    sheet_tree: &'a ImportSheetTree,
    port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    not_connected_nets: &'a BTreeSet<KiCadNetName>,
}

fn generate_imported_components(
    args: GenerateImportedComponentsArgs<'_>,
) -> Result<GeneratedComponents> {
    let GenerateImportedComponentsArgs {
        board_dir,
        components,
        reserved_idents,
        schematic,
        sheet_tree,
        port_to_net,
        not_connected_nets,
    } = args;
    let components_root = board_dir.join("components");

    let mut endpoint_pins_by_component: BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadPinNumber>> =
        BTreeMap::new();
    for port in port_to_net.keys() {
        endpoint_pins_by_component
            .entry(port.component.clone())
            .or_default()
            .insert(port.pin.clone());
    }

    let mut part_to_instances: BTreeMap<ImportPartKey, Vec<KiCadUuidPathKey>> = BTreeMap::new();
    let mut part_flags: BTreeMap<ImportPartKey, ImportPartFlags> = BTreeMap::new();
    for (anchor, c) in components {
        if c.layout.is_none() {
            // Only generate component packages for footprints that exist on the PCB.
            continue;
        }
        let definition = component_symbol_definition(c, schematic, sheet_tree)?;
        let key = derive_part_key(c, format_tree(&definition.sexpr, FormatMode::Normal));
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
    let mut module_io_pins: BTreeMap<String, BTreeMap<String, BTreeSet<KiCadPinNumber>>> =
        BTreeMap::new();
    let mut module_skip_defaults: BTreeMap<String, ModuleSkipDefaults> = BTreeMap::new();
    let mut expected_pins_by_anchor = BTreeMap::new();

    for (part_key, part_dir) in part_dir_by_key {
        let Some(instances) = part_to_instances.get(&part_key) else {
            continue;
        };
        // ImportPartKey includes the actual page-local definition, so instances
        // in this group share one embedded symbol definition. Use one
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
            render_component_symbol(&part_dir.component_dir, &part_key.symbol_definition)
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

        let unresolved_footprint = component.layout.as_ref().and_then(|layout| {
            layout.unresolved_footprint.as_ref().map(|unresolved| {
                // Component() requires a footprint string even when the KiCad symbol has no
                // footprint assignment. Preserve KiCad's unset marker without inventing geometry.
                unresolved.source_id.as_deref().unwrap_or("~")
            })
        });
        let zen = render_component_zen(
            &part_dir.component_dir,
            &symbol.symbol,
            &symbol.filename,
            flags,
            &pin_plan,
            unresolved_footprint,
            &part_key.schematic_properties,
        )
        .with_context(|| format!("Failed to render .zen for {}", out_dir.display()))?;

        let sym_path = out_dir.join(&symbol.filename);
        write_file(&sym_path, symbol.library_text.as_bytes())?;
        if let Some((filename, mod_text)) = &footprint.local_file {
            let fp_path = out_dir.join(filename);
            write_file(&fp_path, mod_text.as_bytes())?;
        }
        let zen_path = out_dir.join(&zen.filename);
        write_zen(&zen_path, &zen.zen_text)?;

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
        let skip_defaults = ModuleSkipDefaults::from(flags);
        let component_name = component_gen::sanitize_mpn_for_path(&part_dir.component_dir);

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

    Ok(GeneratedComponents {
        module_decls: module_decls.into_iter().collect(),
        anchor_to_module_ident,
        anchor_to_component_name,
        module_io_pins,
        module_skip_defaults,
        expected_pins_by_anchor,
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

fn explicit_sourcing_property<'a>(
    component: &'a ImportComponentData,
    aliases: &[&str],
) -> Option<&'a str> {
    component
        .best_properties()
        .and_then(|properties| find_property_ci(properties, aliases))
}

fn explicit_mpn(component: &ImportComponentData) -> Option<&str> {
    explicit_sourcing_property(
        component,
        &[
            "mpn",
            "manufacturer_part_number",
            "manufacturer part number",
            "mfr part number",
            "manufacturer_pn",
            "part number",
            "mp",
            "snapeda_pn",
        ],
    )
}

fn explicit_manufacturer(component: &ImportComponentData) -> Option<&str> {
    explicit_sourcing_property(
        component,
        &[
            "manufacturer",
            "manufacturer_name",
            "manufacturer name",
            "mfr",
            "mfr_name",
            "mfg",
        ],
    )
}

fn derive_part_key(component: &ImportComponentData, symbol_definition: String) -> ImportPartKey {
    let props = component.best_properties();

    let mpn = explicit_mpn(component).map(str::to_string);
    let manufacturer = explicit_manufacturer(component).map(str::to_string);

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
        .or_else(|| props.and_then(|p| p.get("Value")).cloned())
        .or_else(|| props.and_then(|p| p.get("Val")).cloned());

    // Parts may share a generated module only when all persisted display fields agree.
    let schematic_properties = props
        .into_iter()
        .flat_map(|properties| properties.iter())
        .filter(|(name, _)| matches!(name.as_str(), "Value" | "Description" | "Footprint"))
        .map(|(name, value)| {
            let name = if name == "Description" {
                "schematic_description"
            } else {
                name
            };
            (name.to_string(), value.clone())
        })
        .collect();

    ImportPartKey {
        mpn,
        manufacturer,
        footprint,
        lib_id,
        symbol_definition,
        value,
        schematic_properties,
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

/// Disambiguate two parts that share a component directory name by appending their footprint.
///
/// The result must remain a valid sanitized component name, because it becomes the directory name,
/// the generated `.zen` filename, and the `Component(name=...)` value.
fn footprint_qualified_component_dir_name(base: &str, footprint_name: &str) -> String {
    let mut base = base.to_string();
    let mut footprint_name = sanitize_component_dir_name(footprint_name);
    let mut excess = base
        .len()
        .saturating_add(2)
        .saturating_add(footprint_name.len())
        .saturating_sub(100);
    if excess > 0 {
        let remove_from_base = excess.min(base.len().saturating_sub(1));
        base.truncate(base.len() - remove_from_base);
        excess -= remove_from_base;
        footprint_name.truncate(footprint_name.len() - excess);
    }
    format!("{base}__{footprint_name}")
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

fn component_symbol_definition(
    component: &ImportComponentData,
    document: &pcb_kicad_sch::SchDocument,
    sheet_tree: &ImportSheetTree,
) -> Result<pcb_kicad_sch::SymbolDefinition> {
    let mut definition = None;
    for key in component
        .schematic
        .as_ref()
        .context("Imported component has no schematic")?
        .units
        .keys()
    {
        let sheet_path = KiCadSheetPath::from_sheetpath_tstamps(&key.sheetpath_tstamps);
        let file = sheet_tree
            .nodes
            .get(&sheet_path)
            .and_then(|sheet| sheet.schematic_file.as_ref())
            .context("Imported symbol has no source sheet")?;
        let page = document
            .pages
            .iter()
            .find(|page| page.file_name.as_deref().map(Path::new) == Some(file.as_path()))
            .context("Imported symbol source sheet was not loaded")?;
        let symbol = page
            .items
            .iter()
            .find_map(|item| match item {
                pcb_kicad_sch::SchItem::Symbol(symbol) if symbol.id == key.symbol_uuid => {
                    Some(symbol)
                }
                _ => None,
            })
            .context("Imported symbol was not found in its source sheet")?;
        let cached = page
            .library
            .definitions
            .get(symbol.library_key())
            .with_context(|| {
                format!(
                    "Missing embedded lib_symbol {} for {} in {}",
                    symbol.library_key(),
                    component.netlist.refdes.as_str(),
                    file.display()
                )
            })?;
        // Extract the actual cache content, but keep the external library identity.
        let cached = cached.renamed(&symbol.lib_id)?;
        anyhow::ensure!(
            definition
                .as_ref()
                .is_none_or(|previous| previous == &cached),
            "Component {} uses different embedded definitions across units",
            component.netlist.refdes.as_str()
        );
        definition = Some(cached);
    }
    definition.context("Imported component has no symbol units")
}

fn render_component_symbol(component_name: &str, sym: &str) -> Result<RenderedComponentSymbol> {
    let library_text =
        format!("(kicad_symbol_lib (version 20251024) (generator pcb_import)\n{sym}\n)\n");
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

    let mut physical_pins: BTreeMap<KiCadPinNumber, Vec<&pcb_eda::Pin>> = BTreeMap::new();
    for pin in &symbol.pins {
        physical_pins
            .entry(KiCadPinNumber::from(pin.number.clone()))
            .or_default()
            .push(pin);
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
        let only_no_connect = physical_pins
            .get(&number)
            .is_some_and(|pins| component_gen::pins_are_only_no_connect(pins.iter().copied()));
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

        if let Some(layout) = component.layout.as_ref()
            && !matches!(
                layout.footprint_geometry,
                ImportFootprintGeometry::Unresolved
            )
        {
            let footprint_pins = layout
                .pads
                .keys()
                .filter(|pin| !pin.as_str().is_empty())
                .cloned()
                .collect::<BTreeSet<_>>();
            let missing_pins = all_pins
                .difference(&footprint_pins)
                .cloned()
                .collect::<BTreeSet<_>>();
            if !missing_pins.is_empty() {
                anyhow::bail!(
                    "KiCad component {} footprint {} is missing numbered pads {:?} required by symbol physical pins {:?}",
                    component.netlist.refdes,
                    layout.fpid.as_deref().unwrap_or("<unknown>"),
                    missing_pins,
                    all_pins
                );
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

fn render_component_zen(
    component_name: &str,
    symbol: &pcb_eda::Symbol,
    symbol_filename: &str,
    flags: ImportPartFlags,
    pin_plan: &PhysicalPinPlan,
    unresolved_footprint: Option<&str>,
    properties: &BTreeMap<String, String>,
) -> Result<RenderedComponentZen> {
    let pins = pin_plan
        .bindings
        .iter()
        .map(|binding| component_gen::GenerateComponentPin {
            logical_name: binding.logical_name.clone(),
            pad_number: binding.pad_number.as_str().to_string(),
            io_name: binding.io_name.clone(),
        })
        .collect::<Vec<_>>();
    let zen_text = component_gen::generate_component_zen_with_pins(
        component_gen::GenerateComponentZenArgs {
            component_name,
            symbol,
            symbol_filename,
            generated_by: "pcb import",
            include_skip_bom: flags.any_skip_bom,
            include_skip_pos: flags.any_skip_pos,
            skip_bom_default: flags.all_skip_bom,
            skip_pos_default: flags.all_skip_pos,
        },
        &pins,
        unresolved_footprint,
        properties,
    )
    .context("Failed to generate component .zen")?;

    Ok(RenderedComponentZen {
        filename: format!("{component_name}.zen"),
        zen_text,
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
                    "Generated component IO {io_name} groups physical pins with different KiCad connectivity on {}",
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
            config_args: BTreeMap::new(),
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
    use super::*;

    #[test]
    fn imported_net_names_are_inferred_only_when_identical() {
        use crate::codegen::board::{
            RenderImportedBoardArgs, render_imported_board, render_imported_sheet_module,
        };

        let names = [
            ("SWITCH_INTERRUPT_N", ImportNetKind::Net),
            ("GND", ImportNetKind::Ground),
            ("VCC", ImportNetKind::Power),
            ("+3V3", ImportNetKind::Power),
            ("Signal.Name", ImportNetKind::Net),
            ("Signal_Name", ImportNetKind::Net),
            ("FOO-BAR", ImportNetKind::Net),
            ("FOO_BAR", ImportNetKind::Net),
        ];
        let nets = names
            .iter()
            .map(|(name, _)| {
                (
                    KiCadNetName::from(name.to_string()),
                    ImportNetData {
                        ports: BTreeSet::new(),
                    },
                )
            })
            .collect();
        let kinds = names
            .into_iter()
            .map(|(name, kind)| {
                (
                    KiCadNetName::from(name.to_string()),
                    ImportNetKindClassification {
                        kind,
                        reasons: BTreeSet::new(),
                    },
                )
            })
            .collect();
        let decls = build_net_decls(&nets, &BTreeSet::new(), &kinds);
        let board = render_imported_board(RenderImportedBoardArgs {
            board_name: "TestBoard",
            copper_layers: 2,
            design_rules: None,
            stackup: None,
            net_decls: &decls.decls,
            module_decls: &[],
            instance_calls: &[],
        });
        let sheet = render_imported_sheet_module("TestSheet", &[], &decls.decls, &[], &[]);
        for source in [board, sheet] {
            for expected in [
                "SWITCH_INTERRUPT_N = Net()",
                "GND = Ground()",
                "VCC = Power()",
                "NET_3V3 = Power(\"+3V3\")",
                "SIGNAL_NAME = Net(\"Signal_Name\")",
                "SIGNAL_NAME_2 = Net(\"Signal_Name_2\")",
                "FOO_BAR = Net(\"FOO-BAR\")",
                "FOO_BAR_2 = Net(\"FOO_BAR\")",
            ] {
                assert!(
                    source.lines().any(|line| line == expected),
                    "missing {expected:?} in:\n{source}"
                );
            }
        }
    }

    #[test]
    fn imported_layout_uses_allocated_net_names() {
        let names = ["Signal.Name", "Signal_Name", "Signal_Name_2", "GND"];
        let nets = names
            .map(|name| {
                (
                    KiCadNetName::from(name.to_string()),
                    ImportNetData {
                        ports: BTreeSet::new(),
                    },
                )
            })
            .into_iter()
            .collect();
        let decls = build_net_decls(&nets, &BTreeSet::new(), &BTreeMap::new());
        let input = r#"(kicad_pcb
            (net 1 "Signal.Name") (net 2 "Signal_Name") (net 3 "Signal_Name_2")
            (net 4 "GND") (net 0 "") (net 6 "PCB_ONLY")
            (footprint "Signal.Name" (property "Path" "Signal.Name")
                (pad "1" smd rect (net 1 "Signal.Name")))
            (segment (net "Signal_Name"))
            (zone (net "Signal_Name_2") (net_name "Signal.Name"))
            (group "Signal.Name"))"#;
        let expected = r#"(kicad_pcb
            (net 1 "Signal_Name") (net 2 "Signal_Name_2") (net 3 "Signal_Name_2_2")
            (net 4 "GND") (net 0 "") (net 6 "PCB_ONLY")
            (footprint "Signal.Name" (property "Path" "Signal.Name")
                (pad "1" smd rect (net 1 "Signal_Name")))
            (segment (net "Signal_Name_2"))
            (zone (net "Signal_Name_2_2") (net_name "Signal_Name"))
            (group "Signal.Name"))"#;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("layout.kicad_pcb");
        for extra in [
            "",
            r#"(net 5 "Signal_Name_2_2")"#,
            r#"(zone (net "Signal_Name_2_2"))"#,
            r#"(zone (net_name "Signal_Name_2_2"))"#,
        ] {
            let input = input.replacen("(kicad_pcb", &format!("(kicad_pcb{extra}"), 1);
            fs::write(&path, &input).unwrap();
            let result = prepatch_imported_layout_kicad_pcb(LayoutPrepatchArgs {
                layout_kicad_pcb: &path,
                pcb_text: &input,
                components: &BTreeMap::new(),
                refdes_instance_names: &BTreeMap::new(),
                net_ident_by_kicad_name: &decls.zener_name_by_kicad_name,
                generated_components: &make_generated_components(BTreeMap::new()),
                sheet_modules: &GeneratedSheetModules::default(),
            });
            if extra.is_empty() {
                result.unwrap();
                assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "Generated net name \"Signal_Name_2_2\" conflicts with an untouched PCB net"
                );
                assert_eq!(fs::read_to_string(&path).unwrap(), input);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_replaces_symlink_without_writing_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("user-file");
        let generated = temp.path().join("generated.kicad_sym");
        fs::write(&target, "user content").expect("write user file");
        symlink(&target, &generated).expect("create generated-file symlink");

        write_file(&generated, b"generated content").expect("write generated file");

        assert_eq!(fs::read_to_string(target).unwrap(), "user content");
        assert_eq!(fs::read_to_string(&generated).unwrap(), "generated content");
        assert!(
            !fs::symlink_metadata(generated)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn footprint_qualifier_preserves_its_separator() {
        assert_eq!(
            footprint_qualified_component_dir_name("Part", "SO 8"),
            "Part__SO_8"
        );
    }

    fn make_anchor(symbol_uuid: &str) -> KiCadUuidPathKey {
        KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: symbol_uuid.to_string(),
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
            module_io_pins: BTreeMap::new(),
            module_skip_defaults: BTreeMap::new(),
            expected_pins_by_anchor: BTreeMap::new(),
        }
    }

    #[test]
    fn inserted_path_property_round_trips_special_characters() {
        let pcb_text = "(kicad_pcb\n\t(footprint \"lib:FP\"\n\t\t(property \"Reference\" \"R1\")\n\t\t(path \"/old-uuid\")\n\t)\n)";
        let desired = "R\"1\\2\n\r\t.R";
        let board = pcb_sexpr::parse(pcb_text).unwrap();
        let patches = compute_set_footprint_sync_hook_patches_by_refdes(
            &board,
            pcb_text,
            &BTreeMap::from([(KiCadRefDes::from("R1".to_string()), desired.to_string())]),
        )
        .unwrap();
        let mut out = Vec::new();
        patches.write_to(pcb_text, &mut out).unwrap();
        let patched = String::from_utf8(out).unwrap();
        let board = pcb_sexpr::parse(&patched).unwrap();
        let footprint = find_child_list(board.as_list().unwrap(), "footprint").unwrap();
        let path = footprint
            .iter()
            .filter_map(Sexpr::as_list)
            .find(|list| {
                list.first().and_then(Sexpr::as_sym) == Some("property")
                    && list.get(1).and_then(Sexpr::as_str) == Some("Path")
            })
            .unwrap()[2]
            .as_str()
            .unwrap();
        assert_eq!(path, desired);
        assert_eq!(
            find_child_list(footprint, "path").unwrap()[1].as_str(),
            Some(pcb_sch::kicad_identity::footprint_kiid_path(path).as_str())
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
    fn resolved_footprint_must_contain_symbol_physical_pins() {
        let anchor = make_anchor("u1");
        let mut component = make_component("U1", BTreeMap::new());
        component.layout = Some(ImportLayoutComponent {
            fpid: Some("Local:OnePad".to_string()),
            unresolved_footprint: None,
            uuid: None,
            layer: None,
            at: None,
            sheetname: None,
            sheetfile: None,
            attrs: Vec::new(),
            properties: BTreeMap::new(),
            pads: BTreeMap::from([
                (
                    KiCadPinNumber::from(String::new()),
                    ImportLayoutPad {
                        net_names: BTreeSet::new(),
                        uuids: BTreeSet::new(),
                    },
                ),
                (
                    KiCadPinNumber::from("1".to_string()),
                    ImportLayoutPad {
                        net_names: BTreeSet::new(),
                        uuids: BTreeSet::new(),
                    },
                ),
            ]),
            footprint_geometry: ImportFootprintGeometry::LibraryFile("(footprint)".to_string()),
        });
        let components = BTreeMap::from([(anchor.clone(), component)]);
        let symbol = pcb_eda::Symbol {
            pins: vec![make_pin("A", "1", None), make_pin("B", "2", None)],
            ..Default::default()
        };

        let error = build_physical_pin_plan(
            &symbol,
            std::slice::from_ref(&anchor),
            &components,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
        .expect_err("resolved footprint must contain every symbol physical pin")
        .to_string();
        assert!(error.contains("footprint Local:OnePad is missing numbered pads"));
        assert!(error.contains("required by symbol physical pins"));
        assert!(!error.contains("KiCadPinNumber(\"\")"));
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
            &pcb_eda::Symbol::default(),
            "USB_DEVICE.kicad_sym",
            ImportPartFlags {
                all_skip_bom: false,
                ..ImportPartFlags::default()
            },
            &plan,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(rendered.filename, "USB_DEVICE__VARIANT.zen");
        assert!(rendered.zen_text.contains("D_POS_3 = io(Net)"));
        assert!(rendered.zen_text.contains("\"D+__3\": \"3\""));
        assert!(rendered.zen_text.contains("\"NC\\\"\\\\é\": \"10\""));
        assert!(!rendered.zen_text.contains("NC = io(Net)"));
    }
    #[test]
    fn renderer_preserves_unresolved_footprint_without_geometry() {
        let rendered = render_component_zen(
            "DEVICE",
            &pcb_eda::Symbol::default(),
            "DEVICE.kicad_sym",
            ImportPartFlags {
                all_skip_bom: false,
                ..ImportPartFlags::default()
            },
            &PhysicalPinPlan {
                bindings: Vec::new(),
                io_pins: BTreeMap::new(),
            },
            Some("Missing:Footprint"),
            &BTreeMap::new(),
        )
        .unwrap();

        assert!(
            rendered
                .zen_text
                .contains("footprint = \"Missing:Footprint\"")
        );
    }
}
