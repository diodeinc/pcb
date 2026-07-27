use super::*;
use anyhow::{Context, Result};
use log::debug;
use pcb_sexpr::Sexpr;
use pcb_sexpr::{board as sexpr_board, kicad as sexpr_kicad};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub(super) fn extract_ir(
    paths: &ImportPaths,
    selection: &ImportSelection,
    validation: &ImportValidationRun,
) -> Result<ImportIr> {
    let pcb_refdes_to_anchor_key = if selection.portable.source_kind == ImportSourceKind::Project {
        extract_kicad_pcb_refdes_to_anchor_key(
            &paths.kicad_project_root,
            &validation.summary.selected,
        )?
    } else {
        BTreeMap::new()
    };

    // KiCad netlist export can update project-local preference files. Run it against the same
    // contained source copy used for validation, then parse the original source files read-only.
    let netlist_sources = portable::stage_project_files(&selection.portable)?;
    let mut netlist = extract_kicad_netlist(
        netlist_sources.path(),
        &validation.summary.selected,
        &pcb_refdes_to_anchor_key,
    )?;

    let schematic = extract_kicad_schematic_data(
        &paths.kicad_project_root,
        &selection.files.kicad_sch,
        &netlist.unit_to_anchor,
        &mut netlist.components,
    )?;

    let schematic_sheet_tree = build_schematic_sheet_tree(
        &paths.kicad_project_root,
        &validation.summary.selected.kicad_sch,
        &netlist.components,
        &schematic.sheet_symbols_by_uuid,
    );

    if selection.portable.source_kind == ImportSourceKind::Project {
        extract_kicad_layout_data(
            &paths.kicad_project_root,
            &validation.summary.selected,
            &mut netlist.components,
        )?;
    } else {
        resolve_standalone_footprints(selection, &mut netlist.components)?;
    }

    Ok(ImportIr {
        components: netlist.components,
        nets: netlist.nets,
        schematic_lib_symbols: schematic.lib_symbols,
        schematic_power_symbol_decls: schematic.power_symbol_decls,
        schematic_sheet_tree,
        hierarchy_plan: ImportHierarchyPlan::default(),
        semantic: ImportSemanticAnalysis::default(),
    })
}

#[derive(Debug)]
struct KiCadSchematicExtraction {
    lib_symbols: BTreeMap<KiCadLibId, String>,
    power_symbol_decls: Vec<ImportSchematicPowerSymbolDecl>,
    sheet_symbols_by_uuid: BTreeMap<String, SchematicSheetSymbol>,
}

#[derive(Debug, Clone)]
struct SchematicSheetSymbol {
    sheet_name: Option<String>,
    /// Resolved schematic file path relative to the project root when possible.
    sheet_file: Option<PathBuf>,
}

#[derive(Debug)]
struct KiCadNetlistExtraction {
    components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    nets: BTreeMap<KiCadNetName, ImportNetData>,
    unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
}

#[derive(Debug)]
struct KiCadNetlistComponentsExtraction {
    components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    refdes_to_anchor: BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
    unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
}

fn extract_kicad_pcb_refdes_to_anchor_key(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
) -> Result<BTreeMap<KiCadRefDes, KiCadUuidPathKey>> {
    let kicad_pcb = selected
        .kicad_pcb
        .as_ref()
        .context("Project import is missing a selected .kicad_pcb file")?;
    let pcb_abs = kicad_project_root.join(kicad_pcb);
    if !pcb_abs.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_abs.display());
    }

    let text = fs::read_to_string(&pcb_abs)
        .with_context(|| format!("Failed to read {}", pcb_abs.display()))?;
    parse_kicad_pcb_refdes_to_anchor_key(&text).with_context(|| {
        format!(
            "Failed to parse KiCad PCB file for refdes/path anchors: {}",
            pcb_abs.display()
        )
    })
}

fn parse_kicad_pcb_refdes_to_anchor_key(
    pcb_text: &str,
) -> Result<BTreeMap<KiCadRefDes, KiCadUuidPathKey>> {
    let root = pcb_sexpr::parse(pcb_text).context("Failed to parse KiCad PCB as S-expression")?;

    let raw = sexpr_board::extract_footprint_refdes_to_kiid_path(&root)
        .map_err(|e| anyhow::anyhow!(e))?;

    let mut out: BTreeMap<KiCadRefDes, KiCadUuidPathKey> = BTreeMap::new();
    for (refdes, path) in raw {
        let refdes = KiCadRefDes::from(refdes);
        let key = KiCadUuidPathKey::from_pcb_path(&path)?;
        if out.insert(refdes.clone(), key).is_some() {
            anyhow::bail!(
                "KiCad PCB contains multiple footprints with refdes {}",
                refdes.as_str()
            );
        }
    }
    Ok(out)
}

fn extract_kicad_schematic_data(
    kicad_project_root: &Path,
    kicad_sch_files: &[PathBuf],
    unit_to_anchor: &BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<KiCadSchematicExtraction> {
    let mut lib_symbols: BTreeMap<KiCadLibId, String> = BTreeMap::new();
    let mut power_symbol_decls: Vec<ImportSchematicPowerSymbolDecl> = Vec::new();
    let mut sheet_symbols_by_uuid: BTreeMap<String, SchematicSheetSymbol> = BTreeMap::new();

    for rel in kicad_sch_files {
        let abs = kicad_project_root.join(rel);
        let text = fs::read_to_string(&abs)
            .with_context(|| format!("Failed to read {}", abs.display()))?;

        let root = pcb_sexpr::parse(&text).with_context(|| {
            format!(
                "Failed to parse KiCad schematic as S-expression: {}",
                abs.display()
            )
        })?;

        // Determine which `lib_id`s are power symbols by inspecting the embedded symbol
        // definitions. KiCad marks power symbols in the library definition (via `(power)`), but
        // placed symbol instances do not include that marker.
        let mut power_lib_ids: BTreeSet<KiCadLibId> = BTreeSet::new();

        // Extract embedded library symbol definitions.
        if let Some(lib) = root.find_list("lib_symbols") {
            for node in lib.iter().skip(1) {
                let Some(items) = node.as_list() else {
                    continue;
                };
                if items.first().and_then(Sexpr::as_sym) != Some("symbol") {
                    continue;
                }
                let Some(lib_id) = items.get(1).and_then(Sexpr::as_str) else {
                    continue;
                };
                let lib_id = KiCadLibId::from(lib_id.to_string());

                if sexpr_kicad::child_list(items, "power").is_some() {
                    power_lib_ids.insert(lib_id.clone());
                }

                let rendered = text
                    .get(node.span.start..node.span.end)
                    .with_context(|| {
                        format!(
                            "Failed to slice embedded lib_symbol S-expression span {}..{} from {}",
                            node.span.start,
                            node.span.end,
                            abs.display()
                        )
                    })?
                    .to_string();
                match lib_symbols.get(&lib_id) {
                    None => {
                        lib_symbols.insert(lib_id, rendered);
                    }
                    Some(existing) if existing == &rendered => {}
                    Some(_) => {
                        debug!(
                            "Conflicting embedded lib_symbols entry for {}; keeping first",
                            lib_id.as_str()
                        );
                    }
                }
            }
        }

        // Extract sheet symbols, which define the schematic hierarchy.
        for sheet in root.find_all_lists("sheet") {
            let Some(sheet_uuid) = sexpr_kicad::string_prop(sheet, "uuid") else {
                continue;
            };
            let props = sexpr_kicad::schematic_properties(sheet);
            let sheet_name = props.get("Sheetname").cloned();
            let sheet_file = props
                .get("Sheetfile")
                .and_then(|raw| resolve_sheet_file(kicad_project_root, rel, raw));

            let new = SchematicSheetSymbol {
                sheet_name,
                sheet_file,
            };
            match sheet_symbols_by_uuid.get(&sheet_uuid) {
                None => {
                    sheet_symbols_by_uuid.insert(sheet_uuid, new);
                }
                Some(existing)
                    if existing.sheet_name == new.sheet_name
                        && existing.sheet_file == new.sheet_file => {}
                Some(_) => {
                    debug!(
                        "Conflicting sheet symbol metadata for uuid {}; keeping first",
                        sheet_uuid
                    );
                }
            }
        }

        // Extract placed symbol instances (direct children of the schematic root).
        for sym in root.find_all_lists("symbol") {
            let Some(symbol_uuid) = sexpr_kicad::string_prop(sym, "uuid") else {
                continue;
            };

            let properties = sexpr_kicad::schematic_properties(sym);
            let lib_id = sexpr_kicad::string_prop(sym, "lib_id").map(KiCadLibId::from);

            let is_power_symbol = lib_id
                .as_ref()
                .is_some_and(|id| power_lib_ids.contains(id) || id.as_str().starts_with("power:"))
                || properties
                    .get("Reference")
                    .map(|r| r.trim_start().starts_with("#PWR"))
                    .unwrap_or(false);

            let instance_paths = sexpr_kicad::schematic_instance_paths(sym);

            if is_power_symbol {
                // Power symbols are usually not present in the KiCad netlist export, so we record
                // them regardless of whether we can join them to `netlist_components`.
                let mut sheet_paths: BTreeSet<KiCadSheetPath> = BTreeSet::new();
                if instance_paths.is_empty() {
                    sheet_paths.insert(KiCadSheetPath::root());
                } else {
                    for instance_path in &instance_paths {
                        if let Ok(key) =
                            key_from_schematic_instance_path(instance_path, &symbol_uuid)
                        {
                            sheet_paths.insert(KiCadSheetPath::from_sheetpath_tstamps(
                                &key.sheetpath_tstamps,
                            ));
                        }
                    }
                }

                let at = sexpr_kicad::schematic_at(sym).map(|(x, y, rot)| ImportSchematicAt {
                    x,
                    y,
                    rot,
                });
                let mirror = sexpr_kicad::sym_prop(sym, "mirror");
                let reference = properties.get("Reference").cloned();
                let value = properties.get("Value").cloned();

                for sheet_path in sheet_paths {
                    power_symbol_decls.push(ImportSchematicPowerSymbolDecl {
                        schematic_file: rel.clone(),
                        sheet_path,
                        symbol_uuid: Some(symbol_uuid.clone()),
                        at: at.clone(),
                        mirror: mirror.clone(),
                        reference: reference.clone(),
                        lib_id: lib_id.clone(),
                        value: value.clone(),
                    });
                }
            }

            if instance_paths.is_empty() {
                continue;
            }

            let selected = select_schematic_symbol_keys(
                &instance_paths,
                &symbol_uuid,
                unit_to_anchor,
                netlist_components,
            )?;
            if selected.is_empty() {
                continue;
            }

            let unit = sexpr_kicad::int_prop(sym, "unit");
            let lib_name = sexpr_kicad::string_prop(sym, "lib_name");
            let at =
                sexpr_kicad::schematic_at(sym).map(|(x, y, rot)| ImportSchematicAt { x, y, rot });
            let mirror = sexpr_kicad::sym_prop(sym, "mirror");

            let in_bom = sexpr_kicad::yes_no_prop(sym, "in_bom");
            let on_board = sexpr_kicad::yes_no_prop(sym, "on_board");
            let dnp = sexpr_kicad::yes_no_prop(sym, "dnp");
            let exclude_from_sim = sexpr_kicad::yes_no_prop(sym, "exclude_from_sim");

            let pins = sexpr_kicad::schematic_pins(sym);

            for (key, anchor, instance_path) in selected {
                let unit_data = ImportSchematicUnit {
                    lib_name: lib_name.clone(),
                    lib_id: lib_id.clone(),
                    unit,
                    at: at.clone(),
                    mirror: mirror.clone(),
                    in_bom,
                    on_board,
                    dnp,
                    exclude_from_sim,
                    instance_path: Some(instance_path),
                    properties: properties.clone(),
                    pins: pins.clone(),
                };

                let Some(entry) = netlist_components.get_mut(&anchor) else {
                    debug!(
                        "Schematic symbol {} is not present in the netlist; skipping",
                        anchor.pcb_path()
                    );
                    continue;
                };

                let sch = entry
                    .schematic
                    .get_or_insert_with(|| ImportSchematicComponent {
                        units: BTreeMap::new(),
                    });
                sch.units.insert(key, unit_data);
            }
        }
    }

    Ok(KiCadSchematicExtraction {
        lib_symbols,
        power_symbol_decls,
        sheet_symbols_by_uuid,
    })
}

fn select_schematic_symbol_keys(
    instance_paths: &[String],
    symbol_uuid: &str,
    unit_to_anchor: &BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>,
    netlist_components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<Vec<(KiCadUuidPathKey, KiCadUuidPathKey, String)>> {
    // A symbol can have multiple project instances (KiCad supports re-using a schematic in
    // multiple projects). Keep every instance path that matches the extracted netlist keys.
    let mut out: Vec<(KiCadUuidPathKey, KiCadUuidPathKey, String)> = Vec::new();
    let mut seen_anchors: BTreeSet<KiCadUuidPathKey> = BTreeSet::new();

    for instance_path in instance_paths {
        let key = key_from_schematic_instance_path(instance_path, symbol_uuid)?;
        let anchor = unit_to_anchor
            .get(&key)
            .cloned()
            .unwrap_or_else(|| key.clone());
        if netlist_components.contains_key(&anchor) && seen_anchors.insert(anchor.clone()) {
            out.push((key, anchor, instance_path.clone()));
        }
    }
    Ok(out)
}

fn resolve_sheet_file(
    kicad_project_root: &Path,
    declared_in_rel: &Path,
    raw: &str,
) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let raw = raw
        .strip_prefix("${KIPRJMOD}/")
        .or_else(|| raw.strip_prefix("${KIPRJMOD}\\"))
        .unwrap_or(raw);

    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        let rel = candidate
            .strip_prefix(kicad_project_root)
            .unwrap_or(&candidate);
        return Some(rel.to_path_buf());
    }

    let base = declared_in_rel.parent().unwrap_or(Path::new(""));
    let abs = kicad_project_root.join(base).join(candidate);
    let rel = abs.strip_prefix(kicad_project_root).unwrap_or(&abs);
    Some(rel.to_path_buf())
}

fn build_schematic_sheet_tree(
    _kicad_project_root: &Path,
    root_schematic_rel: &Path,
    netlist_components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    sheet_symbols_by_uuid: &BTreeMap<String, SchematicSheetSymbol>,
) -> ImportSheetTree {
    let mut all_paths: BTreeSet<KiCadSheetPath> = BTreeSet::new();
    all_paths.insert(KiCadSheetPath::root());

    for key in netlist_components.keys() {
        let sheet_path = KiCadSheetPath::from_sheetpath_tstamps(&key.sheetpath_tstamps);
        // Add this path and all prefixes (ancestors) so the tree contains intermediate sheets.
        let segments: Vec<&str> = sheet_path.segments().collect();
        for i in 0..=segments.len() {
            let p = if i == 0 {
                KiCadSheetPath::root()
            } else {
                KiCadSheetPath::from_sheetpath_tstamps(&format!("/{}/", segments[..i].join("/")))
            };
            all_paths.insert(p);
        }
    }

    let mut nodes: BTreeMap<KiCadSheetPath, ImportSheetNode> = BTreeMap::new();
    // Ensure deterministic construction (parents before children).
    let mut paths_sorted: Vec<KiCadSheetPath> = all_paths.into_iter().collect();
    paths_sorted.sort_by_key(|p| p.depth());

    for path in &paths_sorted {
        if path.as_str() == "/" {
            nodes.insert(
                path.clone(),
                ImportSheetNode {
                    sheet_uuid: None,
                    sheet_name: Some("/".to_string()),
                    schematic_file: Some(root_schematic_rel.to_path_buf()),
                    children: BTreeSet::new(),
                },
            );
            continue;
        }

        let sheet_uuid = path.last_uuid().map(|s| s.to_string());
        let (sheet_name, schematic_file) = sheet_uuid
            .as_deref()
            .and_then(|uuid| sheet_symbols_by_uuid.get(uuid))
            .map(|meta| (meta.sheet_name.clone(), meta.sheet_file.clone()))
            .unwrap_or((None, None));

        nodes.insert(
            path.clone(),
            ImportSheetNode {
                sheet_uuid,
                sheet_name,
                schematic_file,
                children: BTreeSet::new(),
            },
        );
    }

    // Populate child edges.
    for path in paths_sorted {
        let Some(parent) = path.parent() else {
            continue;
        };
        if let Some(parent_node) = nodes.get_mut(&parent) {
            parent_node.children.insert(path);
        }
    }

    ImportSheetTree {
        root_schematic: root_schematic_rel.to_path_buf(),
        nodes,
    }
}

fn extract_kicad_layout_data(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<()> {
    let kicad_pcb = selected
        .kicad_pcb
        .as_ref()
        .context("Project import is missing a selected .kicad_pcb file")?;
    let pcb_abs = kicad_project_root.join(kicad_pcb);
    if !pcb_abs.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_abs.display());
    }

    let pcb_text = fs::read_to_string(&pcb_abs)
        .with_context(|| format!("Failed to read {}", pcb_abs.display()))?;

    let root = pcb_sexpr::parse(&pcb_text).context("Failed to parse KiCad PCB as S-expression")?;

    let footprints =
        sexpr_board::extract_keyed_footprints(&root).map_err(|e| anyhow::anyhow!(e))?;

    for fp in footprints {
        let key = KiCadUuidPathKey::from_pcb_path(&fp.path)?;

        let Some(component) = netlist_components.get_mut(&key) else {
            // Ignore footprints we can't join against netlist-derived component identities.
            continue;
        };

        let sexpr = pcb_text
            .get(fp.span.start..fp.span.end)
            .with_context(|| {
                format!(
                    "Failed to slice footprint S-expression span {}..{} from {}",
                    fp.span.start,
                    fp.span.end,
                    pcb_abs.display()
                )
            })?
            .to_string();

        let mut pads: BTreeMap<KiCadPinNumber, ImportLayoutPad> = BTreeMap::new();
        for pad in fp.pads {
            let number = KiCadPinNumber::from(pad.number);
            let entry = pads.entry(number).or_insert_with(|| ImportLayoutPad {
                net_names: BTreeSet::new(),
                uuids: BTreeSet::new(),
            });

            if let Some(uuid) = pad.uuid {
                entry.uuids.insert(uuid);
            }
            if let Some(net_name) = pad.net_name {
                let net_name = net_name.trim().to_string();
                if !net_name.is_empty() {
                    entry.net_names.insert(KiCadNetName::from(net_name));
                }
            }
        }

        let layout = ImportLayoutComponent {
            fpid: fp.fpid,
            unresolved_footprint: None,
            uuid: fp.uuid,
            layer: fp.layer,
            at: fp.at.map(|at| ImportLayoutAt {
                x: at.x,
                y: at.y,
                rot: at.rot,
            }),
            sheetname: fp.sheetname,
            sheetfile: fp.sheetfile,
            attrs: fp.attrs,
            properties: fp.properties,
            pads,
            footprint_geometry: ImportFootprintGeometry::BoardInstance(sexpr),
        };

        if component.layout.replace(layout).is_some() {
            debug!(
                "Duplicate layout footprint entry for {}; overwriting",
                key.pcb_path()
            );
        }
    }

    Ok(())
}

fn resolve_standalone_footprints(
    selection: &ImportSelection,
    components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<()> {
    let stdlib_root = pcb_zen_core::stdlib::native::discover_source()
        .context("Failed to locate the Zener standard library for KiCad footprint resolution")?;
    let schematic_path = selection
        .portable
        .project_dir
        .join(&selection.portable.root_schematic_rel);
    let kicad_major = schematic_generator_major(&schematic_path);
    let cache_dir = dirs::home_dir().map(|home| home.join(".pcb/cache"));
    let cached_roots = cache_dir
        .as_deref()
        .zip(kicad_major)
        .map(|(cache, major)| cached_kicad_footprint_roots(cache, major))
        .unwrap_or_default();
    let mut unresolved: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Keyed by fpid and holding the *outcome*, so a footprint no library provides is searched once
    // rather than once per component that references it. On a design whose libraries are not installed
    // that is the common path — every component misses — and each miss walked all four locations again.
    type ResolvedFootprint = (
        BTreeMap<KiCadPinNumber, ImportLayoutPad>,
        ImportFootprintGeometry,
    );
    let mut resolved_by_fpid: BTreeMap<String, Option<ResolvedFootprint>> = BTreeMap::new();

    for component in components.values_mut() {
        let Some(fpid) = component
            .netlist
            .footprint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "~")
        else {
            unresolved
                .entry(NO_FOOTPRINT_KEY.to_string())
                .or_default()
                .push(component.netlist.refdes.as_str().to_string());
            component.layout = Some(unresolved_layout_component(None));
            continue;
        };

        // Many components share one footprint, so resolve, read, and parse each fpid once.
        let resolved = match resolved_by_fpid.get(fpid) {
            // Already known to resolve nowhere: record the component and move on without searching.
            Some(None) => {
                unresolved
                    .entry(fpid.to_string())
                    .or_default()
                    .push(component.netlist.refdes.as_str().to_string());
                component.layout = Some(unresolved_layout_component(Some(fpid)));
                continue;
            }
            Some(Some(resolved)) => resolved,
            None => {
                // Ordered by precedence: the project's own libraries, then the bundled stdlib
                // subset, then the version-matched KiCad cache. `copy_to_component` says whether the
                // geometry must be copied into the generated package; the stdlib is a library
                // reference the built board already resolves.
                let lookup = selection
                    .portable
                    .resolved_project_footprints
                    .get(fpid)
                    .map(|path| (path.clone(), true))
                    .or_else(|| {
                        resolve_footprint_in_root(&stdlib_root.join("kicad-footprints"), fpid)
                            .map(|path| (path, false))
                    })
                    .or_else(|| {
                        resolve_footprint_from_roots(&cached_roots, fpid).map(|path| (path, true))
                    });
                let Some((path, copy_to_component)) = lookup else {
                    resolved_by_fpid.insert(fpid.to_string(), None);
                    unresolved
                        .entry(fpid.to_string())
                        .or_default()
                        .push(component.netlist.refdes.as_str().to_string());
                    component.layout = Some(unresolved_layout_component(Some(fpid)));
                    continue;
                };
                let footprint_text = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read footprint {}", path.display()))?;
                let pads = parse_standalone_footprint_pads(&footprint_text)
                    .with_context(|| format!("Failed to parse footprint {fpid}"))?;
                let geometry = if copy_to_component {
                    ImportFootprintGeometry::LibraryFile(footprint_text)
                } else {
                    ImportFootprintGeometry::StandardLibrary
                };
                resolved_by_fpid
                    .entry(fpid.to_string())
                    .or_insert(Some((pads, geometry)))
                    .as_ref()
                    .expect("just inserted a resolved footprint")
            }
        };

        component.layout = Some(ImportLayoutComponent {
            fpid: Some(fpid.to_string()),
            unresolved_footprint: None,
            uuid: None,
            layer: None,
            at: None,
            sheetname: None,
            sheetfile: None,
            attrs: Vec::new(),
            properties: BTreeMap::new(),
            pads: resolved.0.clone(),
            footprint_geometry: resolved.1.clone(),
        });
    }

    if !unresolved.is_empty() {
        eprintln!(
            "{}",
            unresolved_footprint_warning(
                &unresolved,
                components.len(),
                &selection.portable.project_dir,
                kicad_major,
            )
        );
    }

    Ok(())
}

/// A design can reference many broken libraries; list enough to identify the cause without
/// flooding stderr.
const UNRESOLVED_LIBRARY_LIST_LIMIT: usize = 5;

/// Stands in for a component whose schematic names no footprint at all.
///
/// Kept out of the library grouping in [`unresolved_footprint_warning`]: it is not a library nickname,
/// and treating it as one told the user that a library called `<missing footprint>` was not registered
/// — pointing at `fp-lib-table` when the field is simply empty or `~` in KiCad.
const NO_FOOTPRINT_KEY: &str = "<missing footprint>";

/// Builds the unresolved-footprint warning.
///
/// The warning has to be self-diagnosing: which library nicknames failed, where import looked for
/// them, and whether *every* referenced footprint failed (a library that is not registered where
/// import looks) or only some did (a per-footprint gap in the design). Import continues either
/// way, because the structural conversion succeeded and is the valuable part.
fn unresolved_footprint_warning(
    unresolved: &BTreeMap<String, Vec<String>>,
    total_components: usize,
    project_dir: &Path,
    kicad_major: Option<u64>,
) -> String {
    let footprint_count = unresolved.len();
    let component_count = unresolved.values().map(Vec::len).sum::<usize>();
    let without_footprint = unresolved.get(NO_FOOTPRINT_KEY).map_or(0, Vec::len);

    // Group by library nickname: the nickname is what the user registers in KiCad, so it is what
    // makes the warning actionable. Components naming no footprint have no nickname to register and
    // are reported separately.
    let mut by_library: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (fpid, refdeses) in unresolved
        .iter()
        .filter(|(fpid, _)| *fpid != NO_FOOTPRINT_KEY)
    {
        let nickname = fpid.split_once(':').map_or(fpid.as_str(), |(name, _)| name);
        let entry = by_library.entry(nickname).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += refdeses.len();
    }
    let mut ranked = by_library.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.1.cmp(&left.1.1).then_with(|| left.0.cmp(right.0)));

    let diagnosis = if ranked.is_empty() {
        // Nothing was looked up at all: every unresolved component simply names no footprint.
        "no component names a footprint, so there is no library to register — set the Footprint field \
         in KiCad"
            .to_string()
    } else if component_count >= total_components && ranked.len() == 1 {
        format!(
            "every component's footprint is unresolved and they all come from library '{}', so the library is most likely not registered where import looks rather than the design being unusual",
            ranked[0].0
        )
    } else if component_count >= total_components {
        "every component's footprint is unresolved, so these libraries are most likely not registered where import looks rather than the design being unusual".to_string()
    } else {
        format!(
            "{} of {total_components} component(s) resolved, so this is a per-footprint gap rather than a library that is missing entirely",
            total_components - component_count
        )
    };

    let cached_library = match kicad_major {
        Some(major) => format!("the cached kicad-footprints package for KiCad {major}"),
        None => {
            "the cached kicad-footprints package matching the schematic's KiCad version".to_string()
        }
    };

    let mut listed = ranked
        .iter()
        .take(UNRESOLVED_LIBRARY_LIST_LIMIT)
        .map(|(nickname, (footprints, components))| {
            format!("{nickname} ({footprints} footprint(s), {components} component(s))")
        })
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(remaining) = ranked.len().checked_sub(UNRESOLVED_LIBRARY_LIST_LIMIT)
        && remaining > 0
    {
        listed.push_str(&format!(", and {remaining} more library nickname(s)"));
    }

    let no_footprint_note = if without_footprint > 0 {
        format!(
            "\n  {without_footprint} component(s) name no footprint at all, which no library can supply; set the Footprint field in KiCad."
        )
    } else {
        String::new()
    };

    // With no library in play there is nothing to register, so the list and the search locations would
    // only mislead.
    if ranked.is_empty() {
        return format!(
            "Warning: {component_count} component(s) have {footprint_count} unresolved footprint definition(s); {diagnosis}\n  \
             The board still imports with connectivity intact but is not layout-ready."
        );
    }

    format!(
        "Warning: {component_count} component(s) have {footprint_count} unresolved footprint definition(s); {diagnosis}\n  \
         Unresolved libraries: {listed}\n  \
         Looked in: the project fp-lib-table ({}), the global KiCad fp-lib-table (KICAD_CONFIG_HOME or the platform KiCad configuration directory), the bundled KiCad footprint subset, and {cached_library}\n  \
         The board still imports with connectivity intact but is not layout-ready; generated Zener retains the available source footprint IDs. Register the missing library in KiCad or in the project fp-lib-table, then re-import with --force to complete the footprints.{no_footprint_note}",
        project_dir.join("fp-lib-table").display()
    )
}

fn unresolved_layout_component(fpid: Option<&str>) -> ImportLayoutComponent {
    ImportLayoutComponent {
        fpid: fpid.map(str::to_string),
        unresolved_footprint: Some(ImportUnresolvedFootprint {
            source_id: fpid.map(str::to_string),
        }),
        uuid: None,
        layer: None,
        at: None,
        sheetname: None,
        sheetfile: None,
        attrs: Vec::new(),
        properties: BTreeMap::new(),
        pads: BTreeMap::new(),
        footprint_geometry: ImportFootprintGeometry::Unresolved,
    }
}

fn schematic_generator_major(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let root = pcb_sexpr::parse(&text).ok()?;
    let items = root.as_list()?;
    let version = items.iter().find_map(|item| {
        let list = item.as_list()?;
        (list.first().and_then(Sexpr::as_sym) == Some("generator_version"))
            .then(|| {
                list.get(1)
                    .and_then(|value| value.as_str().or_else(|| value.as_sym()))
            })
            .flatten()
    })?;
    version.split('.').next()?.parse().ok()
}

fn cached_kicad_footprint_roots(cache_dir: &Path, major: u64) -> Vec<PathBuf> {
    let package_root = cache_dir.join("gitlab.com/kicad/libraries/kicad-footprints");
    let Ok(entries) = fs::read_dir(package_root) else {
        return Vec::new();
    };
    let mut versions = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return None;
            }
            let version = semver::Version::parse(entry.file_name().to_str()?).ok()?;
            if version.major != major {
                return None;
            }
            Some((version, entry.path()))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|(left, _), (right, _)| right.cmp(left));
    versions.into_iter().map(|(_, path)| path).collect()
}

fn resolve_footprint_from_roots(roots: &[PathBuf], fpid: &str) -> Option<PathBuf> {
    roots
        .iter()
        .find_map(|root| resolve_footprint_in_root(root, fpid))
}

fn resolve_footprint_in_root(root: &Path, fpid: &str) -> Option<PathBuf> {
    let (library, footprint) = fpid.split_once(':')?;
    if library.is_empty()
        || footprint.is_empty()
        || footprint.contains(':')
        || library.contains(['/', '\\'])
        || footprint.contains(['/', '\\'])
    {
        return None;
    }
    let library_path = root.join(format!("{library}.pretty"));
    let library_metadata = fs::symlink_metadata(&library_path).ok()?;
    if !library_metadata.is_dir() || library_metadata.file_type().is_symlink() {
        return None;
    }
    let path = library_path.join(format!("{footprint}.kicad_mod"));
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let canonical_root = root.canonicalize().ok()?;
    let canonical_path = path.canonicalize().ok()?;
    canonical_path
        .starts_with(&canonical_root)
        .then_some(canonical_path)
}

fn parse_standalone_footprint_pads(
    footprint_text: &str,
) -> Result<BTreeMap<KiCadPinNumber, ImportLayoutPad>> {
    let root = pcb_sexpr::parse(footprint_text)
        .context("Failed to parse .kicad_mod as an S-expression")?;
    let mut pads = BTreeMap::new();
    for pad in root.find_all_lists("pad") {
        let Some(number) = pad
            .get(1)
            .and_then(|value| value.as_str().or_else(|| value.as_sym()))
        else {
            continue;
        };
        if number.is_empty() {
            continue;
        }
        pads.entry(KiCadPinNumber::from(number.to_string()))
            .or_insert_with(|| ImportLayoutPad {
                net_names: BTreeSet::new(),
                uuids: BTreeSet::new(),
            });
    }
    // Mechanical and documentation footprints such as logos and mounting holes can legitimately
    // have no numbered pads. Keep their geometry and represent them as pinless components rather
    // than blocking structural schematic import.
    Ok(pads)
}

fn extract_kicad_netlist(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistExtraction> {
    let kicad_sch_abs = kicad_project_root.join(&selected.kicad_sch);
    let netlist_text = export_kicad_sexpr_netlist(&kicad_sch_abs, kicad_project_root)
        .context("Failed to export KiCad netlist")?;
    parse_kicad_sexpr_netlist(&netlist_text, pcb_refdes_to_anchor_key)
        .context("Failed to parse KiCad netlist")
}

fn export_kicad_sexpr_netlist(kicad_sch_abs: &Path, working_dir: &Path) -> Result<String> {
    if !kicad_sch_abs.exists() {
        anyhow::bail!("Schematic file not found: {}", kicad_sch_abs.display());
    }

    let tmp = NamedTempFile::new().context("Failed to create temporary netlist file")?;

    pcb_kicad::KiCadCliBuilder::new()
        .command("sch")
        .subcommand("export")
        .subcommand("netlist")
        .arg("--format")
        .arg("kicadsexpr")
        .arg("--output")
        .arg(tmp.path().to_string_lossy())
        .arg(kicad_sch_abs.to_string_lossy())
        .current_dir(working_dir.to_string_lossy().to_string())
        .run()
        .context("kicad-cli sch export netlist failed")?;

    fs::read_to_string(tmp.path())
        .with_context(|| format!("Failed to read generated netlist {}", tmp.path().display()))
}

fn parse_kicad_sexpr_netlist(
    netlist_text: &str,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistExtraction> {
    let root =
        pcb_sexpr::parse(netlist_text).context("Failed to parse KiCad netlist as S-expression")?;

    let comps = parse_kicad_sexpr_netlist_components(&root, pcb_refdes_to_anchor_key)?;
    let nets = parse_kicad_sexpr_netlist_nets(&root, &comps.refdes_to_anchor)?;

    Ok(KiCadNetlistExtraction {
        components: comps.components,
        nets,
        unit_to_anchor: comps.unit_to_anchor,
    })
}

fn parse_kicad_sexpr_netlist_components(
    root: &Sexpr,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistComponentsExtraction> {
    let components = root
        .find_list("components")
        .ok_or_else(|| anyhow::anyhow!("Netlist missing (components ...) section"))?;

    let mut by_key: BTreeMap<KiCadUuidPathKey, ImportComponentData> = BTreeMap::new();
    let mut refdes_to_key: BTreeMap<KiCadRefDes, KiCadUuidPathKey> = BTreeMap::new();
    let mut unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey> = BTreeMap::new();
    let mut duplicate_refdeses: BTreeMap<KiCadRefDes, BTreeSet<KiCadUuidPathKey>> = BTreeMap::new();
    let mut duplicate_paths: BTreeMap<KiCadUuidPathKey, BTreeSet<KiCadRefDes>> = BTreeMap::new();

    for node in components.iter().skip(1) {
        let Some(comp) = node.as_list() else {
            continue;
        };
        if comp.first().and_then(Sexpr::as_sym) != Some("comp") {
            continue;
        }

        let refdes = sexpr_kicad::string_prop(comp, "ref")
            .ok_or_else(|| anyhow::anyhow!("Netlist component missing ref"))?;
        let refdes = KiCadRefDes::from(refdes);

        let symbol_uuids = sexpr_kicad::string_list_prop(comp, "tstamps").ok_or_else(|| {
            anyhow::anyhow!("Netlist component {refdes} missing tstamps (symbol UUID)")
        })?;

        let (sheetpath_names, sheetpath_tstamps) = sexpr_kicad::sheetpath(comp)
            .with_context(|| format!("Netlist component {refdes} missing sheetpath (tstamps)"))?;

        let footprint = sexpr_kicad::string_prop(comp, "footprint");
        let value = sexpr_kicad::string_prop(comp, "value");

        let normalized_sheetpath_tstamps = normalize_sheetpath_tstamps(&sheetpath_tstamps);

        let anchor_key = if let Some(anchor_key) = pcb_refdes_to_anchor_key.get(&refdes) {
            anchor_key.clone()
        } else {
            // Fallback: choose the first tstamps entry deterministically.
            let Some(symbol_uuid) = symbol_uuids.first() else {
                anyhow::bail!("Netlist component {refdes} has empty tstamps list");
            };
            KiCadUuidPathKey {
                sheetpath_tstamps: normalized_sheetpath_tstamps.clone(),
                symbol_uuid: symbol_uuid.clone(),
            }
        };

        let mut unit_keys: Vec<KiCadUuidPathKey> = Vec::new();
        for uuid in &symbol_uuids {
            let unit_key = KiCadUuidPathKey {
                sheetpath_tstamps: normalized_sheetpath_tstamps.clone(),
                symbol_uuid: uuid.clone(),
            };
            unit_to_anchor.insert(unit_key.clone(), anchor_key.clone());
            unit_keys.push(unit_key);
        }

        let netlist_component = ImportNetlistComponent {
            refdes: refdes.clone(),
            value,
            footprint,
            sheetpath_names,
            unit_pcb_paths: unit_keys.clone(),
        };

        if let Some(existing_key) = refdes_to_key.get(&refdes) {
            let paths = duplicate_refdeses.entry(refdes.clone()).or_default();
            paths.insert(existing_key.clone());
            paths.insert(anchor_key.clone());
        } else {
            refdes_to_key.insert(refdes.clone(), anchor_key.clone());
        }

        if let Some(existing_component) = by_key.get(&anchor_key) {
            let refdeses = duplicate_paths.entry(anchor_key.clone()).or_default();
            refdeses.insert(existing_component.netlist.refdes.clone());
            refdeses.insert(refdes.clone());
        } else {
            by_key.insert(
                anchor_key.clone(),
                ImportComponentData {
                    netlist: netlist_component,
                    schematic: None,
                    layout: None,
                },
            );
        }
    }

    if !duplicate_refdeses.is_empty() || !duplicate_paths.is_empty() {
        let mut lines = vec!["Ambiguous KiCad component identities:".to_string()];
        for (refdes, paths) in duplicate_refdeses {
            lines.push(format!(
                "  - refdes {} maps to paths {}",
                refdes,
                paths
                    .iter()
                    .map(KiCadUuidPathKey::pcb_path)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for (path, refdeses) in duplicate_paths {
            lines.push(format!(
                "  - path {} maps to refdeses {}",
                path.pcb_path(),
                refdeses
                    .iter()
                    .map(KiCadRefDes::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        anyhow::bail!(lines.join("\n"));
    }

    Ok(KiCadNetlistComponentsExtraction {
        components: by_key,
        refdes_to_anchor: refdes_to_key,
        unit_to_anchor,
    })
}

fn parse_kicad_sexpr_netlist_nets(
    root: &Sexpr,
    refdes_to_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<BTreeMap<KiCadNetName, ImportNetData>> {
    let nets = root
        .find_list("nets")
        .ok_or_else(|| anyhow::anyhow!("Netlist missing (nets ...) section"))?;

    let mut out: BTreeMap<KiCadNetName, ImportNetData> = BTreeMap::new();

    for node in nets.iter().skip(1) {
        let Some(net) = node.as_list() else {
            continue;
        };
        if net.first().and_then(Sexpr::as_sym) != Some("net") {
            continue;
        }

        let name = sexpr_kicad::string_prop(net, "name")
            .ok_or_else(|| anyhow::anyhow!("Netlist net missing name"))?;
        let name = KiCadNetName::from(name);

        let mut ports: BTreeSet<ImportNetPort> = BTreeSet::new();

        for child in net.iter().skip(1) {
            let Some(items) = child.as_list() else {
                continue;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("node") {
                continue;
            }

            let node_ref = sexpr_kicad::string_prop(items, "ref")
                .ok_or_else(|| anyhow::anyhow!("Netlist net {name} contains node without ref"))?;
            let node_ref = KiCadRefDes::from(node_ref);

            let pin = sexpr_kicad::string_prop(items, "pin").ok_or_else(|| {
                anyhow::anyhow!("Netlist net {name} contains node without pin (ref {node_ref})")
            })?;
            let pin = KiCadPinNumber::from(pin);

            let Some(key) = refdes_to_key.get(&node_ref) else {
                debug!("Netlist net {name} references unknown refdes {node_ref}; skipping");
                continue;
            };

            ports.insert(ImportNetPort {
                component: key.clone(),
                pin,
            });
        }

        if out.insert(name.clone(), ImportNetData { ports }).is_some() {
            anyhow::bail!("Netlist produced a duplicate net name: {}", name.as_str());
        }
    }

    Ok(out)
}

fn key_from_schematic_instance_path(
    instance_path: &str,
    symbol_uuid: &str,
) -> Result<KiCadUuidPathKey> {
    let trimmed = instance_path.trim();
    if !trimmed.starts_with('/') {
        anyhow::bail!("Expected schematic instance path to start with '/': {instance_path:?}");
    }
    let parts: Vec<&str> = trimmed
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    // Instance paths include the root schematic UUID as the first segment; PCB paths do not.
    let sheet_parts = if parts.len() <= 1 {
        &[][..]
    } else {
        &parts[1..]
    };
    let sheetpath_tstamps = if sheet_parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", sheet_parts.join("/"))
    };

    Ok(KiCadUuidPathKey {
        sheetpath_tstamps,
        symbol_uuid: symbol_uuid.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kicad_sexpr_netlist_and_builds_uuid_path_keys() -> Result<()> {
        let netlist = r#"
(export (version "E")
  (design (source "x") (date "x") (tool "Eeschema"))
  (components
    (comp (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0402_1005Metric")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"))
    (comp (ref "U1")
      (value "MCU")
      (footprint "Package_QFP:LQFP-48_7x7mm_P0.5mm")
      (sheetpath (names "/SoM/") (tstamps "/11111111-2222-3333-4444-555555555555/"))
      (tstamps "99999999-8888-7777-6666-555555555555"))
  )
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "U1") (pin "3") (pintype "power_in")))
  )
)
"#;

        let mut pcb_refdes_to_anchor_key: BTreeMap<KiCadRefDes, KiCadUuidPathKey> = BTreeMap::new();
        pcb_refdes_to_anchor_key.insert(
            KiCadRefDes::from("R1".to_string()),
            KiCadUuidPathKey::from_pcb_path("/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")?,
        );
        pcb_refdes_to_anchor_key.insert(
            KiCadRefDes::from("U1".to_string()),
            KiCadUuidPathKey::from_pcb_path(
                "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555",
            )?,
        );

        let parsed = parse_kicad_sexpr_netlist(netlist, &pcb_refdes_to_anchor_key)?;
        assert_eq!(parsed.components.len(), 2);
        assert_eq!(parsed.nets.len(), 1);

        assert!(
            parsed
                .components
                .contains_key(&KiCadUuidPathKey::from_pcb_path(
                    "/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
                )?)
        );
        assert!(
            parsed
                .components
                .contains_key(&KiCadUuidPathKey::from_pcb_path(
                    "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555"
                )?)
        );

        let net = parsed
            .nets
            .get(&KiCadNetName::from("VCC".to_string()))
            .expect("missing net");
        assert!(net.ports.contains(&ImportNetPort {
            component: KiCadUuidPathKey::from_pcb_path("/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")?,
            pin: KiCadPinNumber::from("1".to_string())
        }));
        assert!(net.ports.contains(&ImportNetPort {
            component: KiCadUuidPathKey::from_pcb_path(
                "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555",
            )?,
            pin: KiCadPinNumber::from("3".to_string())
        }));

        Ok(())
    }

    #[test]
    fn select_schematic_symbol_key_prefers_path_matching_netlist() -> Result<()> {
        let symbol_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        // Key from "/root" collapses to sheetpath "/" (root UUID stripped) => "/<symbol_uuid>"
        let root_path = "/11111111-2222-3333-4444-555555555555".to_string();
        // Key from "/root/sheet" => sheetpath "/sheet/" => "/sheet/<symbol_uuid>"
        let other_path =
            "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555"
                .to_string();

        let instance_paths = vec![other_path.clone(), root_path.clone()];

        let mut netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData> =
            BTreeMap::new();
        netlist_components.insert(
            KiCadUuidPathKey {
                sheetpath_tstamps: "/".to_string(),
                symbol_uuid: symbol_uuid.to_string(),
            },
            ImportComponentData {
                netlist: ImportNetlistComponent {
                    refdes: KiCadRefDes::from("R1".to_string()),
                    value: None,
                    footprint: None,
                    sheetpath_names: Some("/".to_string()),
                    unit_pcb_paths: vec![],
                },
                schematic: None,
                layout: None,
            },
        );

        let unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey> = BTreeMap::new();

        let selected = select_schematic_symbol_keys(
            &instance_paths,
            symbol_uuid,
            &unit_to_anchor,
            &netlist_components,
        )?;
        let selected = selected
            .into_iter()
            .next()
            .expect("expected to select a matching path");

        assert_eq!(selected.2, root_path);
        assert_eq!(selected.1.sheetpath_tstamps, "/");
        assert_eq!(selected.1.symbol_uuid, symbol_uuid);
        Ok(())
    }

    #[test]
    fn extract_schematic_data_captures_symbol_at_and_rotation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let sch_rel = PathBuf::from("root.kicad_sch");
        let sch_abs = dir.path().join(&sch_rel);

        let schematic = r##"
(kicad_sch
  (version 20230121)
  (generator "eeschema")
  (uuid "root-uuid")
  (lib_symbols
    (symbol "custompower:+1V8"
      (power)
    )
  )
  (symbol
    (lib_id "Device:R")
    (at 10 20 90)
    (unit 1)
    (uuid "sym-a")
    (instances
      (project "demo"
        (path "/root-uuid")
      )
    )
  )
  (symbol
    (lib_id "Device:C")
    (at 30.5 40.25)
    (unit 1)
    (uuid "sym-b")
    (instances
      (project "demo"
        (path "/root-uuid")
      )
    )
  )
  (symbol
    (lib_id "custompower:+1V8")
    (at 1 2 0)
    (unit 1)
    (uuid "sym-pwr")
    (property "Value" "+1V8")
    (instances
      (project "demo"
        (path "/root-uuid"
          (reference "#PWR01")
          (unit 1)
        )
      )
    )
  )
)
"##;
        fs::write(&sch_abs, schematic)?;

        let anchor_a = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "sym-a".to_string(),
        };
        let anchor_b = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "sym-b".to_string(),
        };
        let mut netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData> =
            BTreeMap::new();
        for (anchor, refdes) in [(&anchor_a, "R1"), (&anchor_b, "C1")] {
            netlist_components.insert(
                anchor.clone(),
                ImportComponentData {
                    netlist: ImportNetlistComponent {
                        refdes: KiCadRefDes::from(refdes.to_string()),
                        value: None,
                        footprint: None,
                        sheetpath_names: Some("/".to_string()),
                        unit_pcb_paths: vec![anchor.clone()],
                    },
                    schematic: None,
                    layout: None,
                },
            );
        }

        let unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey> = BTreeMap::new();
        let extracted = extract_kicad_schematic_data(
            dir.path(),
            &[sch_rel],
            &unit_to_anchor,
            &mut netlist_components,
        )?;

        let a = netlist_components
            .get(&anchor_a)
            .and_then(|c| c.schematic.as_ref())
            .and_then(|s| s.units.get(&anchor_a))
            .and_then(|u| u.at.as_ref())
            .expect("missing at for sym-a");
        assert_eq!(a.x, 10.0);
        assert_eq!(a.y, 20.0);
        assert_eq!(a.rot, Some(90.0));

        let b = netlist_components
            .get(&anchor_b)
            .and_then(|c| c.schematic.as_ref())
            .and_then(|s| s.units.get(&anchor_b))
            .and_then(|u| u.at.as_ref())
            .expect("missing at for sym-b");
        assert_eq!(b.x, 30.5);
        assert_eq!(b.y, 40.25);
        assert_eq!(b.rot, None);

        assert!(
            extracted.power_symbol_decls.iter().any(|d| {
                d.lib_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "custompower:+1V8")
                    && d.value.as_deref() == Some("+1V8")
            }),
            "expected to extract a power symbol decl"
        );

        Ok(())
    }

    #[test]
    fn extracts_power_symbol_decls_using_reference_prefix_when_lib_symbols_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let sch_rel = PathBuf::from("root.kicad_sch");
        let sch_abs = dir.path().join(&sch_rel);

        let schematic = r##"
(kicad_sch
  (version 20230121)
  (generator "eeschema")
  (uuid "root-uuid")
  (symbol
    (lib_id "custompower:+1V8")
    (at 1 2 0)
    (unit 1)
    (uuid "sym-pwr")
    (property "Reference" "#PWR01")
    (property "Value" "+1V8")
    (instances
      (project "demo"
        (path "/root-uuid"
          (reference "#PWR01")
          (unit 1)
        )
      )
    )
  )
)
"##;
        fs::write(&sch_abs, schematic)?;

        let mut netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData> =
            BTreeMap::new();
        let unit_to_anchor: BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey> = BTreeMap::new();

        let extracted = extract_kicad_schematic_data(
            dir.path(),
            &[sch_rel],
            &unit_to_anchor,
            &mut netlist_components,
        )?;

        assert!(
            extracted.power_symbol_decls.iter().any(|d| {
                d.lib_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "custompower:+1V8")
                    && d.value.as_deref() == Some("+1V8")
            }),
            "expected to extract a power symbol decl via Reference=#PWR..."
        );

        Ok(())
    }

    fn standalone_selection(
        root: &Path,
        resolved_project_footprints: BTreeMap<String, PathBuf>,
    ) -> ImportSelection {
        ImportSelection {
            board_name: "root".to_string(),
            board_name_source: BoardNameSource::KicadSchArgument,
            files: KicadDiscoveredFiles::default(),
            selected: SelectedKicadFiles {
                kicad_pro: None,
                kicad_sch: PathBuf::from("root.kicad_sch"),
                kicad_pcb: None,
            },
            portable: PortableKicadProject {
                project_dir: root.to_path_buf(),
                project_name: "root".to_string(),
                source_kind: ImportSourceKind::Schematic,
                kicad_pro_rel: None,
                root_schematic_rel: PathBuf::from("root.kicad_sch"),
                primary_kicad_pcb_rel: None,
                schematic_files_rel: vec![PathBuf::from("root.kicad_sch")],
                files_to_bundle_rel: vec![PathBuf::from("root.kicad_sch")],
                resolved_project_footprints,
                extra_files_to_bundle: Vec::new(),
                manifest_json: "{}".to_string(),
            },
        }
    }

    fn component(refdes: &str, fpid: Option<&str>, uuid: &str) -> ImportComponentData {
        let anchor = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: uuid.to_string(),
        };
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from(refdes.to_string()),
                value: Some("value".to_string()),
                footprint: fpid.map(ToOwned::to_owned),
                sheetpath_names: Some("/".to_string()),
                unit_pcb_paths: vec![anchor],
            },
            schematic: None,
            layout: None,
        }
    }
    #[test]
    fn accepts_padless_mechanical_footprint() -> Result<()> {
        let pads = parse_standalone_footprint_pads(
            r#"(footprint "oshw-logo" (layer "F.Cu")
                (fp_rect (start 0 0) (end 1 1) (stroke (width 0.1) (type default)) (fill none) (layer "F.SilkS")))"#,
        )?;
        assert!(pads.is_empty());
        Ok(())
    }
    #[test]
    fn standalone_standard_footprint_preserves_kicad_id() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let selection = standalone_selection(dir.path(), BTreeMap::new());
        let anchor = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "r1".to_string(),
        };
        let mut components = BTreeMap::from([(
            anchor.clone(),
            component("R1", Some("Resistor_SMD:R_0402_1005Metric"), "r1"),
        )]);

        resolve_standalone_footprints(&selection, &mut components)?;
        let footprint = components
            .get(&anchor)
            .and_then(|component| component.layout.as_ref())
            .expect("resolved footprint");
        assert_eq!(
            footprint.fpid.as_deref(),
            Some("Resistor_SMD:R_0402_1005Metric")
        );
        assert!(matches!(
            &footprint.footprint_geometry,
            ImportFootprintGeometry::StandardLibrary
        ));
        assert!(
            footprint
                .pads
                .contains_key(&KiCadPinNumber::from("1".to_string()))
        );
        assert!(
            footprint
                .pads
                .contains_key(&KiCadPinNumber::from("2".to_string()))
        );
        Ok(())
    }

    #[test]
    fn standalone_project_footprint_copies_exact_geometry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let footprint_path = dir.path().join("Local.pretty/Thing.kicad_mod");
        fs::create_dir_all(footprint_path.parent().unwrap())?;
        let footprint_text = r#"(footprint "Thing"
  (version 20240108)
  (generator pcbnew)
  (pad "1" thru_hole circle (at 0 0) (size 1 1) (drill 0.5) (layers "*.Cu" "*.Mask"))
  (pad "2" thru_hole circle (at 2 0) (size 1 1) (drill 0.5) (layers "*.Cu" "*.Mask")))"#;
        fs::write(&footprint_path, footprint_text)?;
        let selection = standalone_selection(
            dir.path(),
            BTreeMap::from([("Local:Thing".to_string(), footprint_path)]),
        );
        let anchor = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "u1".to_string(),
        };
        let mut components =
            BTreeMap::from([(anchor.clone(), component("U1", Some("Local:Thing"), "u1"))]);

        resolve_standalone_footprints(&selection, &mut components)?;
        let resolved = components[&anchor].layout.as_ref().unwrap();
        assert!(matches!(
            &resolved.footprint_geometry,
            ImportFootprintGeometry::LibraryFile(text) if text == footprint_text
        ));
        assert_eq!(resolved.pads.len(), 2);
        Ok(())
    }

    #[test]
    fn unresolved_footprints_are_retained_for_later_completion() {
        let dir = tempfile::tempdir().unwrap();
        let selection = standalone_selection(dir.path(), BTreeMap::new());
        let a = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "a".to_string(),
        };
        let b = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "b".to_string(),
        };
        let c = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "c".to_string(),
        };
        let mut components = BTreeMap::from([
            (a, component("U1", Some("Missing:One"), "a")),
            (b, component("U2", Some("Missing:One"), "b")),
            (c, component("U3", None, "c")),
        ]);

        resolve_standalone_footprints(&selection, &mut components).unwrap();
        for component in components.values() {
            let layout = component
                .layout
                .as_ref()
                .expect("unresolved footprint record");
            assert!(matches!(
                layout.footprint_geometry,
                ImportFootprintGeometry::Unresolved
            ));
            assert!(layout.pads.is_empty());
            assert!(layout.unresolved_footprint.is_some());
        }
    }

    /// Every footprint failing from one library is a library-registration problem; a few failing is
    /// A component whose schematic names no footprint has no library to register. Grouping it with the
    /// real libraries told the user a library called `<missing footprint>` was not registered, pointing
    /// at `fp-lib-table` when the Footprint field is simply empty in KiCad.
    #[test]
    fn a_component_naming_no_footprint_is_not_reported_as_a_missing_library() {
        let only_missing = BTreeMap::from([(
            NO_FOOTPRINT_KEY.to_string(),
            vec!["R1".to_string(), "R2".to_string()],
        )]);
        let warning = unresolved_footprint_warning(&only_missing, 2, Path::new("/design"), Some(9));

        assert!(
            !warning.contains("library '<missing footprint>'"),
            "must not name the placeholder as a library: {warning}"
        );
        assert!(
            warning.contains("no component names a footprint"),
            "must say what is actually wrong: {warning}"
        );
        assert!(
            !warning.contains("fp-lib-table"),
            "there is no library to register, so the search locations only mislead: {warning}"
        );

        // Mixed: a real library *and* components with no footprint. Both get said, separately.
        let mixed = BTreeMap::from([
            ("SparkFun-Cap:C_0402".to_string(), vec!["C1".to_string()]),
            (NO_FOOTPRINT_KEY.to_string(), vec!["R1".to_string()]),
        ]);
        let warning = unresolved_footprint_warning(&mixed, 4, Path::new("/design"), Some(9));
        assert!(warning.contains("SparkFun-Cap (1 footprint(s), 1 component(s))"));
        assert!(
            !warning.contains("<missing footprint> ("),
            "the placeholder must stay out of the library list: {warning}"
        );
        assert!(
            warning.contains("1 component(s) name no footprint at all"),
            "and be counted on its own: {warning}"
        );
    }

    /// a design quirk. The warning has to say which happened, and name the library either way.
    #[test]
    fn unresolved_warning_names_the_library_and_distinguishes_total_from_partial_failure() {
        let all_unresolved = BTreeMap::from([
            (
                "antmicro-footprints:R_0402".to_string(),
                vec!["R1".to_string(), "R2".to_string()],
            ),
            (
                "antmicro-footprints:C_0402".to_string(),
                vec!["C1".to_string()],
            ),
        ]);
        let warning =
            unresolved_footprint_warning(&all_unresolved, 3, Path::new("/design"), Some(9));
        assert!(warning.starts_with(
            "Warning: 3 component(s) have 2 unresolved footprint definition(s); every component's footprint is unresolved and they all come from library 'antmicro-footprints'"
        ));
        assert!(warning.contains(
            "Unresolved libraries: antmicro-footprints (2 footprint(s), 3 component(s))"
        ));
        // Built with `join` so the assertion holds wherever the path separator differs.
        assert!(warning.contains(&format!(
            "the project fp-lib-table ({})",
            Path::new("/design").join("fp-lib-table").display()
        )));
        assert!(warning.contains("the global KiCad fp-lib-table (KICAD_CONFIG_HOME"));
        assert!(warning.contains("the cached kicad-footprints package for KiCad 9"));
        assert!(warning.contains("connectivity intact but is not layout-ready"));
        assert!(warning.contains("re-import with --force"));

        let partial = BTreeMap::from([("OneOff:Thing".to_string(), vec!["U9".to_string()])]);
        let warning = unresolved_footprint_warning(&partial, 40, Path::new("/design"), None);
        assert!(warning.contains(
            "39 of 40 component(s) resolved, so this is a per-footprint gap rather than a library that is missing entirely"
        ));
        assert!(warning.contains("Unresolved libraries: OneOff (1 footprint(s), 1 component(s))"));
        assert!(
            warning.contains("the cached kicad-footprints package matching the schematic's KiCad")
        );
    }

    #[test]
    fn ambiguous_netlist_identities_report_paths_and_refdeses_together() -> Result<()> {
        let netlist = r#"
(export (version "E")
  (components
    (comp (ref "R1") (value "1k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (sheetpath (names "/") (tstamps "/")) (tstamps "same"))
    (comp (ref "R2") (value "2k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (sheetpath (names "/") (tstamps "/")) (tstamps "same"))
    (comp (ref "U1") (value "A") (footprint "Package_DIP:DIP-8_W7.62mm")
      (sheetpath (names "/") (tstamps "/")) (tstamps "u-a"))
    (comp (ref "U1") (value "B") (footprint "Package_DIP:DIP-8_W7.62mm")
      (sheetpath (names "/child/") (tstamps "/child/")) (tstamps "u-b")))
  (nets))
"#;
        let root = pcb_sexpr::parse(netlist)?;
        let error = parse_kicad_sexpr_netlist_components(&root, &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("path /same maps to refdeses R1, R2"));
        assert!(error.contains("refdes U1 maps to paths /u-a, /child/u-b"));
        Ok(())
    }
}
