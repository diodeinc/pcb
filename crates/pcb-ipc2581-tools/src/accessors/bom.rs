use std::collections::BTreeMap;

use ipc2581::types::{BomCategory, Characteristics};
use pcb_sch::Alternative;
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// BOM statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomStats {
    pub total_unique_parts: usize,
    pub total_instances: usize,
    pub has_avl: bool,
}

impl BomStats {
    pub fn new(total_unique_parts: usize, total_instances: usize, has_avl: bool) -> Self {
        Self {
            total_unique_parts,
            total_instances,
            has_avl,
        }
    }
}

/// Extracted characteristics data from IPC-2581 BOM items
///
/// This is an intermediate representation that decouples raw IPC-2581
/// data from the output format. Commands can use this to build their
/// own domain-specific models.
#[derive(Debug, Default, Clone)]
pub struct CharacteristicsData {
    pub package: Option<String>,
    pub value: Option<String>,
    pub path: Option<String>,
    pub matcher: Option<String>,
    pub alternatives: Vec<Alternative>,
    pub properties: BTreeMap<String, String>,
    pub component_type: Option<String>,
    pub resistance: Option<String>,
    pub capacitance: Option<String>,
    pub voltage: Option<String>,
    pub dielectric: Option<String>,
    pub esr: Option<String>,
}

/// AVL lookup result containing primary part and alternatives
#[derive(Debug, Clone)]
pub struct AvlLookup {
    pub primary_mpn: Option<String>,
    pub primary_manufacturer: Option<String>,
    pub alternatives: Vec<Alternative>,
}

impl<'a> IpcAccessor<'a> {
    /// Get BOM statistics
    ///
    /// Returns None if no BOM section exists
    pub fn bom_stats(&self) -> Option<BomStats> {
        let bom = self.ipc.bom()?;

        let mut total_unique_parts = 0;
        let mut total_instances = 0;

        for item in &bom.items {
            // Skip document category items (test points, etc.)
            if matches!(item.category, Some(BomCategory::Document)) {
                continue;
            }

            total_unique_parts += 1;

            // Count reference designators
            for ref_des in &item.ref_des_list {
                if self.ipc.resolve(ref_des.name).is_empty() {
                    continue;
                }
                total_instances += 1;
            }
        }

        let has_avl = self.ipc.avl().is_some();

        Some(BomStats::new(total_unique_parts, total_instances, has_avl))
    }

    /// Extract characteristics from IPC-2581 Characteristics
    ///
    /// Returns package, value, alternatives, and custom properties.
    /// Note: MPN and Manufacturer should come from AVL (canonical IPC-2581 way)
    pub fn extract_characteristics(&self, chars: &Characteristics) -> CharacteristicsData {
        let mut data = CharacteristicsData::default();

        for textual in &chars.textuals {
            if let (Some(name), Some(val)) = (textual.name, textual.value) {
                let name_str = self.ipc.resolve(name).to_string();
                let name_lower = name_str.to_lowercase();
                let val_str = self.ipc.resolve(val).to_string();

                match name_lower.as_str() {
                    "package" | "footprint" => data.package = Some(val_str),
                    "value" => data.value = Some(val_str),
                    "path" => data.path = Some(val_str),
                    "matcher" => data.matcher = Some(val_str),
                    "alternatives" => {
                        if let Some(alternative) = parse_alternative_json(&val_str) {
                            data.alternatives.push(alternative);
                        }
                    }
                    // Generic component fields
                    "type" => data.component_type = Some(val_str.to_lowercase()),
                    "resistance" => data.resistance = Some(val_str),
                    "capacitance" => data.capacitance = Some(val_str),
                    "voltage" => data.voltage = Some(val_str),
                    "dielectric" => data.dielectric = Some(val_str),
                    "esr" => data.esr = Some(val_str),
                    // Exclude well-known fields (MPN/Manufacturer come from AVL)
                    // and instance-specific metadata
                    "mpn"
                    | "manufacturerpartnumber"
                    | "partnumber"
                    | "manufacturer"
                    | "prefix"
                    | "symbol_name"
                    | "symbol_path" => {}
                    _ => {
                        data.properties.insert(name_str, val_str);
                    }
                }
            }
        }

        data
    }

    /// Look up MPN, manufacturer, and alternatives from AVL section
    ///
    /// Per IPC-2581 spec: rank=1 or chosen=true is primary, rest are alternatives
    pub fn lookup_avl(&self, oem_design_number_ref: ipc2581::Symbol) -> AvlLookup {
        let Some(avl) = self.ipc.avl() else {
            return AvlLookup {
                primary_mpn: None,
                primary_manufacturer: None,
                alternatives: Vec::new(),
            };
        };

        let oem_design_number_str = self.ipc.resolve(oem_design_number_ref);
        let Some(avl_item) = avl
            .items
            .iter()
            .find(|item| self.ipc.resolve(item.oem_design_number) == oem_design_number_str)
        else {
            return AvlLookup {
                primary_mpn: None,
                primary_manufacturer: None,
                alternatives: Vec::new(),
            };
        };

        if avl_item.vmpn_list.is_empty() {
            return AvlLookup {
                primary_mpn: None,
                primary_manufacturer: None,
                alternatives: Vec::new(),
            };
        }

        // Sort by priority: chosen → rank (ascending) → unranked
        let mut sorted_vmpn: Vec<_> = avl_item.vmpn_list.iter().collect();
        sorted_vmpn.sort_by(|a, b| a.cmp_priority(b));

        // First entry is primary
        let primary = sorted_vmpn[0];
        let primary_mpn = primary
            .mpns
            .first()
            .map(|m| self.ipc.resolve(m.name).to_string());
        let primary_manufacturer = primary.vendors.first().and_then(|v| {
            self.ipc
                .resolve_enterprise(v.enterprise_ref)
                .map(|s| s.to_string())
        });

        // Rest are alternatives
        let alternatives = sorted_vmpn[1..]
            .iter()
            .filter_map(|vmpn| {
                let mpn = self.ipc.resolve(vmpn.mpns.first()?.name).to_string();
                let manufacturer = self
                    .ipc
                    .resolve_enterprise(vmpn.vendors.first()?.enterprise_ref)?
                    .to_string();
                Some(Alternative { mpn, manufacturer })
            })
            .collect();

        AvlLookup {
            primary_mpn,
            primary_manufacturer,
            alternatives,
        }
    }
}

/// Parse alternative part data from JSON string
///
/// Handles HTML-encoded JSON like: {&quot;mpn&quot;: &quot;...&quot;, &quot;manufacturer&quot;: &quot;...&quot;}
fn parse_alternative_json(json_str: &str) -> Option<Alternative> {
    // Decode HTML entities using quick-xml
    let decoded = quick_xml::escape::unescape(json_str).ok()?.to_string();

    // Parse as JSON
    let parsed: serde_json::Value = serde_json::from_str(&decoded).ok()?;

    // Extract mpn and manufacturer
    let mpn = parsed.get("mpn")?.as_str()?.to_string();
    let manufacturer = parsed.get("manufacturer")?.as_str()?.to_string();

    Some(Alternative { mpn, manufacturer })
}
