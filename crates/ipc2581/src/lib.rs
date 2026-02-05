// Pure IPC-2581 parser modules
mod checksum;
mod intern;
mod parse;
pub mod types;
pub mod units;

pub use intern::{Interner, Symbol};
pub use types::*;

use checksum::validate_checksum;
use parse::Parser;
use roxmltree::Document;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Ipc2581Error {
    #[error("XML parse error: {0}")]
    XmlParse(#[from] roxmltree::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid checksum: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("Missing required element: {0}")]
    MissingElement(&'static str),

    #[error("Missing required attribute '{attr}' on element '{element}'")]
    MissingAttribute {
        element: &'static str,
        attr: &'static str,
    },

    #[error("Invalid attribute value: {0}")]
    InvalidAttribute(String),

    #[error("Invalid IPC-2581 structure: {0}")]
    InvalidStructure(String),

    #[error("Unsupported revision: {0}")]
    UnsupportedRevision(String),
}

pub type Result<T> = std::result::Result<T, Ipc2581Error>;

/// Main IPC-2581 document structure
#[derive(Debug)]
pub struct Ipc2581 {
    interner: Interner,
    revision: Symbol,
    content: Content,
    logistic_header: Option<LogisticHeader>,
    history_record: Option<HistoryRecord>,
    ecad: Option<Ecad>,
    bom: Option<Bom>,
    avl: Option<Avl>,
}

impl Ipc2581 {
    /// Parse IPC-2581 from XML string
    pub fn parse(xml: &str) -> Result<Self> {
        // Validate checksum if present
        validate_checksum(xml)?;

        // Parse XML with roxmltree
        let doc = Document::parse(xml)?;

        // Validate namespace
        let root = doc.root_element();
        if root.tag_name().namespace() != Some("http://webstds.ipc.org/2581") {
            return Err(Ipc2581Error::InvalidStructure(format!(
                "Expected IPC-2581 namespace, got {:?}",
                root.tag_name().namespace()
            )));
        }

        // Parse into our structures
        let mut parser = Parser::new();
        let parsed = parser.parse_document(&doc)?;

        Ok(Self {
            interner: parser.interner,
            revision: parsed.revision,
            content: parsed.content,
            logistic_header: parsed.logistic_header,
            history_record: parsed.history_record,
            ecad: parsed.ecad,
            bom: parsed.bom,
            avl: parsed.avl,
        })
    }

    /// Parse IPC-2581 from file
    pub fn parse_file(path: impl AsRef<Path>) -> Result<Self> {
        let xml = std::fs::read_to_string(path)?;
        Self::parse(&xml)
    }

    /// Get the revision string (e.g., "C")
    pub fn revision(&self) -> &str {
        self.interner.resolve(self.revision)
    }

    /// Get the content section
    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Get the logistic header if present
    pub fn logistic_header(&self) -> Option<&LogisticHeader> {
        self.logistic_header.as_ref()
    }

    /// Get the history record if present
    pub fn history_record(&self) -> Option<&HistoryRecord> {
        self.history_record.as_ref()
    }

    /// Get the Ecad section if present
    pub fn ecad(&self) -> Option<&Ecad> {
        self.ecad.as_ref()
    }

    /// Get the BOM section if present
    pub fn bom(&self) -> Option<&Bom> {
        self.bom.as_ref()
    }

    /// Get the AVL section if present
    pub fn avl(&self) -> Option<&Avl> {
        self.avl.as_ref()
    }

    /// Look up an Enterprise by its ID reference and return its name
    /// Filters out placeholder names like "Manufacturer" or "NONE"
    pub fn resolve_enterprise(&self, enterprise_ref: Symbol) -> Option<&str> {
        let logistic = self.logistic_header.as_ref()?;
        let enterprise = logistic
            .enterprises
            .iter()
            .find(|e| e.id == enterprise_ref)?;

        let name = enterprise.name.map(|name| self.resolve(name))?;

        // Filter out placeholder/template values
        match name {
            "Manufacturer" | "NONE" | "N/A" | "" => None,
            _ => Some(name),
        }
    }

    /// Resolve a symbol to its string value
    pub fn resolve(&self, sym: Symbol) -> &str {
        self.interner.resolve(sym)
    }

    /// Get reference to the string interner
    pub fn interner(&self) -> &Interner {
        &self.interner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_types_compile() {
        let _err = Ipc2581Error::MissingElement("test");
        let _err = Ipc2581Error::MissingAttribute {
            element: "Circle",
            attr: "diameter",
        };
    }

    #[test]
    fn parse_simple_document() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
</IPC-2581>"#;

        let result = Ipc2581::parse(xml);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let doc = result.unwrap();
        assert_eq!(doc.revision(), "C");
        assert_eq!(doc.resolve(doc.content().role_ref), "Owner");
    }

    #[test]
    fn parse_function_mode_with_numeric_level() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="B" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY" level="1"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
</IPC-2581>"#;

        let result = Ipc2581::parse(xml);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let doc = result.unwrap();
        assert_eq!(doc.revision(), "B");
        assert_eq!(
            doc.content().function_mode.level,
            Some(types::content::Level(1))
        );
    }

    #[test]
    fn parse_document_with_avl() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Avl name="Test_AVL">
    <AvlHeader title="Test" source="Test" author="Test" datetime="2025-01-04" version="1"/>
    <AvlItem OEMDesignNumber="PART_001">
      <AvlVmpn qualified="true" chosen="true">
        <AvlMpn name="TEST123" rank="1"/>
        <AvlVendor enterpriseRef="TestVendor"/>
      </AvlVmpn>
    </AvlItem>
  </Avl>
</IPC-2581>"#;

        let result = Ipc2581::parse(xml);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let doc = result.unwrap();
        assert!(doc.avl().is_some(), "AVL section should be parsed");

        let avl = doc.avl().unwrap();
        assert_eq!(doc.resolve(avl.name), "Test_AVL");
        assert_eq!(avl.items.len(), 1);

        let item = &avl.items[0];
        assert_eq!(doc.resolve(item.oem_design_number), "PART_001");
        assert_eq!(item.vmpn_list.len(), 1);

        let vmpn = &item.vmpn_list[0];
        assert_eq!(vmpn.qualified, Some(true));
        assert_eq!(vmpn.chosen, Some(true));
        assert_eq!(vmpn.mpns.len(), 1);
        assert_eq!(doc.resolve(vmpn.mpns[0].name), "TEST123");
    }

    #[test]
    fn parse_bom_with_description() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Bom name="TestBOM">
    <BomHeader assembly="Test Design" revision="1.0"/>
    <BomItem OEMDesignNumberRef="XO32-12MHZ" quantity="1" pinCount="4" category="ELECTRICAL" description="HCMOS Clock Oscillator">
      <RefDes name="U4" packageRef="SG210" populate="true" layerRef="F.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Frequency" textualCharacteristicValue="12MHz"/>
      </Characteristics>
    </BomItem>
  </Bom>
</IPC-2581>"#;

        let result = Ipc2581::parse(xml);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let doc = result.unwrap();
        assert!(doc.bom().is_some(), "BOM section should be parsed");

        let bom = doc.bom().unwrap();
        assert_eq!(doc.resolve(bom.name), "TestBOM");
        assert_eq!(bom.items.len(), 1);

        let item = &bom.items[0];
        assert_eq!(doc.resolve(item.oem_design_number_ref), "XO32-12MHZ");

        // Verify description attribute is parsed
        assert!(item.description.is_some(), "Description should be present");
        assert_eq!(
            doc.resolve(item.description.unwrap()),
            "HCMOS Clock Oscillator"
        );

        // Verify other attributes
        assert_eq!(item.quantity, Some(1));
        assert_eq!(item.pin_count, Some(4));
        assert_eq!(item.ref_des_list.len(), 1);
        assert_eq!(doc.resolve(item.ref_des_list[0].name), "U4");
    }
}
