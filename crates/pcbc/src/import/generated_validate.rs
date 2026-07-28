use super::*;
use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, InstanceKind, InstanceRef, Schematic};
use starlark::collections::SmallMap;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) fn validate_generated_zen(
    board: &MaterializedBoard,
    ir: &ImportIr,
    expected_pins_by_refdes: &BTreeMap<KiCadRefDes, BTreeSet<KiCadPinNumber>>,
    instance_name_by_refdes: &BTreeMap<KiCadRefDes, String>,
) -> Result<()> {
    // Inverted once: the built instance path carries generated instance names, and this is the only
    // exact way back to the source refdes.
    let refdes_by_instance_name = instance_name_by_refdes
        .iter()
        .map(|(refdes, name)| (name.clone(), refdes.as_str().to_string()))
        .collect::<BTreeMap<_, _>>();
    let eval_state = offline_eval_state(&board.board_dir)
        .context("Failed to resolve the board while validating generated Zener")?;
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
            "Generated Zener is not interface-compatible with the imported board{diagnostic_context}"
        );
    }
    let schematic = result.schematic.with_context(|| {
        format!(
            "Generated Zener is not interface-compatible with the imported board{diagnostic_context}"
        )
    })?;

    let partition_count = verify_physical_partitions(
        ir,
        &schematic,
        expected_pins_by_refdes,
        &refdes_by_instance_name,
    )
    .context("Generated Zener is not electrically compatible with the KiCad schematic")?;

    eprintln!(
        "Validated generated Zener against {} physical-pin partition(s)",
        partition_count,
    );
    Ok(())
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

/// Map a built component back to the source reference designator it came from.
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
    use std::path::PathBuf;

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
}
