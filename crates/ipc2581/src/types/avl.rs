use crate::{Interner, Symbol};

/// Escape XML special characters to prevent injection
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// AVL (Approved Vendor List) section
#[derive(Debug, Clone)]
pub struct Avl {
    pub name: Symbol,
    pub header: Option<AvlHeader>,
    pub items: Vec<AvlItem>,
}

impl Avl {
    /// Serialize to XML string
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut xml = format!(
            "  <Avl name=\"{}\">\n",
            escape_xml(interner.resolve(self.name))
        );

        if let Some(ref header) = self.header {
            xml.push_str(&header.to_xml(interner));
        }

        for item in &self.items {
            xml.push_str(&item.to_xml(interner));
        }

        xml.push_str("  </Avl>\n");
        xml
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
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut xml = format!(
            "    <AvlHeader title=\"{}\" source=\"{}\" author=\"{}\" datetime=\"{}\" version=\"{}\"",
            escape_xml(interner.resolve(self.title)),
            escape_xml(interner.resolve(self.source)),
            escape_xml(interner.resolve(self.author)),
            escape_xml(interner.resolve(self.datetime)),
            self.version
        );

        if let Some(comment) = self.comment {
            xml.push_str(&format!(
                " comment=\"{}\"",
                escape_xml(interner.resolve(comment))
            ));
        }

        if let Some(mod_ref) = self.mod_ref {
            xml.push_str(&format!(
                " modRef=\"{}\"",
                escape_xml(interner.resolve(mod_ref))
            ));
        }

        xml.push_str("/>\n");
        xml
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
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut xml = format!(
            "    <AvlItem OEMDesignNumber=\"{}\">\n",
            escape_xml(interner.resolve(self.oem_design_number))
        );

        for vmpn in &self.vmpn_list {
            xml.push_str(&vmpn.to_xml(interner));
        }

        for spec_ref in &self.spec_refs {
            xml.push_str(&format!(
                "      <SpecRef id=\"{}\"/>\n",
                escape_xml(interner.resolve(*spec_ref))
            ));
        }

        xml.push_str("    </AvlItem>\n");
        xml
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
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut xml = String::from("      <AvlVmpn");

        if let Some(evpl_vendor) = self.evpl_vendor {
            xml.push_str(&format!(
                " evplVendor=\"{}\"",
                escape_xml(interner.resolve(evpl_vendor))
            ));
        }

        if let Some(evpl_mpn) = self.evpl_mpn {
            xml.push_str(&format!(
                " evplMpn=\"{}\"",
                escape_xml(interner.resolve(evpl_mpn))
            ));
        }

        if let Some(qualified) = self.qualified {
            xml.push_str(&format!(" qualified=\"{}\"", qualified));
        }

        if let Some(chosen) = self.chosen {
            xml.push_str(&format!(" chosen=\"{}\"", chosen));
        }

        xml.push_str(">\n");

        for mpn in &self.mpns {
            xml.push_str(&mpn.to_xml(interner));
        }

        for vendor in &self.vendors {
            xml.push_str(&vendor.to_xml(interner));
        }

        xml.push_str("      </AvlVmpn>\n");
        xml
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
    pub fn to_xml(&self, interner: &Interner) -> String {
        let mut xml = format!(
            "        <AvlMpn name=\"{}\"",
            escape_xml(interner.resolve(self.name))
        );

        if let Some(rank) = self.rank {
            xml.push_str(&format!(" rank=\"{}\"", rank));
        }

        if let Some(cost) = self.cost {
            xml.push_str(&format!(" cost=\"{}\"", cost));
        }

        if let Some(ref ms) = self.moisture_sensitivity {
            xml.push_str(&format!(" moistureSensitivity=\"{}\"", ms.as_str()));
        }

        if let Some(avail) = self.availability {
            xml.push_str(&format!(" availability=\"{}\"", avail));
        }

        if let Some(other) = self.other {
            xml.push_str(&format!(
                " other=\"{}\"",
                escape_xml(interner.resolve(other))
            ));
        }

        xml.push_str("/>\n");
        xml
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
    pub fn to_xml(&self, interner: &Interner) -> String {
        format!(
            "        <AvlVendor enterpriseRef=\"{}\"/>\n",
            escape_xml(interner.resolve(self.enterprise_ref))
        )
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
    fn test_xml_escaping() {
        assert_eq!(escape_xml("normal"), "normal");
        assert_eq!(escape_xml("A&B"), "A&amp;B");
        assert_eq!(escape_xml("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape_xml("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(escape_xml("'single'"), "&apos;single&apos;");
        assert_eq!(
            escape_xml("R&D <script>alert('xss')</script>"),
            "R&amp;D &lt;script&gt;alert(&apos;xss&apos;)&lt;/script&gt;"
        );
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

        let xml = mpn.to_xml(&interner);
        assert!(xml.contains("R&amp;D &lt;test&gt;"));
        assert!(!xml.contains("<test>"));
    }
}
