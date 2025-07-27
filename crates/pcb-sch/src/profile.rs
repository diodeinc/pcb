use crate::BomEntry;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProfileError {
    #[error("Failed to read profile file: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Failed to parse YAML: {0}")]
    YamlError(#[from] serde_yaml::Error),
    #[error("Profile validation error: {0}")]
    ValidationError(String),
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),
    #[error("Template error: {0}")]
    TemplateError(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BomProfile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub outputs: HashMap<String, OutputConfig>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub conditions: RuleConditions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RuleConditions {
    FullRule {
        #[serde(rename = "match")]
        match_conditions: HashMap<String, serde_yaml::Value>,
        set: HashMap<String, serde_yaml::Value>,
    },
    CompactRule(HashMap<String, serde_yaml::Value>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutputConfig {
    pub format: String,
    #[serde(default)]
    pub file: String,  // empty string means stdout
    #[serde(default)]
    pub columns: HashMap<String, String>, // "Header": "field", empty means all fields
}

impl BomProfile {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ProfileError> {
        let content = fs::read_to_string(path)?;
        let profile: BomProfile = serde_yaml::from_str(&content)?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), ProfileError> {
        if self.version != 1 {
            return Err(ProfileError::ValidationError(format!(
                "Unsupported profile version: {}",
                self.version
            )));
        }

        if self.outputs.is_empty() {
            return Err(ProfileError::ValidationError(
                "Profile must have at least one output".to_string(),
            ));
        }

        // Validate output formats
        for (name, output) in &self.outputs {
            match output.format.as_str() {
                "csv" | "json" | "html" | "table" => {}
                _ => {
                    return Err(ProfileError::ValidationError(format!(
                        "Unsupported output format '{}' in output '{}'",
                        output.format, name
                    )));
                }
            }
            // No longer require columns to be specified - empty means all fields
        }

        Ok(())
    }

    pub fn apply_rules(&self, entries: &mut [BomEntry]) -> Result<(), ProfileError> {
        for entry in entries.iter_mut() {
            for rule in &self.rules {
                if self.rule_matches(rule, entry)? {
                    self.apply_rule_actions(rule, entry)?;
                }
            }
        }
        Ok(())
    }

    fn rule_matches(&self, rule: &Rule, entry: &BomEntry) -> Result<bool, ProfileError> {
        let match_conditions = match &rule.conditions {
            RuleConditions::FullRule { match_conditions, .. } => match_conditions,
            RuleConditions::CompactRule(conditions) => {
                // For compact rules, check if it's a match.field type condition
                for (key, _) in conditions {
                    if key.starts_with("match.") {
                        // This is a compact rule with match conditions
                        let mut extracted_conditions = HashMap::new();
                        for (k, v) in conditions {
                            if let Some(field) = k.strip_prefix("match.") {
                                extracted_conditions.insert(field.to_string(), v.clone());
                            }
                        }
                        return self.check_match_conditions(&extracted_conditions, entry);
                    }
                }
                return Ok(false); // No match conditions in compact rule
            }
        };

        self.check_match_conditions(match_conditions, entry)
    }

    fn check_match_conditions(
        &self,
        conditions: &HashMap<String, serde_yaml::Value>,
        entry: &BomEntry,
    ) -> Result<bool, ProfileError> {
        for (field, expected_value) in conditions {
            let entry_value = self.get_field_value(entry, field);
            
            if !self.value_matches(&entry_value, expected_value)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn get_field_value(&self, entry: &BomEntry, field: &str) -> String {
        match field {
            "designator" => entry.designator.clone(),
            "mpn" => entry.mpn.as_deref().unwrap_or("").to_string(),
            "manufacturer" => entry.manufacturer.as_deref().unwrap_or("").to_string(),
            "lcsc" => entry.lcsc.as_deref().unwrap_or("").to_string(),
            "package" => entry.package.as_deref().unwrap_or("").to_string(),
            "value" => entry.value.as_deref().unwrap_or("").to_string(),
            "description" => entry.description.as_deref().unwrap_or("").to_string(),
            "dnp" => entry.dnp.to_string(),
            "quantity" => entry.quantity.to_string(),
            _ => String::new(),
        }
    }

    fn value_matches(
        &self,
        entry_value: &str,
        expected_value: &serde_yaml::Value,
    ) -> Result<bool, ProfileError> {
        match expected_value {
            serde_yaml::Value::String(s) => {
                if s.starts_with('/') && s.ends_with('/') && s.len() > 2 {
                    // Regex pattern
                    let pattern = &s[1..s.len() - 1];
                    let regex = Regex::new(pattern)?;
                    Ok(regex.is_match(entry_value))
                } else {
                    // Exact string match
                    Ok(entry_value == s)
                }
            }
            serde_yaml::Value::Bool(b) => {
                // For boolean fields like dnp
                match entry_value.as_ref() {
                    "true" => Ok(*b),
                    "false" => Ok(!*b),
                    _ => Ok(false),
                }
            }
            serde_yaml::Value::Number(n) => {
                if let Ok(entry_num) = entry_value.parse::<f64>() {
                    Ok((entry_num - n.as_f64().unwrap_or(0.0)).abs() < f64::EPSILON)
                } else {
                    Ok(false)
                }
            }
            _ => Ok(false),
        }
    }

    fn apply_rule_actions(&self, rule: &Rule, entry: &mut BomEntry) -> Result<(), ProfileError> {
        let set_actions = match &rule.conditions {
            RuleConditions::FullRule { set, .. } => set,
            RuleConditions::CompactRule(conditions) => {
                // Extract set.field actions from compact rule
                let mut extracted_set = HashMap::new();
                for (key, value) in conditions {
                    if let Some(field) = key.strip_prefix("set.") {
                        extracted_set.insert(field.to_string(), value.clone());
                    }
                }
                
                // If no explicit set actions, check for simple field assignments
                if extracted_set.is_empty() {
                    for (key, value) in conditions {
                        if !key.starts_with("match.") && !key.starts_with("set.") {
                            // This might be a simple field assignment
                            extracted_set.insert(key.clone(), value.clone());
                        }
                    }
                }
                
                return self.apply_set_actions(&extracted_set, entry);
            }
        };

        self.apply_set_actions(set_actions, entry)
    }

    fn apply_set_actions(
        &self,
        actions: &HashMap<String, serde_yaml::Value>,
        entry: &mut BomEntry,
    ) -> Result<(), ProfileError> {
        for (field, value) in actions {
            let string_value = self.resolve_template_value(value, entry)?;
            self.set_field_value(entry, field, &string_value);
        }
        Ok(())
    }

    fn resolve_template_value(
        &self,
        value: &serde_yaml::Value,
        entry: &BomEntry,
    ) -> Result<String, ProfileError> {
        let string_value = match value {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Number(n) => n.to_string(),
            serde_yaml::Value::Bool(b) => b.to_string(),
            _ => return Err(ProfileError::TemplateError("Unsupported value type".to_string())),
        };

        // Simple template substitution for {{field}} patterns
        let mut result = string_value.clone();
        let template_regex = Regex::new(r"\{\{(\w+)\}\}")?;
        
        for captures in template_regex.captures_iter(&string_value) {
            if let Some(field_name) = captures.get(1) {
                let field_value = self.get_field_value(entry, field_name.as_str());
                result = result.replace(&captures[0], &field_value);
            }
        }

        Ok(result)
    }

    fn set_field_value(&self, entry: &mut BomEntry, field: &str, value: &str) {
        match field {
            "designator" => entry.designator = value.to_string(),
            "mpn" => entry.mpn = if value.is_empty() { None } else { Some(value.to_string()) },
            "manufacturer" => entry.manufacturer = if value.is_empty() { None } else { Some(value.to_string()) },
            "lcsc" => entry.lcsc = if value.is_empty() { None } else { Some(value.to_string()) },
            "package" => entry.package = if value.is_empty() { None } else { Some(value.to_string()) },
            "value" => entry.value = if value.is_empty() { None } else { Some(value.to_string()) },
            "description" => entry.description = if value.is_empty() { None } else { Some(value.to_string()) },
            "dnp" => entry.dnp = value.to_lowercase() == "true",
            // quantity is read-only during rule processing
            _ => {}
        }
    }

    pub fn filter_entries(&self, entries: Vec<BomEntry>, output_name: &str) -> Result<Vec<BomEntry>, ProfileError> {
        let output = self.outputs.get(output_name).ok_or_else(|| {
            ProfileError::ValidationError(format!("Output '{}' not found", output_name))
        })?;

        // Auto-filter DNP entries if dnp column is not included
        let include_dnp = output.columns.values().any(|field| field == "dnp");
        
        let filtered: Vec<BomEntry> = if include_dnp {
            entries
        } else {
            entries.into_iter().filter(|entry| !entry.dnp).collect()
        };

        Ok(filtered)
    }

    pub fn group_entries(&self, entries: Vec<BomEntry>) -> Vec<BomEntry> {
        use std::collections::HashMap;

        type GroupKey = (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, bool);
        let mut grouped: HashMap<GroupKey, BomEntry> = HashMap::new();

        for entry in entries {
            let key = (
                entry.mpn.clone(),
                entry.manufacturer.clone(),
                entry.lcsc.clone(),
                entry.package.clone(),
                entry.value.clone(),
                entry.description.clone(),
                entry.dnp,
            );

            grouped
                .entry(key)
                .and_modify(|existing| {
                    if !existing.designator.is_empty() {
                        existing.designator.push(',');
                    }
                    existing.designator.push_str(&entry.designator);
                    existing.quantity += entry.quantity;
                })
                .or_insert(entry);
        }

        let mut result: Vec<_> = grouped.into_values().collect();
        result.sort_by(|a, b| a.designator.cmp(&b.designator));
        result
    }

    pub fn substitute_file_path(&self, path: &str, design_name: &str) -> String {
        path.replace("{design}", design_name)
    }

    /// Get all available BOM fields with their display names
    pub fn all_columns() -> Vec<(&'static str, &'static str)> {
        vec![
            ("RefDes", "designator"),
            ("MPN", "mpn"),
            ("Manufacturer", "manufacturer"),
            ("LCSC", "lcsc"),
            ("Package", "package"),
            ("Value", "value"),
            ("Description", "description"),
            ("DNP", "dnp"),
            ("Qty", "quantity"),
        ]
    }

    /// Get columns for an output, using all columns if none specified
    pub fn get_output_columns(&self, output_name: &str) -> Result<Vec<(String, String)>, ProfileError> {
        let output = self.outputs.get(output_name).ok_or_else(|| {
            ProfileError::ValidationError(format!("Output '{}' not found", output_name))
        })?;

        if output.columns.is_empty() {
            // Use all available columns
            Ok(Self::all_columns()
                .into_iter()
                .map(|(header, field)| (header.to_string(), field.to_string()))
                .collect())
        } else {
            // Use specified columns
            Ok(output.columns.iter()
                .map(|(header, field)| (header.clone(), field.clone()))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_parsing() {
        let yaml = r#"
version: 1
rules:
  - name: "Test rule"
    match:
      designator: /^R\d+$/
      value: "10kOhm"
    set:
      mpn: "RC0603FR-0710KL"
      description: "10k resistor"

outputs:
  assembly:
    format: csv
    file: "bom/{design}_assembly.csv"
    columns:
      "RefDes": "designator"
      "MPN": "mpn"
      "Value": "value"
"#;

        let profile: BomProfile = serde_yaml::from_str(yaml).expect("Failed to parse profile");
        assert_eq!(profile.version, 1);
        assert_eq!(profile.rules.len(), 1);
        assert_eq!(profile.outputs.len(), 1);

        let output = &profile.outputs["assembly"];
        assert_eq!(output.format, "csv");
        assert_eq!(output.columns.len(), 3);
    }

    #[test]
    fn test_compact_rule_parsing() {
        let yaml = r#"
version: 1
rules:
  - match.mpn: "OLD_PART"
    set.mpn: "NEW_PART"

outputs:
  test:
    format: csv
    file: "test.csv"
    columns:
      "RefDes": "designator"
"#;

        let profile: BomProfile = serde_yaml::from_str(yaml).expect("Failed to parse profile");
        assert_eq!(profile.rules.len(), 1);
    }
}
