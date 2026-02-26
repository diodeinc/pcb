use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

pub const PRIMARY_PROPERTY_NAMES: [&str; 7] = [
    "Reference",
    "Value",
    "Footprint",
    "Datasheet",
    "Description",
    "ki_keywords",
    "ki_fp_filters",
];
const LEGACY_DESCRIPTION_PROPERTY_NAME: &str = "ki_description";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCustomPropertyKeys {
    keys: Vec<String>,
}

impl fmt::Display for InvalidCustomPropertyKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "custom_properties contains reserved KiCad keys: {}",
            self.keys.join(", ")
        )
    }
}

impl Error for InvalidCustomPropertyKeys {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SymbolMetadata {
    #[serde(default)]
    pub primary: SymbolPrimaryProperties,
    #[serde(default)]
    custom_properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint_filters: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSymbolMetadata {
    #[serde(default)]
    primary: SymbolPrimaryProperties,
    #[serde(default)]
    custom_properties: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for SymbolMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSymbolMetadata::deserialize(deserializer)?;
        SymbolMetadata::new(raw.primary, raw.custom_properties).map_err(serde::de::Error::custom)
    }
}

impl SymbolMetadata {
    pub fn new(
        primary: SymbolPrimaryProperties,
        custom_properties: BTreeMap<String, String>,
    ) -> Result<Self, InvalidCustomPropertyKeys> {
        validate_custom_property_keys(&custom_properties)?;
        Ok(Self::new_unchecked(primary, custom_properties))
    }

    pub fn custom_properties(&self) -> &BTreeMap<String, String> {
        &self.custom_properties
    }

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

        let description = property_value(&all_properties, "Description")
            .or_else(|| property_value(&all_properties, LEGACY_DESCRIPTION_PROPERTY_NAME));

        let primary = SymbolPrimaryProperties {
            reference: property_value(&all_properties, "Reference"),
            value: property_value(&all_properties, "Value"),
            footprint: property_value(&all_properties, "Footprint"),
            datasheet: property_value(&all_properties, "Datasheet"),
            description,
            keywords: property_value(&all_properties, "ki_keywords")
                .map(|value| split_space_separated(&value)),
            footprint_filters: property_value(&all_properties, "ki_fp_filters")
                .map(|value| split_space_separated(&value)),
        };

        let custom_properties: BTreeMap<String, String> = all_properties
            .into_iter()
            .filter(|(name, _)| !is_reserved_property(name))
            .collect();
        debug_assert!(validate_custom_property_keys(&custom_properties).is_ok());
        Self::new_unchecked(primary, custom_properties)
    }

    pub fn to_properties_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();

        insert_optional(&mut out, "Reference", &self.primary.reference);
        insert_optional(&mut out, "Value", &self.primary.value);
        insert_optional(&mut out, "Footprint", &self.primary.footprint);
        insert_optional(&mut out, "Datasheet", &self.primary.datasheet);
        insert_optional(&mut out, "Description", &self.primary.description);
        insert_joined_optional(&mut out, "ki_keywords", &self.primary.keywords);
        insert_joined_optional(&mut out, "ki_fp_filters", &self.primary.footprint_filters);

        for (key, value) in &self.custom_properties {
            debug_assert!(!is_reserved_property(key));
            out.insert(key.clone(), value.clone());
        }

        out
    }

    fn new_unchecked(
        primary: SymbolPrimaryProperties,
        custom_properties: BTreeMap<String, String>,
    ) -> Self {
        Self {
            primary,
            custom_properties,
        }
    }
}

fn validate_custom_property_keys(
    custom_properties: &BTreeMap<String, String>,
) -> Result<(), InvalidCustomPropertyKeys> {
    let invalid_keys: Vec<String> = custom_properties
        .keys()
        .filter(|key| is_reserved_property(key))
        .cloned()
        .collect();

    if invalid_keys.is_empty() {
        Ok(())
    } else {
        Err(InvalidCustomPropertyKeys { keys: invalid_keys })
    }
}

fn is_reserved_property(name: &str) -> bool {
    is_primary_property(name) || name == LEGACY_DESCRIPTION_PROPERTY_NAME
}

pub fn is_primary_property(name: &str) -> bool {
    PRIMARY_PROPERTY_NAMES.contains(&name)
}

pub fn primary_field_name(property_key: &str) -> Option<&'static str> {
    match property_key {
        "Reference" => Some("reference"),
        "Value" => Some("value"),
        "Footprint" => Some("footprint"),
        "Datasheet" => Some("datasheet"),
        "Description" => Some("description"),
        "ki_keywords" => Some("keywords"),
        "ki_fp_filters" => Some("footprint_filters"),
        _ => None,
    }
}

fn property_value(properties: &BTreeMap<String, String>, name: &str) -> Option<String> {
    properties.get(name).cloned()
}

fn insert_optional(map: &mut BTreeMap<String, String>, key: &'static str, value: &Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value.clone());
    }
}

fn insert_joined_optional(
    map: &mut BTreeMap<String, String>,
    key: &'static str,
    value: &Option<Vec<String>>,
) {
    if let Some(value) = value {
        map.insert(key.to_string(), value.join(" "));
    }
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
    use serde_json::json;

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
        assert_eq!(
            metadata.primary.keywords,
            Some(vec!["opamp".to_string(), "low-noise".to_string()])
        );
        assert_eq!(
            metadata.primary.footprint_filters,
            Some(vec!["SOIC*".to_string(), "TSSOP*".to_string()])
        );
        assert_eq!(
            metadata.custom_properties().get("Manufacturer_Name"),
            Some(&"Texas Instruments".to_string())
        );
        assert_eq!(
            metadata.custom_properties().get("Voltage"),
            Some(&"2.7V-36V".to_string())
        );
    }

    #[test]
    fn canonicalizes_legacy_description_alias() {
        let metadata =
            SymbolMetadata::from_property_iter(vec![("ki_description", "Alias description")]);
        assert_eq!(
            metadata.primary.description.as_deref(),
            Some("Alias description")
        );
        assert!(!metadata.custom_properties().contains_key("ki_description"));
    }

    #[test]
    fn drops_legacy_description_alias_when_canonical_is_present() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Description", "Canonical description"),
            ("ki_description", "Legacy description"),
        ]);
        assert_eq!(
            metadata.primary.description.as_deref(),
            Some("Canonical description")
        );
        assert!(!metadata.custom_properties().contains_key("ki_description"));
    }

    #[test]
    fn rejects_reserved_custom_property_keys_on_deserialize() {
        let err = serde_json::from_value::<SymbolMetadata>(json!({
            "primary": {},
            "custom_properties": {
                "Reference": "U",
                "ki_description": "Legacy"
            }
        }))
        .expect_err("reserved keys must be rejected");

        let msg = err.to_string();
        assert!(msg.contains("Reference"));
        assert!(msg.contains("ki_description"));
    }

    #[test]
    fn serializes_back_to_kicad_property_map() {
        let metadata = SymbolMetadata::new(
            SymbolPrimaryProperties {
                reference: Some("R".to_string()),
                value: Some("10k".to_string()),
                footprint: Some("Resistor_SMD:R_0603_1608Metric".to_string()),
                datasheet: None,
                description: Some("Resistor".to_string()),
                keywords: Some(vec!["resistor".to_string(), "0603".to_string()]),
                footprint_filters: Some(vec!["R_*".to_string()]),
            },
            BTreeMap::from([("Tolerance".to_string(), "1%".to_string())]),
        )
        .expect("custom keys are valid");

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

    #[test]
    fn serializes_empty_custom_properties_field() {
        let metadata = SymbolMetadata::default();
        let value = serde_json::to_value(&metadata).expect("metadata should serialize");
        let obj = value.as_object().expect("metadata should be object");
        assert!(obj.contains_key("custom_properties"));
    }

    #[test]
    fn preserves_empty_primary_placeholders() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Reference", "U"),
            ("Value", "X"),
            ("Footprint", ""),
            ("Datasheet", ""),
            ("ki_keywords", ""),
            ("ki_fp_filters", ""),
        ]);
        let map = metadata.to_properties_map();
        assert_eq!(map.get("Footprint"), Some(&"".to_string()));
        assert_eq!(map.get("Datasheet"), Some(&"".to_string()));
        assert_eq!(map.get("ki_keywords"), Some(&"".to_string()));
        assert_eq!(map.get("ki_fp_filters"), Some(&"".to_string()));
    }
}
