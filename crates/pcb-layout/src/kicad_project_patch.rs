use anyhow::{Context, Result};
use pcb_zen_core::lang::stackup::BoardConfig;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const DEFAULT_COLOR: &str = "rgba(0, 0, 0, 0.000)";
const DEFAULT_WIRE_WIDTH_MIL: i64 = 6;
const DEFAULT_BUS_WIDTH_MIL: i64 = 12;
const DEFAULT_LINE_STYLE: i64 = 0;
const FLOAT_EQ_EPS: f64 = 1e-9;

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
];

pub(crate) fn patch_kicad_pro(
    pro_path: &Path,
    board_config: &BoardConfig,
    assignments: &HashMap<String, String>,
) -> Result<()> {
    let source = fs::read_to_string(pro_path)
        .with_context(|| format!("Failed to read {}", pro_path.display()))?;
    let mut project: Value = serde_json::from_str(&source)
        .with_context(|| format!("Failed to parse {}", pro_path.display()))?;

    patch_project_value(&mut project, board_config, assignments);

    let mut serialized = serde_json::to_string_pretty(&project)?;
    serialized.push('\n');

    fs::write(pro_path, serialized)
        .with_context(|| format!("Failed to write {}", pro_path.display()))
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
    patch_constraints(project, board_config);
    patch_predefined_sizes(project, board_config);
    patch_netclasses(project, board_config);

    if !assignments.is_empty() {
        patch_netclass_patterns(project, assignments);
    }
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
        if let Some(value) = get_nested_number(constraints, source_path) {
            set_value_at_path(project, target_path, Value::from(value));
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

    if let Some(track_widths) = get_nested_array(predefined_sizes, &["track_widths"]) {
        let mut widths: Vec<f64> = get_array_at_path(project, "board.design_settings.track_widths")
            .into_iter()
            .flat_map(|existing| existing.iter())
            .filter_map(Value::as_f64)
            .collect();

        ensure_width_present(&mut widths, 0.0);

        for width in track_widths.iter().filter_map(Value::as_f64) {
            ensure_width_present(&mut widths, width);
        }

        set_value_at_path(
            project,
            "board.design_settings.track_widths",
            Value::Array(widths.into_iter().map(Value::from).collect()),
        );
    }

    if let Some(via_dimensions) = get_nested_array(predefined_sizes, &["via_dimensions"]) {
        let mut vias: Vec<Value> =
            get_array_at_path(project, "board.design_settings.via_dimensions")
                .cloned()
                .unwrap_or_default();

        ensure_via_present(&mut vias, 0.0, 0.0);

        for entry in via_dimensions {
            let Some(diameter) = entry.get("diameter").and_then(Value::as_f64) else {
                continue;
            };
            let Some(drill) = entry.get("drill").and_then(Value::as_f64) else {
                continue;
            };
            ensure_via_present(&mut vias, diameter, drill);
        }

        set_value_at_path(
            project,
            "board.design_settings.via_dimensions",
            Value::Array(vias),
        );
    }
}

fn patch_netclasses(project: &mut Value, board_config: &BoardConfig) {
    let netclasses = board_config.netclasses();
    if netclasses.is_empty() {
        return;
    }

    let mut classes = get_array_at_path(project, "net_settings.classes")
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
    }

    set_value_at_path(project, "net_settings.classes", Value::Array(classes));
}

fn patch_netclass_patterns(project: &mut Value, assignments: &HashMap<String, String>) {
    let mut patterns = get_array_at_path(project, "net_settings.netclass_patterns")
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

    set_value_at_path(
        project,
        "net_settings.netclass_patterns",
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

fn get_nested_number(value: &Value, path: &[&str]) -> Option<f64> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
        .and_then(Value::as_f64)
}

fn get_nested_array<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
        .and_then(Value::as_array)
}

fn get_array_at_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Vec<Value>> {
    dotted_path
        .split('.')
        .try_fold(root, |current, segment| current.get(segment))
        .and_then(Value::as_array)
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

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= FLOAT_EQ_EPS
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
}

fn set_value_at_path(root: &mut Value, dotted_path: &str, value: Value) {
    let mut segments = dotted_path.split('.').peekable();
    let mut current = root;

    while let Some(segment) = segments.next() {
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
    use super::patch_project_value;
    use pcb_zen_core::lang::stackup::BoardConfig;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn patches_constraints_and_predefined_sizes() {
        let mut project = json!({
            "board": {
                "design_settings": {
                    "rules": {},
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
        assert!(classes[0]
            .as_object()
            .expect("default class must be object")
            .contains_key("tuning_profile"));
        assert!(!classes[2]
            .as_object()
            .expect("non-default class must be object")
            .contains_key("tuning_profile"));

        let patterns = project["net_settings"]["netclass_patterns"]
            .as_array()
            .unwrap();
        assert_eq!(patterns.len(), 3);
        assert_eq!(patterns[0]["pattern"], "LEGACY");
        assert_eq!(patterns[1]["pattern"], "USB_DN");
        assert_eq!(patterns[2]["pattern"], "USB_DP");
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
}
