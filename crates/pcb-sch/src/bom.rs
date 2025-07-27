use crate::{AttributeValue, InstanceKind, Schematic, Symbol};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;

/// A single row in the Bill of Materials
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct BomEntry {
    pub designator: String,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub lcsc: Option<String>,
    pub package: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub dnp: bool,
    pub quantity: usize,
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

        // Extract attributes with case variations
        let mpn = get_string_attribute_variants(&instance.attributes, &["MPN", "Mpn", "mpn"]);
        let manufacturer = get_string_attribute_variants(&instance.attributes, &["Manufacturer", "manufacturer", "Mfg", "mfg"]);
        let lcsc = get_string_attribute_variants(&instance.attributes, &["LCSC", "Lcsc", "LCSC_Part_Number", "lcsc"]);
        let package = get_string_attribute_variants(&instance.attributes, &["Package", "package"]);
        let description = get_string_attribute_variants(&instance.attributes, &["Description", "description"]);
        
        // Determine if component should be populated
        let do_not_populate = get_string_attribute_variants(&instance.attributes, &["do_not_populate", "Do_not_populate", "DNP", "dnp"])
            .map(|s| s.to_lowercase() == "true" || s == "1")
            .unwrap_or(false);
        
        // Check if it's a test component that should typically not be populated
        let is_test_component = designator.starts_with("TP") || // Test points
            get_string_attribute_variants(&instance.attributes, &["type", "Type"])
                .map(|t| t.to_lowercase().contains("test"))
                .unwrap_or(false);
        
        let dnp = do_not_populate || is_test_component;

        // Extract value with precedence: mpn > Value > value > Val > type
        const VALUE_KEYS: &[&str] = &["Value", "value", "Val", "type"];
        let value = mpn.clone().or_else(|| {
            VALUE_KEYS
                .iter()
                .find_map(|&key| get_string_attribute(&instance.attributes, key))
        });

        bom_entries.push(BomEntry {
            designator,
            mpn,
            manufacturer,
            lcsc,
            package,
            value,
            description,
            dnp,
            quantity: 1,
        });
    }

    // Sort by designator for consistent output
    bom_entries.sort_by(|a, b| a.designator.cmp(&b.designator));
    bom_entries
}

/// Write BOM entries to CSV format
pub fn write_bom_csv<W: Write>(entries: &[BomEntry], mut writer: W) -> std::io::Result<()> {
    // Write CSV header
    writeln!(writer, "Designator,Quantity,MPN,Manufacturer,LCSC,Package,Value,Description,DNP")?;

    // Write each entry
    for entry in entries {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{}",
            escape_csv_field(&entry.designator),
            entry.quantity,
            escape_csv_field(&entry.mpn.as_deref().unwrap_or("")),
            escape_csv_field(&entry.manufacturer.as_deref().unwrap_or("")),
            escape_csv_field(&entry.lcsc.as_deref().unwrap_or("")),
            escape_csv_field(&entry.package.as_deref().unwrap_or("")),
            escape_csv_field(&entry.value.as_deref().unwrap_or("")),
            escape_csv_field(&entry.description.as_deref().unwrap_or("")),
            if entry.dnp { "Yes" } else { "No" }
        )?;
    }

    Ok(())
}

/// Helper function to extract string values from attributes
fn get_string_attribute(attributes: &HashMap<Symbol, AttributeValue>, key: &str) -> Option<String> {
    attributes.get(key).and_then(|attr| {
        match attr {
            AttributeValue::String(s) => Some(s.clone()),
            AttributeValue::Physical(s) => Some(s.clone()),
            _ => None,
        }
    })
}

/// Helper function to extract string values from attributes, trying multiple key variations
fn get_string_attribute_variants(attributes: &HashMap<Symbol, AttributeValue>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|&key| get_string_attribute(attributes, key))
}

/// Group BOM entries that have identical properties
pub fn group_bom_entries(entries: Vec<BomEntry>) -> Vec<BomEntry> {
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
                existing.designator.push(',');
                existing.designator.push_str(&entry.designator);
                existing.quantity += entry.quantity;
            })
            .or_insert(entry);
    }

    let mut result: Vec<_> = grouped.into_values().collect();
    result.sort_by(|a, b| a.designator.cmp(&b.designator));
    result
}

/// Write BOM entries to JSON format
pub fn write_bom_json<W: Write>(entries: &[BomEntry], writer: W) -> std::io::Result<()> {
    serde_json::to_writer_pretty(writer, entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

/// Write BOM entries to HTML format
pub fn write_bom_html<W: Write>(entries: &[BomEntry], mut writer: W) -> std::io::Result<()> {
    writeln!(writer, r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <title>Bill of Materials</title>
    <style>
        body {{ font-family: Arial, sans-serif; margin: 20px; }}
        table {{ border-collapse: collapse; width: 100%; }}
        th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
        th {{ background-color: #f2f2f2; font-weight: bold; }}
        tr:nth-child(even) {{ background-color: #f9f9f9; }}
        .dnp {{ color: #888; font-style: italic; }}
    </style>
</head>
<body>
    <h1>Bill of Materials</h1>
    <table>
        <thead>
            <tr>
                <th>Designator</th>
                <th>Quantity</th>
                <th>MPN</th>
                <th>Manufacturer</th>
                <th>LCSC</th>
                <th>Package</th>
                <th>Value</th>
                <th>Description</th>
                <th>DNP</th>
            </tr>
        </thead>
        <tbody>"#)?;

    for entry in entries {
        let dnp_class = if entry.dnp { " class=\"dnp\"" } else { "" };
        writeln!(
            writer,
            r#"            <tr{}>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
                <td>{}</td>
            </tr>"#,
            dnp_class,
            html_escape(&entry.designator),
            entry.quantity,
            html_escape(entry.mpn.as_deref().unwrap_or("")),
            html_escape(entry.manufacturer.as_deref().unwrap_or("")),
            html_escape(entry.lcsc.as_deref().unwrap_or("")),
            html_escape(entry.package.as_deref().unwrap_or("")),
            html_escape(entry.value.as_deref().unwrap_or("")),
            html_escape(entry.description.as_deref().unwrap_or("")),
            if entry.dnp { "Yes" } else { "No" }
        )?;
    }

    writeln!(writer, r#"        </tbody>
    </table>
</body>
</html>"#)?;

    Ok(())
}

/// Escape HTML special characters
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Escape CSV field by quoting if it contains commas, quotes, or newlines
fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_escape_csv_field() {
        assert_eq!(escape_csv_field("simple"), "simple");
        assert_eq!(escape_csv_field("with,comma"), "\"with,comma\"");
        assert_eq!(escape_csv_field("with\"quote"), "\"with\"\"quote\"");
        assert_eq!(escape_csv_field("with\nnewline"), "\"with\nnewline\"");
    }

    #[test]
    fn test_get_string_attribute() {
        let mut attributes = HashMap::new();
        attributes.insert("test".to_string(), AttributeValue::String("value".to_string()));
        attributes.insert("physical".to_string(), AttributeValue::Physical("10kOhm".to_string()));
        attributes.insert("number".to_string(), AttributeValue::Number(42.0));

        assert_eq!(get_string_attribute(&attributes, "test"), Some("value".to_string()));
        assert_eq!(get_string_attribute(&attributes, "physical"), Some("10kOhm".to_string()));
        assert_eq!(get_string_attribute(&attributes, "number"), None);
        assert_eq!(get_string_attribute(&attributes, "missing"), None);
    }

    #[test]
    fn test_group_bom_entries() {
        let entries = vec![
            BomEntry {
                designator: "R1".to_string(),
                mpn: None,
                manufacturer: None,
                lcsc: None,
                package: Some("0402".to_string()),
                value: Some("10kOhm".to_string()),
                description: None,
                dnp: false,
                quantity: 1,
            },
            BomEntry {
                designator: "R2".to_string(),
                mpn: None,
                manufacturer: None,
                lcsc: None,
                package: Some("0402".to_string()),
                value: Some("10kOhm".to_string()),
                description: None,
                dnp: false,
                quantity: 1,
            },
            BomEntry {
                designator: "C1".to_string(),
                mpn: None,
                manufacturer: None,
                lcsc: None,
                package: Some("0402".to_string()),
                value: Some("100nF".to_string()),
                description: None,
                dnp: false,
                quantity: 1,
            },
        ];

        let grouped = group_bom_entries(entries);
        assert_eq!(grouped.len(), 2);
        
        let resistor_entry = grouped.iter().find(|e| e.value.as_deref() == Some("10kOhm")).unwrap();
        assert_eq!(resistor_entry.designator, "R1,R2");
        assert_eq!(resistor_entry.quantity, 2);
        
        let capacitor_entry = grouped.iter().find(|e| e.value.as_deref() == Some("100nF")).unwrap();
        assert_eq!(capacitor_entry.designator, "C1");
        assert_eq!(capacitor_entry.quantity, 1);
    }

    #[test]
    fn test_get_string_attribute_variants() {
        let mut attributes = HashMap::new();
        attributes.insert("MPN".to_string(), AttributeValue::String("ABC123".to_string()));
        attributes.insert("Description".to_string(), AttributeValue::String("A resistor".to_string()));
        attributes.insert("value".to_string(), AttributeValue::String("10k".to_string()));

        // Should find MPN (first variant)
        assert_eq!(get_string_attribute_variants(&attributes, &["MPN", "Mpn", "mpn"]), Some("ABC123".to_string()));
        
        // Should find Description (first variant)
        assert_eq!(get_string_attribute_variants(&attributes, &["Description", "description"]), Some("A resistor".to_string()));
        
        // Should find value when MPN is not present
        attributes.remove("MPN");
        assert_eq!(get_string_attribute_variants(&attributes, &["MPN", "Mpn", "mpn"]), None);
        
        // Should still find description
        assert_eq!(get_string_attribute_variants(&attributes, &["Description", "description"]), Some("A resistor".to_string()));
    }
}
