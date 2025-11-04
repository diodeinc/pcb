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
}
