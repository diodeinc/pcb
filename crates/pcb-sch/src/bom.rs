use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{InstanceKind, PhysicalValue, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bom {
    pub entries: HashMap<String, BomEntry>,   // path -> BomEntry
    pub designators: HashMap<String, String>, // path -> designator
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedBomEntry {
    pub designators: BTreeSet<String>,
    #[serde(flatten)]
    pub entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Alternative {
    pub mpn: String,
    pub manufacturer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BomEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generic_data: Option<GenericComponent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<MatchedOffer>,
    pub dnp: bool,
}

impl BomEntry {
    pub fn matches_mpn(&self, mpn: &str) -> bool {
        // Check main MPN
        if let Some(entry_mpn) = &self.mpn {
            if entry_mpn == mpn {
                return true;
            }
        }

        // Check alternatives
        self.alternatives.iter().any(|alt| alt.mpn == mpn)
    }

    pub fn matches_generic(&self, key: &GenericMatchingKey) -> bool {
        // Check package compatibility
        if let Some(entry_package) = &self.package {
            if &key.package != entry_package {
                return false;
            }
        } else {
            // Entry has no package specified, cannot match a specific package requirement
            return false;
        }

        // Check component-specific matching
        if let Some(generic_data) = &self.generic_data {
            generic_data.matches(&key.component)
        } else {
            false
        }
    }

    pub fn add_offers(&mut self, key: BomMatchingKey, offers: Vec<Offer>) {
        self.offers
            .extend(offers.into_iter().map(|offer| MatchedOffer {
                offer,
                matched_by: key.clone(),
            }));
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UngroupedBomEntry {
    pub path: String,
    pub designator: String,
    #[serde(flatten)]
    pub entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "component_type")]
pub enum GenericComponent {
    Capacitor(Capacitor),
    Resistor(Resistor),
}

impl GenericComponent {
    pub fn matches(&self, key: &GenericComponent) -> bool {
        match (self, key) {
            (GenericComponent::Resistor(resistor), GenericComponent::Resistor(key_resistor)) => {
                resistor.matches(key_resistor)
            }
            (
                GenericComponent::Capacitor(capacitor),
                GenericComponent::Capacitor(key_capacitor),
            ) => capacitor.matches(key_capacitor),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capacitor {
    pub capacitance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dielectric: Option<Dielectric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<PhysicalValue>,
}

impl Capacitor {
    pub fn matches(&self, key: &Capacitor) -> bool {
        // Check capacitance range (key range must fit within component tolerance)
        if !key.capacitance.fits_within_default(&self.capacitance) {
            return false;
        }

        // Check voltage: key voltage must be > component voltage
        if let (Some(key_voltage), Some(component_voltage)) = (&key.voltage, &self.voltage) {
            if key_voltage.value > component_voltage.value {
                return false;
            }
        }

        // Check dielectric: key dielectric must match component dielectric
        if let (Some(key_dielec), Some(component_dielec)) = (&key.dielectric, &self.dielectric) {
            if key_dielec != component_dielec {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<PhysicalValue>,
}

impl Resistor {
    pub fn matches(&self, key: &Resistor) -> bool {
        // Check resistance range (key range must fit within component tolerance)
        if !key.resistance.fits_within_default(&self.resistance) {
            return false;
        }

        // Check voltage: key voltage must be > component voltage
        if let (Some(key_voltage), Some(component_voltage)) = (&key.voltage, &self.voltage) {
            if key_voltage.value > component_voltage.value {
                return false;
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BomMatchingKey {
    Mpn(String),
    Generic(GenericMatchingKey),
    Path(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericMatchingKey {
    #[serde(flatten)]
    pub component: GenericComponent,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Offer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor_pn: Option<String>,
    pub manufacturer: Option<String>,
    pub manufacturer_pn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MatchedOffer {
    #[serde(flatten)]
    pub offer: Offer,
    pub matched_by: BomMatchingKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomMatchingRule {
    pub key: BomMatchingKey,
    pub offers: Vec<Offer>,
}

impl Bom {
    /// Get the number of entries in the BOM
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the BOM is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Create a BOM from raw entries and designators
    pub fn new(entries: HashMap<String, BomEntry>, designators: HashMap<String, String>) -> Self {
        Bom {
            entries,
            designators,
        }
    }

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
                    alternatives: instance.alternatives_attr(),
                    generic_data: detect_generic_component(instance),
                    offers: Vec::new(),
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
            BomMatchingKey::Path(target_paths) => {
                for target_path in target_paths {
                    if let Some(entry) = self.entries.get_mut(target_path) {
                        entry.add_offers(rule.key.clone(), rule.offers.clone());
                    }
                }
            }
            BomMatchingKey::Mpn(mpn) => {
                for entry in self.entries.values_mut() {
                    if entry.matches_mpn(mpn) {
                        entry.add_offers(rule.key.clone(), rule.offers.clone());
                    }
                }
            }
            BomMatchingKey::Generic(generic_key) => {
                for entry in self.entries.values_mut() {
                    if entry.matches_generic(generic_key) {
                        entry.add_offers(rule.key.clone(), rule.offers.clone());
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

/// Errors that can occur during KiCad BOM generation
#[derive(Debug, thiserror::Error)]
pub enum KiCadBomError {
    #[error("Failed to execute kicad-cli: {0}")]
    KiCadCliError(String),

    #[error("Failed to parse CSV: {0}")]
    CsvError(#[from] csv::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Parse KiCad CSV BOM into our internal BOM structure
pub fn parse_kicad_csv_bom(csv_content: &str) -> Result<Bom, KiCadBomError> {
    let mut reader = csv::Reader::from_reader(csv_content.as_bytes());
    let mut entries = HashMap::new();
    let mut designators = HashMap::new();

    for result in reader.records() {
        let record = result?;

        if record.is_empty() {
            continue;
        }

        // Get fields by position (matching our kicad-cli labels order)
        let reference = record.get(0).unwrap_or("").trim();
        let value = record.get(1).unwrap_or("").trim();
        let footprint = record.get(2).unwrap_or("").trim();
        let manufacturer = record.get(3).unwrap_or("").trim();
        let mpn = record.get(4).unwrap_or("").trim();
        let description = record.get(5).unwrap_or("").trim();
        let dnp = record.get(6).unwrap_or("").trim();

        // Skip power symbols and net labels
        if reference.is_empty() || reference.starts_with('#') {
            continue;
        }

        let path = format!("kicad::{}", reference);

        // Helper to convert empty string to None
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());

        let entry = BomEntry {
            mpn: non_empty(mpn).or_else(|| {
                // Use Value as MPN if it looks like a part number (no spaces)
                non_empty(value).filter(|v| !v.contains(' '))
            }),
            alternatives: Vec::new(),
            manufacturer: non_empty(manufacturer),
            package: non_empty(footprint).map(|fp| {
                // Remove library prefix (e.g., "Lib:Package" -> "Package")
                fp.split(':').next_back().unwrap_or(&fp).to_string()
            }),
            value: non_empty(value),
            description: non_empty(description),
            generic_data: None,
            offers: Vec::new(),
            dnp: dnp == "DNP" || dnp.to_lowercase() == "yes" || dnp == "1",
        };

        entries.insert(path.clone(), entry);
        designators.insert(path, reference.to_string());
    }

    Ok(Bom {
        entries,
        designators,
    })
}

/// Detect generic components based on Type attribute
fn detect_generic_component(instance: &crate::Instance) -> Option<GenericComponent> {
    match instance.component_type()?.as_str() {
        "resistor" => {
            if let Some(resistance) = instance.physical_attr(&["Resistance", "resistance"]) {
                let voltage = instance.physical_attr(&["Voltage", "voltage"]);
                return Some(GenericComponent::Resistor(Resistor {
                    resistance,
                    voltage,
                }));
            }
        }
        "capacitor" => {
            if let Some(capacitance) = instance.physical_attr(&["Capacitance", "capacitance"]) {
                let dielectric = instance
                    .string_attr(&["Dielectric", "dielectric"])
                    .and_then(|d| d.parse().ok());

                let esr = instance.physical_attr(&["ESR", "esr", "Esr"]);
                let voltage = instance.physical_attr(&["Voltage", "voltage"]);

                return Some(GenericComponent::Capacitor(Capacitor {
                    capacitance,
                    dielectric,
                    esr,
                    voltage,
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
            symbol_positions: HashMap::new(),
        }
    }

    #[test]
    fn test_detect_generic_component() {
        // Create a mock resistor with Type attribute
        let mut attributes = HashMap::new();
        attributes.insert(
            "Type".to_string(),
            AttributeValue::String("resistor".to_string()),
        );
        attributes.insert(
            "resistance".to_string(),
            AttributeValue::String("10k 1%".to_string()),
        );

        let instance = test_instance(attributes);
        let result = detect_generic_component(&instance);

        match result {
            Some(GenericComponent::Resistor(resistor)) => {
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
            "capacitance".to_string(),
            AttributeValue::String("100nF 20%".to_string()),
        );
        capacitor_attributes.insert(
            "Dielectric".to_string(),
            AttributeValue::String("X7R".to_string()),
        );

        let instance = test_instance(capacitor_attributes);
        let result = detect_generic_component(&instance);

        match result {
            Some(GenericComponent::Capacitor(capacitor)) => {
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

        let resistor: GenericComponent = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            GenericComponent::Resistor(r) => {
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

        let capacitor: GenericComponent = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            GenericComponent::Capacitor(c) => {
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
        let original_resistor = GenericComponent::Resistor(Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: GenericComponent = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }

    #[test]
    fn test_resistor_matching() {
        // Component: 1kΩ ±0% (defaults to ±1%)
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Ohms),
            voltage: None,
        };

        // Key: 1kΩ ±1% - should match (exact fit)
        let matching_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&matching_key));

        // Key: 1kΩ ±0.5% - should match (tighter tolerance fits)
        let tighter_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.005, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&tighter_key));

        // Key: 1kΩ ±5% - should NOT match (looser tolerance doesn't fit)
        let looser_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&looser_key));

        // Key: 2kΩ ±1% - should NOT match (different value)
        let different_value_key = Resistor {
            resistance: PhysicalValue::new(2000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&different_value_key));
    }

    #[test]
    fn test_resistor_voltage_matching() {
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(50.0, 0.0, PhysicalUnit::Volts)),
        };

        // Key voltage (25V) <= component voltage (50V) - should match
        let lower_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(25.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(component_resistor.matches(&lower_voltage_key));

        // Key voltage (100V) > component voltage (50V) - should NOT match
        let higher_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(100.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(!component_resistor.matches(&higher_voltage_key));

        // No component voltage specified - should match any key voltage
        let no_voltage_component = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        let any_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(no_voltage_component.matches(&any_voltage_key));
    }

    #[test]
    fn test_capacitor_matching() {
        // Component: 100nF ±10% X7R
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: Some(Dielectric::X7R),
            esr: None,
            voltage: None,
        };

        // Key: 100nF ±10% X7R - should match (exact)
        let matching_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&matching_key));

        // Key: 100nF ±5% X7R - should match (tighter tolerance)
        let tighter_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.05, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&tighter_key));

        // Key: 100nF ±20% X7R - should NOT match (looser tolerance)
        let looser_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.2, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(!component_capacitor.matches(&looser_key));

        // Key: 100nF ±10% C0G - should NOT match (different dielectric)
        let different_dielectric_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::C0G),
            esr: None,
        };
        assert!(!component_capacitor.matches(&different_dielectric_key));

        // Key: No dielectric specified - should match (no requirement)
        let no_dielectric_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: None,
            esr: None,
        };
        assert!(component_capacitor.matches(&no_dielectric_key));
    }

    #[test]
    fn test_capacitor_no_dielectric_component() {
        // Component: 100nF ±10% (no dielectric specified)
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: None,
            esr: None,
            voltage: None,
        };

        // Key: Any dielectric specified - should match (no component requirement)
        let x7r_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&x7r_key));
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
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Ohms),
                voltage: None,
            })),
            offers: Vec::new(),
            dnp: false,
        };

        bom.entries.insert("R1.R".to_string(), resistor_entry);
        bom.designators.insert("R1.R".to_string(), "R1".to_string());

        // Test resistor matching rule
        let resistor_rule = BomMatchingRule {
            key: BomMatchingKey::Generic(GenericMatchingKey {
                component: GenericComponent::Resistor(Resistor {
                    resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
                    voltage: None,
                }),
                package: "0603".to_string(),
            }),
            offers: vec![Offer {
                distributor: Some("digikey".to_string()),
                distributor_pn: Some("311-1.00KHRCT-ND".to_string()),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
                rank: None,
            }],
        };

        bom.apply_bom_rule(&resistor_rule);

        // Verify the rule was applied
        let entry = &bom.entries["R1.R"];
        assert_eq!(entry.offers.len(), 1);
        let expected_matched_offer = MatchedOffer {
            offer: Offer {
                distributor: Some("digikey".to_string()),
                distributor_pn: Some("311-1.00KHRCT-ND".to_string()),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
                rank: None,
            },
            matched_by: resistor_rule.key.clone(),
        };
        assert!(entry.offers.contains(&expected_matched_offer));

        // Test path matching rule
        let path_rule = BomMatchingRule {
            key: BomMatchingKey::Path(vec!["R1.R".to_string()]),
            offers: vec![Offer {
                distributor: Some("mouser".to_string()),
                distributor_pn: Some("603-RC0603FR-071KL".to_string()),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
                rank: None,
            }],
        };

        bom.apply_bom_rule(&path_rule);

        // Verify the path rule added another offer (now we have 2 offers)
        let entry = &bom.entries["R1.R"];
        assert_eq!(entry.offers.len(), 2);
        let expected_mouser_matched_offer = MatchedOffer {
            offer: Offer {
                distributor: Some("mouser".to_string()),
                distributor_pn: Some("603-RC0603FR-071KL".to_string()),
                manufacturer: Some("Yageo".to_string()),
                manufacturer_pn: Some("RC0603FR-071KL".to_string()),
                rank: None,
            },
            matched_by: path_rule.key.clone(),
        };
        assert!(entry.offers.contains(&expected_mouser_matched_offer));
    }
}
