use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{InstanceKind, PhysicalValue, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bom {
    entries: HashMap<String, BomEntry>,   // path -> BomEntry
    designators: HashMap<String, String>, // path -> designator
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GroupedBomEntry {
    designators: BTreeSet<String>,
    #[serde(flatten)]
    entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct BomEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    mpn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    alternatives: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    well_known_module: Option<WellKnownComponent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voltage: Option<PhysicalValue>,
    dnp: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UngroupedBomEntry {
    path: String,
    designator: String,
    #[serde(flatten)]
    entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "component_type")]
pub enum WellKnownComponent {
    Capacitor(Capacitor),
    Resistor(Resistor),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capacitor {
    pub capacitance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dielectric: Option<Dielectric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl Bom {
    pub fn from_schematic(schematic: &Schematic) -> Self {
        let mut designators = HashMap::<String, String>::new();
        let mut entries = HashMap::<String, BomEntry>::new();

        schematic
            .instances
            .iter()
            .filter(|(_, instance)| instance.kind == InstanceKind::Component)
            .for_each(|(instance_ref, instance)| {
                let designator = instance.reference_designator.clone().unwrap();
                let path = instance_ref.instance_path.join(".");
                let bom_entry = BomEntry {
                    mpn: instance.mpn(),
                    manufacturer: instance.manufacturer(),
                    description: instance.description(),
                    package: instance.package(),
                    value: instance.value(),
                    alternatives: instance.string_list_attr(&["__alternatives__"]),
                    well_known_module: detect_well_known_module(instance),
                    voltage: instance.physical_attr(&["__voltage__"]),
                    dnp: instance.dnp(),
                };
                entries.insert(path.clone(), bom_entry);
                designators.insert(path, designator);
            });

        Bom {
            entries,
            designators,
        }
    }

    pub fn ungrouped_json(&self) -> String {
        let mut entries = self
            .entries
            .iter()
            .map(|(path, entry)| UngroupedBomEntry {
                path: path.clone(),
                designator: self.designators[path].clone(),
                entry: entry.clone(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.designator.cmp(&b.designator));
        serde_json::to_string_pretty(&entries).unwrap()
    }

    pub fn grouped_json(&self) -> String {
        // Group entries by their BomEntry content
        let mut groups = HashMap::<BomEntry, BTreeSet<String>>::new();

        for (path, entry) in &self.entries {
            groups
                .entry(entry.clone())
                .or_default()
                .insert(self.designators[path].clone());
        }

        let mut grouped_entries = groups
            .into_iter()
            .map(|(entry, designators)| GroupedBomEntry { entry, designators })
            .collect::<Vec<_>>();

        grouped_entries.sort_by(|a, b| {
            let a_designator = a.designators.iter().next().unwrap();
            let b_designator = b.designators.iter().next().unwrap();
            a_designator.cmp(b_designator)
        });

        serde_json::to_string_pretty(&grouped_entries).unwrap()
    }
}

/// Detect well-known modules based on Type attribute
fn detect_well_known_module(instance: &crate::Instance) -> Option<WellKnownComponent> {
    match instance.component_type()?.as_str() {
        "resistor" => {
            if let Some(resistance) = instance.physical_attr(&["__resistance__"]) {
                return Some(WellKnownComponent::Resistor(Resistor { resistance }));
            }
        }
        "capacitor" => {
            if let Some(capacitance) = instance.physical_attr(&["__capacitance__"]) {
                let dielectric = instance
                    .string_attr(&["Dielectric", "dielectric"])
                    .and_then(|d| d.parse().ok());

                let esr = instance.physical_attr(&["__esr__"]);

                return Some(WellKnownComponent::Capacitor(Capacitor {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttributeValue, Instance, ModuleRef, PhysicalUnit};
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_instance(attributes: HashMap<String, AttributeValue>) -> Instance {
        Instance {
            type_ref: ModuleRef {
                source_path: PathBuf::new(),
                module_name: String::default(),
            },
            kind: InstanceKind::Component,
            attributes,
            children: Default::default(),
            reference_designator: Some("U1".to_string()),
        }
    }

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

        let instance = test_instance(attributes);
        let result = detect_well_known_module(&instance);

        match result {
            Some(WellKnownComponent::Resistor(resistor)) => {
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

        let instance = test_instance(capacitor_attributes);
        let result = detect_well_known_module(&instance);

        match result {
            Some(WellKnownComponent::Capacitor(capacitor)) => {
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

        let resistor: WellKnownComponent = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            WellKnownComponent::Resistor(r) => {
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

        let capacitor: WellKnownComponent = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            WellKnownComponent::Capacitor(c) => {
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
        let original_resistor = WellKnownComponent::Resistor(Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: WellKnownComponent = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }
}
