use anyhow::{Context, Result};
use pcb_sch::{PhysicalUnit, physical::PhysicalValue};
use pcb_zen_core::lang::stackup::{BoardConfig, DesignRules, NetClass};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::{PCB_GIT_HASH_PLACEHOLDER, PCB_VERSION_PLACEHOLDER};

const DEFAULT_COLOR: &str = "rgba(0, 0, 0, 0.000)";
const DEFAULT_WIRE_WIDTH_MIL: i64 = 6;
const DEFAULT_BUS_WIDTH_MIL: i64 = 12;
const DEFAULT_LINE_STYLE: i64 = 0;
const FLOAT_EQ_EPS: f64 = 1e-9;
/// `TUNING_PROFILE::PROFILE_TYPE` values KiCad stores in a tuning profile's `type`.
const TUNING_PROFILE_SINGLE: i64 = 0;
const TUNING_PROFILE_DIFFERENTIAL: i64 = 1;

const CONSTRAINT_MAPPINGS: &[(&[&str], &str)] = &[
    (
        &["copper", "minimum_clearance"],
        "board.design_settings.rules.min_clearance",
    ),
    (
        &["copper", "minimum_track_width"],
        "board.design_settings.rules.min_track_width",
    ),
    (
        &["copper", "minimum_connection_width"],
        "board.design_settings.rules.min_connection",
    ),
    (
        &["copper", "minimum_annular_width"],
        "board.design_settings.rules.min_via_annular_width",
    ),
    (
        &["copper", "minimum_via_diameter"],
        "board.design_settings.rules.min_via_diameter",
    ),
    (
        &["copper", "copper_to_hole_clearance"],
        "board.design_settings.rules.min_hole_clearance",
    ),
    (
        &["copper", "copper_to_edge_clearance"],
        "board.design_settings.rules.min_copper_edge_clearance",
    ),
    (
        &["holes", "minimum_through_hole"],
        "board.design_settings.rules.min_through_hole_diameter",
    ),
    (
        &["holes", "hole_to_hole_clearance"],
        "board.design_settings.rules.min_hole_to_hole",
    ),
    (
        &["uvias", "minimum_uvia_diameter"],
        "board.design_settings.rules.min_microvia_diameter",
    ),
    (
        &["uvias", "minimum_uvia_hole"],
        "board.design_settings.rules.min_microvia_drill",
    ),
    (
        &["silkscreen", "minimum_item_clearance"],
        "board.design_settings.rules.min_silk_clearance",
    ),
    (
        &["silkscreen", "minimum_text_height"],
        "board.design_settings.rules.min_text_height",
    ),
    (
        &["solder_mask", "to_copper_clearance"],
        "board.design_settings.rules.solder_mask_to_copper_clearance",
    ),
    (
        &["zones", "minimum_clearance"],
        "board.design_settings.defaults.zones.min_clearance",
    ),
];

/// Constraints KiCad reads only from the board file's `(setup ...)`, in the order it writes
/// them: (constraint path, setup key, written when zero).
pub(crate) const SETUP_RULES: &[(&[&str], &str, bool)] = &[
    (&["solder_mask", "clearance"], "pad_to_mask_clearance", true),
    (
        &["solder_mask", "minimum_width"],
        "solder_mask_min_width",
        false,
    ),
];

pub(crate) fn patch_kicad_pro(
    pro_path: &Path,
    board_config: Option<&BoardConfig>,
    assignments: &HashMap<String, String>,
    layout_name: &str,
) -> Result<()> {
    let source = fs::read_to_string(pro_path)
        .with_context(|| format!("Failed to read {}", pro_path.display()))?;
    let mut project: Value = serde_json::from_str(&source)
        .with_context(|| format!("Failed to parse {}", pro_path.display()))?;

    patch_text_variables(&mut project, layout_name);
    if let Some(board_config) = board_config {
        patch_project_value(&mut project, board_config, assignments);
    }

    let mut serialized = serde_json::to_string_pretty(&project)?;
    serialized.push('\n');

    fs::write(pro_path, serialized)
        .with_context(|| format!("Failed to write {}", pro_path.display()))
}

pub(crate) fn extract_design_rules(
    pro_path: &Path,
    board: &pcb_sexpr::Sexpr,
) -> Result<Option<DesignRules>> {
    let source = fs::read_to_string(pro_path)
        .with_context(|| format!("Failed to read {}", pro_path.display()))?;
    let project: Value = serde_json::from_str(&source)
        .with_context(|| format!("Failed to parse {}", pro_path.display()))?;
    Ok(extract_design_rules_from_project_value(&project, board))
}

fn patch_project_value(
    project: &mut Value,
    board_config: &BoardConfig,
    assignments: &HashMap<String, String>,
) {
    // .kicad_pro patch policy:
    // - Constraints: set only mapped fields provided by board_config.
    // - Predefined sizes: merge/add missing Zener entries; keep existing user entries.
    // - Netclasses: upsert by class name and patch known fields only.
    // - Netclass patterns: upsert by pattern (when assignments are present).
    // - Never delete unknown keys/classes/patterns; repeated runs are idempotent.
    // - Drop the `SETUP_RULES` copies earlier versions wrote here; KiCad ignores them.
    if let Some(rules) = project
        .pointer_mut("/board/design_settings/rules")
        .and_then(Value::as_object_mut)
    {
        rules.remove("solder_mask_clearance");
        rules.remove("solder_mask_min_width");
    }
    patch_constraints(project, board_config);
    patch_predefined_sizes(project, board_config);
    patch_netclasses(project, board_config);

    if !assignments.is_empty() {
        patch_netclass_patterns(project, assignments);
    }
}

fn patch_text_variables(project: &mut Value, layout_name: &str) {
    let vars = ensure_object(
        ensure_object(project)
            .entry("text_variables".to_string())
            .or_insert_with(|| Value::Object(Map::new())),
    );

    vars.insert(
        "PCB_NAME".to_string(),
        Value::String(layout_name.to_string()),
    );
    vars.insert(
        "PCB_VERSION".to_string(),
        Value::String(PCB_VERSION_PLACEHOLDER.to_string()),
    );
    vars.insert(
        "PCB_GIT_HASH".to_string(),
        Value::String(PCB_GIT_HASH_PLACEHOLDER.to_string()),
    );
}

fn extract_design_rules_from_project_value(
    project: &Value,
    board: &pcb_sexpr::Sexpr,
) -> Option<DesignRules> {
    let constraints = extract_constraints(project, board);
    let predefined_sizes = extract_predefined_sizes(project);
    let netclasses = extract_netclasses(project);

    if constraints.is_none() && predefined_sizes.is_none() && netclasses.is_empty() {
        return None;
    }

    Some(DesignRules {
        constraints,
        predefined_sizes,
        netclasses,
    })
}

fn extract_constraints(project: &Value, board: &pcb_sexpr::Sexpr) -> Option<Value> {
    let mut constraints = Value::Object(Map::new());

    for (source_path, target_path) in CONSTRAINT_MAPPINGS {
        if let Some(value) =
            get_value_at_iter(project, target_path.split('.')).and_then(Value::as_f64)
        {
            set_value_at_iter(
                &mut constraints,
                source_path.iter().copied(),
                Value::from(value),
            );
        }
    }
    let setup = board.find_list("setup").unwrap_or_default();
    for (source_path, key, _) in SETUP_RULES {
        if let Some(value) = pcb_sexpr::find_child_list(setup, key)
            .and_then(|item| item.get(1))
            .and_then(|v| v.as_float().or_else(|| v.as_int().map(|v| v as f64)))
        {
            set_value_at_iter(
                &mut constraints,
                source_path.iter().copied(),
                Value::from(value),
            );
        }
    }

    if constraints.as_object().is_some_and(Map::is_empty) {
        None
    } else {
        Some(constraints)
    }
}

fn extract_predefined_sizes(project: &Value) -> Option<Value> {
    let mut deduped_track_widths: Vec<f64> = Vec::new();
    for width in get_value_at_iter(project, "board.design_settings.track_widths".split('.'))
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
        .filter_map(Value::as_f64)
        .filter(|width| !is_placeholder_width(*width))
    {
        ensure_width_present(&mut deduped_track_widths, width);
    }
    let track_widths: Vec<Value> = deduped_track_widths.into_iter().map(Value::from).collect();

    let mut via_dimensions: Vec<Value> = Vec::new();
    for (diameter, drill) in
        get_value_at_iter(project, "board.design_settings.via_dimensions".split('.'))
            .and_then(Value::as_array)
            .into_iter()
            .flat_map(|items| items.iter())
            .filter_map(via_pair)
            .filter(|pair| !is_placeholder_via(*pair))
    {
        ensure_via_present(&mut via_dimensions, diameter, drill);
    }

    let mut predefined = Map::new();
    if !track_widths.is_empty() {
        predefined.insert("track_widths".to_string(), Value::Array(track_widths));
    }
    if !via_dimensions.is_empty() {
        predefined.insert("via_dimensions".to_string(), Value::Array(via_dimensions));
    }

    (!predefined.is_empty()).then_some(Value::Object(predefined))
}

fn extract_netclasses(project: &Value) -> Vec<NetClass> {
    get_value_at_iter(project, "net_settings.classes".split('.'))
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|classes| classes.iter())
        .filter_map(|class| {
            let name = class.get("name").and_then(Value::as_str)?.to_string();
            let priority = class
                .get("priority")
                .and_then(value_as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let color = class
                .get("pcb_color")
                .and_then(Value::as_str)
                .filter(|color| *color != DEFAULT_COLOR)
                .map(ToString::to_string);

            Some(NetClass {
                name,
                clearance: class.get("clearance").and_then(Value::as_f64),
                track_width: class.get("track_width").and_then(Value::as_f64),
                via_diameter: class.get("via_diameter").and_then(Value::as_f64),
                via_drill: class.get("via_drill").and_then(Value::as_f64),
                microvia_diameter: class.get("microvia_diameter").and_then(Value::as_f64),
                microvia_drill: class.get("microvia_drill").and_then(Value::as_f64),
                diff_pair_width: class.get("diff_pair_width").and_then(Value::as_f64),
                diff_pair_gap: class.get("diff_pair_gap").and_then(Value::as_f64),
                diff_pair_via_gap: class.get("diff_pair_via_gap").and_then(Value::as_f64),
                priority,
                color,
                single_ended_impedance: impedance(class, "single_ended_impedance"),
                differential_pair_impedance: impedance(class, "differential_pair_impedance"),
            })
        })
        .collect()
}

/// A netclass impedance target pcb layout wrote, in ohms.
fn impedance(class: &Value, key: &str) -> Option<PhysicalValue> {
    let ohms = class.get(key).and_then(Value::as_f64)?;
    (ohms.is_finite() && ohms > 0.0).then(|| PhysicalValue::new(ohms, 0.0, PhysicalUnit::Ohms))
}

fn patch_constraints(project: &mut Value, board_config: &BoardConfig) {
    let Some(constraints) = board_config
        .design_rules
        .as_ref()
        .and_then(|dr| dr.constraints.as_ref())
    else {
        return;
    };

    for (source_path, target_path) in CONSTRAINT_MAPPINGS {
        if let Some(value) =
            get_value_at_iter(constraints, source_path.iter().copied()).and_then(Value::as_f64)
        {
            set_value_at_iter(project, target_path.split('.'), Value::from(value));
        }
    }
}

fn patch_predefined_sizes(project: &mut Value, board_config: &BoardConfig) {
    let Some(predefined_sizes) = board_config
        .design_rules
        .as_ref()
        .and_then(|dr| dr.predefined_sizes.as_ref())
    else {
        return;
    };

    if let Some(track_widths) =
        get_value_at_iter(predefined_sizes, ["track_widths"]).and_then(Value::as_array)
    {
        // KiCad reserves index 0 as the "use netclass" sentinel.
        let widths = std::iter::once(0.0)
            .chain(
                get_value_at_iter(project, "board.design_settings.track_widths".split('.'))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flat_map(|existing| existing.iter())
                    .filter_map(Value::as_f64),
            )
            .chain(track_widths.iter().filter_map(Value::as_f64))
            .filter(|width| !is_placeholder_width(*width))
            .fold(vec![0.0], |mut widths, width| {
                ensure_width_present(&mut widths, width);
                widths
            });

        set_value_at_iter(
            project,
            "board.design_settings.track_widths".split('.'),
            Value::Array(widths.into_iter().map(Value::from).collect()),
        );
    }

    if let Some(via_dimensions) =
        get_value_at_iter(predefined_sizes, ["via_dimensions"]).and_then(Value::as_array)
    {
        // KiCad reserves index 0 as the "use netclass" sentinel.
        let vias = get_value_at_iter(project, "board.design_settings.via_dimensions".split('.'))
            .and_then(Value::as_array)
            .into_iter()
            .flat_map(|existing| existing.iter().cloned())
            .filter(|entry| !via_pair(entry).is_some_and(is_placeholder_via))
            .fold(
                vec![json!({ "diameter": 0.0, "drill": 0.0 })],
                |mut vias, entry| {
                    vias.push(entry);
                    vias
                },
            );

        let vias = via_dimensions
            .iter()
            .filter_map(via_pair)
            .filter(|pair| !is_placeholder_via(*pair))
            .fold(vias, |mut vias, (diameter, drill)| {
                ensure_via_present(&mut vias, diameter, drill);
                vias
            });

        set_value_at_iter(
            project,
            "board.design_settings.via_dimensions".split('.'),
            Value::Array(vias),
        );
    }
}

fn patch_netclasses(project: &mut Value, board_config: &BoardConfig) {
    let netclasses = board_config.netclasses();
    if netclasses.is_empty() {
        return;
    }

    let mut classes = get_value_at_iter(project, "net_settings.classes".split('.'))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut class_index = index_by_string_field(&classes, "name");

    let mut ordered: Vec<&_> = netclasses
        .iter()
        .filter(|nc| nc.name != "Default")
        .collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    let iter = netclasses
        .iter()
        .filter(|nc| nc.name == "Default")
        .chain(ordered);

    let mut next_non_default_priority = classes
        .iter()
        .filter(|class| class.get("name").and_then(Value::as_str) != Some("Default"))
        .filter_map(|class| class.get("priority").and_then(value_as_i64))
        .max()
        .map(|max| max + 1)
        .unwrap_or(0);
    let mut tuning_profiles = Vec::new();

    for netclass in iter {
        let existing_idx = class_index.get(&netclass.name).copied();
        let obj = upsert_object_by_string_field(
            &mut classes,
            &mut class_index,
            "name",
            netclass.name.as_str(),
        );

        if netclass.name == "Default" {
            obj.insert("priority".to_string(), Value::from(i64::from(i32::MAX)));
        } else if existing_idx.is_none() || !obj.contains_key("priority") {
            let priority = if let Some(priority) = netclass.priority {
                i64::from(priority)
            } else {
                let priority = next_non_default_priority;
                next_non_default_priority += 1;
                priority
            };
            obj.insert("priority".to_string(), Value::from(priority));
        }

        let pcb_color = netclass
            .color
            .as_deref()
            .map(kicad_css_color_string)
            .or_else(|| {
                obj.get("pcb_color")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| DEFAULT_COLOR.to_string());
        obj.insert("pcb_color".to_string(), Value::String(pcb_color));

        let schematic_color = obj
            .get("schematic_color")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| DEFAULT_COLOR.to_string());
        obj.insert(
            "schematic_color".to_string(),
            Value::String(schematic_color),
        );

        if netclass.name == "Default" {
            obj.entry("wire_width".to_string())
                .or_insert(Value::from(DEFAULT_WIRE_WIDTH_MIL));
            obj.entry("bus_width".to_string())
                .or_insert(Value::from(DEFAULT_BUS_WIDTH_MIL));
            obj.entry("line_style".to_string())
                .or_insert(Value::from(DEFAULT_LINE_STYLE));
        }

        set_number_if_some(obj, "clearance", netclass.clearance);
        set_number_if_some(obj, "track_width", netclass.track_width);
        set_number_if_some(obj, "via_diameter", netclass.via_diameter);
        set_number_if_some(obj, "via_drill", netclass.via_drill);
        set_number_if_some(obj, "microvia_diameter", netclass.microvia_diameter);
        set_number_if_some(obj, "microvia_drill", netclass.microvia_drill);
        set_number_if_some(obj, "diff_pair_width", netclass.diff_pair_width);
        set_number_if_some(obj, "diff_pair_gap", netclass.diff_pair_gap);
        set_number_if_some(obj, "diff_pair_via_gap", netclass.diff_pair_via_gap);

        // Not KiCad fields: Zener owns them, so a target removed from the class goes too.
        for (key, ohms) in [
            (
                "single_ended_impedance",
                netclass.single_ended_impedance_ohms(),
            ),
            (
                "differential_pair_impedance",
                netclass.differential_pair_impedance_ohms(),
            ),
        ] {
            match ohms {
                Some(ohms) => obj.insert(key.to_string(), Value::from(ohms)),
                None => obj.remove(key),
            };
        }

        // KiCad's tuning profile has one type, so a class with both targets gets the differential one.
        let target = netclass
            .differential_pair_impedance_ohms()
            .map(|ohms| (TUNING_PROFILE_DIFFERENTIAL, ohms))
            .or_else(|| {
                netclass
                    .single_ended_impedance_ohms()
                    .map(|ohms| (TUNING_PROFILE_SINGLE, ohms))
            });
        // Each impedance class gets a profile of its own name, so classes with different targets
        // never share one. Once the class loses its target, it no longer selects that profile.
        let profile = obj.get("tuning_profile").and_then(Value::as_str);
        if let Some((profile_type, ohms)) = target {
            obj.insert(
                "tuning_profile".to_string(),
                Value::String(netclass.name.clone()),
            );
            tuning_profiles.push((netclass.name.clone(), profile_type, ohms));
        } else if profile == Some(netclass.name.as_str()) {
            obj.insert("tuning_profile".to_string(), Value::String(String::new()));
        }
    }

    set_value_at_iter(
        project,
        "net_settings.classes".split('.'),
        Value::Array(classes),
    );

    if !tuning_profiles.is_empty() {
        patch_tuning_profiles(project, &tuning_profiles);
    }
}

/// Upsert the KiCad tuning profile of each impedance-controlled netclass, named after the class.
/// Only the type and target are Zener's; per-layer geometry, delays and via overrides that the
/// designer set up in KiCad are kept. A new profile has no layer entries, so KiCad derives no routing or DRC rules
/// from it until the designer adds them.
fn patch_tuning_profiles(project: &mut Value, targets: &[(String, i64, f64)]) {
    let root = ensure_object(project)
        .entry("tuning_profiles".to_string())
        .or_insert_with(|| json!({ "meta": { "version": 0 } }));
    let mut profiles = get_value_at_iter(root, ["tuning_profiles_impedance_geometric"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut index = index_by_string_field(&profiles, "profile_name");

    for (name, profile_type, ohms) in targets {
        let (profile_type, ohms) = (*profile_type, *ohms);
        let profile =
            upsert_object_by_string_field(&mut profiles, &mut index, "profile_name", name);
        profile.insert("type".to_string(), Value::from(profile_type));
        profile.insert("target_impedance".to_string(), Value::from(ohms));
        // KiCad reads every one of these keys without a default.
        for (key, default) in [
            ("enable_time_domain_tuning", Value::Bool(false)),
            ("layer_entries", json!([])),
            ("via_prop_delay", Value::from(0)),
            ("via_overrides", json!([])),
        ] {
            profile.entry(key.to_string()).or_insert(default);
        }
    }

    set_value_at_iter(
        root,
        ["tuning_profiles_impedance_geometric"],
        Value::Array(profiles),
    );
}

fn patch_netclass_patterns(project: &mut Value, assignments: &HashMap<String, String>) {
    let mut patterns = get_value_at_iter(project, "net_settings.netclass_patterns".split('.'))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pattern_index = index_by_string_field(&patterns, "pattern");

    let mut sorted: Vec<_> = assignments.iter().collect();
    sorted.sort_by_key(|(name, _)| *name);

    for (pattern_name, netclass_name) in sorted {
        let obj = upsert_object_by_string_field(
            &mut patterns,
            &mut pattern_index,
            "pattern",
            pattern_name.as_str(),
        );
        obj.insert(
            "netclass".to_string(),
            Value::String(netclass_name.to_string()),
        );
    }

    set_value_at_iter(
        project,
        "net_settings.netclass_patterns".split('.'),
        Value::Array(patterns),
    );
}

fn set_number_if_some(obj: &mut Map<String, Value>, key: &str, value: Option<f64>) {
    if let Some(value) = value {
        obj.insert(key.to_string(), Value::from(value));
    }
}

fn kicad_css_color_string(raw: &str) -> String {
    let color = raw.trim();
    if let Some(hex) = color.strip_prefix('#') {
        let bytes = match hex.len() {
            6 => u32::from_str_radix(hex, 16).ok().map(|v| {
                (
                    ((v >> 16) & 0xFF) as u8,
                    ((v >> 8) & 0xFF) as u8,
                    (v & 0xFF) as u8,
                    0xFF_u8,
                )
            }),
            8 => u32::from_str_radix(hex, 16).ok().map(|v| {
                (
                    ((v >> 24) & 0xFF) as u8,
                    ((v >> 16) & 0xFF) as u8,
                    ((v >> 8) & 0xFF) as u8,
                    (v & 0xFF) as u8,
                )
            }),
            _ => None,
        };

        if let Some((r, g, b, a)) = bytes {
            if a == 0xFF {
                return format!("rgb({r}, {g}, {b})");
            }

            return format!("rgba({r}, {g}, {b}, {:.3})", a as f64 / 255.0);
        }
    }

    color.to_string()
}

fn ensure_width_present(widths: &mut Vec<f64>, width: f64) {
    if !widths.iter().any(|existing| approx_eq(*existing, width)) {
        widths.push(width);
    }
}

fn ensure_via_present(vias: &mut Vec<Value>, diameter: f64, drill: f64) {
    let exists = vias.iter().any(|entry| {
        let Some((existing_diameter, existing_drill)) = via_pair(entry) else {
            return false;
        };
        approx_eq(existing_diameter, diameter) && approx_eq(existing_drill, drill)
    });

    if !exists {
        vias.push(json!({ "diameter": diameter, "drill": drill }));
    }
}

fn via_pair(value: &Value) -> Option<(f64, f64)> {
    let diameter = value.get("diameter")?.as_f64()?;
    let drill = value.get("drill")?.as_f64()?;
    Some((diameter, drill))
}

fn is_placeholder_width(width: f64) -> bool {
    approx_eq(width, 0.0)
}

fn is_placeholder_via((diameter, drill): (f64, f64)) -> bool {
    approx_eq(diameter, 0.0) && approx_eq(drill, 0.0)
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= FLOAT_EQ_EPS
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
}

fn get_value_at_iter<I, S>(root: &Value, path: I) -> Option<&Value>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    path.into_iter()
        .try_fold(root, |current, segment| current.get(segment.as_ref()))
}

fn set_value_at_iter<I, S>(root: &mut Value, path: I, value: Value)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut current = root;
    let mut segments = path.into_iter().peekable();
    while let Some(segment) = segments.next() {
        let segment = segment.as_ref();
        if segments.peek().is_none() {
            ensure_object(current).insert(segment.to_string(), value);
            return;
        }
        let object = ensure_object(current);
        current = object
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("value must be object")
}

fn index_by_string_field(items: &[Value], key: &str) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if let Some(name) = item.get(key).and_then(Value::as_str) {
            index.entry(name.to_string()).or_insert(idx);
        }
    }
    index
}

fn upsert_object_by_string_field<'a>(
    items: &'a mut Vec<Value>,
    index: &mut HashMap<String, usize>,
    key: &str,
    value: &str,
) -> &'a mut Map<String, Value> {
    let idx = if let Some(idx) = index.get(value).copied() {
        idx
    } else {
        items.push(Value::Object(Map::new()));
        let idx = items.len() - 1;
        index.insert(value.to_string(), idx);
        idx
    };

    let obj = ensure_object(&mut items[idx]);
    obj.insert(key.to_string(), Value::String(value.to_string()));
    obj
}

#[cfg(test)]
mod tests {
    use super::{extract_design_rules_from_project_value, patch_project_value};
    use pcb_zen_core::lang::stackup::BoardConfig;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn patches_constraints_and_predefined_sizes() {
        let mut project = json!({
            "board": {
                "design_settings": {
                    "rules": {
                        "solder_mask_clearance": 0.05,
                        "solder_mask_min_width": 0.1
                    },
                    "defaults": {},
                    "track_widths": [],
                    "via_dimensions": []
                }
            },
            "net_settings": {
                "classes": [],
                "netclass_patterns": []
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "constraints": {
                    "copper": {
                        "minimum_clearance": 0.12,
                        "minimum_track_width": 0.11
                    },
                    "holes": {
                        "minimum_through_hole": 0.3
                    },
                    "solder_mask": {
                        "clearance": 0.02,
                        "minimum_width": 0.05,
                        "to_copper_clearance": 0.01
                    },
                    "zones": {
                        "minimum_clearance": 0.25
                    }
                },
                "predefined_sizes": {
                    "track_widths": [0.15, 0.2],
                    "via_dimensions": [
                        {"diameter": 0.5, "drill": 0.3}
                    ]
                }
            }
        }))
        .unwrap();

        patch_project_value(&mut project, &config, &HashMap::new());

        assert_eq!(
            project["board"]["design_settings"]["rules"]["min_clearance"],
            json!(0.12)
        );
        assert_eq!(
            project["board"]["design_settings"]["rules"]["min_track_width"],
            json!(0.11)
        );
        assert_eq!(
            project["board"]["design_settings"]["rules"]["min_through_hole_diameter"],
            json!(0.3)
        );
        // KiCad reads these from the board file, not the project.
        let rules = project["board"]["design_settings"]["rules"]
            .as_object()
            .unwrap();
        assert!(!rules.contains_key("solder_mask_clearance"));
        assert!(!rules.contains_key("solder_mask_min_width"));
        assert_eq!(
            project["board"]["design_settings"]["rules"]["solder_mask_to_copper_clearance"],
            json!(0.01)
        );
        assert_eq!(
            project["board"]["design_settings"]["defaults"]["zones"]["min_clearance"],
            json!(0.25)
        );
        assert_eq!(
            project["board"]["design_settings"]["track_widths"],
            json!([0.0, 0.15, 0.2])
        );
        assert_eq!(
            project["board"]["design_settings"]["via_dimensions"],
            json!([{"diameter": 0.0, "drill": 0.0}, {"diameter": 0.5, "drill": 0.3}])
        );
    }

    #[test]
    fn merges_predefined_sizes_without_deleting_existing_entries() {
        let mut project = json!({
            "board": {
                "design_settings": {
                    "track_widths": [0.0, 0.1, 0.2],
                    "via_dimensions": [
                        {"diameter": 0.0, "drill": 0.0},
                        {"diameter": 0.4, "drill": 0.2, "note": "keep"},
                        {"diameter": 0.5, "drill": 0.3}
                    ]
                }
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "predefined_sizes": {
                    "track_widths": [0.2, 0.25],
                    "via_dimensions": [
                        {"diameter": 0.5, "drill": 0.3},
                        {"diameter": 0.6, "drill": 0.3}
                    ]
                }
            }
        }))
        .unwrap();

        patch_project_value(&mut project, &config, &HashMap::new());

        assert_eq!(
            project["board"]["design_settings"]["track_widths"],
            json!([0.0, 0.1, 0.2, 0.25])
        );
        assert_eq!(
            project["board"]["design_settings"]["via_dimensions"],
            json!([
                {"diameter": 0.0, "drill": 0.0},
                {"diameter": 0.4, "drill": 0.2, "note": "keep"},
                {"diameter": 0.5, "drill": 0.3},
                {"diameter": 0.6, "drill": 0.3}
            ])
        );
    }

    #[test]
    fn normalizes_predefined_size_placeholders_to_index_zero() {
        let mut project = json!({
            "board": {
                "design_settings": {
                    "track_widths": [0.2, 0.0, 0.25],
                    "via_dimensions": [
                        {"diameter": 0.4, "drill": 0.2},
                        {"diameter": 0.0, "drill": 0.0, "note": "keep"},
                        {"diameter": 0.5, "drill": 0.3}
                    ]
                }
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "predefined_sizes": {
                    "track_widths": [0.3],
                    "via_dimensions": [
                        {"diameter": 0.6, "drill": 0.3}
                    ]
                }
            }
        }))
        .unwrap();

        patch_project_value(&mut project, &config, &HashMap::new());

        assert_eq!(
            project["board"]["design_settings"]["track_widths"],
            json!([0.0, 0.2, 0.25, 0.3])
        );
        assert_eq!(
            project["board"]["design_settings"]["via_dimensions"],
            json!([
                {"diameter": 0.0, "drill": 0.0},
                {"diameter": 0.4, "drill": 0.2},
                {"diameter": 0.5, "drill": 0.3},
                {"diameter": 0.6, "drill": 0.3}
            ])
        );
    }

    #[test]
    fn patches_netclasses_and_patterns() {
        let mut project = json!({
            "board": {
                "design_settings": {
                    "rules": {}
                }
            },
            "net_settings": {
                "classes": [
                    {
                        "name": "Default",
                        "priority": 2147483647,
                        "wire_width": 6,
                        "bus_width": 12,
                        "line_style": 0,
                        "pcb_color": "rgba(0, 0, 0, 0.000)",
                        "schematic_color": "rgba(0, 0, 0, 0.000)",
                        "tuning_profile": ""
                    },
                    {
                        "name": "OldClass",
                        "priority": 0
                    }
                ],
                "netclass_patterns": [
                    {"pattern": "LEGACY", "netclass": "OldClass"}
                ]
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "netclasses": [
                    {
                        "name": "Default",
                        "track_width": 0.19
                    },
                    {
                        "name": "USB",
                        "clearance": 0.2,
                        "track_width": 0.22,
                        "color": "#00C200FF"
                    }
                ]
            }
        }))
        .unwrap();

        let mut assignments = HashMap::new();
        assignments.insert("USB_DN".to_string(), "USB".to_string());
        assignments.insert("USB_DP".to_string(), "USB".to_string());

        patch_project_value(&mut project, &config, &assignments);

        let classes = project["net_settings"]["classes"].as_array().unwrap();
        assert_eq!(classes.len(), 3);
        assert_eq!(classes[0]["name"], "Default");
        assert_eq!(classes[0]["track_width"], json!(0.19));
        assert_eq!(classes[0]["wire_width"], json!(6));
        assert_eq!(classes[1]["name"], "OldClass");
        assert_eq!(classes[2]["name"], "USB");
        assert_eq!(classes[2]["priority"], json!(1));
        assert_eq!(classes[2]["track_width"], json!(0.22));
        assert_eq!(classes[2]["pcb_color"], "rgb(0, 194, 0)");
        assert!(
            classes[0]
                .as_object()
                .expect("default class must be object")
                .contains_key("tuning_profile")
        );
        assert!(
            !classes[2]
                .as_object()
                .expect("non-default class must be object")
                .contains_key("tuning_profile")
        );

        let patterns = project["net_settings"]["netclass_patterns"]
            .as_array()
            .unwrap();
        assert_eq!(patterns.len(), 3);
        assert_eq!(patterns[0]["pattern"], "LEGACY");
        assert_eq!(patterns[1]["pattern"], "USB_DN");
        assert_eq!(patterns[2]["pattern"], "USB_DP");
    }

    #[test]
    fn writes_impedance_targets_and_tuning_profiles() {
        let impedance =
            |ohms: &str| json!({"nominal": ohms, "min": ohms, "max": ohms, "unit": "Ohms"});
        let mut project = json!({
            "net_settings": {
                "classes": [
                    // A class that lost its target since the last sync stops selecting its profile.
                    {"name": "GBE_100", "differential_pair_impedance": 100.0, "tuning_profile": "GBE_100"},
                    // Classes with different targets that shared one profile get one each,
                    // and the shared profile keeps its target.
                    {"name": "PCIE_85", "tuning_profile": "Shared"},
                    {"name": "SE_50", "tuning_profile": "Shared"},
                    // A class without a target keeps the profile the designer picked.
                    {"name": "MANUAL", "tuning_profile": "Shared"}
                ]
            },
            "tuning_profiles": {
                "meta": {"version": 0},
                "tuning_profiles_impedance_geometric": [{
                    "profile_name": "Shared",
                    "type": 1,
                    "target_impedance": 77.0,
                    "enable_time_domain_tuning": false,
                    "layer_entries": [],
                    "via_prop_delay": 0,
                    "via_overrides": []
                }, {
                    "profile_name": "USB2_90",
                    "type": 0,
                    "target_impedance": 45.0,
                    "enable_time_domain_tuning": true,
                    "layer_entries": [{"signal_layer": "F.Cu", "width": 120000}],
                    "via_prop_delay": 0,
                    "via_overrides": []
                }]
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "netclasses": [
                    {"name": "GBE_100"},
                    {"name": "MANUAL"},
                    {"name": "PCIE_85", "differential_pair_impedance": impedance("85")},
                    {"name": "USB2_90", "differential_pair_impedance": impedance("90")},
                    {
                        "name": "SE_50",
                        "single_ended_impedance": impedance("50"),
                    },
                    {
                        "name": "BOTH",
                        "single_ended_impedance": impedance("45"),
                        "differential_pair_impedance": impedance("90.5"),
                    }
                ]
            }
        }))
        .unwrap();

        patch_project_value(&mut project, &config, &HashMap::new());

        let classes = project["net_settings"]["classes"].as_array().unwrap();
        let class = |name: &str| {
            classes
                .iter()
                .find(|class| class["name"] == name)
                .unwrap()
                .as_object()
                .unwrap()
        };
        assert!(!class("GBE_100").contains_key("differential_pair_impedance"));
        assert_eq!(class("GBE_100")["tuning_profile"], "");
        assert_eq!(class("MANUAL")["tuning_profile"], "Shared");
        assert_eq!(class("PCIE_85")["differential_pair_impedance"], json!(85.0));
        assert_eq!(class("PCIE_85")["tuning_profile"], "PCIE_85");
        assert_eq!(class("SE_50")["tuning_profile"], "SE_50");
        assert_eq!(class("USB2_90")["differential_pair_impedance"], json!(90.0));
        assert_eq!(class("USB2_90")["tuning_profile"], "USB2_90");
        assert_eq!(class("SE_50")["single_ended_impedance"], json!(50.0));
        assert!(!class("SE_50").contains_key("differential_pair_impedance"));
        assert_eq!(class("BOTH")["single_ended_impedance"], json!(45.0));
        assert_eq!(class("BOTH")["differential_pair_impedance"], json!(90.5));

        assert_eq!(
            project["tuning_profiles"]["tuning_profiles_impedance_geometric"],
            json!([
                {
                    "profile_name": "Shared",
                    "type": 1,
                    "target_impedance": 77.0,
                    "enable_time_domain_tuning": false,
                    "layer_entries": [],
                    "via_prop_delay": 0,
                    "via_overrides": []
                },
                {
                    "profile_name": "USB2_90",
                    "type": 1,
                    "target_impedance": 90.0,
                    "enable_time_domain_tuning": true,
                    "layer_entries": [{"signal_layer": "F.Cu", "width": 120000}],
                    "via_prop_delay": 0,
                    "via_overrides": []
                },
                {
                    "profile_name": "BOTH",
                    "type": 1,
                    "target_impedance": 90.5,
                    "enable_time_domain_tuning": false,
                    "layer_entries": [],
                    "via_prop_delay": 0,
                    "via_overrides": []
                },
                {
                    "profile_name": "PCIE_85",
                    "type": 1,
                    "target_impedance": 85.0,
                    "enable_time_domain_tuning": false,
                    "layer_entries": [],
                    "via_prop_delay": 0,
                    "via_overrides": []
                },
                {
                    "profile_name": "SE_50",
                    "type": 0,
                    "target_impedance": 50.0,
                    "enable_time_domain_tuning": false,
                    "layer_entries": [],
                    "via_prop_delay": 0,
                    "via_overrides": []
                }
            ])
        );

        // Repeated runs are idempotent.
        let once = project.clone();
        patch_project_value(&mut project, &config, &HashMap::new());
        assert_eq!(project, once);
    }

    #[test]
    fn preserves_existing_class_priority_on_upsert() {
        let mut project = json!({
            "net_settings": {
                "classes": [
                    {
                        "name": "Default",
                        "priority": 2147483647
                    },
                    {
                        "name": "USB",
                        "priority": 7
                    }
                ]
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({
            "design_rules": {
                "netclasses": [
                    {
                        "name": "USB",
                        "priority": 0,
                        "track_width": 0.22
                    }
                ]
            }
        }))
        .unwrap();

        patch_project_value(&mut project, &config, &HashMap::new());

        let classes = project["net_settings"]["classes"].as_array().unwrap();
        assert_eq!(classes[1]["name"], "USB");
        assert_eq!(classes[1]["priority"], json!(7));
        assert_eq!(classes[1]["track_width"], json!(0.22));
    }

    #[test]
    fn keeps_existing_netclass_patterns_when_no_assignments() {
        let mut project = json!({
            "net_settings": {
                "netclass_patterns": [
                    {"pattern": "VCC", "netclass": "Power"}
                ]
            }
        });

        let config: BoardConfig = serde_json::from_value(json!({})).unwrap();
        patch_project_value(&mut project, &config, &HashMap::new());

        assert_eq!(
            project["net_settings"]["netclass_patterns"],
            json!([{"pattern": "VCC", "netclass": "Power"}])
        );
    }

    #[test]
    fn extracts_design_rules_for_constraints_sizes_and_netclasses() {
        let project = json!({
            "board": {
                "design_settings": {
                    "rules": {
                        "min_clearance": 0.12,
                        "min_track_width": 0.11,
                        "min_through_hole_diameter": 0.3,
                        "solder_mask_clearance": 0.9,
                        "solder_mask_min_width": 0.9,
                        "solder_mask_to_copper_clearance": 0.01
                    },
                    "defaults": {
                        "zones": {
                            "min_clearance": 0.25
                        }
                    },
                    "track_widths": [0.0, 0.15, 0.2],
                    "via_dimensions": [
                        {"diameter": 0.0, "drill": 0.0},
                        {"diameter": 0.5, "drill": 0.3}
                    ]
                }
            },
            "net_settings": {
                "classes": [
                    {
                        "name": "Default",
                        "priority": 2147483647,
                        "track_width": 0.2,
                        "pcb_color": "rgba(0, 0, 0, 0.000)"
                    },
                    {
                        "name": "USB",
                        "priority": 1,
                        "differential_pair_impedance": 90.0,
                        "clearance": 0.18,
                        "track_width": 0.22,
                        "pcb_color": "rgb(0, 194, 0)"
                    }
                ]
            }
        });

        // KiCad ignores the project's mask clearance and minimum width; the board's apply.
        let board = pcb_sexpr::parse(
            "(kicad_pcb (setup (pad_to_mask_clearance 0.02) (solder_mask_min_width 0.05)))",
        )
        .unwrap();
        let design_rules = extract_design_rules_from_project_value(&project, &board)
            .expect("expected design rules");

        assert_eq!(
            design_rules.constraints,
            Some(json!({
                "copper": {
                    "minimum_clearance": 0.12,
                    "minimum_track_width": 0.11
                },
                "holes": {
                    "minimum_through_hole": 0.3
                },
                "solder_mask": {
                    "clearance": 0.02,
                    "minimum_width": 0.05,
                    "to_copper_clearance": 0.01
                },
                "zones": {
                    "minimum_clearance": 0.25
                }
            }))
        );
        assert_eq!(
            design_rules.predefined_sizes,
            Some(json!({
                "track_widths": [0.15, 0.2],
                "via_dimensions": [{"diameter": 0.5, "drill": 0.3}]
            }))
        );
        assert_eq!(design_rules.netclasses.len(), 2);
        assert_eq!(design_rules.netclasses[0].name, "Default");
        assert_eq!(design_rules.netclasses[0].color, None);
        assert_eq!(design_rules.netclasses[1].name, "USB");
        assert_eq!(
            design_rules.netclasses[1].color.as_deref(),
            Some("rgb(0, 194, 0)")
        );
        assert_eq!(
            design_rules.netclasses[1].differential_pair_impedance_ohms(),
            Some(90.0)
        );
        assert_eq!(design_rules.netclasses[1].single_ended_impedance, None);
    }

    #[test]
    fn returns_none_for_empty_design_rules() {
        let project = json!({
            "board": {
                "design_settings": {
                    "track_widths": [0.0],
                    "via_dimensions": [{"diameter": 0.0, "drill": 0.0}]
                }
            },
            "net_settings": {
                "classes": []
            }
        });

        let board = pcb_sexpr::parse("(kicad_pcb (setup))").unwrap();
        assert!(extract_design_rules_from_project_value(&project, &board).is_none());
    }
}
