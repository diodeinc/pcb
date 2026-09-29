use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn analyze(ir: &ImportIr) -> ImportSemanticAnalysis {
    ImportSemanticAnalysis {
        net_kinds: detect_net_kinds(ir),
    }
}

fn detect_net_kinds(ir: &ImportIr) -> ImportNetKindAnalysis {
    let mut hints_by_net: BTreeMap<KiCadNetName, BTreeSet<ImportNetKind>> = BTreeMap::new();
    let mut reasons_by_net: BTreeMap<KiCadNetName, BTreeSet<String>> = BTreeMap::new();

    for decl in &ir.schematic_power_symbol_decls {
        let Some(value) = decl
            .value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let net_name = KiCadNetName::from(value.to_string());

        let hint = match decl
            .lib_id
            .as_ref()
            .map(|id| id.as_str())
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str()
        {
            id if is_ground_power_symbol_id(id) => ImportNetKind::Ground,
            id if is_power_symbol_id(id) => ImportNetKind::Power,
            _ => continue,
        };

        hints_by_net
            .entry(net_name.clone())
            .or_default()
            .insert(hint);

        let mut reason = String::new();
        reason.push_str("declared_by_power_symbol:");
        if let Some(lib_id) = decl.lib_id.as_ref() {
            reason.push_str(lib_id.as_str());
        } else {
            reason.push_str("<unknown>");
        }
        reason.push(':');
        reason.push_str(decl.sheet_path.as_str());
        reasons_by_net.entry(net_name).or_default().insert(reason);
    }

    let mut by_net: BTreeMap<KiCadNetName, ImportNetKindClassification> = BTreeMap::new();

    for net_name in ir.nets.keys() {
        let hints = hints_by_net.get(net_name);
        let has_ground = hints.is_some_and(|s| s.contains(&ImportNetKind::Ground));
        let has_power = hints.is_some_and(|s| s.contains(&ImportNetKind::Power));

        let kind = match (has_ground, has_power) {
            (true, true) => ImportNetKind::Net, // conflict: fall back to Net
            (true, false) => ImportNetKind::Ground,
            (false, true) => ImportNetKind::Power,
            (false, false) => ImportNetKind::Net,
        };

        let mut reasons = reasons_by_net.get(net_name).cloned().unwrap_or_default();
        if has_ground && has_power {
            reasons.insert("conflict:power_and_ground_decls".to_string());
        }

        by_net.insert(
            net_name.clone(),
            ImportNetKindClassification { kind, reasons },
        );
    }

    ImportNetKindAnalysis { by_net }
}

fn is_power_symbol_id(lib_id_upper: &str) -> bool {
    // Conservative: require a KiCad `(power)` symbol AND some recognizable token in the symbol id.
    // Non-ground rails are classified as Power when they look like rails (+/- supplies, VCC, etc.).
    if lib_id_upper.contains("PWR_FLAG") {
        return false;
    }
    lib_id_upper.contains("+")
        || lib_id_upper.contains("VCC")
        || lib_id_upper.contains("VDD")
        || lib_id_upper.contains("VBAT")
        || lib_id_upper.contains("VIN")
        || lib_id_upper.contains("VPP")
        || lib_id_upper.contains("VDDA")
}

fn is_ground_power_symbol_id(lib_id_upper: &str) -> bool {
    if lib_id_upper.contains("PWR_FLAG") {
        return false;
    }
    lib_id_upper.contains("GND")
        || lib_id_upper.contains("GROUND")
        || lib_id_upper.contains("EARTH")
        || lib_id_upper.contains("CHASSIS")
}

#[cfg(test)]
mod net_kind_tests {
    use super::*;
    use std::path::PathBuf;

    fn empty_ir() -> ImportIr {
        ImportIr {
            components: BTreeMap::new(),
            nets: BTreeMap::new(),
            schematic_lib_symbols: BTreeMap::new(),
            schematic_power_symbol_decls: Vec::new(),
            schematic_sheet_tree: ImportSheetTree {
                root_schematic: PathBuf::from("root.kicad_sch"),
                nodes: BTreeMap::new(),
            },
            hierarchy_plan: ImportHierarchyPlan::default(),
            semantic: ImportSemanticAnalysis::default(),
        }
    }

    fn make_net(name: &str) -> (KiCadNetName, ImportNetData) {
        (
            KiCadNetName::from(name.to_string()),
            ImportNetData {
                ports: BTreeSet::new(),
            },
        )
    }

    fn power_decl(value: &str, lib_id: &str) -> ImportSchematicPowerSymbolDecl {
        ImportSchematicPowerSymbolDecl {
            schematic_file: PathBuf::from("root.kicad_sch"),
            sheet_path: KiCadSheetPath::root(),
            symbol_uuid: Some("deadbeef".to_string()),
            at: None,
            mirror: None,
            reference: None,
            lib_id: Some(KiCadLibId::from(lib_id.to_string())),
            value: Some(value.to_string()),
        }
    }

    #[test]
    fn classifies_ground_and_power_from_power_symbols() {
        let mut ir = empty_ir();
        let (gnd, gnd_data) = make_net("GND");
        ir.nets.insert(gnd, gnd_data);
        let (v1v8, v1v8_data) = make_net("+1V8");
        ir.nets.insert(v1v8, v1v8_data);
        let (sig, sig_data) = make_net("SIG");
        ir.nets.insert(sig, sig_data);

        ir.schematic_power_symbol_decls = vec![
            power_decl("GND", "power:GND"),
            power_decl("+1V8", "power:+1V8"),
            power_decl("GND", "power:PWR_FLAG"), // ignored
        ];

        let analysis = analyze(&ir);
        assert_eq!(
            analysis
                .net_kinds
                .by_net
                .get(&KiCadNetName::from("GND".to_string()))
                .unwrap()
                .kind,
            ImportNetKind::Ground
        );
        assert_eq!(
            analysis
                .net_kinds
                .by_net
                .get(&KiCadNetName::from("+1V8".to_string()))
                .unwrap()
                .kind,
            ImportNetKind::Power
        );
        assert_eq!(
            analysis
                .net_kinds
                .by_net
                .get(&KiCadNetName::from("SIG".to_string()))
                .unwrap()
                .kind,
            ImportNetKind::Net
        );
    }
}
