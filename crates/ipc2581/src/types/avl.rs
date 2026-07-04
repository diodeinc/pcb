use crate::{Interner, Symbol};
use uppsala::XmlWriter;

/// AVL (Approved Vendor List) section
#[derive(Debug, Clone)]
pub struct Avl {
    pub name: Symbol,
    pub header: Option<AvlHeader>,
    pub items: Vec<AvlItem>,
}

impl Avl {
    /// Serialize to an XML fragment (compact; callers reformat the document).
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut writer = XmlWriter::new();
        self.write(&mut writer, interner);
        writer.into_string()
    }

    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        writer.start_element("Avl", &[("name", interner.resolve(self.name))]);
        if let Some(ref header) = self.header {
            header.write(writer, interner);
        }
        for item in &self.items {
            item.write(writer, interner);
        }
        writer.end_element("Avl");
    }
}

/// AVL header metadata
#[derive(Debug, Clone)]
pub struct AvlHeader {
    pub title: Symbol,
    pub source: Symbol,
    pub author: Symbol,
    pub datetime: Symbol,
    pub version: u32,
    pub comment: Option<Symbol>,
    pub mod_ref: Option<Symbol>,
}

impl AvlHeader {
    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        let version = self.version.to_string();
        let mut attrs = vec![
            ("title", interner.resolve(self.title)),
            ("source", interner.resolve(self.source)),
            ("author", interner.resolve(self.author)),
            ("datetime", interner.resolve(self.datetime)),
            ("version", version.as_str()),
        ];
        if let Some(comment) = self.comment {
            attrs.push(("comment", interner.resolve(comment)));
        }
        if let Some(mod_ref) = self.mod_ref {
            attrs.push(("modRef", interner.resolve(mod_ref)));
        }
        writer.empty_element("AvlHeader", &attrs);
    }
}

/// AVL item representing sourcing options for a single part
#[derive(Debug, Clone)]
pub struct AvlItem {
    /// References OEMDesignNumber from BOM
    pub oem_design_number: Symbol,
    /// List of vendor/manufacturer/part number alternatives
    pub vmpn_list: Vec<AvlVmpn>,
    /// Optional specification references
    pub spec_refs: Vec<Symbol>,
}

impl AvlItem {
    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        writer.start_element(
            "AvlItem",
            &[("OEMDesignNumber", interner.resolve(self.oem_design_number))],
        );
        for vmpn in &self.vmpn_list {
            vmpn.write(writer, interner);
        }
        for spec_ref in &self.spec_refs {
            writer.empty_element("SpecRef", &[("id", interner.resolve(*spec_ref))]);
        }
        writer.end_element("AvlItem");
    }
}

/// Vendor/Manufacturer/Part Number combination (one sourcing alternative)
#[derive(Debug, Clone)]
pub struct AvlVmpn {
    /// External vendor part library reference (optional)
    pub evpl_vendor: Option<Symbol>,
    /// External MPN reference (optional)
    pub evpl_mpn: Option<Symbol>,
    /// Part is qualified for use
    pub qualified: Option<bool>,
    /// Part was selected/chosen
    pub chosen: Option<bool>,
    /// List of manufacturer part numbers (typically one)
    pub mpns: Vec<AvlMpn>,
    /// List of vendor/distributor references
    pub vendors: Vec<AvlVendor>,
}

impl AvlVmpn {
    /// Compare by priority: chosen flag first, then rank (Some before None, ascending)
    /// Returns Ordering for use in sort_by
    pub fn cmp_priority(&self, other: &Self) -> std::cmp::Ordering {
        let chosen_a = if self.chosen == Some(true) { 0 } else { 1 };
        let chosen_b = if other.chosen == Some(true) { 0 } else { 1 };

        let rank_a = self.mpns.first().and_then(|m| m.rank);
        let rank_b = other.mpns.first().and_then(|m| m.rank);

        // Compare by chosen flag first
        match chosen_a.cmp(&chosen_b) {
            std::cmp::Ordering::Equal => {
                // Then by rank (Some before None, lower number first)
                match (rank_a, rank_b) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            }
            other => other,
        }
    }

    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        let mut attrs = Vec::new();
        if let Some(evpl_vendor) = self.evpl_vendor {
            attrs.push(("evplVendor", interner.resolve(evpl_vendor).to_string()));
        }
        if let Some(evpl_mpn) = self.evpl_mpn {
            attrs.push(("evplMpn", interner.resolve(evpl_mpn).to_string()));
        }
        if let Some(qualified) = self.qualified {
            attrs.push(("qualified", qualified.to_string()));
        }
        if let Some(chosen) = self.chosen {
            attrs.push(("chosen", chosen.to_string()));
        }
        writer.start_element_with("AvlVmpn", attrs);
        for mpn in &self.mpns {
            mpn.write(writer, interner);
        }
        for vendor in &self.vendors {
            vendor.write(writer, interner);
        }
        writer.end_element("AvlVmpn");
    }
}

/// Manufacturer Part Number with metadata
#[derive(Debug, Clone)]
pub struct AvlMpn {
    /// The actual manufacturer part number string
    pub name: Symbol,
    /// Ranking where 1 is best (optional)
    pub rank: Option<u32>,
    /// Cost per part (optional)
    pub cost: Option<f64>,
    /// Moisture sensitivity level (optional)
    pub moisture_sensitivity: Option<MoistureSensitivity>,
    /// Part is available (optional)
    pub availability: Option<bool>,
    /// Additional information (optional)
    pub other: Option<Symbol>,
}

impl AvlMpn {
    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        let mut attrs = vec![("name", interner.resolve(self.name).to_string())];
        if let Some(rank) = self.rank {
            attrs.push(("rank", rank.to_string()));
        }
        if let Some(cost) = self.cost {
            attrs.push(("cost", cost.to_string()));
        }
        if let Some(ref ms) = self.moisture_sensitivity {
            attrs.push(("moistureSensitivity", ms.as_str().to_string()));
        }
        if let Some(avail) = self.availability {
            attrs.push(("availability", avail.to_string()));
        }
        if let Some(other) = self.other {
            attrs.push(("other", interner.resolve(other).to_string()));
        }
        writer.empty_element_with("AvlMpn", attrs);
    }
}

/// J-STD-020 Moisture Sensitivity Levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoistureSensitivity {
    Unlimited,
    OneYear,
    FourWeeks,
    Hours168,
    Hours72,
    Hours48,
    Hours24,
    Bake,
}

impl MoistureSensitivity {
    /// Parse from IPC-2581 string value
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "UNLIMITED" => Some(Self::Unlimited),
            "1_YEAR" => Some(Self::OneYear),
            "4_WEEKS" => Some(Self::FourWeeks),
            "168_HOURS" => Some(Self::Hours168),
            "72_HOURS" => Some(Self::Hours72),
            "48_HOURS" => Some(Self::Hours48),
            "24_HOURS" => Some(Self::Hours24),
            "BAKE" => Some(Self::Bake),
            _ => None,
        }
    }

    /// Convert to IPC-2581 string value
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unlimited => "UNLIMITED",
            Self::OneYear => "1_YEAR",
            Self::FourWeeks => "4_WEEKS",
            Self::Hours168 => "168_HOURS",
            Self::Hours72 => "72_HOURS",
            Self::Hours48 => "48_HOURS",
            Self::Hours24 => "24_HOURS",
            Self::Bake => "BAKE",
        }
    }
}

/// Vendor/Distributor reference
#[derive(Debug, Clone)]
pub struct AvlVendor {
    /// References Enterprise ID in LogisticHeader
    pub enterprise_ref: Symbol,
}

impl AvlVendor {
    pub fn write(&self, writer: &mut XmlWriter, interner: &Interner) {
        writer.empty_element(
            "AvlVendor",
            &[("enterpriseRef", interner.resolve(self.enterprise_ref))],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Interner;

    #[test]
    fn test_moisture_sensitivity_parse() {
        assert_eq!(
            MoistureSensitivity::parse("UNLIMITED"),
            Some(MoistureSensitivity::Unlimited)
        );
        assert_eq!(
            MoistureSensitivity::parse("1_YEAR"),
            Some(MoistureSensitivity::OneYear)
        );
        assert_eq!(
            MoistureSensitivity::parse("168_HOURS"),
            Some(MoistureSensitivity::Hours168)
        );
        assert_eq!(MoistureSensitivity::parse("INVALID"), None);
    }

    #[test]
    fn test_moisture_sensitivity_as_str() {
        assert_eq!(MoistureSensitivity::Unlimited.as_str(), "UNLIMITED");
        assert_eq!(MoistureSensitivity::OneYear.as_str(), "1_YEAR");
        assert_eq!(MoistureSensitivity::Hours168.as_str(), "168_HOURS");
        assert_eq!(MoistureSensitivity::Bake.as_str(), "BAKE");
    }

    #[test]
    fn test_avl_mpn_xml_with_dangerous_characters() {
        let mut interner = Interner::new();
        let dangerous_name = interner.intern("R&D <test>");

        let mpn = AvlMpn {
            name: dangerous_name,
            rank: Some(1),
            cost: None,
            moisture_sensitivity: None,
            availability: None,
            other: None,
        };

        let mut writer = XmlWriter::new();
        mpn.write(&mut writer, &interner);
        let xml = writer.into_string();
        assert!(xml.contains("R&amp;D &lt;test&gt;"));
        assert!(!xml.contains("<test>"));
    }
}
