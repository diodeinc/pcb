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
    let pcb_refdes_to_anchor_key = extract_kicad_pcb_refdes_to_anchor_key(
        &paths.kicad_project_root,
        &validation.summary.selected,
    )?;

    let mut netlist = extract_kicad_netlist(
        &paths.kicad_project_root,
        &validation.summary.selected,
        &pcb_refdes_to_anchor_key,
    )?;

    let schematic_lib_symbols = extract_kicad_schematic_data(
        &paths.kicad_project_root,
        &selection.files.kicad_sch,
        &netlist.unit_to_anchor,
        &mut netlist.components,
    )?;

    extract_kicad_layout_data(
        &paths.kicad_project_root,
        &validation.summary.selected,
        &mut netlist.components,
    )?;

    Ok(ImportIr {
        components: netlist.components,
        nets: netlist.nets,
        schematic_lib_symbols,
    })
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
    let pcb_abs = kicad_project_root.join(&selected.kicad_pcb);
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
) -> Result<BTreeMap<KiCadLibId, String>> {
    let mut lib_symbols: BTreeMap<KiCadLibId, String> = BTreeMap::new();

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

                let rendered = node.to_string();
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

        // Extract placed symbol instances (direct children of the schematic root).
        for sym in root.find_all_lists("symbol") {
            let Some(symbol_uuid) = sexpr_kicad::string_prop(sym, "uuid") else {
                continue;
            };

            let instance_path = sexpr_kicad::schematic_instance_path(sym);
            let Some(instance_path) = instance_path else {
                continue;
            };

            let key = key_from_schematic_instance_path(&instance_path, &symbol_uuid)?;
            let Some(anchor) = unit_to_anchor.get(&key).cloned().or(Some(key.clone())) else {
                continue;
            };

            let unit = sexpr_kicad::int_prop(sym, "unit");
            let lib_id = sexpr_kicad::string_prop(sym, "lib_id").map(KiCadLibId::from);
            let at =
                sexpr_kicad::schematic_at(sym).map(|(x, y, rot)| ImportSchematicAt { x, y, rot });
            let mirror = sexpr_kicad::string_prop(sym, "mirror");

            let in_bom = sexpr_kicad::yes_no_prop(sym, "in_bom");
            let on_board = sexpr_kicad::yes_no_prop(sym, "on_board");
            let dnp = sexpr_kicad::yes_no_prop(sym, "dnp");
            let exclude_from_sim = sexpr_kicad::yes_no_prop(sym, "exclude_from_sim");

            let properties = sexpr_kicad::schematic_properties(sym);
            let pins = sexpr_kicad::schematic_pins(sym);

            let unit_data = ImportSchematicUnit {
                lib_id,
                unit,
                at,
                mirror,
                in_bom,
                on_board,
                dnp,
                exclude_from_sim,
                instance_path: Some(instance_path),
                properties,
                pins,
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

    Ok(lib_symbols)
}

fn extract_kicad_layout_data(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    netlist_components: &mut BTreeMap<KiCadUuidPathKey, ImportComponentData>,
) -> Result<()> {
    let pcb_abs = kicad_project_root.join(&selected.kicad_pcb);
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
            footprint_sexpr: sexpr,
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

        if refdes_to_key
            .insert(refdes.clone(), anchor_key.clone())
            .is_some()
        {
            anyhow::bail!("Duplicate refdes in netlist: {}", refdes.as_str());
        }

        if by_key
            .insert(
                anchor_key.clone(),
                ImportComponentData {
                    netlist: netlist_component,
                    schematic: None,
                    layout: None,
                },
            )
            .is_some()
        {
            debug!(
                "Duplicate netlist component key {}; overwriting",
                anchor_key
            );
        }
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

        assert!(parsed
            .components
            .contains_key(&KiCadUuidPathKey::from_pcb_path(
                "/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
            )?));
        assert!(parsed
            .components
            .contains_key(&KiCadUuidPathKey::from_pcb_path(
                "/11111111-2222-3333-4444-555555555555/99999999-8888-7777-6666-555555555555"
            )?));

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
}
