use anyhow::{Context, Result, bail};
use pcb_sch::kicad_netlist::try_format_footprint_with_package_roots;
use pcb_sch::{AttributeValue, InstanceKind, Schematic};
use pcb_sexpr::Sexpr;
use pcb_sexpr::board::{extract_keyed_footprints, footprint_name_from_fpid};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const UUID_NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port {
    pub component_path: String,
    pub pad_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveFootprint {
    pub fpid: String,
    pub reference: Option<String>,
    pub pads: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveNetlist {
    pub footprints: BTreeMap<String, EffectiveFootprint>,
    pub nets: BTreeMap<String, BTreeSet<Port>>,
    pub port_to_net: BTreeMap<Port, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDiff {
    pub kind: &'static str,
    pub severity: DiffSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSeverity {
    Warning,
    Error,
}

impl SemanticDiff {
    fn error(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            severity: DiffSeverity::Error,
            message: message.into(),
        }
    }

    fn warning(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            severity: DiffSeverity::Warning,
            message: message.into(),
        }
    }
}

impl EffectiveNetlist {
    fn rebuild_nets(&mut self) {
        let mut grouped: BTreeMap<String, BTreeSet<Port>> = BTreeMap::new();
        for (port, net_name) in &self.port_to_net {
            grouped
                .entry(net_name.clone())
                .or_default()
                .insert(port.clone());
        }
        self.nets = grouped
            .into_iter()
            .filter(|(_, ports)| ports.len() > 1)
            .collect();
    }
}

pub fn source_effective_netlist(schematic: &Schematic) -> Result<EffectiveNetlist> {
    let mut effective = EffectiveNetlist::default();

    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }

        let component_path = instance_ref.instance_path.join(".");
        let Some(AttributeValue::String(fp_attr)) = instance.attributes.get("footprint") else {
            bail!("component `{component_path}` is missing footprint attribute");
        };
        let (fpid, _) = try_format_footprint_with_package_roots(fp_attr, &schematic.package_roots)
            .with_context(|| format!("Failed to resolve footprint path '{fp_attr}'"))?;

        effective.footprints.insert(
            component_path,
            EffectiveFootprint {
                fpid,
                reference: instance.reference_designator.clone(),
                pads: BTreeSet::new(),
            },
        );
    }

    for (net_name, net) in &schematic.nets {
        for port_ref in &net.ports {
            let Some(component_ref) = schematic.component_ref_for_port(port_ref) else {
                bail!("missing component owner for netlist port `{port_ref}`");
            };
            let component_path = component_ref.instance_path.join(".");
            let Some(port_instance) = schematic.instances.get(port_ref) else {
                continue;
            };
            let Some(AttributeValue::Array(pads)) = port_instance.attributes.get("pads") else {
                continue;
            };

            for pad in pads {
                let AttributeValue::String(pad_name) = pad else {
                    continue;
                };
                if let Some(fp) = effective.footprints.get_mut(&component_path) {
                    fp.pads.insert(pad_name.clone());
                }
                let port = Port {
                    component_path: component_path.clone(),
                    pad_name: pad_name.clone(),
                };
                let effective_net_name = if net.kind == "NotConnected" {
                    format!("unconnected-({component_path}:{pad_name})")
                } else {
                    net_name.clone()
                };
                if let Some(previous) = effective
                    .port_to_net
                    .insert(port.clone(), effective_net_name.clone())
                    && previous != effective_net_name
                {
                    bail!(
                        "source port {}:{} is assigned to both `{previous}` and `{effective_net_name}`",
                        port.component_path,
                        port.pad_name
                    );
                }
            }
        }
    }

    effective.rebuild_nets();
    Ok(effective)
}

pub fn layout_effective_netlist(
    board: &Sexpr,
    expected: &EffectiveNetlist,
) -> Result<(EffectiveNetlist, Vec<SemanticDiff>)> {
    let mut effective = EffectiveNetlist::default();
    let mut diagnostics = Vec::new();
    let keyed = extract_keyed_footprints(board).map_err(anyhow::Error::msg)?;

    let mut kiid_to_path: BTreeMap<String, String> = BTreeMap::new();
    for path in expected.footprints.keys() {
        kiid_to_path.insert(uuid_for_path(path), path.clone());
    }

    for fp in keyed {
        let property_path = fp.properties.get("Path").filter(|s| !s.is_empty()).cloned();
        let component_path = if let Some(path) = property_path.clone() {
            let expected_kiid = expected_kiid_path(&path);
            if fp.path != expected_kiid {
                diagnostics.push(SemanticDiff::warning(
                    "layout.sync.unmanaged_footprint",
                    format!(
                        "Footprint {} ({}:{}) is not managed by sync: expected KIID path {expected_kiid}, found {}",
                        fp.properties
                            .get("Reference")
                            .map(String::as_str)
                            .unwrap_or("<unknown>"),
                        path,
                        fp.fpid.as_deref().unwrap_or(""),
                        fp.path
                    ),
                ));
                continue;
            }
            path
        } else {
            let uuid = fp.path.trim_matches('/').rsplit('/').next().unwrap_or("");
            let Some(path) = kiid_to_path.get(uuid).cloned() else {
                continue;
            };
            path
        };

        let mut pads = BTreeSet::new();
        let mut seen_pad_nets: BTreeMap<String, String> = BTreeMap::new();
        for pad in &fp.pads {
            if pad.number.is_empty() {
                continue;
            }
            pads.insert(pad.number.clone());
            let Some(net_name) = pad.net_name.as_ref().filter(|s| !s.is_empty()) else {
                continue;
            };
            if let Some(previous) = seen_pad_nets.insert(pad.number.clone(), net_name.clone())
                && previous != *net_name
            {
                diagnostics.push(SemanticDiff::error(
                    "layout.sync.pad_assignment",
                    format!(
                        "Footprint {component_path} has duplicate physical pad {} assigned to both `{previous}` and `{net_name}`",
                        pad.number
                    ),
                ));
            }
        }

        for (pad_name, net_name) in seen_pad_nets {
            effective.port_to_net.insert(
                Port {
                    component_path: component_path.clone(),
                    pad_name,
                },
                net_name,
            );
        }

        effective.footprints.insert(
            component_path,
            EffectiveFootprint {
                fpid: fp.fpid.unwrap_or_default(),
                reference: fp.properties.get("Reference").cloned(),
                pads,
            },
        );
    }

    effective.rebuild_nets();
    Ok((effective, diagnostics))
}

pub fn diff_effective_netlists(
    expected: &EffectiveNetlist,
    actual: &EffectiveNetlist,
) -> Vec<SemanticDiff> {
    let mut diffs = Vec::new();
    let mut explained_ports = BTreeSet::new();

    for (path, expected_fp) in &expected.footprints {
        match actual.footprints.get(path) {
            None => {
                diffs.push(SemanticDiff::error(
                    "layout.sync.missing_footprint",
                    format!("Managed footprint {path} is missing from layout"),
                ));
                mark_ports(path, expected, &mut explained_ports);
            }
            Some(actual_fp) => {
                if comparable_fpid(&expected_fp.fpid) != comparable_fpid(&actual_fp.fpid) {
                    diffs.push(SemanticDiff::error(
                        "layout.sync.footprint_fpid",
                        format!(
                            "Managed footprint {path} has FPID `{}` in source but `{}` in layout",
                            expected_fp.fpid, actual_fp.fpid
                        ),
                    ));
                    mark_ports(path, expected, &mut explained_ports);
                    mark_ports(path, actual, &mut explained_ports);
                } else if !expected_fp.pads.is_subset(&actual_fp.pads) {
                    let missing: BTreeSet<_> = expected_fp
                        .pads
                        .difference(&actual_fp.pads)
                        .cloned()
                        .collect();
                    diffs.push(SemanticDiff::error(
                        "layout.sync.pad_inventory",
                        format!(
                            "Managed footprint {path} is missing source logical pad(s) in layout: [{}]",
                            join_strings(&missing)
                        ),
                    ));
                    mark_ports(path, expected, &mut explained_ports);
                    mark_ports(path, actual, &mut explained_ports);
                }
            }
        }
    }

    for path in actual.footprints.keys() {
        if !expected.footprints.contains_key(path) {
            diffs.push(SemanticDiff::error(
                "layout.sync.extra_footprint",
                format!("Layout contains extra managed footprint {path}"),
            ));
            mark_ports(path, actual, &mut explained_ports);
        }
    }

    let mut consumed_expected = BTreeSet::new();
    let mut consumed_actual = BTreeSet::new();
    let expected_sigs = unique_signature_index(&expected.nets);
    let actual_sigs = unique_signature_index(&actual.nets);
    for (signature, expected_net) in expected_sigs {
        if signature.iter().any(|p| explained_ports.contains(p)) {
            continue;
        }
        if let Some(actual_net) = actual_sigs.get(&signature)
            && expected_net != *actual_net
        {
            diffs.push(SemanticDiff::warning(
                "layout.sync.net_rename",
                format!(
                    "Layout net `{actual_net}` has the same connectivity as source net `{expected_net}`"
                ),
            ));
            consumed_expected.insert(expected_net);
            consumed_actual.insert(actual_net.clone());
        }
    }

    let common_ports: BTreeSet<_> = expected
        .port_to_net
        .keys()
        .filter(|p| actual.port_to_net.contains_key(*p) && !explained_ports.contains(*p))
        .cloned()
        .collect();
    let mut by_expected: BTreeMap<String, BTreeMap<String, BTreeSet<Port>>> = BTreeMap::new();
    let mut by_actual: BTreeMap<String, BTreeMap<String, BTreeSet<Port>>> = BTreeMap::new();
    for port in &common_ports {
        let expected_net = expected.port_to_net.get(port).unwrap();
        let actual_net = actual.port_to_net.get(port).unwrap();
        if expected_net == actual_net
            || consumed_expected.contains(expected_net)
            || consumed_actual.contains(actual_net)
        {
            continue;
        }
        by_expected
            .entry(expected_net.clone())
            .or_default()
            .entry(actual_net.clone())
            .or_default()
            .insert(port.clone());
        by_actual
            .entry(actual_net.clone())
            .or_default()
            .entry(expected_net.clone())
            .or_default()
            .insert(port.clone());
    }

    for (expected_net, actual_groups) in &by_expected {
        if actual_groups.len() > 1 {
            diffs.push(SemanticDiff::error(
                "layout.sync.net_split",
                format!(
                    "Source net `{expected_net}` is split across layout nets: {}",
                    actual_groups.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
            consumed_expected.insert(expected_net.clone());
            consumed_actual.extend(actual_groups.keys().cloned());
        }
    }
    for (actual_net, expected_groups) in &by_actual {
        if expected_groups.len() > 1 {
            diffs.push(SemanticDiff::error(
                "layout.sync.net_merge",
                format!(
                    "Layout net `{actual_net}` merges source nets: {}",
                    expected_groups
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            consumed_actual.insert(actual_net.clone());
            consumed_expected.extend(expected_groups.keys().cloned());
        }
    }

    for (net_name, expected_ports) in &expected.nets {
        if consumed_expected.contains(net_name) {
            continue;
        }
        let actual_ports = actual.nets.get(net_name).cloned().unwrap_or_default();
        let missing: BTreeSet<_> = expected_ports
            .difference(&actual_ports)
            .filter(|p| !explained_ports.contains(*p))
            .cloned()
            .collect();
        if !missing.is_empty() {
            diffs.push(SemanticDiff::error(
                "layout.sync.pad_assignment",
                format!(
                    "Net `{net_name}` is missing {} pad assignment(s) in layout: {}",
                    missing.len(),
                    sample_ports(&missing)
                ),
            ));
        }
    }
    for (net_name, actual_ports) in &actual.nets {
        if consumed_actual.contains(net_name) {
            continue;
        }
        let expected_ports = expected.nets.get(net_name).cloned().unwrap_or_default();
        let extra: BTreeSet<_> = actual_ports
            .difference(&expected_ports)
            .filter(|p| !explained_ports.contains(*p))
            .cloned()
            .collect();
        if !extra.is_empty() {
            diffs.push(SemanticDiff::error(
                "layout.sync.pad_assignment",
                format!(
                    "Net `{net_name}` has {} extra pad assignment(s) in layout: {}",
                    extra.len(),
                    sample_ports(&extra)
                ),
            ));
        }
    }

    diffs
}

fn mark_ports(path: &str, netlist: &EffectiveNetlist, explained: &mut BTreeSet<Port>) {
    for port in netlist.port_to_net.keys() {
        if port.component_path == path {
            explained.insert(port.clone());
        }
    }
}

fn unique_signature_index(nets: &BTreeMap<String, BTreeSet<Port>>) -> BTreeMap<Vec<Port>, String> {
    let mut signatures: BTreeMap<Vec<Port>, Option<String>> = BTreeMap::new();
    for (net, ports) in nets {
        let signature: Vec<_> = ports.iter().cloned().collect();
        signatures
            .entry(signature)
            .and_modify(|v| *v = None)
            .or_insert_with(|| Some(net.clone()));
    }
    signatures
        .into_iter()
        .filter_map(|(sig, net)| net.map(|n| (sig, n)))
        .collect()
}

fn sample_ports(ports: &BTreeSet<Port>) -> String {
    let mut sample: Vec<_> = ports
        .iter()
        .take(5)
        .map(|p| format!("{}:{}", p.component_path, p.pad_name))
        .collect();
    if ports.len() > sample.len() {
        sample.push(format!("… and {} more", ports.len() - sample.len()));
    }
    sample.join(", ")
}

fn join_strings(values: &BTreeSet<String>) -> String {
    values.iter().cloned().collect::<Vec<_>>().join(", ")
}

fn comparable_fpid(fpid: &str) -> String {
    footprint_name_from_fpid(fpid)
}

fn uuid_for_path(path: &str) -> String {
    Uuid::new_v5(&UUID_NAMESPACE_URL, path.as_bytes()).to_string()
}

fn expected_kiid_path(path: &str) -> String {
    let uuid = uuid_for_path(path);
    format!("/{uuid}/{uuid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(component_path: &str, pad_name: &str) -> Port {
        Port {
            component_path: component_path.to_string(),
            pad_name: pad_name.to_string(),
        }
    }

    fn netlist(assignments: &[(&str, &str, &str)]) -> EffectiveNetlist {
        let mut n = EffectiveNetlist::default();
        for (component_path, pad_name, net_name) in assignments {
            n.footprints
                .entry((*component_path).to_string())
                .or_insert_with(|| EffectiveFootprint {
                    fpid: "Test:FP".to_string(),
                    reference: None,
                    pads: BTreeSet::new(),
                })
                .pads
                .insert((*pad_name).to_string());
            n.port_to_net
                .insert(port(component_path, pad_name), (*net_name).to_string());
        }
        n.rebuild_nets();
        n
    }

    #[test]
    fn exact_net_rename_is_warning_only() {
        let expected = netlist(&[("R1", "1", "A"), ("R2", "1", "A")]);
        let actual = netlist(&[("R1", "1", "B"), ("R2", "1", "B")]);

        let diffs = diff_effective_netlists(&expected, &actual);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, "layout.sync.net_rename");
        assert_eq!(diffs[0].severity, DiffSeverity::Warning);
    }

    #[test]
    fn split_net_is_error() {
        let expected = netlist(&[("R1", "1", "A"), ("R2", "1", "A"), ("R3", "1", "A")]);
        let actual = netlist(&[("R1", "1", "B"), ("R2", "1", "C"), ("R3", "1", "C")]);

        let diffs = diff_effective_netlists(&expected, &actual);

        assert!(diffs.iter().any(|d| d.kind == "layout.sync.net_split"));
    }

    #[test]
    fn merge_net_is_error() {
        let expected = netlist(&[
            ("R1", "1", "A"),
            ("R2", "1", "A"),
            ("R3", "1", "B"),
            ("R4", "1", "B"),
        ]);
        let actual = netlist(&[
            ("R1", "1", "C"),
            ("R2", "1", "C"),
            ("R3", "1", "C"),
            ("R4", "1", "C"),
        ]);

        let diffs = diff_effective_netlists(&expected, &actual);

        assert!(diffs.iter().any(|d| d.kind == "layout.sync.net_merge"));
    }

    #[test]
    fn single_pad_nets_are_ignored() {
        let expected = netlist(&[("R1", "1", "unconnected-R1-1")]);
        let actual = netlist(&[("R1", "1", "unconnected-other")]);

        let diffs = diff_effective_netlists(&expected, &actual);

        assert!(diffs.is_empty());
    }

    #[test]
    fn duplicate_physical_pads_with_same_net_collapse() {
        let path = "R1";
        let uuid = uuid_for_path(path);
        let board = pcb_sexpr::parse(&format!(
            r#"(kicad_pcb
              (footprint "Test:FP"
                (path "/{uuid}/{uuid}")
                (property "Path" "{path}")
                (pad "1" smd rect (net 1 "A"))
                (pad "1" smd rect (net 1 "A"))))"#
        ))
        .unwrap();
        let expected = netlist(&[("R1", "1", "A"), ("R2", "1", "A")]);

        let (actual, diagnostics) = layout_effective_netlist(&board, &expected).unwrap();

        assert!(diagnostics.is_empty());
        assert_eq!(actual.port_to_net.get(&port("R1", "1")).unwrap(), "A");
    }

    #[test]
    fn duplicate_physical_pads_with_conflicting_nets_diagnose() {
        let path = "R1";
        let uuid = uuid_for_path(path);
        let board = pcb_sexpr::parse(&format!(
            r#"(kicad_pcb
              (footprint "Test:FP"
                (path "/{uuid}/{uuid}")
                (property "Path" "{path}")
                (pad "1" smd rect (net 1 "A"))
                (pad "1" smd rect (net 2 "B"))))"#
        ))
        .unwrap();
        let expected = netlist(&[("R1", "1", "A"), ("R2", "1", "A")]);

        let (_actual, diagnostics) = layout_effective_netlist(&board, &expected).unwrap();

        assert!(
            diagnostics
                .iter()
                .any(|d| d.kind == "layout.sync.pad_assignment")
        );
    }
}
