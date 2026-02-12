use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn analyze(ir: &ImportIr) -> ImportSemanticAnalysis {
    ImportSemanticAnalysis {
        passives: detect_passives(ir),
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

fn detect_passives(ir: &ImportIr) -> ImportPassiveAnalysis {
    let mut by_component: BTreeMap<KiCadUuidPathKey, ImportPassiveClassification> = BTreeMap::new();
    let mut summary = ImportPassiveSummary::default();

    for (anchor, component) in &ir.components {
        if component.layout.is_none() {
            continue;
        }

        let classification = classify_passive(component);

        match (classification.kind, classification.confidence) {
            (Some(ImportPassiveKind::Resistor), Some(ImportPassiveConfidence::High)) => {
                summary.resistor_high += 1;
            }
            (Some(ImportPassiveKind::Resistor), Some(ImportPassiveConfidence::Medium)) => {
                summary.resistor_medium += 1;
            }
            (Some(ImportPassiveKind::Resistor), Some(ImportPassiveConfidence::Low)) => {
                summary.resistor_low += 1;
            }
            (Some(ImportPassiveKind::Capacitor), Some(ImportPassiveConfidence::High)) => {
                summary.capacitor_high += 1;
            }
            (Some(ImportPassiveKind::Capacitor), Some(ImportPassiveConfidence::Medium)) => {
                summary.capacitor_medium += 1;
            }
            (Some(ImportPassiveKind::Capacitor), Some(ImportPassiveConfidence::Low)) => {
                summary.capacitor_low += 1;
            }
            (None, _) => {
                if classification.pad_count != Some(2) {
                    summary.non_two_pad += 1;
                } else {
                    summary.unknown += 1;
                }
            }
            _ => {
                summary.unknown += 1;
            }
        }

        by_component.insert(anchor.clone(), classification);
    }

    ImportPassiveAnalysis {
        by_component,
        summary,
    }
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

fn classify_passive(component: &ImportComponentData) -> ImportPassiveClassification {
    let mut signals: BTreeSet<String> = BTreeSet::new();

    let pad_count = component.layout.as_ref().map(|l| l.pads.len());
    if let Some(n) = pad_count {
        signals.insert(format!("pad_count:{n}"));
    }

    let ref_prefix = refdes_prefix(component.netlist.refdes.as_str());
    if !ref_prefix.is_empty() {
        signals.insert(format!("refdes_prefix:{ref_prefix}"));
    }

    let props = component.best_properties();

    let lib_id = component
        .schematic
        .as_ref()
        .and_then(|s| s.units.values().next())
        .and_then(|u| u.lib_id.clone());
    if let Some(lib_id) = lib_id.as_ref() {
        signals.insert(format!("lib_id:{lib_id}"));
    }

    let footprint = component
        .layout
        .as_ref()
        .and_then(|l| l.fpid.clone())
        .or_else(|| component.netlist.footprint.clone());
    if let Some(fp) = footprint.as_ref() {
        signals.insert(format!("footprint:{fp}"));
    }

    let value = component
        .netlist
        .value
        .clone()
        .or_else(|| props.and_then(|p| p.get("Value")).cloned());
    if let Some(v) = value.as_ref() {
        signals.insert(format!("value:{v}"));
    }

    // Only attempt stdlib-generic promotion for explicit R/C refdeses. This is
    // intentionally conservative; we can broaden this later.
    let requested_kind = match ref_prefix.as_str() {
        "R" => Some(ImportPassiveKind::Resistor),
        "C" => Some(ImportPassiveKind::Capacitor),
        _ => None,
    };

    let package = extract_package(
        footprint.as_deref(),
        lib_id.as_ref().map(|l| l.as_str()),
        value.as_deref(),
    );
    if let Some(pkg) = package {
        signals.insert(format!("package:{}", pkg.as_str()));
    }

    let mpn = props.and_then(|p| {
        find_property_ci(
            p,
            &[
                "mpn",
                "manufacturer_part_number",
                "manufacturer part number",
                "mfr part number",
                "manufacturer_pn",
                "part number",
            ],
        )
    });
    let manufacturer = props.and_then(|p| find_property_ci(p, &["manufacturer", "mfr", "mfg"]));

    let tolerance = props.and_then(|p| find_property_ci(p, &["tolerance", "tol"]));
    let voltage =
        props.and_then(|p| find_property_ci(p, &["voltage", "voltage rating", "rated voltage"]));
    let dielectric = props.and_then(|p| find_property_ci(p, &["dielectric"]));
    let power = props.and_then(|p| find_property_ci(p, &["power", "power rating"]));

    if let Some(m) = manufacturer {
        signals.insert(format!("manufacturer:{m}"));
    }
    if let Some(m) = mpn {
        signals.insert(format!("mpn:{m}"));
    }
    if let Some(t) = tolerance {
        signals.insert(format!("tolerance:{t}"));
    }
    if let Some(v) = voltage {
        signals.insert(format!("voltage:{v}"));
    }
    if let Some(d) = dielectric {
        signals.insert(format!("dielectric:{d}"));
    }
    if let Some(p) = power {
        signals.insert(format!("power:{p}"));
    }

    let (kind, confidence, parsed_value) = match requested_kind {
        Some(ImportPassiveKind::Resistor) => classify_resistor(
            pad_count,
            package,
            value.as_deref(),
            lib_id.as_ref().map(|l| l.as_str()),
            footprint.as_deref(),
            &mut signals,
        ),
        Some(ImportPassiveKind::Capacitor) => classify_capacitor(
            pad_count,
            package,
            value.as_deref(),
            lib_id.as_ref().map(|l| l.as_str()),
            footprint.as_deref(),
            &mut signals,
        ),
        None => (None, None, None),
    };

    ImportPassiveClassification {
        kind,
        confidence,
        pad_count,
        package,
        parsed_value,
        mpn: mpn.map(|s| s.to_string()),
        manufacturer: manufacturer.map(|s| s.to_string()),
        tolerance: tolerance.map(|s| s.to_string()),
        voltage: voltage.map(|s| s.to_string()),
        dielectric: dielectric.map(|s| s.to_string()),
        power: power.map(|s| s.to_string()),
        signals,
    }
}

fn classify_resistor(
    pad_count: Option<usize>,
    package: Option<ImportPassivePackage>,
    value: Option<&str>,
    lib_id: Option<&str>,
    footprint: Option<&str>,
    signals: &mut BTreeSet<String>,
) -> (
    Option<ImportPassiveKind>,
    Option<ImportPassiveConfidence>,
    Option<String>,
) {
    if pad_count != Some(2) {
        return (None, None, None);
    }

    let lib_match = lib_id.is_some_and(lib_id_looks_like_resistor);
    let fp_match = footprint.is_some_and(footprint_looks_like_resistor);
    let lib_contra = lib_id.is_some_and(lib_id_looks_like_capacitor);
    let fp_contra = footprint.is_some_and(footprint_looks_like_capacitor);

    if lib_match {
        signals.insert("hint:lib_id_resistor".to_string());
    }
    if fp_match {
        signals.insert("hint:footprint_resistor".to_string());
    }
    if lib_contra {
        signals.insert("contra:lib_id_capacitor".to_string());
    }
    if fp_contra {
        signals.insert("contra:footprint_capacitor".to_string());
    }
    if lib_contra || fp_contra {
        return (None, None, None);
    }
    if !lib_match && !fp_match {
        return (None, None, None);
    }

    let Some(package) = package else {
        return (None, None, None);
    };
    // Resistive stdlib generics don't support 01005 in our current flow.
    if matches!(package, ImportPassivePackage::P01005) {
        return (None, None, None);
    }

    let Some(parsed) = value.and_then(parse_resistance) else {
        return (None, None, None);
    };
    signals.insert("parsed_value:resistance".to_string());

    let confidence = if lib_match && fp_match {
        ImportPassiveConfidence::High
    } else {
        ImportPassiveConfidence::Medium
    };

    (
        Some(ImportPassiveKind::Resistor),
        Some(confidence),
        Some(parsed),
    )
}

fn classify_capacitor(
    pad_count: Option<usize>,
    package: Option<ImportPassivePackage>,
    value: Option<&str>,
    lib_id: Option<&str>,
    footprint: Option<&str>,
    signals: &mut BTreeSet<String>,
) -> (
    Option<ImportPassiveKind>,
    Option<ImportPassiveConfidence>,
    Option<String>,
) {
    if pad_count != Some(2) {
        return (None, None, None);
    }

    let lib_match = lib_id.is_some_and(lib_id_looks_like_capacitor);
    let fp_match = footprint.is_some_and(footprint_looks_like_capacitor);
    let lib_contra = lib_id.is_some_and(lib_id_looks_like_resistor);
    let fp_contra = footprint.is_some_and(footprint_looks_like_resistor);

    if lib_match {
        signals.insert("hint:lib_id_capacitor".to_string());
    }
    if fp_match {
        signals.insert("hint:footprint_capacitor".to_string());
    }
    if lib_contra {
        signals.insert("contra:lib_id_resistor".to_string());
    }
    if fp_contra {
        signals.insert("contra:footprint_resistor".to_string());
    }
    if lib_contra || fp_contra {
        return (None, None, None);
    }
    if !lib_match && !fp_match {
        return (None, None, None);
    }

    let Some(_package) = package else {
        return (None, None, None);
    };

    let Some(parsed) = value.and_then(parse_capacitance) else {
        return (None, None, None);
    };
    signals.insert("parsed_value:capacitance".to_string());

    let confidence = if lib_match && fp_match {
        ImportPassiveConfidence::High
    } else {
        ImportPassiveConfidence::Medium
    };

    (
        Some(ImportPassiveKind::Capacitor),
        Some(confidence),
        Some(parsed),
    )
}

fn refdes_prefix(refdes: &str) -> String {
    let mut out = String::new();
    for c in refdes.chars() {
        if c.is_ascii_digit() {
            break;
        }
        if c.is_ascii_alphabetic() {
            out.push(c.to_ascii_uppercase());
        }
    }
    out
}

fn lib_id_looks_like_resistor(lib_id: &str) -> bool {
    let s = lib_id.to_ascii_lowercase();
    if s.contains("resistor") {
        return true;
    }
    let (_lib, name) = s.rsplit_once(':').unwrap_or(("", &s));
    name == "r"
        || name.starts_with("r_")
        || name.starts_with("r-")
        || name == "r_small"
        || name == "r_us"
}

fn lib_id_looks_like_capacitor(lib_id: &str) -> bool {
    let s = lib_id.to_ascii_lowercase();
    if s.contains("capacitor") {
        return true;
    }
    let (_lib, name) = s.rsplit_once(':').unwrap_or(("", &s));
    name == "c"
        || name.starts_with("c_")
        || name.starts_with("c-")
        || name == "c_small"
        || name == "cp"
        || name == "cp1"
        || name == "c_pol"
}

fn footprint_looks_like_resistor(footprint: &str) -> bool {
    let s = footprint.to_ascii_lowercase();
    if s.contains("resistor") {
        return true;
    }
    let name = s.rsplit_once(':').map(|(_, n)| n).unwrap_or(&s);
    name.starts_with("r_") || name.starts_with("r-")
}

fn footprint_looks_like_capacitor(footprint: &str) -> bool {
    let s = footprint.to_ascii_lowercase();
    if s.contains("capacitor") {
        return true;
    }
    let name = s.rsplit_once(':').map(|(_, n)| n).unwrap_or(&s);
    name.starts_with("c_") || name.starts_with("c-")
}

fn extract_package(
    footprint: Option<&str>,
    lib_id: Option<&str>,
    value: Option<&str>,
) -> Option<ImportPassivePackage> {
    for s in [footprint, lib_id, value].into_iter().flatten() {
        if let Some(pkg) = parse_package_from_text(s) {
            return Some(pkg);
        }
    }
    None
}

fn parse_package_from_text(text: &str) -> Option<ImportPassivePackage> {
    let s = text.to_ascii_lowercase();

    // Prefer explicit imperial codes when present.
    if contains_code(&s, "01005") {
        return Some(ImportPassivePackage::P01005);
    }
    if contains_code(&s, "0201") {
        return Some(ImportPassivePackage::P0201);
    }
    if contains_code(&s, "0402") {
        return Some(ImportPassivePackage::P0402);
    }
    if contains_code(&s, "0603") {
        return Some(ImportPassivePackage::P0603);
    }
    if contains_code(&s, "0805") {
        return Some(ImportPassivePackage::P0805);
    }
    if contains_code(&s, "1206") {
        return Some(ImportPassivePackage::P1206);
    }
    if contains_code(&s, "1210") {
        return Some(ImportPassivePackage::P1210);
    }

    // Fall back to common metric "####metric" encodings.
    if s.contains("0402metric") {
        return Some(ImportPassivePackage::P01005);
    }
    if s.contains("0603metric") {
        return Some(ImportPassivePackage::P0201);
    }
    if s.contains("1005metric") {
        return Some(ImportPassivePackage::P0402);
    }
    if s.contains("1608metric") {
        return Some(ImportPassivePackage::P0603);
    }
    if s.contains("2012metric") {
        return Some(ImportPassivePackage::P0805);
    }
    if s.contains("3216metric") {
        return Some(ImportPassivePackage::P1206);
    }
    if s.contains("3225metric") {
        return Some(ImportPassivePackage::P1210);
    }

    None
}

fn contains_code(haystack: &str, code: &str) -> bool {
    let mut start = 0usize;
    while let Some(pos) = haystack[start..].find(code) {
        let idx = start + pos;
        let before = haystack[..idx].chars().next_back();
        let after = haystack[idx + code.len()..].chars().next();
        let ok_before = before.is_none_or(|c| !c.is_ascii_digit());
        let ok_after = after.is_none_or(|c| !c.is_ascii_digit());
        if ok_before && ok_after {
            return true;
        }
        start = idx + code.len();
    }
    false
}

fn parse_resistance(raw: &str) -> Option<String> {
    // Try common project convention: "R_10k_0402"
    let parts = tokenize_import_value(raw);
    if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("r") {
        if let Some(v) = parse_resistance_token(parts[1]) {
            return Some(v);
        }
    }
    for part in &parts {
        if let Some(v) = parse_resistance_token(part) {
            return Some(v);
        }
    }
    // Avoid merging a standalone refdes prefix token like "R" with following
    // value/package tokens ("R"+"10" -> "R10", "R"+"0402" -> "R0402").
    let merge_parts = if parts.first().is_some_and(|p| p.eq_ignore_ascii_case("r")) {
        &parts[1..]
    } else {
        &parts[..]
    };
    parse_from_merged_tokens(merge_parts, parse_resistance_token)
}

fn parse_resistance_token(token: &str) -> Option<String> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    let mut s = t.to_ascii_uppercase();
    s = s.replace('Ω', "OHM");
    s = s.replace('Ω', "OHM");
    s = s.replace('µ', "U");
    s = s.replace('μ', "U");
    s = s.replace(',', ".");
    s = s.replace("OHMS", "OHM");
    s = s.replace("KOHM", "K");
    s = s.replace("MOHM", "M");

    if s.ends_with("OHM") {
        let num = s.strip_suffix("OHM")?.trim();
        if num.is_empty() {
            return None;
        }
        if is_number(num) {
            return Some(num.to_ascii_lowercase());
        }
        // Allow "10KOHM" style normalization above; otherwise fail.
        return None;
    }

    // R10 / R005 => 0.10 / 0.005
    if let Some(frac) = s.strip_prefix('R') {
        if !frac.is_empty() && frac.chars().all(|c| c.is_ascii_digit()) {
            let out = format!("0.{frac}");
            return Some(normalize_decimal_string(&out));
        }
    }

    // Reject tokens that contain capacitance-like units.
    if s.contains("UF") || s.contains("NF") || s.contains("PF") {
        return None;
    }

    // 0R / 10R0 / 49R9
    if let Some((a, b)) = s.split_once('R') {
        if !a.is_empty()
            && a.chars().all(|c| c.is_ascii_digit())
            && b.chars().all(|c| c.is_ascii_digit())
        {
            if b.is_empty() {
                return Some(a.to_string());
            }
            if b.chars().all(|c| c == '0') {
                return Some(a.to_string());
            }
            return Some(format!("{a}.{b}"));
        }
        if a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) {
            if a.is_empty() {
                return None;
            }
            if b.is_empty() {
                return Some(a.to_string());
            }
            if b.chars().all(|c| c == '0') {
                return Some(a.to_string());
            }
            return Some(format!("{a}.{b}"));
        }
    }

    // 4K7 / 10K / 1M / 2M2
    for (sep, suffix) in [('K', "k"), ('M', "M")].into_iter() {
        if let Some((a, b)) = s.split_once(sep) {
            if a.is_empty() || !a.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if !b.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if b.is_empty() {
                return Some(format!("{a}{suffix}"));
            }
            return Some(format!("{a}.{b}{suffix}"));
        }
    }

    // Decimal with suffix: "4.7k"
    if let Some(last) = s.chars().last() {
        if matches!(last, 'K' | 'M') {
            let num = &s[..s.len() - 1];
            if is_number(num) {
                return Some(format!(
                    "{}{}",
                    num.to_ascii_lowercase(),
                    last.to_ascii_lowercase()
                ));
            }
        }
    }

    None
}

fn parse_capacitance(raw: &str) -> Option<String> {
    // Try common project convention: "C_100n_0402"
    let parts = tokenize_import_value(raw);
    if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("c") {
        if let Some(v) = parse_capacitance_token(parts[1]) {
            return Some(v);
        }
    }
    for part in &parts {
        if let Some(v) = parse_capacitance_token(part) {
            return Some(v);
        }
    }
    parse_from_merged_tokens(&parts, parse_capacitance_token)
}

fn parse_capacitance_token(token: &str) -> Option<String> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    let mut s = t.to_ascii_uppercase();
    s = s.replace('µ', "U");
    s = s.replace('μ', "U");
    s = s.replace(',', ".");
    s = s.replace(' ', "");

    // Reject tokens that look like a resistance designation (prevents "10K" being treated as cap).
    if s.contains("OHM") || s.contains('K') || s.contains('M') || s.contains('R') {
        // Allow explicit capacitor units below to override this, e.g. "10UF".
    }

    // Normalize trailing 'F' but always emit an explicit Farads unit to satisfy
    // stdlib `config("value", Capacitance)` parsing.
    if s.ends_with('F') && s.len() > 1 {
        s.pop();
    }

    // 4U7 / 100N / 22P / 0.1U
    for (sep, suffix) in [('P', "p"), ('N', "n"), ('U', "u")].into_iter() {
        if let Some((a, b)) = s.split_once(sep) {
            if a.is_empty() || !is_number(a) {
                continue;
            }
            if !b.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if b.is_empty() {
                return Some(format!("{}{}F", a.to_ascii_lowercase(), suffix));
            }
            return Some(format!("{}.{}{}F", a.to_ascii_lowercase(), b, suffix));
        }
    }

    // 0.1U style
    if let Some(last) = s.chars().last() {
        if matches!(last, 'P' | 'N' | 'U') {
            let num = &s[..s.len() - 1];
            if is_number(num) {
                return Some(format!(
                    "{}{}F",
                    num.to_ascii_lowercase(),
                    last.to_ascii_lowercase()
                ));
            }
        }
    }

    None
}

fn parse_from_merged_tokens(
    parts: &[&str],
    parse_token: fn(&str) -> Option<String>,
) -> Option<String> {
    for width in [2usize, 3usize] {
        if parts.len() < width {
            continue;
        }
        for i in 0..=parts.len() - width {
            let merged = parts[i..i + width].join("");
            if let Some(v) = parse_token(&merged) {
                return Some(v);
            }
        }
    }
    None
}

fn tokenize_import_value(raw: &str) -> Vec<&str> {
    raw.split(|c: char| {
        c == '_'
            || c == '-'
            || c == '/'
            || c == ':'
            || c == '|'
            || c == '('
            || c == ')'
            || c == '['
            || c == ']'
            || c == '{'
            || c == '}'
            || c.is_whitespace()
    })
    .filter(|p| !p.is_empty())
    .collect()
}

fn normalize_decimal_string(s: &str) -> String {
    let trimmed = s.trim();
    if let Some((a, b)) = trimmed.split_once('.') {
        let b = b.trim_end_matches('0');
        if b.is_empty() {
            a.to_string()
        } else {
            format!("{a}.{b}")
        }
    } else {
        trimmed.to_string()
    }
}

fn is_number(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let mut seen_dot = false;
    for c in s.chars() {
        if c == '.' {
            if seen_dot {
                return false;
            }
            seen_dot = true;
            continue;
        }
        if !c.is_ascii_digit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package() {
        for (raw, want) in [
            ("R_10k_0402", ImportPassivePackage::P0402),
            ("C_100n_0603", ImportPassivePackage::P0603),
            ("C_22uF_0805", ImportPassivePackage::P0805),
            ("C_22uF_1206", ImportPassivePackage::P1206),
            ("C_10uF_1210", ImportPassivePackage::P1210),
            ("C_1206_3216Metric", ImportPassivePackage::P1206),
            ("C_1210_3225Metric", ImportPassivePackage::P1210),
        ] {
            assert_eq!(
                parse_package_from_text(raw),
                Some(want),
                "package parse: {raw}"
            );
        }
        assert_eq!(parse_package_from_text("SOT-23-5"), None);
    }

    #[test]
    fn test_classify_resistor_allows_1206() {
        let mut signals = BTreeSet::new();
        let (kind, confidence, parsed_value) = classify_resistor(
            Some(2),
            Some(ImportPassivePackage::P1206),
            Some("10k"),
            Some("Device:R"),
            Some("Resistor_SMD:R_1206_3216Metric"),
            &mut signals,
        );
        assert_eq!(kind, Some(ImportPassiveKind::Resistor));
        assert_eq!(confidence, Some(ImportPassiveConfidence::High));
        assert_eq!(parsed_value.as_deref(), Some("10k"));
    }

    #[test]
    fn test_classify_capacitor_allows_1210() {
        let mut signals = BTreeSet::new();
        let (kind, confidence, parsed_value) = classify_capacitor(
            Some(2),
            Some(ImportPassivePackage::P1210),
            Some("10uF"),
            Some("Device:C"),
            Some("Capacitor_SMD:C_1210_3225Metric"),
            &mut signals,
        );
        assert_eq!(kind, Some(ImportPassiveKind::Capacitor));
        assert_eq!(confidence, Some(ImportPassiveConfidence::High));
        assert_eq!(parsed_value.as_deref(), Some("10uF"));
    }

    #[test]
    fn test_parse_resistance() {
        for (raw, want) in [
            ("10k", "10k"),
            ("4K7", "4.7k"),
            ("49R9", "49.9"),
            ("10R0", "10"),
            ("R10", "0.1"),
            ("1M", "1M"),
            ("10 k", "10k"),
            ("10 kΩ", "10k"),
            ("10 kOhm", "10k"),
            ("R_10k_0402", "10k"),
            ("10ohm", "10"),
            ("10Ω", "10"),
        ] {
            assert_eq!(
                parse_resistance(raw).as_deref(),
                Some(want),
                "resistance parse: {raw}"
            );
        }
        for raw in [
            "Murata-BLM21PG_0805",
            "LED_G_0603",
            "100n",
            "0.1uF",
            "R_10_0402",
            "R_0402",
        ] {
            assert!(parse_resistance(raw).is_none(), "expected none: {raw}");
        }
    }

    #[test]
    fn test_parse_capacitance() {
        for (raw, want) in [
            ("100n", "100nF"),
            ("0.1uF", "0.1uF"),
            ("1UF", "1uF"),
            ("1 uF", "1uF"),
            ("2.2 uF", "2.2uF"),
            ("2,2uF", "2.2uF"),
            ("1uF/16V", "1uF"),
            ("1 μF", "1uF"),
            ("22P", "22pF"),
            ("C_100n_0402", "100nF"),
            ("10u", "10uF"),
            ("4u7", "4.7uF"),
        ] {
            assert_eq!(
                parse_capacitance(raw).as_deref(),
                Some(want),
                "capacitance parse: {raw}"
            );
        }
        for raw in ["10k", "4K7", "R_10k_0402", "GRM155R61H104KE14D"] {
            assert!(parse_capacitance(raw).is_none(), "expected none: {raw}");
        }
    }

    #[test]
    fn test_refdes_prefix() {
        assert_eq!(refdes_prefix("R49"), "R");
        assert_eq!(refdes_prefix("C1"), "C");
        assert_eq!(refdes_prefix("RN3"), "RN");
        assert_eq!(refdes_prefix("#PWR01"), "PWR");
    }
}
