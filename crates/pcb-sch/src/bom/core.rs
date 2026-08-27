use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::natural_string::NaturalString;
use crate::{InstanceKind, PhysicalValue, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bom {
    pub entries: HashMap<String, BomEntry>,   // path -> BomEntry
    pub designators: HashMap<String, String>, // path -> designator
    #[serde(skip)]
    pub availability: HashMap<String, super::availability::Availability>, // path -> availability data
}

/// Trim surrounding whitespace and truncate descriptions to 100 characters.
pub fn trim_description(s: Option<String>) -> Option<String> {
    s.map(|s| {
        let trimmed = s.trim();
        if trimmed.chars().count() > 100 {
            format!("{} ...", trimmed.chars().take(96).collect::<String>())
        } else {
            trimmed.to_string()
        }
    })
    .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedBomEntry {
    pub designators: BTreeSet<NaturalString>,
    #[serde(flatten)]
    pub entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Alternative {
    pub mpn: String,
    pub manufacturer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Part {
    pub mpn: String,
    pub manufacturer: String,
    #[serde(default)]
    pub qualifications: Vec<String>,
}

impl Part {
    pub fn from_attr_value(attr: &crate::AttributeValue) -> Option<Self> {
        match attr {
            crate::AttributeValue::Json(json) => serde_json::from_value(json.clone()).ok(),
            crate::AttributeValue::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        }
    }
}

impl From<Part> for Alternative {
    fn from(part: Part) -> Self {
        Self {
            mpn: part.mpn,
            manufacturer: part.manufacturer,
        }
    }
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dnp: bool,
    /// Whether this component should be excluded from BOM output (e.g., fiducials, test points)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_bom: bool,
    /// Additional properties from IPC-2581 textual characteristics
    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UngroupedBomEntry {
    pub path: String,
    pub designator: String,
    #[serde(flatten)]
    pub entry: BomEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<super::availability::Availability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "component_type")]
pub enum GenericComponent {
    Capacitor(Capacitor),
    Crystal(Crystal),
    FerriteBead(FerriteBead),
    Inductor(Inductor),
    Led(Led),
    PinHeader(PinHeader),
    Rectifier(Rectifier),
    Resistor(Resistor),
    Tvs(Tvs),
    Zener(Zener),
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Crystal {
    pub frequency: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_capacitance: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Led {
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_voltage: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_current: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PinHeader {
    pub pitch: String,
    pub rows: u32,
    /// Number of pin positions per row.
    pub pins: u32,
    pub orientation: String,
    pub gender: String,
    pub mount: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FerriteBead {
    pub impedance: PhysicalValue,
    pub frequency: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dcr: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Inductor {
    pub inductance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dcr: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rectifier {
    pub technology: String,
    pub reverse_voltage: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_current: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_voltage: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tvs {
    pub direction: String,
    pub reverse_standoff_voltage: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverse_clamping_voltage: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_pulse_power: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacitance: Option<PhysicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Zener {
    pub zener_voltage: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power: Option<PhysicalValue>,
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
            availability: HashMap::new(),
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
                    description: trim_description(instance.description()),
                    package: instance.package(),
                    value: instance.value(),
                    alternatives: instance.alternatives_attr(),
                    generic_data: detect_generic_component(instance),
                    dnp: instance.dnp(),
                    skip_bom: instance.skip_bom(),
                    properties: BTreeMap::new(),
                };
                entries.insert(path.clone(), bom_entry);
                designators.insert(path, designator);
            });

        Bom {
            entries,
            designators,
            availability: HashMap::new(),
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
                availability: self.availability.get(path).cloned(),
            })
            .collect::<Vec<_>>();
        // Sort by DNP status first (non-DNP before DNP), then by designator naturally
        entries.sort_by(|a, b| match a.entry.dnp.cmp(&b.entry.dnp) {
            std::cmp::Ordering::Equal => natord::compare(&a.designator, &b.designator),
            other => other,
        });
        serde_json::to_string_pretty(&entries).unwrap()
    }

    #[cfg(feature = "table")]
    pub(crate) fn grouped_entries(&self) -> Vec<GroupedBomEntry> {
        // Group entries by their BomEntry content
        let mut groups = HashMap::<BomEntry, BTreeSet<NaturalString>>::new();

        for (path, entry) in &self.entries {
            let group = groups.entry(entry.clone()).or_default();
            group.insert(self.designators[path].clone().into());
        }

        // Convert to vec
        let mut grouped_entries = groups
            .into_iter()
            .map(|(entry, designators)| GroupedBomEntry { entry, designators })
            .collect::<Vec<_>>();

        grouped_entries.sort_by(|a, b| {
            // Sort by DNP status first (non-DNP before DNP)
            match a.entry.dnp.cmp(&b.entry.dnp) {
                std::cmp::Ordering::Equal => {
                    // Within same DNP status, sort by first designator
                    // BTreeSet<NaturalString> maintains natural order, so first() is correct
                    a.designators
                        .iter()
                        .next()
                        .cmp(&b.designators.iter().next())
                }
                other => other,
            }
        });

        grouped_entries
    }

    /// Filter out components that have skip_bom=true
    /// Returns a new Bom with excluded components removed
    pub fn filter_excluded(&self) -> Self {
        let entries: HashMap<_, _> = self
            .entries
            .iter()
            .filter(|(_, entry)| !entry.skip_bom)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let designators: HashMap<_, _> = entries
            .keys()
            .filter_map(|path| {
                self.designators
                    .get(path)
                    .map(|d| (path.clone(), d.clone()))
            })
            .collect();

        Bom {
            entries,
            designators,
            availability: HashMap::new(),
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
            dnp: dnp == "DNP" || dnp.to_lowercase() == "yes" || dnp == "1",
            skip_bom: false, // KiCad CSV exports don't include this field
            properties: BTreeMap::new(),
        };

        entries.insert(path.clone(), entry);
        designators.insert(path, reference.to_string());
    }

    Ok(Bom {
        entries,
        designators,
        availability: HashMap::new(),
    })
}

fn positive_integer_attr(instance: &crate::Instance, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        let crate::AttributeValue::Number(value) = instance.attributes.get(*key)? else {
            return None;
        };
        (value.is_finite() && *value > 0.0 && value.fract() == 0.0 && *value <= u32::MAX as f64)
            .then_some(*value as u32)
    })
}

fn detect_generic_component(instance: &crate::Instance) -> Option<GenericComponent> {
    match instance.component_type()?.as_str() {
        "resistor" => {
            if let Some(resistance) = instance.physical_attr(&["Resistance", "resistance"]) {
                let voltage = instance.physical_attr(&["Voltage", "voltage"]);
                let power = instance.physical_attr(&["Power", "power"]);
                return Some(GenericComponent::Resistor(Resistor {
                    resistance,
                    voltage,
                    power,
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
        "ferrite_bead" => {
            if let (Some(impedance), Some(frequency)) = (
                instance.physical_attr(&["impedance"]),
                instance.physical_attr(&["frequency"]),
            ) {
                return Some(GenericComponent::FerriteBead(FerriteBead {
                    impedance,
                    frequency,
                    current: instance.physical_attr(&["current"]),
                    dcr: instance.physical_attr(&["dcr"]),
                }));
            }
        }
        "crystal" => {
            if let Some(frequency) = instance.physical_attr(&["frequency", "Frequency"]) {
                return Some(GenericComponent::Crystal(Crystal {
                    frequency,
                    load_capacitance: instance
                        .physical_attr(&["load_capacitance", "Load_capacitance"]),
                    esr: instance.physical_attr(&["esr", "ESR", "Esr"]),
                }));
            }
        }
        "inductor" => {
            if let Some(inductance) = instance.physical_attr(&["inductance"]) {
                return Some(GenericComponent::Inductor(Inductor {
                    inductance,
                    current: instance.physical_attr(&["current"]),
                    dcr: instance.physical_attr(&["dcr"]),
                }));
            }
        }
        "led" => {
            if let Some(color) = instance.string_attr(&["color", "Color"]) {
                return Some(GenericComponent::Led(Led {
                    color,
                    forward_voltage: instance
                        .physical_attr(&["forward_voltage", "Forward_voltage"]),
                    forward_current: instance
                        .physical_attr(&["forward_current", "Forward_current"]),
                }));
            }
        }
        "connector" => {
            let is_pin_header = instance
                .string_attr(&["connector_type", "Connector_type"])
                .is_some_and(|value| value.eq_ignore_ascii_case("Pin Header"));
            if is_pin_header
                && let (
                    Some(pitch),
                    Some(rows),
                    Some(pins),
                    Some(orientation),
                    Some(gender),
                    Some(mount),
                ) = (
                    instance.string_attr(&["pitch", "Pitch"]),
                    positive_integer_attr(instance, &["rows", "Rows"]),
                    positive_integer_attr(instance, &["pins", "Pins"]),
                    instance.string_attr(&["orientation", "Orientation"]),
                    instance.string_attr(&["gender", "Gender"]),
                    instance.string_attr(&["mount", "Mount"]),
                )
            {
                return Some(GenericComponent::PinHeader(PinHeader {
                    pitch,
                    rows,
                    pins,
                    orientation,
                    gender,
                    mount,
                }));
            }
        }
        "rectifier" => {
            if let (Some(technology), Some(reverse_voltage)) = (
                instance.string_attr(&["technology"]),
                instance.physical_attr(&["reverse_voltage"]),
            ) {
                return Some(GenericComponent::Rectifier(Rectifier {
                    technology,
                    reverse_voltage,
                    forward_current: instance.physical_attr(&["forward_current"]),
                    forward_voltage: instance.physical_attr(&["forward_voltage"]),
                }));
            }
        }
        "tvs" => {
            if let (Some(direction), Some(reverse_standoff_voltage)) = (
                instance.string_attr(&["direction"]),
                instance.physical_attr(&["reverse_standoff_voltage"]),
            ) {
                return Some(GenericComponent::Tvs(Tvs {
                    direction,
                    reverse_standoff_voltage,
                    reverse_clamping_voltage: instance.physical_attr(&["reverse_clamping_voltage"]),
                    peak_pulse_power: instance.physical_attr(&["peak_pulse_power"]),
                    capacitance: instance.physical_attr(&["capacitance"]),
                }));
            }
        }
        "zener" => {
            if let Some(zener_voltage) = instance.physical_attr(&["zener_voltage"]) {
                return Some(GenericComponent::Zener(Zener {
                    zener_voltage,
                    power: instance.physical_attr(&["power"]),
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
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromPrimitive;
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
            internal_connectivity: Default::default(),
            symbol_positions: HashMap::new(),
        }
    }

    #[test]
    fn trim_description_preserves_utf8_at_the_truncation_boundary() {
        let description = format!("{}Ω{}", "a".repeat(95), "b".repeat(10));

        assert_eq!(
            trim_description(Some(description)),
            Some(format!("{}Ω ...", "a".repeat(95)))
        );
    }

    #[test]
    fn trim_description_limits_unicode_characters() {
        let exactly_100 = "Ω".repeat(100);
        assert_eq!(
            trim_description(Some(exactly_100.clone())),
            Some(exactly_100)
        );

        let truncated = trim_description(Some("Ω".repeat(101))).unwrap();
        assert_eq!(truncated, format!("{} ...", "Ω".repeat(96)));
        assert_eq!(truncated.chars().count(), 100);
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
                    resistor.resistance.nominal,
                    Decimal::from_f64(10000.0).unwrap()
                );
                assert_eq!(
                    resistor.resistance.tolerance(),
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
                    (capacitor.capacitance.nominal - expected_value).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(
                    capacitor.capacitance.tolerance(),
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
        // Note: New format uses nominal/min/max instead of value/tolerance
        let resistor_json = r#"{
            "component_type": "Resistor",
            "resistance": {"nominal": "10000.0", "min": "9900.0", "max": "10100.0", "unit": "Ohms"}
        }"#;

        let resistor: GenericComponent = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            GenericComponent::Resistor(r) => {
                assert_eq!(r.resistance.nominal, Decimal::from_f64(10000.0).unwrap());
                assert_eq!(r.resistance.min, Decimal::from_f64(9900.0).unwrap());
                assert_eq!(r.resistance.max, Decimal::from_f64(10100.0).unwrap());
            }
            _ => panic!("Expected Resistor variant"),
        }

        // Capacitor should deserialize with component_type tag
        let capacitor_json = r#"{
            "component_type": "Capacitor",
            "capacitance": {"nominal": "1e-7", "min": "8e-8", "max": "1.2e-7", "unit": "Farads"},
            "dielectric": "X7R"
        }"#;

        let capacitor: GenericComponent = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            GenericComponent::Capacitor(c) => {
                let expected_nominal = Decimal::from_f64(1e-7).unwrap();
                assert!(
                    (c.capacitance.nominal - expected_nominal).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(c.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected Capacitor variant"),
        }

        // Test round-trip serialization
        let original_resistor = GenericComponent::Resistor(Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
            power: None,
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: GenericComponent = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }
}
