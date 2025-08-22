use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AttributeValue, InstanceKind, PhysicalValue, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomEntry {
    pub path: String,
    pub designator: String,
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
    pub voltage: Option<PhysicalValue>,
    pub dnp: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregatedBomEntry {
    pub designators: BTreeSet<String>,
    pub manufacturer: Option<String>,
    pub mpn: Option<String>,
    pub alternatives: Vec<String>,
    pub package: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub well_known_module: Option<WellKnownModule>,
    pub voltage: Option<PhysicalValue>,
    pub dnp: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupKey {
    mpn: Option<String>,
    manufacturer: Option<String>,
    package: Option<String>,
    value: Option<String>,
    description: Option<String>,
    alternatives: Vec<String>,
    dnp: bool,
}

impl From<&BomEntry> for GroupKey {
    fn from(entry: &BomEntry) -> Self {
        Self {
            mpn: entry.mpn.clone(),
            manufacturer: entry.manufacturer.clone(),
            package: entry.package.clone(),
            value: entry.value.clone(),
            description: entry.description.clone(),
            alternatives: entry.alternatives.clone(),
            dnp: entry.dnp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "component_type")]
pub enum WellKnownModule {
    Capacitor(Capacitor),
    Resistor(Resistor),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capacitor {
    pub capacitance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dielectric: Option<Dielectric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: PhysicalValue,
}

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

impl FromStr for Dielectric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "C0G" => Ok(Dielectric::C0G),
            "NP0" => Ok(Dielectric::NP0),
            "X5R" => Ok(Dielectric::X5R),
            "X7R" => Ok(Dielectric::X7R),
            "X7S" => Ok(Dielectric::X7S),
            "X7T" => Ok(Dielectric::X7T),
            "Y5V" => Ok(Dielectric::Y5V),
            "Z5U" => Ok(Dielectric::Z5U),
            _ => Err(format!("Unknown dielectric: {s}")),
        }
    }
}

/// Generate ungrouped BOM entries from a schematic
pub fn generate_bom_entries(schematic: &mut Schematic) -> BTreeMap<String, BomEntry> {
    schematic.assign_reference_designators();

    let mut bom_entries = BTreeMap::new();

    // Iterate through all instances and find components
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }

        let designator = instance
            .reference_designator
            .clone()
            .unwrap_or_else(|| format!("?{}", instance_ref.instance_path.join(".")));

        let path = instance_ref.instance_path.join(".");

        // Extract attributes directly from the original map
        let mpn = get_string_attribute(&instance.attributes, &["MPN", "Mpn", "mpn"]);
        let manufacturer =
            get_string_attribute(&instance.attributes, &["Manufacturer", "manufacturer"]);
        let package = get_string_attribute(&instance.attributes, &["Package", "package"]);
        let description =
            get_string_attribute(&instance.attributes, &["Description", "description"]);
        let voltage = get_physical_attribute(&instance.attributes, &["__voltage__"]);

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

        // Extract alternates from structured AttributeValue::Array
        let alternatives = instance
            .attributes
            .get("__alternatives__")
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

        let well_known_module = detect_well_known_module(&instance.attributes);

        bom_entries.insert(
            path.clone(),
            BomEntry {
                path: path.clone(),
                designator,
                mpn,
                manufacturer,
                alternatives,
                package,
                value,
                description,
                well_known_module,
                dnp,
                voltage,
            },
        );
    }

    bom_entries
}

/// Detect well-known modules based on Type attribute
fn detect_well_known_module(
    attributes: &HashMap<String, AttributeValue>,
) -> Option<WellKnownModule> {
    let module_type = get_string_attribute(attributes, &["Type"])?;

    match module_type.to_lowercase().as_str() {
        "resistor" => {
            if let Some(resistance) = get_physical_attribute(attributes, &["__resistance__"]) {
                return Some(WellKnownModule::Resistor(Resistor { resistance }));
            }
        }
        "capacitor" => {
            if let Some(capacitance) = get_physical_attribute(attributes, &["__capacitance__"]) {
                let dielectric =
                    get_string_attribute(attributes, &["Dielectric"]).and_then(|d| d.parse().ok());

                let esr = get_physical_attribute(attributes, &["__esr__"]);

                return Some(WellKnownModule::Capacitor(Capacitor {
                    capacitance,
                    dielectric,
                    esr,
                }));
            }
        }
        _ => {}
    }

    None
}

/// Group BOM entries that have identical properties
pub fn group_bom_entries(entries: BTreeMap<String, BomEntry>) -> Vec<AggregatedBomEntry> {
    use std::collections::HashMap;

    let mut grouped: HashMap<GroupKey, AggregatedBomEntry> = HashMap::new();

    for (_, entry) in entries {
        let key = GroupKey::from(&entry);

        grouped
            .entry(key)
            .and_modify(|existing| {
                existing.designators.insert(entry.designator.clone());
            })
            .or_insert(AggregatedBomEntry {
                designators: {
                    let mut set = BTreeSet::new();
                    set.insert(entry.designator);
                    set
                },
                manufacturer: entry.manufacturer,
                mpn: entry.mpn,
                alternatives: entry.alternatives,
                package: entry.package,
                value: entry.value,
                description: entry.description,
                well_known_module: entry.well_known_module,
                voltage: entry.voltage,
                dnp: entry.dnp,
            });
    }

    let mut result: Vec<_> = grouped.into_values().collect();
    result.sort_by(|a, b| a.designators.first().cmp(&b.designators.first()));
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
            AttributeValue::Physical(pv) => Some(pv.to_string()),
            _ => None,
        })
    })
}

/// Helper function to extract PhysicalValue from attributes, trying multiple key variations
fn get_physical_attribute(
    attributes: &HashMap<String, AttributeValue>,
    keys: &[&str],
) -> Option<PhysicalValue> {
    keys.iter().find_map(|&key| {
        attributes.get(key).and_then(|attr| match attr {
            AttributeValue::Physical(pv) => Some(pv.clone()),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PhysicalUnit;
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    #[test]
    fn test_detect_well_known_module() {
        // Create a mock resistor with Type attribute
        let mut attributes = HashMap::new();
        attributes.insert(
            "Type".to_string(),
            AttributeValue::String("resistor".to_string()),
        );
        attributes.insert(
            "__resistance__".to_string(),
            AttributeValue::Physical(PhysicalValue::new(10000.0, 0.01, PhysicalUnit::Ohms)),
        );

        let result = detect_well_known_module(&attributes);

        match result {
            Some(WellKnownModule::Resistor(resistor)) => {
                assert_eq!(
                    resistor.resistance.value,
                    Decimal::from_f64(10000.0).unwrap()
                );
                assert_eq!(
                    resistor.resistance.tolerance,
                    Decimal::from_f64(0.01).unwrap()
                );
            }
            _ => panic!("Expected resistor module"),
        }

        // Test capacitor detection
        let mut capacitor_attributes = HashMap::new();
        capacitor_attributes.insert(
            "Type".to_string(),
            AttributeValue::String("capacitor".to_string()),
        );
        capacitor_attributes.insert(
            "__capacitance__".to_string(),
            AttributeValue::Physical(PhysicalValue::new(100e-9, 0.2, PhysicalUnit::Farads)),
        );
        capacitor_attributes.insert(
            "Dielectric".to_string(),
            AttributeValue::String("X7R".to_string()),
        );

        let result = detect_well_known_module(&capacitor_attributes);

        match result {
            Some(WellKnownModule::Capacitor(capacitor)) => {
                let expected_value = Decimal::from_f64(100e-9).unwrap();
                assert!(
                    (capacitor.capacitance.value - expected_value).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(
                    capacitor.capacitance.tolerance,
                    Decimal::from_f64(0.2).unwrap()
                );
                assert_eq!(capacitor.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected capacitor module"),
        }
    }

    #[test]
    fn test_tagged_serde() {
        // Test that serde can distinguish between modules using component_type tag

        // Resistor should deserialize with component_type tag
        let resistor_json = r#"{
            "component_type": "Resistor",
            "resistance": {"value": "10000.0", "tolerance": "0.01", "unit": "Ohms"}
        }"#;

        let resistor: WellKnownModule = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            WellKnownModule::Resistor(r) => {
                assert_eq!(r.resistance.value, Decimal::from_f64(10000.0).unwrap());
                assert_eq!(r.resistance.tolerance, Decimal::from_f64(0.01).unwrap());
            }
            _ => panic!("Expected Resistor variant"),
        }

        // Capacitor should deserialize with component_type tag
        let capacitor_json = r#"{
            "component_type": "Capacitor",
            "capacitance": {"value": "100e-9", "tolerance": "0.2", "unit": "Farads"},
            "dielectric": "X7R"
        }"#;

        let capacitor: WellKnownModule = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            WellKnownModule::Capacitor(c) => {
                let expected_value = Decimal::from_f64(100e-9).unwrap();
                assert!(
                    (c.capacitance.value - expected_value).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(c.capacitance.tolerance, Decimal::from_f64(0.2).unwrap());
                assert_eq!(c.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected Capacitor variant"),
        }

        // Test round-trip serialization
        let original_resistor = WellKnownModule::Resistor(Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: WellKnownModule = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }

    #[test]
    fn test_get_string_attribute() {
        let mut attributes = HashMap::new();
        attributes.insert(
            "Mpn".to_string(),
            AttributeValue::String("RC0603FR-0710KL".to_string()),
        );
        attributes.insert(
            "__resistance__".to_string(),
            AttributeValue::Physical(PhysicalValue::new(10000.0, 0.0, PhysicalUnit::Ohms)),
        );

        // Test string attribute extraction
        let mpn = get_string_attribute(&attributes, &["MPN", "Mpn", "mpn"]);
        assert_eq!(mpn, Some("RC0603FR-0710KL".to_string()));

        // Test physical value converted to string
        let resistance_str = get_string_attribute(&attributes, &["__resistance__"]);
        assert!(resistance_str.is_some());

        // Test missing attribute
        let missing = get_string_attribute(&attributes, &["Missing"]);
        assert_eq!(missing, None);
    }

    #[test]
    fn test_get_physical_attribute() {
        let mut attributes = HashMap::new();
        let physical_value = PhysicalValue::new(4700.0, 0.01, PhysicalUnit::Ohms);
        attributes.insert(
            "__resistance__".to_string(),
            AttributeValue::Physical(physical_value.clone()),
        );
        attributes.insert(
            "StringValue".to_string(),
            AttributeValue::String("not physical".to_string()),
        );

        // Test physical attribute extraction
        let resistance = get_physical_attribute(&attributes, &["__resistance__"]);
        assert_eq!(resistance, Some(physical_value));

        // Test non-physical attribute
        let string_val = get_physical_attribute(&attributes, &["StringValue"]);
        assert_eq!(string_val, None);

        // Test missing attribute
        let missing = get_physical_attribute(&attributes, &["Missing"]);
        assert_eq!(missing, None);
    }
}
