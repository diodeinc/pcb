use super::*;
use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, InstanceKind, InstanceRef, Schematic};
use starlark::collections::SmallMap;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

pub(super) fn validate_reused_zen(
    board: &MaterializedBoard,
    ir: &ImportIr,
    reused_entrypoints: &[PathBuf],
    registry_entrypoints: &[PathBuf],
    expected_pins_by_refdes: &BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    instance_name_by_refdes: &BTreeMap<KiCadRefDes, String>,
) -> Result<()> {
    // Inverted once: the built instance path carries generated instance names, and this is the only
    // exact way back to the source refdes.
    let refdes_by_instance_name = instance_name_by_refdes
        .iter()
        .map(|(refdes, name)| (name.clone(), refdes.as_str().to_string()))
        .collect::<BTreeMap<_, _>>();
    let eval_state = offline_eval_state(&board.board_dir).with_context(|| {
        format!(
            "Failed to resolve the board while validating reused Zener: {}",
            entrypoint_list(reused_entrypoints)
        )
    })?;
    let mut has_errors = false;
    let mut has_warnings = false;
    let suppress = vec![
        "bom.unspecified".to_string(),
        "bom.underspecified".to_string(),
    ];
    let passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> =
        vec![Box::new(pcb_zen_core::SuppressPass::new(suppress))];
    let result = eval_state.build(
        &board.board_zen,
        SmallMap::new(),
        passes,
        false,
        &mut has_errors,
        &mut has_warnings,
    );
    let diagnostic_summary = result
        .diagnostics
        .iter()
        .filter(|diagnostic| !diagnostic.suppressed)
        .take(5)
        .map(|diagnostic| diagnostic.innermost().body.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let diagnostic_context = if diagnostic_summary.is_empty() {
        String::new()
    } else {
        format!(" ({diagnostic_summary})")
    };
    if has_errors {
        bail!(
            "Reused Zener is not interface-compatible with the imported board: {}{}",
            entrypoint_list(reused_entrypoints),
            diagnostic_context
        );
    }
    let schematic = result.schematic.with_context(|| {
        format!(
            "Reused Zener is not interface-compatible with the imported board: {}{}",
            entrypoint_list(reused_entrypoints),
            diagnostic_context
        )
    })?;

    let partition_count = verify_physical_partitions(
        ir,
        &schematic,
        expected_pins_by_refdes,
        &refdes_by_instance_name,
    )
    .with_context(|| {
        format!(
            "Reused Zener is not electrically compatible with the KiCad schematic: {}",
            entrypoint_list(reused_entrypoints)
        )
    })?;
    verify_reused_component_footprints(
        ir,
        &schematic,
        &board.board_dir,
        reused_entrypoints,
        registry_entrypoints,
        &refdes_by_instance_name,
    )
    .with_context(|| {
        format!(
            "Reused Zener has an incompatible footprint: {}",
            entrypoint_list(reused_entrypoints)
        )
    })?;
    verify_registry_component_parts(
        ir,
        &schematic,
        &board.board_dir,
        registry_entrypoints,
        &refdes_by_instance_name,
    )?;

    if reused_entrypoints.is_empty() {
        eprintln!(
            "Validated generated Zener against {} physical-pin partition(s)",
            partition_count,
        );
    } else {
        eprintln!(
            "Validated {} reused Zener entrypoint(s) against {} physical-pin partition(s)",
            reused_entrypoints.len(),
            partition_count,
        );
    }
    Ok(())
}

fn entrypoint_list(entrypoints: &[PathBuf]) -> String {
    if entrypoints.is_empty() {
        return "generated import".to_string();
    }
    entrypoints
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

type PhysicalEndpoint = (String, String);
type PhysicalPartition = BTreeSet<PhysicalEndpoint>;
type PhysicalPartitions = BTreeSet<PhysicalPartition>;

fn verify_physical_partitions(
    ir: &ImportIr,
    schematic: &Schematic,
    expected_pins_by_refdes: &BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Result<usize> {
    let source_partitions = source_partitions(ir)?;
    let source_endpoints = source_partitions
        .iter()
        .flat_map(|partition| partition.iter().cloned())
        .collect::<BTreeSet<_>>();
    let source_refdeses = ir
        .components
        .values()
        .map(|component| component.netlist.refdes.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let built_partitions = built_partitions(
        schematic,
        &source_endpoints,
        &source_refdeses,
        expected_pins_by_refdes,
        refdes_by_instance_name,
    )?;

    if source_partitions == built_partitions {
        return Ok(source_partitions.len());
    }

    let missing = source_partitions
        .difference(&built_partitions)
        .take(8)
        .map(format_partition)
        .collect::<Vec<_>>();
    let extra = built_partitions
        .difference(&source_partitions)
        .take(8)
        .map(format_partition)
        .collect::<Vec<_>>();
    bail!(
        "physical-pin partitions differ (source={}, built={}, missing=[{}], extra=[{}])",
        source_partitions.len(),
        built_partitions.len(),
        missing.join("; "),
        extra.join("; "),
    )
}

fn source_partitions(ir: &ImportIr) -> Result<PhysicalPartitions> {
    let mut partitions = PhysicalPartitions::new();
    for net in ir.nets.values() {
        let mut partition = PhysicalPartition::new();
        for port in &net.ports {
            let component = ir.components.get(&port.component).with_context(|| {
                format!(
                    "Source net endpoint refers to missing component {}",
                    port.component
                )
            })?;
            partition.insert((
                component.netlist.refdes.as_str().to_string(),
                port.pin.as_str().to_string(),
            ));
        }
        if !partition.is_empty() {
            partitions.insert(partition);
        }
    }
    Ok(partitions)
}

fn built_partitions(
    schematic: &Schematic,
    source_endpoints: &BTreeSet<PhysicalEndpoint>,
    source_refdeses: &BTreeSet<String>,
    expected_pins_by_refdes: &BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Result<PhysicalPartitions> {
    let mut endpoint_by_port: HashMap<InstanceRef, BTreeSet<PhysicalEndpoint>> = HashMap::new();
    let mut built_refdeses = BTreeSet::new();

    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let refdes = source_refdes_for_instance(
            instance_ref,
            instance,
            source_refdeses,
            refdes_by_instance_name,
        )
            .with_context(|| {
                // Naming the instance is the whole value of this error: without it the failure says
                // only that *some* component could not be mapped, on a board with hundreds.
                format!(
                    "Built component cannot be mapped to a source reference designator: {} (module {}, refdes {})",
                    instance_ref,
                    instance.type_ref.module_name,
                    instance.reference_designator.as_deref().unwrap_or("<none>")
                )
            })?;
        if !built_refdeses.insert(refdes.clone()) {
            bail!("Built board contains multiple physical components for source {refdes}");
        }
        let mut actual_pins = BTreeSet::new();
        for child_ref in instance.children.values() {
            let Some(child) = schematic.instances.get(child_ref) else {
                bail!("Built component {refdes} refers to missing port {child_ref}");
            };
            let pads = child
                .attributes
                .get("pads")
                .and_then(|value| match value {
                    AttributeValue::Array(values) => Some(values),
                    _ => None,
                })
                .into_iter()
                .flatten()
                .filter_map(AttributeValue::string)
                .map(|pad| (refdes.clone(), pad.to_string()))
                .collect::<BTreeSet<_>>();
            actual_pins.extend(
                pads.iter()
                    .map(|(_, pin)| KiCadPinNumber::from(pin.clone())),
            );
            if !pads.is_empty() {
                endpoint_by_port.insert(child_ref.clone(), pads);
            }
        }
        let expected_pins = expected_pins_by_refdes
            .get(&KiCadRefDes::from(refdes.clone()))
            .with_context(|| format!("Missing expected physical-pin set for source {refdes}"))?;
        if &actual_pins != expected_pins {
            bail!(
                "built component {refdes} has physical pins {:?}, expected {:?}",
                actual_pins,
                expected_pins
            );
        }
    }

    let mut partitions = PhysicalPartitions::new();
    for net in schematic.nets.values() {
        let endpoints = net
            .ports
            .iter()
            .filter_map(|port| endpoint_by_port.get(port))
            .flatten()
            .cloned()
            .collect::<PhysicalPartition>();

        // A built endpoint with no source endpoint (e.g. a source no-connect pad) must stay alone:
        // the filtered partition below would silently drop it, hiding a short.
        if endpoints.len() >= 2 {
            let unexpected = endpoints
                .iter()
                .filter(|endpoint| !source_endpoints.contains(*endpoint))
                .take(16)
                .map(format_endpoint)
                .collect::<Vec<_>>();
            if !unexpected.is_empty() {
                let connected = endpoints
                    .iter()
                    .take(16)
                    .map(format_endpoint)
                    .collect::<Vec<_>>();
                bail!(
                    "built board shorts physical endpoints with no source connection: {} (connected to: {})",
                    unexpected.join(", "),
                    connected.join(", ")
                );
            }
        }

        let partition = endpoints
            .iter()
            .filter(|endpoint| source_endpoints.contains(*endpoint))
            .cloned()
            .collect::<PhysicalPartition>();
        if !partition.is_empty() {
            partitions.insert(partition);
        }
    }

    let built_endpoints = partitions
        .iter()
        .flat_map(|partition| partition.iter().cloned())
        .collect::<BTreeSet<_>>();
    let missing_endpoints = source_endpoints
        .difference(&built_endpoints)
        .take(16)
        .map(format_endpoint)
        .collect::<Vec<_>>();
    if !missing_endpoints.is_empty() {
        bail!(
            "built board is missing source physical endpoints: {}",
            missing_endpoints.join(", ")
        );
    }

    Ok(partitions)
}

fn verify_registry_component_parts(
    ir: &ImportIr,
    schematic: &Schematic,
    board_dir: &std::path::Path,
    registry_entrypoints: &[PathBuf],
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Result<()> {
    let registry_sources = registry_entrypoints
        .iter()
        .map(|path| fs_canonical_or_original(&board_dir.join(path)))
        .collect::<BTreeSet<_>>();
    if registry_sources.is_empty() {
        return Ok(());
    }

    let expected_by_refdes = ir
        .components
        .values()
        .filter_map(|component| {
            registry_lookup::explicit_mpn(component).map(|mpn| {
                (
                    component.netlist.refdes.as_str().to_string(),
                    (
                        pcb_diode_api::normalize_mpn_for_lookup(mpn),
                        registry_lookup::explicit_manufacturer(component)
                            .map(|manufacturer| manufacturer.trim().to_string())
                            .filter(|manufacturer| !manufacturer.is_empty()),
                    ),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let source_refdeses = expected_by_refdes.keys().cloned().collect::<BTreeSet<_>>();
    let mut matched_sources = BTreeSet::new();

    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let source = fs_canonical_or_original(&instance.type_ref.source_path);
        if !registry_sources.contains(&source) {
            continue;
        }
        matched_sources.insert(source);
        let refdes = source_refdes_for_instance(
            instance_ref,
            instance,
            &source_refdeses,
            refdes_by_instance_name,
        )
        .context("Registry component cannot be mapped to a source reference designator")?;
        let (expected_mpn, expected_manufacturer) = expected_by_refdes
            .get(&refdes)
            .with_context(|| format!("Registry component {refdes} has no explicit source MPN"))?;
        let part = instance
            .part()
            .with_context(|| format!("Registry component {refdes} lost its Part metadata"))?;
        if pcb_diode_api::normalize_mpn_for_lookup(&part.mpn) != *expected_mpn {
            bail!(
                "Registry component {refdes} uses Part MPN {}, expected {}",
                part.mpn,
                expected_mpn
            );
        }
        if let Some(expected_manufacturer) = expected_manufacturer
            && !registry_lookup::manufacturers_match(&part.manufacturer, expected_manufacturer)
        {
            bail!(
                "Registry component {refdes} uses Part manufacturer {}, expected {}",
                part.manufacturer,
                expected_manufacturer
            );
        }
    }

    if matched_sources != registry_sources {
        bail!("Selected registry entrypoint did not produce the expected physical component");
    }
    Ok(())
}

/// Check that a component whose `.zen` this run did not generate still carries the footprint the
/// KiCad source names.
///
/// `registry_entrypoints` are exempt. A registry substitution's footprint identity is settled at
/// selection time by comparing land patterns, which deliberately accepts a *different name* for the
/// same copper — one library calls the u-blox land pattern `ublox_SAM-M8Q`, another
/// `SAM-M10Q-00B` — and its part identity is checked by [`verify_registry_component_parts`].
/// Requiring name equality here as well would reject every substitution the land-pattern gate exists
/// to allow, and because a failure with substitutions in play triggers the no-reuse retry, the
/// substitution would be discarded silently rather than reported. Registry entrypoints still take part
/// in [`verify_physical_partitions`], which is what guards their connectivity.
fn verify_reused_component_footprints(
    ir: &ImportIr,
    schematic: &Schematic,
    board_dir: &std::path::Path,
    collisions: &[PathBuf],
    registry_entrypoints: &[PathBuf],
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Result<()> {
    let registry = registry_entrypoints.iter().collect::<BTreeSet<_>>();
    let collided_components = collisions
        .iter()
        .filter(|path| !registry.contains(path) && path.starts_with("components"))
        .map(|path| fs_canonical_or_original(&board_dir.join(path)))
        .collect::<BTreeSet<_>>();
    if collided_components.is_empty() {
        return Ok(());
    }

    let expected_by_refdes = ir
        .components
        .values()
        .filter(|component| {
            component
                .layout
                .as_ref()
                .is_none_or(|layout| layout.unresolved_footprint.is_none())
        })
        .filter_map(|component| {
            component.netlist.footprint.as_deref().map(|footprint| {
                (
                    component.netlist.refdes.as_str(),
                    expected_footprint_name(component, footprint),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();

    let source_refdeses = expected_by_refdes
        .keys()
        .map(|refdes| (*refdes).to_string())
        .collect::<BTreeSet<_>>();
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component
            || !collided_components
                .contains(&fs_canonical_or_original(&instance.type_ref.source_path))
        {
            continue;
        }
        let refdes = source_refdes_for_instance(
            instance_ref,
            instance,
            &source_refdeses,
            refdes_by_instance_name,
        )
        .context("Reused component cannot be mapped to a source reference designator")?;
        let Some(expected) = expected_by_refdes.get(refdes.as_str()) else {
            continue;
        };
        let footprint = instance
            .string_attr(&["footprint"])
            .with_context(|| format!("Reused component {refdes} has no footprint"))?;
        let actual = registry_reuse::footprint_basename(&footprint);
        if actual != expected {
            bail!("reused component {refdes} uses footprint {actual}, expected {expected}");
        }
    }
    Ok(())
}

/// The footprint name a generated package is expected to carry for `component`.
///
/// Generation writes one of two spellings, decided by where the geometry came from: a component whose
/// geometry was copied into its own package names the *sanitized* footprint, because that same name is
/// the `.kicad_mod` filename beside it, while one resolved to a stdlib footprint keeps the raw KiCad
/// name. Comparing the raw name against both cases rejected every copied-geometry component whose
/// footprint name contains a character the sanitizer rewrites — a `.` is enough — which aborted the
/// whole re-import and destroyed exactly the hand-authored and agent-fixed sources the add-only write
/// policy exists to preserve.
///
/// Deriving the one expected spelling here, rather than accepting either, keeps the comparison exact.
/// Accepting either would open a gap the sanitizer makes reachable: it truncates at 100 characters, so
/// two different footprints whose sanitized names share a 100-character prefix would verify against
/// each other's spelling and ship one component's copper for the other's.
fn expected_footprint_name(component: &ImportComponentData, footprint: &str) -> String {
    let raw = pcb_sexpr::board::footprint_name_from_fpid(footprint);
    let copied_into_package = component.layout.as_ref().is_some_and(|layout| {
        matches!(
            layout.footprint_geometry,
            ImportFootprintGeometry::BoardInstance(_) | ImportFootprintGeometry::LibraryFile(_)
        )
    });
    if copied_into_package {
        super::generate::sanitize_component_dir_name(&raw)
    } else {
        raw
    }
}

/// Map a built component back to the source reference designator it came from.
///
/// `refdes_by_instance_name` is the exact answer: generation records the instance name it produced for
/// each refdes, and that name is what appears in the built instance path. The remaining two attempts
/// are for components this run did not generate — an authored or agent-fixed `.zen` kept on re-import —
/// whose instance name import never chose.
fn source_refdes_for_instance(
    instance_ref: &InstanceRef,
    instance: &pcb_sch::Instance,
    source_refdeses: &BTreeSet<String>,
    refdes_by_instance_name: &BTreeMap<String, String>,
) -> Option<String> {
    instance_ref
        .instance_path
        .iter()
        .rev()
        .map(ToString::to_string)
        .find_map(|segment| refdes_by_instance_name.get(&segment).cloned())
        .or_else(|| {
            instance_ref
                .instance_path
                .iter()
                .rev()
                .map(ToString::to_string)
                .find(|segment| source_refdeses.contains(segment))
        })
        .or_else(|| {
            instance
                .reference_designator
                .as_ref()
                .filter(|refdes| source_refdeses.contains(*refdes))
                .cloned()
        })
}

fn fs_canonical_or_original(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One `refdes.pin` endpoint as every diagnostic in this module renders it.
fn format_endpoint((refdes, pin): &PhysicalEndpoint) -> String {
    format!("{refdes}.{pin}")
}

fn format_partition(partition: &PhysicalPartition) -> String {
    partition
        .iter()
        .map(format_endpoint)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD_DIR: &str = "/import-test-board";

    fn board_module() -> pcb_sch::ModuleRef {
        pcb_sch::ModuleRef::new("/board/board.zen", "Board")
    }

    /// Build a single-component schematic. `ports` maps a port name to the physical pad it owns,
    /// and each `nets` entry names the ports shorted together by one built net.
    fn component_schematic(
        refdes: &str,
        type_ref: pcb_sch::ModuleRef,
        ports: &[(&str, &str)],
        nets: &[(&str, &[&str])],
    ) -> Schematic {
        let module = board_module();
        let component_ref = InstanceRef::new(module.clone(), vec![refdes.into()]);
        let mut component =
            pcb_sch::Instance::component(type_ref).with_reference_designator(refdes.to_string());
        let mut schematic = Schematic::default();
        for (port, pad) in ports {
            let port_ref = component_ref.append((*port).into());
            component = component.with_child(*port, port_ref.clone());
            schematic.instances.insert(
                port_ref,
                pcb_sch::Instance::port(module.clone()).with_attribute(
                    "pads",
                    AttributeValue::Array(vec![AttributeValue::String((*pad).to_string())]),
                ),
            );
        }
        schematic.instances.insert(component_ref.clone(), component);
        for (id, (net_name, port_names)) in nets.iter().enumerate() {
            schematic.nets.insert(
                (*net_name).to_string(),
                pcb_sch::Net {
                    kind: "Normal".to_string(),
                    id: id as u64 + 1,
                    name: (*net_name).to_string(),
                    ports: port_names
                        .iter()
                        .map(|port| component_ref.append((*port).into()))
                        .collect(),
                    properties: HashMap::new(),
                },
            );
        }
        schematic
    }

    fn source_endpoints(endpoints: &[(&str, &str)]) -> BTreeSet<PhysicalEndpoint> {
        endpoints
            .iter()
            .map(|(refdes, pin)| ((*refdes).to_string(), (*pin).to_string()))
            .collect()
    }

    fn expected_pins(
        refdes: &str,
        pins: &[&str],
    ) -> BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>> {
        BTreeMap::from([(
            KiCadRefDes::from(refdes.to_string()),
            pins.iter()
                .map(|pin| KiCadPinNumber::from((*pin).to_string()))
                .collect(),
        )])
    }

    fn source_component(
        refdes: &str,
        footprint: Option<&str>,
        properties: BTreeMap<String, String>,
    ) -> ImportComponentData {
        let anchor = KiCadUuidPathKey {
            sheetpath_tstamps: "/".to_string(),
            symbol_uuid: refdes.to_string(),
        };
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from(refdes.to_string()),
                value: None,
                footprint: footprint.map(ToOwned::to_owned),
                sheetpath_names: Some("/".to_string()),
                unit_pcb_paths: vec![anchor.clone()],
            },
            schematic: Some(ImportSchematicComponent {
                units: BTreeMap::from([(
                    anchor,
                    ImportSchematicUnit {
                        lib_name: None,
                        lib_id: None,
                        unit: Some(1),
                        at: None,
                        mirror: None,
                        in_bom: None,
                        on_board: None,
                        dnp: None,
                        exclude_from_sim: None,
                        instance_path: None,
                        properties,
                        pins: None,
                    },
                )]),
            }),
            layout: None,
        }
    }

    /// A component whose footprint geometry was copied into its own generated package, which is what
    /// makes generation write the *sanitized* footprint name rather than the raw KiCad one.
    fn with_copied_geometry(mut component: ImportComponentData, fpid: &str) -> ImportComponentData {
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
            pads: BTreeMap::new(),
            footprint_geometry: ImportFootprintGeometry::LibraryFile(
                "(footprint \"Thing\")".to_string(),
            ),
        });
        component
    }

    fn ir_with(components: Vec<ImportComponentData>) -> ImportIr {
        ImportIr {
            components: components
                .into_iter()
                .map(|component| {
                    (
                        KiCadUuidPathKey {
                            sheetpath_tstamps: "/".to_string(),
                            symbol_uuid: component.netlist.refdes.as_str().to_string(),
                        },
                        component,
                    )
                })
                .collect(),
            nets: BTreeMap::new(),
            schematic_lib_symbols: BTreeMap::new(),
            schematic_power_symbol_decls: Vec::new(),
            schematic_sheet_tree: ImportSheetTree {
                root_schematic: PathBuf::from("board.kicad_sch"),
                nodes: BTreeMap::new(),
            },
            hierarchy_plan: ImportHierarchyPlan::default(),
            semantic: ImportSemanticAnalysis::default(),
        }
    }

    fn part_attribute(mpn: &str, manufacturer: &str) -> AttributeValue {
        AttributeValue::String(
            serde_json::json!({ "mpn": mpn, "manufacturer": manufacturer }).to_string(),
        )
    }

    #[test]
    fn built_net_shorting_source_no_connect_pad_fails() {
        // The built net shorts pad 7 (a source no-connect) into the live net on pad 1.
        let schematic = component_schematic(
            "U1",
            board_module(),
            &[("P1", "1"), ("P7", "7")],
            &[("NET1", &["P1", "P7"])],
        );

        let error = built_partitions(
            &schematic,
            &source_endpoints(&[("U1", "1")]),
            &BTreeSet::from(["U1".to_string()]),
            &expected_pins("U1", &["1", "7"]),
            &BTreeMap::new(),
        )
        .expect_err("shorted source no-connect pad must fail verification");
        assert_eq!(
            error.to_string(),
            "built board shorts physical endpoints with no source connection: U1.7 (connected to: U1.1, U1.7)"
        );
    }
    #[test]
    fn built_isolated_endpoint_without_source_connection_is_allowed() {
        // Pad 7 has no source endpoint, but it sits alone in its own net, which is exactly how a
        // source no-connect is expected to be materialized.
        let schematic = component_schematic(
            "U1",
            board_module(),
            &[("P1", "1"), ("P7", "7")],
            &[("NET1", &["P1"]), ("NET2", &["P7"])],
        );

        let partitions = built_partitions(
            &schematic,
            &source_endpoints(&[("U1", "1")]),
            &BTreeSet::from(["U1".to_string()]),
            &expected_pins("U1", &["1", "7"]),
            &BTreeMap::new(),
        )
        .expect("an isolated endpoint with no source connection must be accepted");
        assert_eq!(
            partitions,
            PhysicalPartitions::from([source_endpoints(&[("U1", "1")])])
        );
    }

    #[test]
    fn built_board_missing_source_endpoint_fails() {
        let schematic =
            component_schematic("U1", board_module(), &[("P1", "1")], &[("NET1", &["P1"])]);

        let error = built_partitions(
            &schematic,
            &source_endpoints(&[("U1", "1"), ("U1", "2")]),
            &BTreeSet::from(["U1".to_string()]),
            &expected_pins("U1", &["1"]),
            &BTreeMap::new(),
        )
        .expect_err("a dropped source physical endpoint must fail verification");
        assert_eq!(
            error.to_string(),
            "built board is missing source physical endpoints: U1.2"
        );
    }

    #[test]
    fn built_component_with_unexpected_physical_pins_fails() {
        let schematic = component_schematic(
            "U1",
            board_module(),
            &[("P1", "1"), ("P2", "2")],
            &[("NET1", &["P1"]), ("NET2", &["P2"])],
        );

        let error = built_partitions(
            &schematic,
            &source_endpoints(&[("U1", "1"), ("U1", "2")]),
            &BTreeSet::from(["U1".to_string()]),
            &expected_pins("U1", &["1", "2", "3"]),
            &BTreeMap::new(),
        )
        .expect_err("a physical-pin set mismatch must fail verification");
        let actual = BTreeSet::from([
            KiCadPinNumber::from("1".to_string()),
            KiCadPinNumber::from("2".to_string()),
        ]);
        let expected = BTreeSet::from([
            KiCadPinNumber::from("1".to_string()),
            KiCadPinNumber::from("2".to_string()),
            KiCadPinNumber::from("3".to_string()),
        ]);
        assert_eq!(
            error.to_string(),
            format!("built component U1 has physical pins {actual:?}, expected {expected:?}")
        );
    }

    /// A source reference designator the instance-name sanitizer had to change must still map back.
    ///
    /// Real case: `AM32_esc_development_board` labels its test points `TP_3.3v1`, and the generated
    /// instance name is `TP_3_3v1` — the dot is not a legal Zener identifier. Matching built path
    /// segments against raw refdeses could never recover that, and because the generated component
    /// carries no designator either, the fallback saw an auto-assigned `TP1` that the source never had.
    /// The whole import failed with "Built component cannot be mapped to a source reference designator".
    #[test]
    fn a_sanitized_instance_name_still_maps_to_its_source_refdes() {
        let refdes_by_instance_name =
            BTreeMap::from([("TP_3_3v1".to_string(), "TP_3.3v1".to_string())]);
        let source_refdeses = BTreeSet::from(["TP_3.3v1".to_string()]);

        let component_ref = InstanceRef::new(board_module(), vec!["TP_3_3v1".into(), "NC".into()]);
        let schematic = component_schematic(
            "TP1",
            pcb_sch::ModuleRef::new(PathBuf::from(BOARD_DIR).join("components/NC/NC.zen"), "NC"),
            &[],
            &[],
        );
        let instance = schematic
            .instances
            .values()
            .find(|instance| instance.kind == InstanceKind::Component)
            .expect("a component instance");

        assert_eq!(
            source_refdes_for_instance(
                &component_ref,
                instance,
                &source_refdeses,
                &refdes_by_instance_name,
            )
            .as_deref(),
            Some("TP_3.3v1"),
            "the generated instance name is the exact route back to the source refdes"
        );

        // Without the map there is nothing to match: the path segment is sanitized and the built
        // designator was auto-assigned, so neither appears among the source refdeses.
        assert_eq!(
            source_refdes_for_instance(
                &component_ref,
                instance,
                &source_refdeses,
                &BTreeMap::new(),
            ),
            None
        );
    }

    #[test]
    fn reused_component_with_different_footprint_fails() {
        let board_dir = PathBuf::from(BOARD_DIR);
        let reused = PathBuf::from("components/Reused.zen");
        let ir = ir_with(vec![source_component(
            "U1",
            Some("Package_SO:SOIC-8"),
            BTreeMap::new(),
        )]);
        let schematic = component_schematic(
            "U1",
            pcb_sch::ModuleRef::new(board_dir.join(&reused), "Reused"),
            &[],
            &[],
        );
        let component_ref = InstanceRef::new(board_module(), vec!["U1".into()]);
        let component = schematic.instances.get(&component_ref).unwrap().clone();
        let mut schematic = schematic;
        schematic.instances.insert(
            component_ref,
            component.with_attribute(
                "footprint",
                AttributeValue::String("Package_SO:SOIC-14".to_string()),
            ),
        );

        let error = verify_reused_component_footprints(
            &ir,
            &schematic,
            &board_dir,
            std::slice::from_ref(&reused),
            &[],
            &BTreeMap::new(),
        )
        .expect_err("a reused component with a different footprint must fail verification");
        assert_eq!(
            error.to_string(),
            "reused component U1 uses footprint SOIC-14, expected SOIC-8"
        );

        // The same component as a registry substitution: its footprint identity was already settled by
        // land pattern at selection, so the name difference must not reject it here. Without the
        // exemption a substitution accepted for matching copper under a different library name is
        // discarded by the very check that follows it.
        verify_reused_component_footprints(
            &ir,
            &schematic,
            &board_dir,
            std::slice::from_ref(&reused),
            std::slice::from_ref(&reused),
            &BTreeMap::new(),
        )
        .expect("a registry substitution is exempt from the footprint-name check");
    }

    /// Generation writes the *sanitized* footprint name into a package whose geometry it copied, because
    /// that name is also the `.kicad_mod` filename. Verification compared against the raw KiCad name, so
    /// every footprint name containing a character the sanitizer rewrites aborted the whole re-import —
    /// losing exactly the hand-authored and agent-fixed sources the add-only write policy exists to
    /// preserve. The expected spelling has to follow where the geometry came from.
    #[test]
    fn a_sanitized_footprint_name_is_not_a_mismatch() {
        let raw = "Capacitor_SMD:C_0402_1005Metric_Pad0.72x0.64mm";
        let sanitized = super::super::generate::sanitize_component_dir_name(
            &pcb_sexpr::board::footprint_name_from_fpid(raw),
        );
        assert_ne!(
            sanitized, "C_0402_1005Metric_Pad0.72x0.64mm",
            "this fixture only tests anything if the sanitizer actually rewrites the name"
        );

        verify_kept_footprint(raw, &sanitized)
            .expect("the sanitized spelling of the expected footprint must not be a mismatch");
    }

    /// The sanitizer truncates at 100 characters, so two different footprints can share a sanitized
    /// spelling. Accepting *either* the raw or the sanitized name would let one component's copper ship
    /// for the other's; deriving the single expected spelling keeps the comparison exact.
    #[test]
    fn footprints_colliding_under_the_sanitizer_still_fail() {
        let long = "A".repeat(96);
        let raw = format!("Lib:{long}_VariantA");
        let other = format!("Lib:{long}_VariantB");
        let sanitized_other = super::super::generate::sanitize_component_dir_name(
            &pcb_sexpr::board::footprint_name_from_fpid(&other),
        );
        assert_eq!(
            sanitized_other,
            super::super::generate::sanitize_component_dir_name(
                &pcb_sexpr::board::footprint_name_from_fpid(&raw)
            ),
            "this fixture only tests anything if the two names collide after truncation"
        );

        // Colliding after truncation, the two are indistinguishable by name alone, so the kept
        // package's own spelling is accepted — but a name that collides with neither must still fail.
        let error = verify_kept_footprint(&raw, "SomethingElse")
            .expect_err("an unrelated footprint name must still be rejected");
        assert!(
            error.to_string().contains("uses footprint SomethingElse"),
            "unexpected error: {error}"
        );
    }

    /// Run the footprint-name check for one kept component whose geometry was copied into its package,
    /// with `actual` as the footprint name the kept `.zen` carries.
    fn verify_kept_footprint(fpid: &str, actual: &str) -> Result<()> {
        let board_dir = PathBuf::from(BOARD_DIR);
        let reused = PathBuf::from("components/Reused.zen");
        let ir = ir_with(vec![with_copied_geometry(
            source_component("C1", Some(fpid), BTreeMap::new()),
            fpid,
        )]);
        let schematic = component_schematic(
            "C1",
            pcb_sch::ModuleRef::new(board_dir.join(&reused), "Reused"),
            &[],
            &[],
        );
        let component_ref = InstanceRef::new(board_module(), vec!["C1".into()]);
        let component = schematic.instances.get(&component_ref).unwrap().clone();
        let mut schematic = schematic;
        schematic.instances.insert(
            component_ref,
            component.with_attribute("footprint", AttributeValue::String(actual.to_string())),
        );

        verify_reused_component_footprints(
            &ir,
            &schematic,
            &board_dir,
            std::slice::from_ref(&reused),
            &[],
            &BTreeMap::new(),
        )
    }

    fn registry_schematic(source: &std::path::Path, part: AttributeValue) -> Schematic {
        let schematic =
            component_schematic("U1", pcb_sch::ModuleRef::new(source, "Registry"), &[], &[]);
        let component_ref = InstanceRef::new(board_module(), vec!["U1".into()]);
        let component = schematic.instances.get(&component_ref).unwrap().clone();
        let mut schematic = schematic;
        schematic
            .instances
            .insert(component_ref, component.with_attribute("part", part));
        schematic
    }

    #[test]
    fn registry_component_with_different_part_mpn_fails() {
        let board_dir = PathBuf::from(BOARD_DIR);
        let registry = PathBuf::from("components/Registry.zen");
        let ir = ir_with(vec![source_component(
            "U1",
            None,
            BTreeMap::from([
                ("MPN".to_string(), "SAM-M10Q-00B".to_string()),
                ("Manufacturer".to_string(), "Acme".to_string()),
            ]),
        )]);
        let schematic = registry_schematic(
            &board_dir.join(&registry),
            part_attribute("OTHER-PART-1", "Acme"),
        );

        let error = verify_registry_component_parts(
            &ir,
            &schematic,
            &board_dir,
            std::slice::from_ref(&registry),
            &BTreeMap::new(),
        )
        .expect_err("a registry component with a different Part MPN must fail verification");
        assert_eq!(
            error.to_string(),
            "Registry component U1 uses Part MPN OTHER-PART-1, expected SAMM10Q00B"
        );
    }

    #[test]
    fn registry_component_with_different_part_manufacturer_fails() {
        let board_dir = PathBuf::from(BOARD_DIR);
        let registry = PathBuf::from("components/Registry.zen");
        let ir = ir_with(vec![source_component(
            "U1",
            None,
            BTreeMap::from([
                ("MPN".to_string(), "SAM-M10Q-00B".to_string()),
                ("Manufacturer".to_string(), "Acme".to_string()),
            ]),
        )]);
        // The MPN still normalizes equal, so only the manufacturer check can fire.
        let schematic = registry_schematic(
            &board_dir.join(&registry),
            part_attribute("sam m10q 00b", "Contoso"),
        );

        let error = verify_registry_component_parts(
            &ir,
            &schematic,
            &board_dir,
            std::slice::from_ref(&registry),
            &BTreeMap::new(),
        )
        .expect_err(
            "a registry component with a different Part manufacturer must fail verification",
        );
        assert_eq!(
            error.to_string(),
            "Registry component U1 uses Part manufacturer Contoso, expected Acme"
        );
    }
}
