use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PRIMARY_PROPERTY_NAMES: [&str; 7] = [
    "Reference",
    "Value",
    "Footprint",
    "Datasheet",
    "Description",
    "ki_keywords",
    "ki_fp_filters",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolMetadata {
    #[serde(default)]
    pub primary: SymbolPrimaryProperties,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolPrimaryProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint_filters: Vec<String>,
}

impl SymbolMetadata {
    pub fn from_property_iter<I, K, V>(properties: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let all_properties: BTreeMap<String, String> = properties
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();

        let primary = SymbolPrimaryProperties {
            reference: property_value(&all_properties, "Reference"),
            value: property_value(&all_properties, "Value"),
            footprint: property_value(&all_properties, "Footprint"),
            datasheet: property_value(&all_properties, "Datasheet"),
            description: property_value(&all_properties, "Description"),
            keywords: split_space_separated(
                property_value(&all_properties, "ki_keywords")
                    .as_deref()
                    .unwrap_or_default(),
            ),
            footprint_filters: split_space_separated(
                property_value(&all_properties, "ki_fp_filters")
                    .as_deref()
                    .unwrap_or_default(),
            ),
        };

        let custom_properties = all_properties
            .into_iter()
            .filter(|(name, _)| !is_primary_property(name))
            .collect();

        Self {
            primary,
            custom_properties,
        }
    }

    pub fn to_properties_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();

        if let Some(reference) = &self.primary.reference {
            out.insert("Reference".to_string(), reference.clone());
        }
        if let Some(value) = &self.primary.value {
            out.insert("Value".to_string(), value.clone());
        }
        if let Some(footprint) = &self.primary.footprint {
            out.insert("Footprint".to_string(), footprint.clone());
        }
        if let Some(datasheet) = &self.primary.datasheet {
            out.insert("Datasheet".to_string(), datasheet.clone());
        }
        if let Some(description) = &self.primary.description {
            out.insert("Description".to_string(), description.clone());
        }
        if !self.primary.keywords.is_empty() {
            out.insert("ki_keywords".to_string(), self.primary.keywords.join(" "));
        }
        if !self.primary.footprint_filters.is_empty() {
            out.insert(
                "ki_fp_filters".to_string(),
                self.primary.footprint_filters.join(" "),
            );
        }

        for (key, value) in &self.custom_properties {
            if !is_primary_property(key) {
                out.insert(key.clone(), value.clone());
            }
        }

        out
    }
}

pub fn is_primary_property(name: &str) -> bool {
    PRIMARY_PROPERTY_NAMES.contains(&name)
}

fn property_value(properties: &BTreeMap<String, String>, name: &str) -> Option<String> {
    properties
        .get(name)
        .cloned()
        .filter(|value| !value.is_empty())
}

fn split_space_separated(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primary_and_custom_properties() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Reference", "U"),
            ("Value", "OPA2171"),
            ("Footprint", "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm"),
            ("Datasheet", "https://example.com/ds.pdf"),
            ("Description", "Low-noise op-amp"),
            ("ki_keywords", "opamp low-noise"),
            ("ki_fp_filters", "SOIC* TSSOP*"),
            ("Manufacturer_Name", "Texas Instruments"),
            ("Voltage", "2.7V-36V"),
        ]);

        assert_eq!(metadata.primary.reference.as_deref(), Some("U"));
        assert_eq!(metadata.primary.value.as_deref(), Some("OPA2171"));
        assert_eq!(
            metadata.primary.footprint.as_deref(),
            Some("Package_SO:SOIC-8_3.9x4.9mm_P1.27mm")
        );
        assert_eq!(
            metadata.primary.datasheet.as_deref(),
            Some("https://example.com/ds.pdf")
        );
        assert_eq!(
            metadata.primary.description.as_deref(),
            Some("Low-noise op-amp")
        );
        assert_eq!(metadata.primary.keywords, vec!["opamp", "low-noise"]);
        assert_eq!(metadata.primary.footprint_filters, vec!["SOIC*", "TSSOP*"]);
        assert_eq!(
            metadata.custom_properties.get("Manufacturer_Name"),
            Some(&"Texas Instruments".to_string())
        );
        assert_eq!(
            metadata.custom_properties.get("Voltage"),
            Some(&"2.7V-36V".to_string())
        );
    }

    #[test]
    fn non_primary_alias_stays_in_custom_properties() {
        let metadata =
            SymbolMetadata::from_property_iter(vec![("ki_description", "Alias description")]);
        assert_eq!(metadata.primary.description, None);
        assert_eq!(
            metadata.custom_properties.get("ki_description"),
            Some(&"Alias description".to_string())
        );
    }

    #[test]
    fn serializes_back_to_kicad_property_map() {
        let metadata = SymbolMetadata {
            primary: SymbolPrimaryProperties {
                reference: Some("R".to_string()),
                value: Some("10k".to_string()),
                footprint: Some("Resistor_SMD:R_0603_1608Metric".to_string()),
                datasheet: None,
                description: Some("Resistor".to_string()),
                keywords: vec!["resistor".to_string(), "0603".to_string()],
                footprint_filters: vec!["R_*".to_string()],
            },
            custom_properties: BTreeMap::from([("Tolerance".to_string(), "1%".to_string())]),
        };

        let map = metadata.to_properties_map();

        assert_eq!(map.get("Reference"), Some(&"R".to_string()));
        assert_eq!(map.get("Value"), Some(&"10k".to_string()));
        assert_eq!(
            map.get("Footprint"),
            Some(&"Resistor_SMD:R_0603_1608Metric".to_string())
        );
        assert_eq!(map.get("Description"), Some(&"Resistor".to_string()));
        assert_eq!(map.get("ki_keywords"), Some(&"resistor 0603".to_string()));
        assert_eq!(map.get("ki_fp_filters"), Some(&"R_*".to_string()));
        assert_eq!(map.get("Tolerance"), Some(&"1%".to_string()));
    }
}
