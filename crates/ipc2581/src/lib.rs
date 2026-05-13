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
use std::path::Path;
use std::sync::LazyLock;
use thiserror::Error;
use uppsala::XsdValidator;

const IPC_2581C_XSD: &str = include_str!("../IPC-2581C.xsd");

static IPC_2581C_VALIDATOR: LazyLock<std::result::Result<XsdValidator, String>> =
    LazyLock::new(|| {
        let schema_doc = uppsala::parse(IPC_2581C_XSD).map_err(|err| err.to_string())?;
        XsdValidator::from_schema(&schema_doc).map_err(|err| err.to_string())
    });

#[derive(Debug, Error)]
pub enum Ipc2581Error {
    #[error("XML parse error: {0}")]
    XmlParse(String),

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

    #[error("IPC-2581 schema validation failed: {0}")]
    SchemaValidation(String),
}

pub type Result<T> = std::result::Result<T, Ipc2581Error>;

/// Validate IPC-2581 XML against the vendored IPC-2581C XML Schema.
pub fn validate(xml: &str) -> Result<()> {
    let validator = IPC_2581C_VALIDATOR
        .as_ref()
        .map_err(|err| Ipc2581Error::SchemaValidation(err.clone()))?;
    let doc = uppsala::parse(xml).map_err(|err| Ipc2581Error::SchemaValidation(err.to_string()))?;

    let errors = validator.validate(&doc);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Ipc2581Error::SchemaValidation(
            errors
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }
}

/// Validate an IPC-2581 XML file against the vendored IPC-2581C XML Schema.
pub fn validate_file(path: impl AsRef<Path>) -> Result<()> {
    let xml = std::fs::read_to_string(path)?;
    validate(&xml)
}

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
    /// Validate IPC-2581 XML against the vendored IPC-2581C XML Schema.
    pub fn validate(xml: &str) -> Result<()> {
        validate(xml)
    }

    /// Validate an IPC-2581 XML file against the vendored IPC-2581C XML Schema.
    pub fn validate_file(path: impl AsRef<Path>) -> Result<()> {
        validate_file(path)
    }

    /// Parse IPC-2581 from XML string
    pub fn parse(xml: &str) -> Result<Self> {
        // Validate checksum if present
        validate_checksum(xml)?;

        // Parse XML with Uppsala's arena-backed DOM.
        let doc = uppsala::parse(xml).map_err(|err| Ipc2581Error::XmlParse(err.to_string()))?;

        // Validate namespace
        let root = doc
            .document_element()
            .ok_or(Ipc2581Error::MissingElement("IPC-2581"))?;
        let root_name = doc.element(root).expect("root is an element").name.clone();
        if root_name.namespace_uri.as_deref() != Some("http://webstds.ipc.org/2581") {
            return Err(Ipc2581Error::InvalidStructure(format!(
                "Expected IPC-2581 namespace, got {:?}",
                root_name.namespace_uri
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
    fn validate_reports_schema_errors() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="NOT_A_MODE"/>
  </Content>
</IPC-2581>"#;

        let err = validate(xml).unwrap_err().to_string();
        assert!(err.contains("schema validation failed"));
        assert!(err.contains("LogisticHeader"));
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
    fn preserves_set_feature_source_order() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="SIGNAL"/>
      <Step name="Board">
        <LayerFeature layerRef="F.Cu">
          <Set>
            <Polyline>
              <PolyBegin x="0" y="0"/>
              <PolyStepSegment x="1" y="0"/>
              <LineDescRef id="trace"/>
            </Polyline>
            <Features>
              <Polygon>
                <PolyBegin x="0" y="0"/>
                <PolyStepSegment x="1" y="0"/>
                <PolyStepSegment x="1" y="1"/>
                <PolyStepSegment x="0" y="0"/>
              </Polygon>
              <UserSpecial>
                <Line startX="0" startY="0" endX="0" endY="1">
                  <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
                </Line>
              </UserSpecial>
            </Features>
            <Polyline>
              <PolyBegin x="2" y="0"/>
              <PolyStepCurve x="3" y="1" centerX="2" centerY="1" clockwise="true"/>
              <LineDescRef id="trace"/>
            </Polyline>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let set = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets[0];

        assert_eq!(set.features.len(), 4);
        assert!(matches!(set.features[0], ecad::SetFeature::Trace(_)));
        assert!(matches!(set.features[1], ecad::SetFeature::Polygon(_)));
        assert!(matches!(set.features[2], ecad::SetFeature::Line(_)));
        assert!(matches!(set.features[3], ecad::SetFeature::Trace(_)));

        assert_eq!(set.traces.len(), 2);
        assert!(matches!(set.traces[1].steps[0], PolyStep::Curve(_)));
        assert_eq!(set.polygons.len(), 1);
        assert_eq!(set.lines.len(), 1);
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
