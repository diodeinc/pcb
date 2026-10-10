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
    staged_root: &Path,
) -> Result<ImportIr> {
    let source_pcb = validation.summary.selected.kicad_pcb.as_deref();
    let pcb_refdes_to_anchor_key = source_pcb
        .map(|pcb| {
            extract_kicad_pcb_refdes_to_anchor_key(staged_root, &paths.kicad_project_root, pcb)
        })
        .transpose()?
        .unwrap_or_default();

    let mut netlist = extract_kicad_netlist(
        staged_root,
        &validation.summary.selected,
        &pcb_refdes_to_anchor_key,
    )?;

    let schematic =
        extract_kicad_schematic_data(&selection.portable.schematic, &mut netlist.components)?;
    netlist.unit_to_anchor = netlist
        .components
        .iter()
        .flat_map(|(anchor, component)| {
            component
                .netlist
                .unit_pcb_paths
                .iter()
                .map(move |key| (key.clone(), anchor.clone()))
        })
        .collect();

    let schematic_sheet_tree = build_schematic_sheet_tree(
        &validation.summary.selected.kicad_sch,
        &netlist.components,
        &schematic.sheet_symbols,
    );

    let layout_pcb = source_pcb
        .map(|relative| staged_root.join(relative))
        .or_else(|| {
            let retained = paths
                .project_dir()
                .join(selection.selected.kicad_sch.with_extension("kicad_pcb"));
            retained.is_file().then_some(retained)
        });
    if let Some(pcb) = layout_pcb {
        let native_unit_anchors = source_pcb.is_some().then_some(&netlist.unit_to_anchor);
        extract_kicad_layout_data(&pcb, native_unit_anchors, &mut netlist.components)?;
    }
    // Missing PCB footprints are source parity findings, not missing schematic components.
    // Reuse standalone resolution without replacing any board-embedded geometry.
    resolve_standalone_footprints(selection, staged_root, &mut netlist.components)?;

    Ok(ImportIr {
        components: netlist.components,
        nets: netlist.nets,
        schematic_lib_symbol_ids: schematic.lib_symbol_ids,
        schematic_power_symbol_decls: schematic.power_symbol_decls,
        schematic_sheet_tree,
        hierarchy_plan: ImportHierarchyPlan::default(),
        semantic: ImportSemanticAnalysis::default(),
    })
}

#[derive(Debug)]
struct KiCadSchematicExtraction {
    lib_symbol_ids: BTreeSet<KiCadLibId>,
    power_symbol_decls: Vec<ImportSchematicPowerSymbolDecl>,
    sheet_symbols: SheetSymbols,
}

#[derive(Debug, Clone)]
struct SchematicSheetSymbol {
    sheet_name: Option<String>,
    /// Resolved schematic file path relative to the project root when possible.
    sheet_file: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct SheetSymbols(BTreeMap<(PathBuf, String), SchematicSheetSymbol>);

impl SheetSymbols {
    fn resolve<'a>(
        &self,
        root_file: &Path,
        sheet_uuids: impl IntoIterator<Item = &'a str>,
    ) -> Option<&SchematicSheetSymbol> {
        let mut file = Some(root_file);
        let mut sheet = None;
        for uuid in sheet_uuids {
            let found = self.0.get(&(file?.to_path_buf(), uuid.to_string()))?;
            file = found.sheet_file.as_deref();
            sheet = Some(found);
        }
        sheet
    }
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
    staged_root: &Path,
    source_root: &Path,
    kicad_pcb: &Path,
) -> Result<BTreeMap<KiCadRefDes, KiCadUuidPathKey>> {
    let staged_pcb = staged_root.join(kicad_pcb);
    let source_pcb = source_root.join(kicad_pcb);
    if !staged_pcb.exists() {
        anyhow::bail!("PCB file not found: {}", source_pcb.display());
    }

    let text = fs::read_to_string(&staged_pcb)
        .with_context(|| format!("Failed to read {}", source_pcb.display()))?;
    parse_kicad_pcb_refdes_to_anchor_key(&text).with_context(|| {
        format!(
            "Failed to parse KiCad PCB file for refdes/path anchors: {}",
            source_pcb.display()
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
    schematic: &pcb_kicad_sch::LoadedProject,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<KiCadSchematicExtraction> {
    let pages = &schematic.document.pages;
    let root = pages
        .first()
        .context("Imported project has no root schematic")?;
    let root_file = PathBuf::from(root.file_name.as_deref().unwrap_or_default());
    let page_file = |page: &pcb_kicad_sch::SchPage| {
        PathBuf::from(page.file_name.as_deref().unwrap_or_default())
    };

    let mut sheet_symbols = SheetSymbols::default();
    for page in pages {
        let file = page.file_name.as_deref().unwrap_or_default();
        for sheet in page.items.iter().filter_map(|item| match item {
            pcb_kicad_sch::SchItem::Sheet(sheet) if sheet.placed => Some(sheet),
            _ => None,
        }) {
            let entry = SchematicSheetSymbol {
                sheet_name: sheet.name.as_ref().map(|name| name.value.clone()),
                sheet_file: Some(PathBuf::from(pcb_kicad_sch::sheet_file(
                    file,
                    sheet.file_name(),
                )?)),
            };
            sheet_symbols
                .0
                .entry((PathBuf::from(file), sheet.id.clone()))
                .or_insert(entry);
        }
    }

    let refdes_to_anchor = netlist_components
        .iter()
        .map(|(key, component)| (component.netlist.refdes.clone(), key.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut power_symbol_decls = Vec::new();
    for page in pages {
        let file = page_file(page);
        let targets_file = |instance_path: &str| {
            instance_path_targets_file(instance_path, &file, &root_file, &sheet_symbols)
        };
        for symbol in page.items.iter().filter_map(|item| match item {
            pcb_kicad_sch::SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        }) {
            let properties: BTreeMap<String, String> = symbol
                .fields
                .values()
                .map(|field| (field.name.clone(), field.value.clone()))
                .collect();
            let lib_id = KiCadLibId::from(symbol.lib_id.clone());
            let at = Some(ImportSchematicAt {
                x: symbol.at.x,
                y: symbol.at.y,
                rot: Some(symbol.rotation.degrees() as f64),
            });
            let mirror = symbol.mirror.map(|axis| {
                match axis {
                    pcb_kicad_sch::MirrorAxis::X => "x",
                    pcb_kicad_sch::MirrorAxis::Y => "y",
                }
                .to_string()
            });
            // KiCad marks power symbols in the library definition, not on placed instances.
            let is_power_symbol = page
                .library
                .definitions
                .get(&symbol.lib_id)
                .and_then(|definition| definition.sexpr.as_list())
                .is_some_and(|items| sexpr_kicad::child_list(items, "power").is_some())
                || symbol.lib_id.starts_with("power:")
                || symbol
                    .reference()
                    .is_some_and(|reference| reference.trim_start().starts_with("#PWR"));

            if is_power_symbol {
                // Power symbols are usually absent from the KiCad netlist export.
                let sheet_paths: BTreeSet<KiCadSheetPath> = if symbol.instances.is_empty() {
                    BTreeSet::from([KiCadSheetPath::root()])
                } else {
                    symbol
                        .instances
                        .iter()
                        .filter(|instance| targets_file(&instance.path))
                        .filter_map(|instance| {
                            key_from_schematic_instance_path(&instance.path, &symbol.id).ok()
                        })
                        .map(|key| KiCadSheetPath::from_sheetpath_tstamps(&key.sheetpath_tstamps))
                        .collect()
                };
                power_symbol_decls.extend(sheet_paths.into_iter().map(|sheet_path| {
                    ImportSchematicPowerSymbolDecl {
                        schematic_file: file.clone(),
                        sheet_path,
                        symbol_uuid: Some(symbol.id.clone()),
                        at: at.clone(),
                        mirror: mirror.clone(),
                        reference: properties.get("Reference").cloned(),
                        lib_id: Some(lib_id.clone()),
                        value: properties.get("Value").cloned(),
                    }
                }));
            }

            let pins = symbol
                .pins
                .iter()
                .map(|pin| (pin.number.clone(), pin.id.clone()))
                .collect::<BTreeMap<_, _>>();
            let exclude_from_sim =
                symbol
                    .unsupported
                    .iter()
                    .find_map(|item| match item.as_list()? {
                        [tag, value] if tag.as_sym() == Some("exclude_from_sim") => {
                            match value.as_sym()? {
                                "yes" => Some(true),
                                "no" => Some(false),
                                _ => None,
                            }
                        }
                        _ => None,
                    });
            for instance in &symbol.instances {
                // A reused file can retain instances from other projects. Their references
                // are unrelated even when their symbol UUID or reference happens to match.
                if instance.path.trim_matches('/').split('/').next() != Some(root.id.as_str())
                    || !targets_file(&instance.path)
                {
                    continue;
                }
                let Some(anchor) = instance.reference.as_ref().and_then(|reference| {
                    refdes_to_anchor.get(&KiCadRefDes::from(reference.clone()))
                }) else {
                    continue;
                };
                let Some(entry) = netlist_components.get_mut(anchor) else {
                    debug!(
                        "Schematic symbol {} is not present in the netlist; skipping",
                        anchor.pcb_path()
                    );
                    continue;
                };
                let key = key_from_schematic_instance_path(&instance.path, &symbol.id)?;
                let unit = ImportSchematicUnit {
                    lib_name: symbol.lib_name.clone(),
                    lib_id: Some(lib_id.clone()),
                    unit: Some(i64::from(instance.unit.unwrap_or(symbol.unit))),
                    at: at.clone(),
                    mirror: mirror.clone(),
                    in_bom: Some(symbol.in_bom),
                    on_board: Some(symbol.on_board),
                    dnp: Some(symbol.dnp),
                    exclude_from_sim,
                    instance_path: Some(instance.path.clone()),
                    properties: properties.clone(),
                    pins: (!pins.is_empty()).then(|| pins.clone()),
                };
                entry
                    .schematic
                    .get_or_insert_with(|| ImportSchematicComponent {
                        units: BTreeMap::new(),
                    })
                    .units
                    .insert(key, unit);
            }
        }
    }

    // The exported netlist has only one sheetpath per physical component and may omit
    // UUIDs of units on other sheets. Replace its provisional paths with actual instances.
    for component in netlist_components.values_mut() {
        if let Some(schematic) = &component.schematic {
            component.netlist.unit_pcb_paths = schematic.units.keys().cloned().collect();
        }
    }

    Ok(KiCadSchematicExtraction {
        lib_symbol_ids: pages
            .iter()
            .flat_map(|page| page.library.definitions.keys())
            .map(|lib_id| KiCadLibId::from(lib_id.clone()))
            .collect(),
        power_symbol_decls,
        sheet_symbols,
    })
}

fn instance_path_targets_file(
    instance_path: &str,
    file: &Path,
    root_file: &Path,
    sheet_symbols: &SheetSymbols,
) -> bool {
    let sheet_uuids = instance_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .skip(1)
        .collect::<Vec<_>>();
    if sheet_uuids.is_empty() {
        return file == root_file;
    }
    sheet_symbols
        .resolve(root_file, sheet_uuids)
        .and_then(|sheet| sheet.sheet_file.as_deref())
        .is_none_or(|sheet_file| sheet_file == file)
}

fn build_schematic_sheet_tree(
    root_schematic_rel: &Path,
    netlist_components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    sheet_symbols: &SheetSymbols,
) -> ImportSheetTree {
    let root_file = pcb_kicad_sch::normalize_schematic_path(root_schematic_rel);
    let mut all_paths: BTreeSet<KiCadSheetPath> = BTreeSet::new();
    all_paths.insert(KiCadSheetPath::root());

    for key in netlist_components.iter().flat_map(|(anchor, component)| {
        std::iter::once(anchor).chain(component.netlist.unit_pcb_paths.iter())
    }) {
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
        let (sheet_name, schematic_file) = sheet_symbols
            .resolve(&root_file, path.segments())
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
    pcb_path: &Path,
    native_unit_anchors: Option<&BTreeMap<KiCadUuidPathKey, KiCadUuidPathKey>>,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<()> {
    let pcb_text = fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read {}", pcb_path.display()))?;

    let root = pcb_sexpr::parse(&pcb_text).context("Failed to parse KiCad PCB as S-expression")?;

    let footprints =
        sexpr_board::extract_keyed_footprints(&root).map_err(|e| anyhow::anyhow!(e))?;
    // Netlist extraction already rejects duplicate references. A retained PCB's
    // UUID paths are Zener sync hooks, not source schematic anchors; references
    // let us reuse its geometry without replacing schematic-derived identity.
    // Source project PCBs can additionally join by native unit path when their
    // reference is stale. Never interpret retained sync hooks as native paths.
    let anchors_by_refdes = netlist_components
        .iter()
        .map(|(key, component)| (component.netlist.refdes.as_str().to_owned(), key.clone()))
        .collect::<BTreeMap<_, _>>();

    for fp in footprints {
        let by_reference = fp
            .properties
            .get("Reference")
            .and_then(|refdes| anchors_by_refdes.get(refdes));
        let by_native_path = native_unit_anchors
            .and_then(|anchors| anchors.get(&KiCadUuidPathKey::from_pcb_path(&fp.path).ok()?));
        if let (Some(reference), Some(native_path)) = (by_reference, by_native_path) {
            anyhow::ensure!(
                reference == native_path,
                "PCB footprint reference {} conflicts with native schematic path {}",
                fp.properties["Reference"],
                fp.path
            );
        }
        let Some(key) = by_reference.or(by_native_path) else {
            // Ignore footprints we can't join against netlist-derived component identities.
            continue;
        };
        let component = netlist_components.get_mut(key).expect("indexed component");

        let sexpr = pcb_text.get(fp.span.start..fp.span.end).with_context(|| {
            format!(
                "Failed to slice footprint S-expression span {}..{} from {}",
                fp.span.start,
                fp.span.end,
                pcb_path.display()
            )
        })?;

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
            footprint_geometry: ImportFootprintGeometry::LibraryFile(
                sexpr_board::transform_board_instance_footprint_to_standalone(sexpr, &root)
                    .map_err(|e| anyhow::anyhow!(e))
                    .with_context(|| {
                        format!(
                            "Failed to transform footprint for {}",
                            component.netlist.refdes.as_str()
                        )
                    })?,
            ),
        };

        anyhow::ensure!(
            component.layout.replace(layout).is_none(),
            "PCB contains multiple footprints for {}",
            component.netlist.refdes.as_str()
        );
    }

    Ok(())
}

fn resolve_standalone_footprints(
    selection: &ImportSelection,
    staged_root: &Path,
    components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<()> {
    // Installed binaries do not necessarily have the repository's `lib/std` source tree nearby.
    // Treat that footprint source as one optional lookup location rather than making it a
    // prerequisite for project/global libraries, the package cache, or unresolved import.
    let stdlib_footprints = pcb_zen_core::stdlib::native::discover_source()
        .ok()
        .map(|root| root.join("kicad-footprints"));
    let schematic_path = staged_root.join(&selection.portable.root_schematic_rel);
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

    for component in components
        .values_mut()
        .filter(|component| component.layout.is_none())
    {
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
                // Ordered by precedence: project/global KiCad library tables, the repository
                // stdlib source when available, then the version-matched KiCad cache.
                // `copy_to_component` says whether the geometry must be copied into the generated
                // package; the stdlib is a library reference the built board already resolves.
                let project_lookup = selection
                    .portable
                    .resolved_project_footprints
                    .get(fpid)
                    .map(|path| (path.clone(), true));
                let lookup = if project_lookup.is_some()
                    || selection.portable.project_footprint_ids.contains(fpid)
                {
                    project_lookup
                } else {
                    stdlib_footprints
                        .as_deref()
                        .and_then(|root| resolve_footprint_in_root(root, fpid))
                        .map(|path| (path, false))
                        .or_else(|| {
                            resolve_footprint_from_roots(&cached_roots, fpid)
                                .map(|path| (path, true))
                        })
                };
                let Some((path, copy_to_component)) = lookup else {
                    resolved_by_fpid.insert(fpid.to_string(), None);
                    unresolved
                        .entry(fpid.to_string())
                        .or_default()
                        .push(component.netlist.refdes.as_str().to_string());
                    component.layout = Some(unresolved_layout_component(Some(fpid)));
                    continue;
                };
                let staged_path = path
                    .strip_prefix(&selection.portable.project_dir)
                    .map(|relative| staged_root.join(relative))
                    .unwrap_or_else(|_| path.clone());
                let footprint_text = fs::read_to_string(&staged_path)
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
        eprintln!("{}", unresolved_footprint_warning(&unresolved));
    }

    Ok(())
}

const UNRESOLVED_FOOTPRINT_LIST_LIMIT: usize = 8;
const NO_FOOTPRINT_KEY: &str = "<missing footprint>";

fn unresolved_footprint_warning(unresolved: &BTreeMap<String, Vec<String>>) -> String {
    let component_count = unresolved.values().map(Vec::len).sum::<usize>();
    let mut listed = unresolved
        .keys()
        .take(UNRESOLVED_FOOTPRINT_LIST_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = unresolved
        .len()
        .saturating_sub(UNRESOLVED_FOOTPRINT_LIST_LIMIT);
    if omitted > 0 {
        listed.push_str(&format!(", and {omitted} more"));
    }

    format!(
        "Warning: {component_count} component(s) have unresolved footprints: {listed}. The board imported with connectivity intact but is not layout-ready."
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
    staged_root: &Path,
    selected: &SelectedKicadFiles,
    pcb_refdes_to_anchor_key: &BTreeMap<KiCadRefDes, KiCadUuidPathKey>,
) -> Result<KiCadNetlistExtraction> {
    let kicad_sch_abs = staged_root.join(&selected.kicad_sch);
    let netlist_text = export_kicad_sexpr_netlist(&kicad_sch_abs, staged_root)
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

        // Provisional owning-sheet paths only: the netlist does not describe every
        // unit's sheet. Schematic extraction replaces these before layout binding.
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
    fn layout_import_rejects_conflicting_native_identity_but_ignores_retained_sync_paths()
    -> Result<()> {
        let board = r#"(kicad_pcb
            (version 20260206) (generator "pcbnew")
            (footprint "Small" (path "/a") (property "Reference" "R2")
                (pad "1" smd rect (size 1 1)))
            (footprint "Large" (path "/b") (property "Reference" "R1")
                (pad "1" smd rect (size 4 4))))"#;
        let netlist = r#"(export
            (components
                (comp (ref "R1") (sheetpath (tstamps "/")) (tstamps "a"))
                (comp (ref "R2") (sheetpath (tstamps "/")) (tstamps "b")))
            (nets))"#;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("board.kicad_pcb");
        fs::write(&path, board)?;
        let pcb_anchors = parse_kicad_pcb_refdes_to_anchor_key(board)?;
        let mut source = parse_kicad_sexpr_netlist(netlist, &pcb_anchors)?;
        let error =
            extract_kicad_layout_data(&path, Some(&source.unit_to_anchor), &mut source.components)
                .expect_err("source paths and swapped references must not exchange geometry");
        assert!(
            error
                .to_string()
                .contains("conflicts with native schematic path")
        );

        // Retained board paths are sync hooks, not native identities. Even a
        // coincidental source-path match must not override the reference join.
        let mut retained = parse_kicad_sexpr_netlist(netlist, &BTreeMap::new())?;
        extract_kicad_layout_data(&path, None, &mut retained.components)?;
        for (native_path, expected) in [("/a", "Large"), ("/b", "Small")] {
            assert_eq!(
                retained.components[&KiCadUuidPathKey::from_pcb_path(native_path)?]
                    .layout
                    .as_ref()
                    .unwrap()
                    .fpid
                    .as_deref(),
                Some(expected)
            );
        }
        Ok(())
    }

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

    fn page(uuid: &str, body: &str) -> String {
        format!(
            r##"(kicad_sch (version 20260306) (generator "eeschema") (uuid "{uuid}") (paper "A4")
            (lib_symbols (symbol "custompower:+1V8" (power) (property "Reference" "#PWR" (at 0 0 0)))) {body})"##
        )
    }

    fn sheet(uuid: &str, name: &str, file: &str) -> String {
        format!(
            r#"(sheet (at 0 0) (size 10 10) (uuid "{uuid}")
            (property "Sheetname" "{name}" (at 0 0 0)) (property "Sheetfile" "{file}" (at 0 0 0)))"#
        )
    }

    fn symbol(
        lib_id: &str,
        uuid: &str,
        at: &str,
        properties: &[(&str, &str)],
        paths: &str,
    ) -> String {
        let properties = properties
            .iter()
            .map(|(name, value)| format!(r#"(property "{name}" "{value}" (at 0 0 0))"#))
            .collect::<String>();
        format!(
            r#"(symbol (lib_id "{lib_id}") (at {at}) (unit 1) (uuid "{uuid}") {properties}
            (instances {paths}))"#
        )
    }

    fn load(files: &[(&str, String)]) -> Result<pcb_kicad_sch::LoadedProject> {
        let files = files.iter().cloned().collect::<BTreeMap<_, _>>();
        pcb_kicad_sch::load_project("root.kicad_pro", |path| Ok(files.get(path).cloned()))
    }

    #[test]
    fn extraction_keeps_reused_sheet_instances_and_foreign_projects_separate() -> Result<()> {
        let schematic = load(&[
            (
                "root.kicad_sch",
                page(
                    "root",
                    &[
                        sheet("sheet-a", "A", "child.kicad_sch"),
                        sheet("sheet-b", "B", "child.kicad_sch"),
                        symbol(
                            "Device:U",
                            "second-unit",
                            "0 0 0",
                            &[("Reference", "U2")],
                            r#"(project "demo" (path "/root" (reference "U1") (unit 2)))"#,
                        ),
                    ]
                    .concat(),
                ),
            ),
            (
                "child.kicad_sch",
                page(
                    "child",
                    &symbol(
                        "Device:U",
                        "shared-symbol",
                        "0 0 0",
                        &[("Reference", "U1")],
                        r#"(project "demo"
                            (path "/root/sheet-a" (reference "U1") (unit 1))
                            (path "/root/sheet-b" (reference "U2") (unit 2)))
                        (project "foreign"
                            (path "/other-root/sheet-a" (reference "U2") (unit 1)))"#,
                    ),
                ),
            ),
        ])?;
        // KiCad can omit the cross-sheet unit from the exported UUID list altogether.
        // Also exercise a PCB anchor on that other sheet, rather than the netlist's sheet.
        for pcb_anchor in [None, Some("/second-unit")] {
            let mut pcb_anchors = BTreeMap::new();
            if let Some(path) = pcb_anchor {
                pcb_anchors.insert(
                    KiCadRefDes::from("U1".to_string()),
                    KiCadUuidPathKey::from_pcb_path(path)?,
                );
            }
            let mut netlist = parse_kicad_sexpr_netlist(
                r#"(export (components
            (comp (ref "U1") (sheetpath (tstamps "/sheet-a/")) (tstamps "shared-symbol"))
            (comp (ref "U2") (sheetpath (tstamps "/sheet-b/")) (tstamps "shared-symbol")))
            (nets))"#,
                &pcb_anchors,
            )?;
            extract_kicad_schematic_data(&schematic, &mut netlist.components)?;
            for (anchor, expected) in [
                (
                    pcb_anchor.unwrap_or("/sheet-a/shared-symbol"),
                    vec![
                        ("/second-unit", Some(2)),
                        ("/sheet-a/shared-symbol", Some(1)),
                    ],
                ),
                (
                    "/sheet-b/shared-symbol",
                    vec![("/sheet-b/shared-symbol", Some(2))],
                ),
            ] {
                let component = &netlist.components[&KiCadUuidPathKey::from_pcb_path(anchor)?];
                let units = &component.schematic.as_ref().unwrap().units;
                assert_eq!(
                    units
                        .iter()
                        .map(|(key, unit)| (key.pcb_path(), unit.unit))
                        .collect::<Vec<_>>(),
                    expected
                        .iter()
                        .map(|(key, unit)| (key.to_string(), *unit))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    component.netlist.unit_pcb_paths,
                    units.keys().cloned().collect::<Vec<_>>()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn extraction_resolves_sheet_files_through_copied_parents() -> Result<()> {
        let leaf = |uuid: &str, reference: &str, paths: &str| {
            page(
                "leaf",
                &symbol(
                    "Device:R",
                    uuid,
                    "0 0 0",
                    &[("Reference", reference)],
                    paths,
                ),
            )
        };
        let schematic = load(&[
            (
                "root.kicad_sch",
                page(
                    "root",
                    &[
                        sheet("sheet-a", "A", "a.kicad_sch"),
                        sheet("sheet-b", "B", "b.kicad_sch"),
                    ]
                    .concat(),
                ),
            ),
            (
                "a.kicad_sch",
                page("copied", &sheet("nested", "N", "left.kicad_sch")),
            ),
            (
                "b.kicad_sch",
                page("copied", &sheet("nested", "N", "right.kicad_sch")),
            ),
            (
                "left.kicad_sch",
                leaf(
                    "r-left",
                    "R1",
                    r#"(project "demo" (path "/root/sheet-a/nested" (reference "R1") (unit 1)))"#,
                ),
            ),
            (
                "right.kicad_sch",
                leaf(
                    "r-right",
                    "R2",
                    r#"(project "demo"
                        (path "/root/sheet-a/nested" (reference "R1") (unit 1))
                        (path "/root/sheet-b/nested" (reference "R2") (unit 1)))"#,
                ),
            ),
        ])?;
        let mut netlist = parse_kicad_sexpr_netlist(
            r#"(export (components
            (comp (ref "R1") (sheetpath (tstamps "/sheet-a/nested/")) (tstamps "r-left"))
            (comp (ref "R2") (sheetpath (tstamps "/sheet-b/nested/")) (tstamps "r-right")))
            (nets))"#,
            &BTreeMap::new(),
        )?;
        let extracted = extract_kicad_schematic_data(&schematic, &mut netlist.components)?;
        let root = Path::new("root.kicad_sch");
        let tree = build_schematic_sheet_tree(root, &netlist.components, &extracted.sheet_symbols);
        for (anchor, file) in [
            ("/sheet-a/nested/r-left", "left.kicad_sch"),
            ("/sheet-b/nested/r-right", "right.kicad_sch"),
        ] {
            let key = KiCadUuidPathKey::from_pcb_path(anchor)?;
            let units = &netlist.components[&key].schematic.as_ref().unwrap().units;
            assert_eq!(
                units.keys().map(|key| key.pcb_path()).collect::<Vec<_>>(),
                [anchor]
            );
            let sheet = KiCadSheetPath::from_sheetpath_tstamps(&key.sheetpath_tstamps);
            assert_eq!(
                tree.nodes[&sheet].schematic_file.as_deref(),
                Some(Path::new(file))
            );
        }
        Ok(())
    }

    #[test]
    fn extracts_symbol_placement_and_power_symbol_decls() -> Result<()> {
        let instance = |reference: &str| {
            format!(r#"(project "demo" (path "/root-uuid" (reference "{reference}") (unit 1)))"#)
        };
        let schematic = load(&[(
            "root.kicad_sch",
            page(
                "root-uuid",
                &[
                    symbol("Device:R", "sym-a", "10 20 90", &[], &instance("R1")),
                    symbol("Device:C", "sym-b", "30.5 40.25 0", &[], &instance("C1")),
                    // Power by library definition, and by reference prefix alone.
                    symbol(
                        "custompower:+1V8",
                        "sym-pwr",
                        "1 2 0",
                        &[("Value", "+1V8")],
                        &instance("#PWR01"),
                    ),
                    symbol(
                        "other:+3V3",
                        "sym-ref",
                        "1 2 0",
                        &[("Reference", "#PWR02"), ("Value", "+3V3")],
                        &instance("#PWR02"),
                    ),
                ]
                .concat(),
            ),
        )])?;
        let anchor = |uuid: &str| KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: uuid.to_string(),
        };
        let mut netlist_components = [("sym-a", "R1"), ("sym-b", "C1")]
            .into_iter()
            .map(|(uuid, refdes)| {
                (
                    anchor(uuid),
                    ImportComponentData {
                        netlist: ImportNetlistComponent {
                            refdes: KiCadRefDes::from(refdes.to_string()),
                            value: None,
                            footprint: None,
                            sheetpath_names: Some("/".to_string()),
                            unit_pcb_paths: vec![anchor(uuid)],
                        },
                        schematic: None,
                        layout: None,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let extracted = extract_kicad_schematic_data(&schematic, &mut netlist_components)?;

        for (uuid, (x, y, rot)) in [("sym-a", (10.0, 20.0, 90.0)), ("sym-b", (30.5, 40.25, 0.0))] {
            let at = netlist_components[&anchor(uuid)]
                .schematic
                .as_ref()
                .unwrap()
                .units[&anchor(uuid)]
                .at
                .clone()
                .unwrap();
            assert_eq!((at.x, at.y, at.rot), (x, y, Some(rot)));
        }
        let decls = extracted
            .power_symbol_decls
            .iter()
            .map(|decl| {
                (
                    decl.lib_id.as_ref().unwrap().as_str(),
                    decl.value.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decls,
            [
                ("custompower:+1V8", Some("+1V8")),
                ("other:+3V3", Some("+3V3"))
            ]
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
                kicad_pro_rel: None,
                root_schematic_rel: PathBuf::from("root.kicad_sch"),
                kicad_pcb_rel: None,
                schematic: pcb_kicad_sch::LoadedProject {
                    project: serde_json::Value::Null,
                    root_schematics: vec!["root.kicad_sch".to_string()],
                    schematic_files: vec!["root.kicad_sch".to_string()],
                    document: Default::default(),
                },
                files_to_bundle_rel: vec![PathBuf::from("root.kicad_sch")],
                resolved_project_footprints,
                project_footprint_ids: BTreeSet::new(),
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

        resolve_standalone_footprints(&selection, dir.path(), &mut components)?;
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

        resolve_standalone_footprints(&selection, dir.path(), &mut components)?;
        let resolved = components[&anchor].layout.as_ref().unwrap();
        assert!(matches!(
            &resolved.footprint_geometry,
            ImportFootprintGeometry::LibraryFile(text) if text == footprint_text
        ));
        assert_eq!(resolved.pads.len(), 2);
        Ok(())
    }

    #[test]
    fn unresolved_project_footprint_does_not_fall_through_to_stdlib() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut selection = standalone_selection(dir.path(), BTreeMap::new());
        let fpid = "Resistor_SMD:R_0402_1005Metric";
        selection
            .portable
            .project_footprint_ids
            .insert(fpid.to_string());
        let anchor = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: "r1".to_string(),
        };
        let mut components = BTreeMap::from([(anchor.clone(), component("R1", Some(fpid), "r1"))]);

        resolve_standalone_footprints(&selection, dir.path(), &mut components)?;

        let footprint = components[&anchor].layout.as_ref().unwrap();
        assert!(matches!(
            footprint.footprint_geometry,
            ImportFootprintGeometry::Unresolved
        ));
        assert_eq!(
            footprint
                .unresolved_footprint
                .as_ref()
                .and_then(|entry| entry.source_id.as_deref()),
            Some(fpid)
        );
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

        resolve_standalone_footprints(&selection, dir.path(), &mut components).unwrap();
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

    #[test]
    fn unresolved_warning_lists_footprint_ids_and_caps_the_output() {
        let unresolved = (0..10)
            .map(|index| {
                (
                    format!("Missing:Footprint_{index}"),
                    vec![format!("U{index}")],
                )
            })
            .collect::<BTreeMap<_, _>>();
        let warning = unresolved_footprint_warning(&unresolved);

        assert!(warning.starts_with("Warning: 10 component(s) have unresolved footprints:"));
        assert!(warning.contains("Missing:Footprint_0"));
        assert!(warning.contains("and 2 more"));
        assert!(!warning.contains("Looked in:"));
        assert!(warning.contains("connectivity intact but is not layout-ready"));
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
