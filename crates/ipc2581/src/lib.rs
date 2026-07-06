// Pure IPC-2581 parser modules
mod checksum;
pub mod edit;
mod parse;
pub mod types;
pub mod units;
pub mod write;

pub use pcb_intern::{Interner, Symbol};
pub use types::*;
pub use uppsala::XmlWriter;

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
    fn parses_all_ipc_line_properties() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="solid"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="SOLID"/></EntryLineDesc>
      <EntryLineDesc id="dotted"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="DOTTED"/></EntryLineDesc>
      <EntryLineDesc id="dashed"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="DASHED"/></EntryLineDesc>
      <EntryLineDesc id="center"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="CENTER"/></EntryLineDesc>
      <EntryLineDesc id="phantom"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="PHANTOM"/></EntryLineDesc>
      <EntryLineDesc id="erase"><LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="ERASE"/></EntryLineDesc>
    </DictionaryLineDesc>
    <DictionaryColor/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).unwrap();
        let properties = doc
            .content()
            .dictionary_line_desc
            .entries
            .iter()
            .map(|entry| entry.line_desc.line_property)
            .collect::<Vec<_>>();

        assert_eq!(
            properties,
            vec![
                Some(types::primitives::LineProperty::Solid),
                Some(types::primitives::LineProperty::Dotted),
                Some(types::primitives::LineProperty::Dashed),
                Some(types::primitives::LineProperty::Center),
                Some(types::primitives::LineProperty::Phantom),
                Some(types::primitives::LineProperty::Erase),
            ]
        );
    }

    #[test]
    fn parses_profile_cutouts_as_direct_polygon_contours() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="2" y="3"/>
            <PolyStepSegment x="4" y="3"/>
            <PolyStepSegment x="4" y="5"/>
            <PolyStepSegment x="2" y="5"/>
            <PolyStepSegment x="2" y="3"/>
          </Cutout>
          <Cutout>
            <Polygon>
              <PolyBegin x="8" y="3"/>
              <PolyStepSegment x="10" y="3"/>
              <PolyStepSegment x="10" y="5"/>
              <PolyStepSegment x="8" y="5"/>
              <PolyStepSegment x="8" y="3"/>
            </Polygon>
          </Cutout>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let profile = doc.ecad().unwrap().cad_data.steps[0]
            .profile
            .as_ref()
            .unwrap();

        assert_eq!(profile.cutouts.len(), 2);
        assert_eq!(profile.cutouts[0].begin, Point { x: 2.0, y: 3.0 });
        assert_eq!(profile.cutouts[1].begin, Point { x: 8.0, y: 3.0 });
    }

    #[test]
    fn preserves_vcut_specs_spec_refs_and_fiducials() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="ANGLE">
          <Property value="90" unit="DEGREES" plusTol="5" minusTol="5" tolPercent="true"/>
        </V_Cut>
        <V_Cut type="THICKNESS_REMAINING">
          <Property value="0.5" unit="MM" plusTol="0.1" minusTol="0.1"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Step name="Panel" type="PALLET">
        <LayerFeature layerRef="TOP">
          <Set>
            <SpecRef id="VCut_1"/>
            <GlobalFiducial>
              <Location x="1" y="2"/>
              <Circle diameter="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let ecad = doc.ecad().unwrap();
        let spec = ecad
            .cad_header
            .specs
            .get(&doc.interner().get("VCut_1").unwrap())
            .unwrap();
        assert_eq!(spec.items.len(), 2);
        assert_eq!(spec.items[0].kind, ecad::SpecItemKind::VCut);
        assert_eq!(doc.resolve(spec.items[0].item_type.unwrap()), "ANGLE");
        assert_eq!(spec.items[0].properties[0].value, Some(90.0));
        assert_eq!(spec.items[0].properties[0].tol_percent, Some(true));

        let layer = &ecad.cad_data.layers[0];
        assert_eq!(doc.resolve(layer.spec_refs[0]), "VCut_1");

        let set = &ecad.cad_data.steps[0].layer_features[0].sets[0];
        assert_eq!(doc.resolve(set.spec_refs[0]), "VCut_1");
        assert_eq!(set.fiducials().count(), 1);
        assert!(matches!(
            set.features[0],
            ecad::SetFeature::Fiducial(ecad::Fiducial {
                kind: ecad::FiducialKind::Global,
                ..
            })
        ));
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
        assert!(matches!(
            set.features[2],
            ecad::SetFeature::UserPrimitive(_)
        ));
        assert!(matches!(set.features[3], ecad::SetFeature::Trace(_)));

        let traces = set.traces().collect::<Vec<_>>();
        assert_eq!(traces.len(), 2);
        assert!(matches!(traces[1].steps[0], PolyStep::Curve(_)));
        assert_eq!(set.polygons().count(), 1);
        assert_eq!(set.lines().count(), 0);
    }

    #[test]
    fn skips_invalid_inline_user_special_inside_features() {
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
            <Features>
              <Line startX="0" startY="0" endX="1" endY="0"/>
              <UserSpecial>
                <RectCenter height="1"/>
              </UserSpecial>
              <Line startX="0" startY="1" endX="1" endY="1"/>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let set = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets[0];

        assert_eq!(set.features.len(), 2);
        assert!(
            set.features
                .iter()
                .all(|feature| matches!(feature, ecad::SetFeature::Line(_)))
        );
    }

    #[test]
    fn preserves_feature_polyline_curves() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.SilkS" layerFunction="LEGEND"/>
      <Step name="Board">
        <LayerFeature layerRef="F.SilkS">
          <Set>
            <Features>
              <Location x="10" y="20"/>
              <Polyline>
                <PolyBegin x="1" y="0"/>
                <PolyStepCurve x="0" y="1" centerX="0" centerY="0" clockwise="false"/>
                <LineDescRef id="fine"/>
              </Polyline>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let set = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets[0];

        assert_eq!(set.features.len(), 1);
        let ecad::SetFeature::Polyline(polyline) = &set.features[0] else {
            panic!("expected feature polyline");
        };
        assert_eq!(polyline.begin, Point { x: 11.0, y: 20.0 });
        assert!(matches!(polyline.steps[0], PolyStep::Curve(_)));
        assert_eq!(set.polylines().count(), 1);
        assert_eq!(set.lines().count(), 0);
    }

    #[test]
    fn applies_features_location_to_polygons() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.SilkS" layerFunction="LEGEND"/>
      <Step name="Board">
        <LayerFeature layerRef="F.SilkS">
          <Set>
            <Features>
              <Location x="10" y="20"/>
              <Polygon>
                <PolyBegin x="0" y="0"/>
                <PolyStepSegment x="1" y="0"/>
                <PolyStepCurve x="0" y="1" centerX="0" centerY="0" clockwise="false"/>
                <PolyStepSegment x="0" y="0"/>
              </Polygon>
              <UserSpecial>
                <Contour>
                  <Polygon>
                    <PolyBegin x="2" y="0"/>
                    <PolyStepSegment x="3" y="0"/>
                    <PolyStepSegment x="2" y="0"/>
                  </Polygon>
                </Contour>
              </UserSpecial>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let set = &doc.ecad().unwrap().cad_data.steps[0].layer_features[0].sets[0];

        let polygons = set.polygons().collect::<Vec<_>>();
        assert_eq!(polygons.len(), 1);
        assert_eq!(polygons[0].begin, Point { x: 10.0, y: 20.0 });
        assert!(matches!(
            polygons[0].steps[1],
            PolyStep::Curve(PolyStepCurve {
                center: Point { x: 10.0, y: 20.0 },
                ..
            })
        ));
        let ecad::SetFeature::UserPrimitive(user_primitive) = &set.features[1] else {
            panic!("expected inline user primitive");
        };
        assert_eq!(user_primitive.x, 10.0);
        assert_eq!(user_primitive.y, 20.0);
        let UserPrimitive::UserSpecial(user_special) = &user_primitive.primitive;
        let UserShapeType::Contour(contour) = &user_special.shapes[0].shape else {
            panic!("expected contour");
        };
        assert_eq!(contour.polygon.begin, Point { x: 2.0, y: 0.0 });
    }

    #[test]
    fn parses_user_special_contours_lines_polylines_and_line_desc_refs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryUser units="MILLIMETER">
      <EntryUser id="U1">
        <UserSpecial>
          <Contour>
            <Polygon>
              <PolyBegin x="0" y="0"/>
              <PolyStepSegment x="1" y="0"/>
              <PolyStepSegment x="0" y="0"/>
              <LineDescRef id="fine"/>
              <FillDesc fillProperty="HOLLOW"/>
            </Polygon>
            <Cutout>
              <PolyBegin x="0.25" y="0.25"/>
              <PolyStepSegment x="0.75" y="0.25"/>
              <PolyStepSegment x="0.25" y="0.25"/>
            </Cutout>
          </Contour>
          <Line startX="0" startY="0" endX="1" endY="0">
            <LineDescRef id="fine"/>
          </Line>
          <Polyline>
            <PolyBegin x="1" y="0"/>
            <PolyStepCurve x="0" y="1" centerX="0" centerY="0" clockwise="false"/>
            <LineDescRef id="fine"/>
          </Polyline>
        </UserSpecial>
      </EntryUser>
    </DictionaryUser>
  </Content>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let primitive = &doc.content().dictionary_user.entries[0].primitive;
        let UserPrimitive::UserSpecial(user_special) = primitive;

        assert_eq!(user_special.shapes.len(), 3);
        let UserShapeType::Contour(contour) = &user_special.shapes[0].shape else {
            panic!("expected contour");
        };
        assert_eq!(contour.cutouts.len(), 1);
        assert_eq!(
            user_special.shapes[0]
                .line_desc_ref
                .map(|symbol| doc.resolve(symbol)),
            Some("fine")
        );
        assert!(matches!(
            user_special.shapes[1].shape,
            UserShapeType::Line(_)
        ));
        assert!(matches!(
            user_special.shapes[2].shape,
            UserShapeType::Polyline(_)
        ));
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

    #[test]
    fn parse_component_preserves_standard_placement_data() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Component refDes="J1" packageRef="CONN_1" part="USB-C" layerRef="B.Cu" layerRefTopside="F.Cu" mountType="THMT" height="1.2">
          <NonstandardAttribute name="owner" value="diode" type="STRING"/>
          <Xform xOffset="0.1" yOffset="0.2" rotation="270.0" mirror="true" faceUp="true" scale="1.0"/>
          <Location x="10.0" y="-2.5"/>
          <SpecRef id="AssemblySpec"/>
        </Component>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let component = &doc.ecad().unwrap().cad_data.steps[0].components[0];

        assert_eq!(doc.resolve(component.ref_des.unwrap()), "J1");
        assert_eq!(doc.resolve(component.package_ref.unwrap()), "CONN_1");
        assert_eq!(doc.resolve(component.part), "USB-C");
        assert_eq!(doc.resolve(component.layer_ref), "B.Cu");
        assert_eq!(
            component.layer_ref_topside.map(|sym| doc.resolve(sym)),
            Some("F.Cu")
        );
        assert_eq!(component.mount_type, MountType::Thmt);
        assert_eq!(component.height, Some(1.2));
        assert_eq!(component.location.x, 10.0);
        assert_eq!(component.location.y, -2.5);

        let xform = component.xform.unwrap();
        assert_eq!(xform.x_offset, 0.1);
        assert_eq!(xform.y_offset, 0.2);
        assert_eq!(xform.rotation, 270.0);
        assert!(xform.mirror);
        assert!(xform.face_up);
        assert_eq!(xform.scale, 1.0);

        assert_eq!(component.nonstandard_attributes.len(), 1);
        assert_eq!(
            doc.resolve(component.nonstandard_attributes[0].name),
            "owner"
        );
        assert_eq!(component.spec_refs.len(), 1);
        assert_eq!(doc.resolve(component.spec_refs[0]), "AssemblySpec");
    }

    #[test]
    fn parse_component_accepts_tht_mount_type_alias() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Component refDes="J1" part="CONN" layerRef="F.Cu" mountType="THT">
          <Location x="0" y="0"/>
        </Component>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let doc = Ipc2581::parse(xml).expect("parse IPC-2581");
        let component = &doc.ecad().unwrap().cad_data.steps[0].components[0];

        assert_eq!(component.mount_type, MountType::Thmt);
    }
}
