use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AttributeValue, Instance, InstanceKind, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomEntry {
    pub designators: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub well_known_module: Option<WellKnownModule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<Value<Voltage>>,
    pub dnp: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WellKnownModule {
    Capacitor(Capacitor),
    Resistor(Resistor),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capacitor {
    pub capacitance: Value<Capacitance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dielectric: Option<Dielectric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<Value<Resistance>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: Value<Resistance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Value<U: Unit> {
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
    #[serde(skip)]
    pub _unit: std::marker::PhantomData<U>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Voltage;
#[derive(Debug, Clone, PartialEq)]
pub struct Capacitance;
#[derive(Debug, Clone, PartialEq)]
pub struct Resistance;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dielectric {
    C0G,
    NP0,
    X5R,
    X7R,
    X7S,
    X7T,
    Y5V,
    Z5U,
}

pub trait Unit {
    fn unit_suffix() -> &'static str;
    fn default_multiplier() -> f64;
}

impl Unit for Voltage {
    fn unit_suffix() -> &'static str {
        "V"
    }

    fn default_multiplier() -> f64 {
        1.0 // Default to volts
    }
}

impl Unit for Capacitance {
    fn unit_suffix() -> &'static str {
        "F"
    }

    fn default_multiplier() -> f64 {
        1e-12 // Default to picofarads
    }
}

impl Unit for Resistance {
    fn unit_suffix() -> &'static str {
        "Ohm"
    }

    fn default_multiplier() -> f64 {
        1.0 // Default to ohms
    }
}

/// Error type for unit parsing
#[derive(Debug, Clone, PartialEq)]
pub struct UnitParseError {
    pub message: String,
}

impl std::fmt::Display for UnitParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unit parse error: {}", self.message)
    }
}

impl std::error::Error for UnitParseError {}

/// Parse unit strings like "100kOhm 1%" into Value<T>
impl<U: Unit> FromStr for Value<U> {
    type Err = UnitParseError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let spec = spec.trim();

        // Split and extract optional tolerance (last token ending with %)
        let mut tokens: Vec<&str> = spec.split_ascii_whitespace().collect();
        let mut tolerance = None;

        if let Some(last) = tokens.last() {
            if let Some(raw) = last.strip_suffix('%') {
                tolerance = Some(
                    raw.parse::<f64>().map_err(|_| UnitParseError {
                        message: format!("invalid tolerance value: '{last}'"),
                    })? / 100.0,
                );
                tokens.pop();
            }
        }

        // Join tokens to handle whitespace variants like "10 kOhm" → "10kOhm"
        let joined: String = tokens.join("");

        // Split numeric part from unit part
        let num_end = joined.find(|c: char| !(c.is_ascii_digit() || c == '.'));

        let (num_str, unit_str) = if let Some(end) = num_end {
            joined.split_at(end)
        } else {
            // No unit found - entire string is the number, use default multiplier
            (joined.as_str(), "")
        };

        let mut numeric = num_str.parse::<f64>().map_err(|_| UnitParseError {
            message: format!("invalid numeric value: '{num_str}'"),
        })?;

        if unit_str.is_empty() {
            // No unit specified, apply default multiplier for this unit type
            numeric *= U::default_multiplier();
        } else {
            // Unit specified, parse it normally
            numeric *= get_si_multiplier(unit_str, U::unit_suffix())?;
        }

        Ok(Value {
            value: numeric,
            tolerance,
            _unit: std::marker::PhantomData,
        })
    }
}

/// Get SI prefix multiplier for a unit string
fn get_si_multiplier(unit_str: &str, expected_suffix: &str) -> Result<f64, UnitParseError> {
    // Order matters - check longer prefixes first to avoid conflicts
    let multipliers = [
        ("Y", 1e24),  // yotta
        ("Z", 1e21),  // zetta
        ("E", 1e18),  // exa
        ("P", 1e15),  // peta
        ("T", 1e12),  // tera
        ("G", 1e9),   // giga
        ("M", 1e6),   // mega
        ("k", 1e3),   // kilo
        ("m", 1e-3),  // milli
        ("u", 1e-6),  // micro
        ("μ", 1e-6),  // micro (Greek letter)
        ("n", 1e-9),  // nano
        ("p", 1e-12), // pico
        ("f", 1e-15), // femto
        ("a", 1e-18), // atto
        ("z", 1e-21), // zepto
        ("y", 1e-24), // yocto
        ("", 1.0),    // base unit (check last)
    ];

    let expected_suffix_lower = expected_suffix.to_lowercase();
    let unit_lower = unit_str.to_lowercase();

    // First, check if the unit string is exactly a prefix (like "k", "M", etc.)
    // This handles cases like "5.1k" where there's no explicit unit
    for (prefix_str, multiplier) in &multipliers {
        if unit_str == *prefix_str {
            return Ok(*multiplier);
        }
    }

    // Check if the unit ends with the expected suffix
    if !unit_lower.ends_with(&expected_suffix_lower) {
        return Err(UnitParseError {
            message: format!("Expected unit '{expected_suffix}' but got '{unit_str}'"),
        });
    }

    // Extract prefix by removing the suffix (case-insensitive)
    let prefix = &unit_str[..unit_str.len() - expected_suffix.len()];

    // Find matching multiplier (case-sensitive for prefixes)
    for (prefix_str, multiplier) in &multipliers {
        if prefix == *prefix_str {
            return Ok(*multiplier);
        }
    }

    Err(UnitParseError {
        message: format!("Invalid unit prefix: '{prefix}' in '{unit_str}'"),
    })
}

/// Generate a Bill of Materials from a schematic
pub fn generate_bom(schematic: &Schematic) -> Vec<BomEntry> {
    // Clone schematic to assign reference designators without mutating original
    let mut working_schematic = schematic.clone();
    working_schematic.assign_reference_designators();

    let mut bom_entries = Vec::new();

    // Iterate through all instances and find components
    for (instance_ref, instance) in &working_schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }

        let designator = instance
            .reference_designator
            .clone()
            .unwrap_or_else(|| format!("?{}", instance_ref.instance_path.join(".")));

        // Extract attributes directly from the original map
        let mpn = get_string_attribute(&instance.attributes, &["MPN", "Mpn", "mpn"]);
        let manufacturer =
            get_string_attribute(&instance.attributes, &["Manufacturer", "manufacturer"]);
        let package = get_string_attribute(&instance.attributes, &["Package", "package"]);
        let description =
            get_string_attribute(&instance.attributes, &["Description", "description"]);
        let voltage = get_string_attribute(&instance.attributes, &["Voltage"])
            .and_then(|v| v.parse::<Value<Voltage>>().ok());

        // Determine if component should be populated
        let do_not_populate = get_string_attribute(
            &instance.attributes,
            &["do_not_populate", "Do_not_populate", "DNP", "dnp"],
        )
        .map(|s| s.to_lowercase() == "true" || s == "1")
        .unwrap_or(false);

        // Check if it's a test component
        let is_test_component = designator.starts_with("TP")
            || get_string_attribute(&instance.attributes, &["type", "Type"])
                .map(|t| t.to_lowercase().contains("test"))
                .unwrap_or(false);

        let dnp = do_not_populate || is_test_component;

        let value = get_string_attribute(&instance.attributes, &["Value"]);

        // Extract alternates from AttributeValue::Array
        let alternatives = instance
            .attributes
            .get("Alternatives")
            .and_then(|attr| match attr {
                AttributeValue::Array(arr) => Some(
                    arr.iter()
                        .filter_map(|av| match av {
                            AttributeValue::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect::<Vec<String>>(),
                ),
                _ => None,
            })
            .unwrap_or_default();

        let well_known_module = detect_well_known_module(instance, &instance.attributes);

        bom_entries.push(BomEntry {
            designators: vec![designator],
            mpn,
            manufacturer,
            alternatives,
            package,
            value,
            description,
            well_known_module,
            dnp,
            voltage,
        });
    }

    // Group entries and return
    group_bom_entries(bom_entries)
}

/// Detect well-known modules based on instance.type_ref
fn detect_well_known_module(
    instance: &Instance,
    attributes: &HashMap<String, AttributeValue>,
) -> Option<WellKnownModule> {
    let source_path = instance.type_ref.source_path.to_string_lossy();
    let module_name = &instance.type_ref.module_name;

    // Check for resistor
    if module_name == "R" && source_path.ends_with("generics/Resistor.zen") {
        if let Some(resistance_str) = get_string_attribute(attributes, &["Resistance"]) {
            if let Ok(resistance) = resistance_str.parse::<Value<Resistance>>() {
                return Some(WellKnownModule::Resistor(Resistor { resistance }));
            }
        }
    }

    // Check for capacitor
    if module_name == "C" && source_path.ends_with("generics/Capacitor.zen") {
        if let Some(capacitance_str) = get_string_attribute(attributes, &["Capacitance"]) {
            if let Ok(capacitance) = capacitance_str.parse::<Value<Capacitance>>() {
                let dielectric = get_string_attribute(attributes, &["Dielectric"]).and_then(|d| {
                    match d.as_str() {
                        "C0G" => Some(Dielectric::C0G),
                        "NP0" => Some(Dielectric::NP0),
                        "X5R" => Some(Dielectric::X5R),
                        "X7R" => Some(Dielectric::X7R),
                        "X7S" => Some(Dielectric::X7S),
                        "X7T" => Some(Dielectric::X7T),
                        "Y5V" => Some(Dielectric::Y5V),
                        "Z5U" => Some(Dielectric::Z5U),
                        _ => None,
                    }
                });

                let esr = get_string_attribute(attributes, &["Esr"])
                    .and_then(|e| e.parse::<Value<Resistance>>().ok());

                return Some(WellKnownModule::Capacitor(Capacitor {
                    capacitance,
                    dielectric,
                    esr,
                }));
            }
        }
    }

    None
}

/// Group BOM entries that have identical properties
fn group_bom_entries(entries: Vec<BomEntry>) -> Vec<BomEntry> {
    use std::collections::HashMap;

    type GroupKey = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
    );
    let mut grouped: HashMap<GroupKey, BomEntry> = HashMap::new();

    for entry in entries {
        let key = (
            entry.mpn.clone(),
            entry.manufacturer.clone(),
            entry.package.clone(),
            entry.value.clone(),
            entry.description.clone(),
            entry.dnp,
        );

        grouped
            .entry(key)
            .and_modify(|existing| {
                existing.designators.extend(entry.designators.clone());
            })
            .or_insert(entry);
    }

    let mut result: Vec<_> = grouped.into_values().collect();
    // Sort designators within each entry
    for entry in &mut result {
        entry.designators.sort();
    }
    result.sort_by(|a, b| a.designators[0].cmp(&b.designators[0]));
    result
}

/// Helper function to extract string values from attributes, trying multiple key variations
fn get_string_attribute(
    attributes: &HashMap<String, AttributeValue>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|&key| {
        attributes.get(key).and_then(|attr| match attr {
            AttributeValue::String(s) => Some(s.clone()),
            AttributeValue::Physical(s) => Some(s.clone()),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_unit_parsing() {
        // Resistance with units
        let r1: Value<Resistance> = "100Ohm".parse().unwrap();
        assert_eq!(r1.value, 100.0);
        assert_eq!(r1.tolerance, None);

        let r2: Value<Resistance> = "4.7kOhm 1%".parse().unwrap();
        assert_eq!(r2.value, 4700.0);
        assert_eq!(r2.tolerance, Some(0.01));

        // Capacitance
        let c1: Value<Capacitance> = "100nF".parse().unwrap();
        assert!((c1.value - 100e-9).abs() < 1e-15);
        assert_eq!(c1.tolerance, None);

        // Voltage
        let v1: Value<Voltage> = "3.3V".parse().unwrap();
        assert_eq!(v1.value, 3.3);
        assert_eq!(v1.tolerance, None);

        // Whitespace variants
        let r3: Value<Resistance> = "10 kOhm".parse().unwrap();
        assert_eq!(r3.value, 10000.0);
        assert_eq!(r3.tolerance, None);

        let v2: Value<Voltage> = "12 V 0.5%".parse().unwrap();
        assert_eq!(v2.value, 12.0);
        assert_eq!(v2.tolerance, Some(0.005));
    }

    #[test]
    fn test_unitless_parsing() {
        // Resistance values without units (default to ohms)
        let r1: Value<Resistance> = "150".parse().unwrap();
        assert_eq!(r1.value, 150.0);
        assert_eq!(r1.tolerance, None);

        let r2: Value<Resistance> = "27".parse().unwrap();
        assert_eq!(r2.value, 27.0);
        assert_eq!(r2.tolerance, None);

        let r3: Value<Resistance> = "261".parse().unwrap();
        assert_eq!(r3.value, 261.0);
        assert_eq!(r3.tolerance, None);

        let r4: Value<Resistance> = "110".parse().unwrap();
        assert_eq!(r4.value, 110.0);
        assert_eq!(r4.tolerance, None);

        // With tolerance but no unit
        let r5: Value<Resistance> = "150 5%".parse().unwrap();
        assert_eq!(r5.value, 150.0);
        assert_eq!(r5.tolerance, Some(0.05));

        // Capacitance values without units (default to picofarads)
        let c1: Value<Capacitance> = "22".parse().unwrap();
        assert!((c1.value - 22e-12).abs() < 1e-15);
        assert_eq!(c1.tolerance, None);

        let c2: Value<Capacitance> = "100".parse().unwrap();
        assert!((c2.value - 100e-12).abs() < 1e-15);
        assert_eq!(c2.tolerance, None);

        // Voltage values without units (default to volts)
        let v1: Value<Voltage> = "3.3".parse().unwrap();
        assert_eq!(v1.value, 3.3);
        assert_eq!(v1.tolerance, None);

        let v2: Value<Voltage> = "12".parse().unwrap();
        assert_eq!(v2.value, 12.0);
        assert_eq!(v2.tolerance, None);

        // Mixed cases - prefix with no explicit unit (defaults apply after prefix)
        let r6: Value<Resistance> = "5.1k".parse().unwrap();
        assert_eq!(r6.value, 5100.0);
        assert_eq!(r6.tolerance, None);

        let r7: Value<Resistance> = "10k 1%".parse().unwrap();
        assert_eq!(r7.value, 10000.0);
        assert_eq!(r7.tolerance, Some(0.01));

        let r8: Value<Resistance> = "47k".parse().unwrap();
        assert_eq!(r8.value, 47000.0);
        assert_eq!(r8.tolerance, None);

        let r9: Value<Resistance> = "33k".parse().unwrap();
        assert_eq!(r9.value, 33000.0);
        assert_eq!(r9.tolerance, None);
    }

    #[test]
    fn test_parse_errors() {
        assert!("".parse::<Value<Resistance>>().is_err());
        assert!("Ohm".parse::<Value<Resistance>>().is_err());
        assert!("100Ohm %".parse::<Value<Resistance>>().is_err());
    }

    #[test]
    fn test_detect_well_known_module() {
        use crate::ModuleRef;
        use std::path::PathBuf;

        // Create a mock resistor instance
        let module_ref = ModuleRef::new(
            PathBuf::from("/path/to/generics/Resistor.zen"),
            "R".to_string(),
        );
        let instance = Instance::component(module_ref);

        let mut attributes = HashMap::new();
        attributes.insert(
            "Resistance".to_string(),
            AttributeValue::String("10kOhm".to_string()),
        );

        let result = detect_well_known_module(&instance, &attributes);

        match result {
            Some(WellKnownModule::Resistor(resistor)) => {
                assert_eq!(resistor.resistance.value, 10000.0);
                assert_eq!(resistor.resistance.tolerance, None);
            }
            _ => panic!("Expected resistor module"),
        }
    }

    #[test]
    fn test_untagged_serde() {
        // Test that serde can distinguish between modules based on field presence

        // Resistor should deserialize when "resistance" field is present
        let resistor_json = r#"{
            "resistance": {"value": 10000.0, "tolerance": 0.01},
            "voltage": {"value": 50.0, "tolerance": 0.0}
        }"#;

        let resistor: WellKnownModule = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            WellKnownModule::Resistor(r) => {
                assert_eq!(r.resistance.value, 10000.0);
                assert_eq!(r.resistance.tolerance, Some(0.01));
            }
            _ => panic!("Expected Resistor variant"),
        }

        // Capacitor should deserialize when "capacitance" field is present
        let capacitor_json = r#"{
            "capacitance": {"value": 100e-9, "tolerance": 0.2},
            "voltage": {"value": 16.0, "tolerance": 0.0},
            "dielectric": "X7R"
        }"#;

        let capacitor: WellKnownModule = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            WellKnownModule::Capacitor(c) => {
                assert!((c.capacitance.value - 100e-9).abs() < 1e-15);
                assert_eq!(c.capacitance.tolerance, Some(0.2));
                assert_eq!(c.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected Capacitor variant"),
        }

        // Test round-trip serialization
        let original_resistor = WellKnownModule::Resistor(Resistor {
            resistance: Value {
                value: 1000.0,
                tolerance: Some(0.05),
                _unit: std::marker::PhantomData,
            },
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: WellKnownModule = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }
}
