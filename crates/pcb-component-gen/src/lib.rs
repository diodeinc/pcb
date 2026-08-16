use anyhow::Result;
use deunicode::deunicode;
use minijinja::{Environment, UndefinedBehavior};
use pcb_eda::{Pin, Symbol};
use std::collections::{BTreeMap, BTreeSet};

const COMPONENT_ZEN_TEMPLATE: &str = include_str!("../templates/component.zen.jinja");

/// Sanitize a string for use as a directory/file name and Zener `Component(name=...)`.
///
/// This is shared across `pcb search` and `pcb import` so the output is consistent.
///
/// Process:
/// 1. Replace unsafe ASCII → underscore (keep a-z A-Z 0-9 - _, keep Unicode)
/// 2. Transliterate Unicode → ASCII
/// 3. Replace leftover unsafe chars → underscore
/// 4. Cleanup: collapse multiple underscores, trim leading/trailing
pub fn sanitize_mpn_for_path(mpn: &str) -> String {
    fn is_safe(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }

    // Replace unsafe ASCII with _, keep Unicode for transliteration.
    let ascii_cleaned: String = mpn
        .chars()
        .map(|c| if c.is_ascii() && !is_safe(c) { '_' } else { c })
        .collect();

    // Transliterate Unicode to ASCII.
    let transliterated = deunicode(&ascii_cleaned);

    // Replace any remaining unsafe chars from transliteration.
    let all_safe: String = transliterated
        .chars()
        .map(|c| if is_safe(c) { c } else { '_' })
        .collect();

    // Collapse multiple underscores and trim.
    let cleaned: String = all_safe
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if cleaned.is_empty() {
        "component".to_string()
    } else {
        cleaned
    }
}

/// Sanitize a pin name to create a valid Starlark identifier.
///
/// Output follows UPPERCASE convention for io() parameters.
///
/// Special handling:
/// - `~` or `!` at start: becomes `N_` prefix (e.g., `~CS` → `N_CS`)
/// - `+` at end: becomes `_POS` suffix (e.g., `V+` → `V_POS`)
/// - `-` at end: becomes `_NEG` suffix (e.g., `V-` → `V_NEG`)
/// - `+` or `-` elsewhere: becomes `_` (e.g., `A+B` → `A_B`)
/// - `#`: becomes `H` (e.g., `CS#` → `CSH`)
/// - All alphanumeric chars: uppercased
pub fn sanitize_pin_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(len + 8);

    for (i, &c) in chars.iter().enumerate() {
        let is_last = i == len.saturating_sub(1);

        match c {
            '+' if is_last => result.push_str("_POS"),
            '-' if is_last => result.push_str("_NEG"),
            '+' | '-' => result.push('_'),
            '~' | '!' => result.push_str("N_"), // NOT prefix
            '#' => result.push('H'),
            c if c.is_alphanumeric() => result.push(c.to_ascii_uppercase()),
            _ => result.push('_'),
        }
    }

    let sanitized = result
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if sanitized.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("P{sanitized}")
    } else {
        sanitized
    }
}

pub struct GenerateComponentZenArgs<'a> {
    pub component_name: &'a str,
    pub symbol: &'a Symbol,
    pub symbol_filename: &'a str,
    pub generated_by: &'a str,
    pub include_skip_bom: bool,
    pub include_skip_pos: bool,
    pub skip_bom_default: bool,
    pub skip_pos_default: bool,
}

/// One explicit logical-signal-to-physical-pin mapping supplied by an EDA importer.
///
/// Most component generation groups pins by the signal names in the symbol. Importers that must
/// preserve physical-pin identity can provide this plan instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateComponentPin {
    pub logical_name: String,
    pub pad_number: String,
    pub io_name: Option<String>,
}

fn pin_type_candidates(pin: &Pin) -> impl Iterator<Item = &str> {
    pin.electrical_type.as_deref().into_iter().chain(
        pin.alternates
            .iter()
            .filter_map(|alt| alt.electrical_type.as_deref()),
    )
}

/// Returns whether a pin group declares electrical types that are all `no_connect`.
///
/// Primary and alternate pin types are both considered. A group without any declared electrical
/// type is not treated as `no_connect`.
pub fn pins_are_only_no_connect<'a>(pins: impl IntoIterator<Item = &'a Pin>) -> bool {
    let mut saw_pin_type = false;
    for pin in pins {
        for pin_type in pin_type_candidates(pin) {
            saw_pin_type = true;
            if pin_type != "no_connect" {
                return false;
            }
        }
    }
    saw_pin_type
}

pub fn generated_signal_io_names(symbol: &Symbol) -> BTreeMap<String, String> {
    let mut signals: BTreeMap<String, Vec<&Pin>> = BTreeMap::new();
    for pin in symbol.canonical_pins() {
        signals
            .entry(pin.signal_name().to_string())
            .or_default()
            .push(pin);
    }

    // Distinct signal names can sanitize to the same identifier (e.g. `~CS` and `!CS`
    // both become `N_CS`). Track assigned names so a suffix like `N_CS_2` cannot collide
    // with another signal's natural sanitized name.
    let mut used = BTreeSet::new();
    signals
        .into_iter()
        .filter_map(|(signal_name, pins)| {
            (!pins_are_only_no_connect(pins)).then(|| {
                let base = sanitize_pin_name(&signal_name);
                let mut name = base.clone();
                let mut n = 2;
                while !used.insert(name.clone()) {
                    name = format!("{base}_{n}");
                    n += 1;
                }
                (signal_name, name)
            })
        })
        .collect()
}

pub fn generate_component_zen(args: GenerateComponentZenArgs<'_>) -> Result<String> {
    generate_component_zen_inner(args, None, None)
}

pub fn generate_component_zen_with_pins(
    args: GenerateComponentZenArgs<'_>,
    pins: &[GenerateComponentPin],
    footprint: Option<&str>,
) -> Result<String> {
    generate_component_zen_inner(args, Some(pins), footprint)
}

fn generate_component_zen_inner(
    args: GenerateComponentZenArgs<'_>,
    explicit_pins: Option<&[GenerateComponentPin]>,
    footprint: Option<&str>,
) -> Result<String> {
    let component_name = sanitize_mpn_for_path(args.component_name);
    let signal_io_names = generated_signal_io_names(args.symbol);

    let pin_groups_vec: Vec<_> = match explicit_pins {
        Some(pins) => pins
            .iter()
            .filter_map(|pin| pin.io_name.as_ref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|name| serde_json::json!({"sanitized_name": name}))
            .collect(),
        None => signal_io_names
            .values()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|name| serde_json::json!({"sanitized_name": name}))
            .collect(),
    };

    let pin_mappings: Vec<_> = match explicit_pins {
        Some(pins) => pins
            .iter()
            .filter_map(|pin| {
                pin.io_name.as_ref().map(|io_name| {
                    serde_json::json!({
                        "original_name_literal": serde_json::to_string(&pin.logical_name).expect("serialize pin name"),
                        "sanitized_name": io_name
                    })
                })
            })
            .collect(),
        None => signal_io_names
            .iter()
            .map(|(signal_name, io_name)| {
                serde_json::json!({
                    "original_name_literal": serde_json::to_string(signal_name).expect("serialize signal name"),
                    "sanitized_name": io_name
                })
            })
            .collect(),
    };
    let pin_defs: Vec<_> = explicit_pins
        .unwrap_or_default()
        .iter()
        .map(|pin| {
            serde_json::json!({
                "logical_name_literal": serde_json::to_string(&pin.logical_name).expect("serialize logical name"),
                "pad_number_literal": serde_json::to_string(&pin.pad_number).expect("serialize pad number"),
            })
        })
        .collect();

    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_template("component.zen", COMPONENT_ZEN_TEMPLATE)?;

    let content = env
        .get_template("component.zen")?
        .render(serde_json::json!({
            "component_name": component_name,
            "component_name_literal": serde_json::to_string(&component_name)?,
            "sym_path_literal": serde_json::to_string(args.symbol_filename)?,
            "pin_groups": pin_groups_vec,
            "pin_mappings": pin_mappings,
            "pin_defs": pin_defs,
            "footprint_literal": footprint.map(serde_json::to_string).transpose()?,
            "generated_by": args.generated_by,
            "include_skip_bom": args.include_skip_bom,
            "include_skip_pos": args.include_skip_pos,
            "skip_bom_default": args.skip_bom_default,
            "skip_pos_default": args.skip_pos_default,
        }))?;

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_pin_name_rules() {
        assert_eq!(sanitize_pin_name("~CS"), "N_CS");
        assert_eq!(sanitize_pin_name("V+"), "V_POS");
        assert_eq!(sanitize_pin_name("V-"), "V_NEG");
        assert_eq!(sanitize_pin_name("A+B"), "A_B");
        assert_eq!(sanitize_pin_name("CS#"), "CSH");
        assert_eq!(sanitize_pin_name("1V8"), "P1V8");
    }

    #[test]
    fn only_no_connect_requires_declared_primary_types() {
        let untyped = Pin::default();
        let no_connect = Pin {
            electrical_type: Some("no_connect".to_string()),
            ..Default::default()
        };
        let input = Pin {
            electrical_type: Some("input".to_string()),
            ..Default::default()
        };

        assert!(!pins_are_only_no_connect([&untyped]));
        assert!(pins_are_only_no_connect([&no_connect]));
        assert!(pins_are_only_no_connect([&no_connect, &no_connect]));
        assert!(!pins_are_only_no_connect([&no_connect, &input]));
    }

    #[test]
    fn only_no_connect_considers_alternate_types() {
        let alternate_no_connect = Pin {
            alternates: vec![pcb_eda::PinAlternate {
                electrical_type: Some("no_connect".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let alternate_input = Pin {
            electrical_type: Some("no_connect".to_string()),
            alternates: vec![pcb_eda::PinAlternate {
                electrical_type: Some("input".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(pins_are_only_no_connect([&alternate_no_connect]));
        assert!(!pins_are_only_no_connect([&alternate_input]));
    }

    #[test]
    fn generates_zen_with_pin_groups_and_mappings() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "~{INT}".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "~{INT}".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "VCC".to_string(),
                    number: "3".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        let mut docstring_lines = zen.lines();
        assert_eq!(docstring_lines.next(), Some("\"\"\""));
        assert_eq!(docstring_lines.next(), Some("MPN1"));
        assert_eq!(docstring_lines.next(), Some(""));
        assert_eq!(
            docstring_lines.next(),
            Some("Auto-generated using `pcb import`.")
        );
        assert!(zen.contains("N_INT = io(Net)"));
        assert!(zen.contains("\"~{INT}\": N_INT"));
        assert!(zen.contains("VCC"));
        assert!(!zen.contains("pin_defs"));
        assert!(!zen.contains("Pins = struct("));
        assert!(!zen.contains("Pins."));
    }

    #[test]
    fn explicit_pin_plan_renders_pin_defs_and_optional_footprint() {
        let zen = generate_component_zen_with_pins(
            GenerateComponentZenArgs {
                component_name: "DEVICE",
                symbol: &pcb_eda::Symbol::default(),
                symbol_filename: "DEVICE.kicad_sym",
                generated_by: "pcb import",
                include_skip_bom: false,
                include_skip_pos: false,
                skip_bom_default: false,
                skip_pos_default: false,
            },
            &[
                GenerateComponentPin {
                    logical_name: "D+__3".to_string(),
                    pad_number: "3".to_string(),
                    io_name: Some("D_POS_3".to_string()),
                },
                GenerateComponentPin {
                    logical_name: "NC\"\\é".to_string(),
                    pad_number: "10".to_string(),
                    io_name: None,
                },
            ],
            Some("Missing:Footprint"),
        )
        .unwrap();

        assert!(zen.contains("D_POS_3 = io(Net)"));
        assert!(zen.contains("footprint = \"Missing:Footprint\""));
        assert!(zen.contains("\"D+__3\": \"3\""));
        assert!(zen.contains("\"NC\\\"\\\\é\": \"10\""));
        assert!(zen.contains("\"D+__3\": D_POS_3"));
        assert!(!zen.contains("NC = io(Net)"));
    }

    #[test]
    fn omits_symbol_backed_fields_even_when_symbol_has_them() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            properties: [
                ("Footprint".to_string(), "X".to_string()),
                ("Datasheet".to_string(), "X.pdf".to_string()),
                (
                    "Manufacturer_Part_Number".to_string(),
                    "SYM-MPN".to_string(),
                ),
                ("Manufacturer_Name".to_string(), "SYM-MFR".to_string()),
            ]
            .into_iter()
            .collect(),
            pins: vec![pcb_eda::Pin {
                name: "VCC".to_string(),
                number: "1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        assert!(!zen.contains("footprint = File("));
        assert!(!zen.contains("datasheet = File("));
        assert!(!zen.contains("part = Part("));
        assert!(!zen.contains("config("));
    }

    #[test]
    fn renders_skip_flags_when_requested() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![pcb_eda::Pin {
                name: "VCC".to_string(),
                number: "1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: true,
            include_skip_pos: true,
            skip_bom_default: false,
            skip_pos_default: true,
        })
        .unwrap();

        assert!(zen.contains("skip_bom = config(bool, default = False)"));
        assert!(zen.contains("skip_pos = config(bool, default = True)"));
        assert!(zen.contains("skip_bom = skip_bom"));
        assert!(zen.contains("skip_pos = skip_pos"));
    }

    #[test]
    fn uses_pin_number_when_kicad_pin_name_is_placeholder() {
        let symbol = pcb_eda::Symbol {
            name: "C".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "~".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "~".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "TP_0.75mm_SMD",
            symbol: &symbol,
            symbol_filename: "C1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        assert!(zen.contains("name = \"TP_0_75mm_SMD\""));
        assert!(zen.contains("\"1\": P1"));
        assert!(zen.contains("\"2\": P2"));
        assert!(!zen.contains("\"~\":"));
    }

    #[test]
    fn duplicate_pin_names_merge_into_single_io() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "NC".to_string(),
                    number: "6".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "NC".to_string(),
                    number: "7".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        // Single io() for the shared signal name
        assert!(zen.contains("NC = io(Net)"));
        assert!(zen.contains("\"NC\": NC"));
        // No pin_defs needed
        assert!(!zen.contains("pin_defs"));
    }

    #[test]
    fn duplicate_pin_numbers_collapse_to_first_public_signal() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            internal_connectivity: pcb_eda::InternalConnectivity {
                duplicate_numbers_are_jumpers: true,
                groups: Vec::new(),
            },
            pins: vec![
                pcb_eda::Pin {
                    name: "A".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "B".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "C".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        assert!(zen.contains("A = io(Net)"));
        assert!(zen.contains("\"A\": A"));
        assert!(!zen.contains("B = io(Net)"));
        assert!(!zen.contains("\"B\": B"));
        assert!(zen.contains("C = io(Net)"));
    }

    #[test]
    fn colliding_pin_names_get_uniquified() {
        // ~CS and !CS both sanitize to N_CS — should not collapse onto one net.
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "~CS".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "!CS".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "VCC".to_string(),
                    number: "3".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        // "!CS" sorts before "~CS" in a BTreeMap, so it claims the base name first.
        assert!(zen.contains("N_CS = io(Net)"));
        assert!(zen.contains("N_CS_2 = io(Net)"));
        assert!(zen.contains("\"!CS\": N_CS"));
        assert!(zen.contains("\"~CS\": N_CS_2"));
        assert!(zen.contains("VCC = io(Net)"));
    }

    #[test]
    fn uniquify_skips_suffixes_that_already_exist() {
        // `!CS`/`~CS` both sanitize to `N_CS`. The naive `_2` suffix would collide
        // with a pin that already sanitizes to `N_CS_2`.
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "~CS".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "!CS".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "N_CS_2".to_string(),
                    number: "3".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let names = generated_signal_io_names(&symbol);
        let unique: BTreeSet<_> = names.values().collect();
        assert_eq!(unique.len(), names.len());
        assert_eq!(names.get("!CS").map(String::as_str), Some("N_CS"));
        assert_eq!(names.get("N_CS_2").map(String::as_str), Some("N_CS_2"));
        assert_eq!(names.get("~CS").map(String::as_str), Some("N_CS_3"));
    }

    #[test]
    fn skips_no_connect_pins_entirely() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "NC".to_string(),
                    number: "1".to_string(),
                    electrical_type: Some("no_connect".to_string()),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "NC".to_string(),
                    number: "2".to_string(),
                    electrical_type: Some("no_connect".to_string()),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "VCC".to_string(),
                    number: "3".to_string(),
                    electrical_type: Some("power_in".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            generated_by: "pcb import",
            include_skip_bom: false,
            include_skip_pos: false,
            skip_bom_default: false,
            skip_pos_default: false,
        })
        .unwrap();

        assert!(zen.contains("VCC = io(Net)"));
        assert!(zen.contains("\"VCC\": VCC"));
        assert!(!zen.contains("NC = io(Net)"));
        assert!(!zen.contains("\"NC\": NC"));
    }
}
