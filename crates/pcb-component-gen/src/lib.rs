use anyhow::Result;
use minijinja::Environment;
use pcb_eda::Symbol;
use std::collections::{BTreeMap, BTreeSet};

const COMPONENT_ZEN_TEMPLATE: &str = include_str!("../templates/component.zen.jinja");

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
    pub mpn: &'a str,
    pub component_name: &'a str,
    pub symbol: &'a Symbol,
    pub symbol_filename: &'a str,
    pub footprint_filename: Option<&'a str>,
    pub datasheet_filename: Option<&'a str>,
    pub manufacturer: Option<&'a str>,
    pub generated_by: &'a str,
}

pub fn generate_component_zen(args: GenerateComponentZenArgs<'_>) -> Result<String> {
    let mut pin_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pin in &args.symbol.pins {
        pin_groups
            .entry(sanitize_pin_name(&pin.name))
            .or_default()
            .insert(pin.name.clone());
    }

    let pin_groups_vec: Vec<_> = pin_groups
        .keys()
        .map(|name| serde_json::json!({"sanitized_name": name}))
        .collect();

    let pin_mappings: Vec<_> = pin_groups
        .iter()
        .flat_map(|(sanitized, originals)| {
            originals.iter().map(move |orig| {
                serde_json::json!({
                    "original_name": orig,
                    "sanitized_name": sanitized
                })
            })
        })
        .collect();

    let mut env = Environment::new();
    env.add_template("component.zen", COMPONENT_ZEN_TEMPLATE)?;

    let content = env
        .get_template("component.zen")?
        .render(serde_json::json!({
            "component_name": args.component_name,
            "mpn": args.mpn,
            "manufacturer": args.manufacturer,
            "sym_path": args.symbol_filename,
            "footprint_path": args.footprint_filename.unwrap_or(&format!("{}.kicad_mod", args.mpn)),
            "pin_groups": pin_groups_vec,
            "pin_mappings": pin_mappings,
            "description": args.symbol.description,
            "datasheet_file": args.datasheet_filename,
            "generated_by": args.generated_by,
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
    fn generates_zen_with_pin_groups_and_mappings() {
        let symbol = pcb_eda::Symbol {
            name: "X".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "~{INT}".to_string(),
                    number: "1".to_string(),
                },
                pcb_eda::Pin {
                    name: "~{INT}".to_string(),
                    number: "2".to_string(),
                },
                pcb_eda::Pin {
                    name: "VCC".to_string(),
                    number: "3".to_string(),
                },
            ],
            ..Default::default()
        };

        let zen = generate_component_zen(GenerateComponentZenArgs {
            mpn: "MPN1",
            component_name: "MPN1",
            symbol: &symbol,
            symbol_filename: "MPN1.kicad_sym",
            footprint_filename: Some("FP.kicad_mod"),
            datasheet_filename: None,
            manufacturer: Some("ACME"),
            generated_by: "pcb import",
        })
        .unwrap();

        assert!(zen.contains("Auto-generated using `pcb import`."));
        assert!(zen.contains("Pins = struct("));
        assert!(zen.contains("N_INT"));
        assert!(zen.contains("\"~{INT}\": Pins.N_INT"));
        assert!(zen.contains("VCC"));
        assert!(zen.contains("footprint = File(\"FP.kicad_mod\")"));
    }
}
