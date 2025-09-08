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
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_part: Option<BomMatchingValue>,
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

impl Capacitor {
    pub fn matches(
        &self,
        key: &CapacitorMatchingKey,
        entry_voltage: &Option<PhysicalValue>,
    ) -> bool {
        // Check capacitance range (key range must fit within component tolerance)
        if !key.capacitance.fits_within_default(&self.capacitance) {
            return false;
        }

        // Check voltage: key voltage must be <= entry voltage (or entry has no voltage requirement)
        if let Some(key_voltage) = &key.voltage {
            if let Some(entry_v) = entry_voltage {
                if key_voltage.value > entry_v.value {
                    return false;
                }
            }
        }

        // Check dielectric: key dielectric must match entry dielectric (or entry has no dielectric requirement)
        if let Some(key_dielectric) = &key.dielectric {
            if let Some(entry_dielectric) = &self.dielectric {
                if key_dielectric != entry_dielectric {
                    return false;
                }
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: PhysicalValue,
}

impl Resistor {
    pub fn matches(
        &self,
        key: &ResistorMatchingKey,
        entry_voltage: &Option<PhysicalValue>,
    ) -> bool {
        // Check resistance range (key range must fit within component tolerance)
        if !key.resistance.fits_within_default(&self.resistance) {
            return false;
        }

        // Check voltage: key voltage must be <= entry voltage (or entry has no voltage requirement)
        if let Some(key_voltage) = &key.voltage {
            if let Some(entry_v) = entry_voltage {
                if key_voltage.value > entry_v.value {
                    return false;
                }
            }
        }

        true
    }
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

// BOM Matching API
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BomMatchingKey {
    Mpn(String),
    Resistor(ResistorMatchingKey),
    Capacitor(CapacitorMatchingKey),
    Path(String),
    Designator(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResistorMatchingKey {
    pub resistance: PhysicalValue,
    pub voltage: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacitorMatchingKey {
    pub capacitance: PhysicalValue,
    pub voltage: Option<PhysicalValue>,
    pub dielectric: Option<Dielectric>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BomMatchingValue {
    pub distributor: String,
    pub distributor_pn: String,
    pub manufacturer: Option<String>,
    pub manufacturer_pn: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomMatchingRule {
    pub key: BomMatchingKey,
    pub value: BomMatchingValue,
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
                    matched_part: None,
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

    pub fn apply_bom_rule(&mut self, rule: &BomMatchingRule) {
        match &rule.key {
            BomMatchingKey::Designator(designator) => {
                // Find path for this designator
                if let Some((path, _)) = self.designators.iter().find(|(_, d)| *d == designator) {
                    if let Some(entry) = self.entries.get_mut(path) {
                        entry.matched_part = Some(rule.value.clone());
                    }
                }
            }
            BomMatchingKey::Path(target_path) => {
                if let Some(entry) = self.entries.get_mut(target_path) {
                    entry.matched_part = Some(rule.value.clone());
                }
            }
            BomMatchingKey::Mpn(mpn) => {
                for entry in self.entries.values_mut() {
                    if entry.mpn.as_ref() == Some(mpn) {
                        entry.matched_part = Some(rule.value.clone());
                    }
                }
            }
            BomMatchingKey::Resistor(resistor_key) => {
                for entry in self.entries.values_mut() {
                    if let Some(WellKnownComponent::Resistor(resistor)) = &entry.well_known_module {
                        if resistor.matches(resistor_key, &entry.voltage) {
                            entry.matched_part = Some(rule.value.clone());
                        }
                    }
                }
            }
            BomMatchingKey::Capacitor(capacitor_key) => {
                for entry in self.entries.values_mut() {
                    if let Some(WellKnownComponent::Capacitor(capacitor)) = &entry.well_known_module
                    {
                        if capacitor.matches(capacitor_key, &entry.voltage) {
                            entry.matched_part = Some(rule.value.clone());
                        }
                    }
                }
            }
        }
    }

    pub fn apply_bom_rules(&mut self, rules: &[BomMatchingRule]) {
        for rule in rules {
            self.apply_bom_rule(rule);
        }
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

    #[test]
    fn test_resistor_matching() {
        // Component: 1kΩ ±0% (defaults to ±1%)
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Ohms),
        };

        // Key: 1kΩ ±1% - should match (exact fit)
        let matching_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&matching_key, &None));

        // Key: 1kΩ ±0.5% - should match (tighter tolerance fits)
        let tighter_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.005, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&tighter_key, &None));

        // Key: 1kΩ ±5% - should NOT match (looser tolerance doesn't fit)
        let looser_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&looser_key, &None));

        // Key: 2kΩ ±1% - should NOT match (different value)
        let different_value_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(2000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&different_value_key, &None));
    }

    #[test]
    fn test_resistor_voltage_matching() {
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
        };
        let component_voltage = Some(PhysicalValue::new(50.0, 0.0, PhysicalUnit::Volts));

        // Key voltage (25V) <= component voltage (50V) - should match
        let lower_voltage_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(25.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(component_resistor.matches(&lower_voltage_key, &component_voltage));

        // Key voltage (100V) > component voltage (50V) - should NOT match
        let higher_voltage_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(100.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(!component_resistor.matches(&higher_voltage_key, &component_voltage));

        // No component voltage specified - should match any key voltage
        let any_voltage_key = ResistorMatchingKey {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(component_resistor.matches(&any_voltage_key, &None));
    }

    #[test]
    fn test_capacitor_matching() {
        // Component: 100nF ±10% X7R
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };

        // Key: 100nF ±10% X7R - should match (exact)
        let matching_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
        };
        assert!(component_capacitor.matches(&matching_key, &None));

        // Key: 100nF ±5% X7R - should match (tighter tolerance)
        let tighter_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.05, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
        };
        assert!(component_capacitor.matches(&tighter_key, &None));

        // Key: 100nF ±20% X7R - should NOT match (looser tolerance)
        let looser_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.2, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
        };
        assert!(!component_capacitor.matches(&looser_key, &None));

        // Key: 100nF ±10% C0G - should NOT match (different dielectric)
        let different_dielectric_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::C0G),
        };
        assert!(!component_capacitor.matches(&different_dielectric_key, &None));

        // Key: No dielectric specified - should match (no requirement)
        let no_dielectric_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: None,
        };
        assert!(component_capacitor.matches(&no_dielectric_key, &None));
    }

    #[test]
    fn test_capacitor_no_dielectric_component() {
        // Component: 100nF ±10% (no dielectric specified)
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: None,
            esr: None,
        };

        // Key: Any dielectric specified - should match (no component requirement)
        let x7r_key = CapacitorMatchingKey {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
        };
        assert!(component_capacitor.matches(&x7r_key, &None));
    }

    #[test]
    fn test_bom_matching_rules() {
        // Create a simple BOM with one resistor
        let mut bom = Bom {
            entries: HashMap::new(),
            designators: HashMap::new(),
        };

        let resistor_entry = BomEntry {
            mpn: None,
            manufacturer: None,
            description: None,
            package: Some("0603".to_string()),
            value: Some("1kOhm".to_string()),
            alternatives: vec![],
            well_known_module: Some(WellKnownComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Ohms),
            })),
            voltage: None,
            matched_part: None,
            dnp: false,
        };

        bom.entries.insert("R1.R".to_string(), resistor_entry);
        bom.designators.insert("R1.R".to_string(), "R1".to_string());

        // Test resistor matching rule
        let resistor_rule = BomMatchingRule {
            key: BomMatchingKey::Resistor(ResistorMatchingKey {
                resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
                voltage: None,
            }),
            value: BomMatchingValue {
                distributor: "digikey".to_string(),
                distributor_pn: "311-1.00KHRCT-ND".to_string(),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
            },
        };

        bom.apply_bom_rule(&resistor_rule);

        // Verify the rule was applied
        let entry = &bom.entries["R1.R"];
        assert!(entry.matched_part.is_some());
        let matched = entry.matched_part.as_ref().unwrap();
        assert_eq!(matched.distributor, "digikey");
        assert_eq!(matched.distributor_pn, "311-1.00KHRCT-ND");
        assert_eq!(matched.manufacturer, Some("Yageo".to_string()));

        // Test designator matching rule
        let designator_rule = BomMatchingRule {
            key: BomMatchingKey::Designator("R1".to_string()),
            value: BomMatchingValue {
                distributor: "mouser".to_string(),
                distributor_pn: "603-RC0603FR-071KL".to_string(),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
            },
        };

        bom.apply_bom_rule(&designator_rule);

        // Verify the designator rule overwrote the previous match
        let entry = &bom.entries["R1.R"];
        let matched = entry.matched_part.as_ref().unwrap();
        assert_eq!(matched.distributor, "mouser");
        assert_eq!(matched.distributor_pn, "603-RC0603FR-071KL");
    }
}
