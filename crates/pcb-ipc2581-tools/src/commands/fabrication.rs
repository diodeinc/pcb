use std::collections::{HashMap, HashSet};
use std::ops::Range;

use anyhow::{Context, Result};
use ipc2581::XmlWriter;
use ipc2581::edit::{self, Doc, Edit, Node};

const EXCLUDED_STEP_CHILDREN: &[&str] =
    &["Package", "Component", "LogicalNet", "Port", "Model", "Dfx"];

const DICTIONARY_REFERENCES: &[DictionaryReference] = &[
    DictionaryReference {
        dictionary: "DictionaryColor",
        entry: "EntryColor",
        reference: "ColorRef",
        attribute: None,
    },
    DictionaryReference {
        dictionary: "DictionaryLineDesc",
        entry: "EntryLineDesc",
        reference: "LineDescRef",
        attribute: Some("lineDescRef"),
    },
    DictionaryReference {
        dictionary: "DictionaryFillDesc",
        entry: "EntryFillDesc",
        reference: "FillDescRef",
        attribute: Some("fillDescRef"),
    },
    DictionaryReference {
        dictionary: "DictionaryFont",
        entry: "EntryFont",
        reference: "FontRef",
        attribute: Some("fontRef"),
    },
    DictionaryReference {
        dictionary: "DictionaryStandard",
        entry: "EntryStandard",
        reference: "StandardPrimitiveRef",
        attribute: Some("standardPrimitiveRef"),
    },
    DictionaryReference {
        dictionary: "DictionaryUser",
        entry: "EntryUser",
        reference: "UserPrimitiveRef",
        attribute: Some("userPrimitiveRef"),
    },
    DictionaryReference {
        dictionary: "DictionaryFirmware",
        entry: "EntryFirmware",
        reference: "FirmwareRef",
        attribute: Some("firmwareRef"),
    },
];

struct DictionaryReference {
    dictionary: &'static str,
    entry: &'static str,
    reference: &'static str,
    attribute: Option<&'static str>,
}

/// Project an IPC-2581 document onto the manufacturing data allowed by
/// IPC-2581C fabrication mode.
///
/// Fabrication mode requires the physical construction and manufacturing
/// artwork, but excludes package, placement, assembly, solder-paste, BOM/AVL,
/// documentation, logical-net, and DFX sections. Optional fabrication data is
/// retained when it can affect the manufactured board.
pub(crate) fn strip_non_manufacturing(xml: &str) -> Result<String> {
    let normalized = normalize_layer_functions(xml)?;
    let filtered = filter_sections_and_layers(&normalized)?;
    let filtered = strip_component_associations(&filtered)?;
    let filtered = prune_unreferenced_definitions(&filtered)?;
    rewrite_function_mode(&filtered)
}

fn normalize_layer_functions(xml: &str) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let mut edits = Vec::new();
    for layer in doc.find_all("Layer") {
        let Some(normalized) =
            doc.attr(layer, "layerFunction")
                .and_then(|function| match function {
                    // These aliases are accepted by the internal parser, but IPC-2581C
                    // names the schema values ROUT and V_CUT.
                    "ROUTE" => Some("ROUT"),
                    "SCORE" => Some("V_CUT"),
                    _ => None,
                })
        else {
            continue;
        };
        let attrs = doc
            .attrs(layer)
            .map(|(name, value)| {
                (
                    name.to_string(),
                    if name == "layerFunction" {
                        normalized.to_string()
                    } else {
                        value.to_string()
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut writer = XmlWriter::new();
        if doc.source(layer).ends_with("/>") {
            writer.empty_element_with("Layer", attrs);
        } else {
            writer.start_element_with("Layer", attrs);
        }
        edits.push(doc.replace_start_tag(layer, writer.into_string()));
    }
    Ok(edit::apply(xml, edits)?)
}

fn filter_sections_and_layers(xml: &str) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;

    let layers = doc
        .find_all("Layer")
        .into_iter()
        .filter_map(|node| {
            Some((
                doc.attr(node, "name")?,
                doc.attr(node, "layerFunction")?,
                node,
            ))
        })
        .collect::<Vec<_>>();
    let known_layers = layers
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<HashSet<_>>();
    let retained_layers = layers
        .iter()
        .filter(|(_, function, _)| is_manufacturing_layer(function))
        .map(|(name, _, _)| *name)
        .collect::<HashSet<_>>();

    let mut deletions = Vec::new();
    for child in doc.children(root) {
        if matches!(doc.name(child), "Bom" | "Avl") {
            deletions.push(child);
        }
    }
    for element in ["BomRef", "AvlRef"] {
        deletions.extend(doc.find_all(element));
    }
    for element in EXCLUDED_STEP_CHILDREN {
        deletions.extend(doc.find_all(element));
    }
    for (name, _, layer) in layers {
        if !retained_layers.contains(name) {
            deletions.push(layer);
        }
    }
    for stackup_layer in doc.find_all("StackupLayer") {
        if doc
            .attr(stackup_layer, "layerOrGroupRef")
            .is_some_and(|name| known_layers.contains(name) && !retained_layers.contains(name))
        {
            deletions.push(stackup_layer);
        }
    }
    for layer_ref in doc.find_all("CADDataLayerRef") {
        if doc
            .attr(layer_ref, "layerId")
            .is_some_and(|name| known_layers.contains(name) && !retained_layers.contains(name))
        {
            deletions.push(layer_ref);
        }
    }
    for layer_ref in doc.find_all("LayerRef") {
        if doc
            .attr(layer_ref, "name")
            .is_some_and(|name| !retained_layers.contains(name))
        {
            deletions.push(layer_ref);
        }
    }
    for layer_feature in doc.find_all("LayerFeature") {
        if doc
            .attr(layer_feature, "layerRef")
            .is_some_and(|name| !retained_layers.contains(name))
        {
            deletions.push(layer_feature);
        }
    }

    apply_deletions(xml, &doc, deletions)
}

fn strip_component_associations(xml: &str) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let mut edits = Vec::new();

    for node in descendants(&doc, root) {
        if matches!(doc.name(node), "PinRef" | "PortRef") {
            edits.push(doc.delete(node));
            continue;
        }

        let attrs = doc
            .attrs(node)
            .filter(|(name, _)| {
                !matches!(
                    *name,
                    "componentRef"
                        | "compRef"
                        | "packageRef"
                        | "pinRef"
                        | "bomRef"
                        | "modelRef"
                        | "matDes"
                )
            })
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<Vec<_>>();
        if attrs.len() == doc.attrs(node).count() {
            continue;
        }

        let mut writer = XmlWriter::new();
        if doc.source(node).ends_with("/>") {
            writer.empty_element_with(doc.name(node), attrs);
        } else {
            writer.start_element_with(doc.name(node), attrs);
        }
        edits.push(doc.replace_start_tag(node, writer.into_string()));
    }

    Ok(edit::apply(xml, edits)?)
}

fn prune_unreferenced_definitions(xml: &str) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let all_nodes = descendants(&doc, root);
    let mut deletions = Vec::new();

    let padstack_refs = all_nodes
        .iter()
        .filter_map(|node| doc.attr(*node, "padstackDefRef"))
        .collect::<HashSet<_>>();
    for definition in doc.find_all("PadStackDef") {
        if doc
            .attr(definition, "name")
            .is_none_or(|name| !padstack_refs.contains(name))
        {
            deletions.push(definition);
        }
    }

    let spec_refs = all_nodes
        .iter()
        .flat_map(|node| {
            let element_ref = (doc.name(*node) == "SpecRef")
                .then(|| doc.attr(*node, "id"))
                .flatten();
            [element_ref, doc.attr(*node, "specRef")]
                .into_iter()
                .flatten()
        })
        .collect::<HashSet<_>>();
    for spec in doc.find_all("Spec") {
        if doc
            .attr(spec, "name")
            .is_none_or(|name| !spec_refs.contains(name))
        {
            deletions.push(spec);
        }
    }

    let without_unreferenced_specs = apply_deletions(xml, &doc, deletions)?;
    prune_unreferenced_dictionary_entries(&without_unreferenced_specs)
}

fn prune_unreferenced_dictionary_entries(xml: &str) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let all_nodes = descendants(&doc, root);
    let deletions = unreferenced_dictionary_entries(&doc, &all_nodes)?;
    apply_deletions(xml, &doc, deletions)
}

fn unreferenced_dictionary_entries<'a>(doc: &'a Doc<'a>, all_nodes: &[Node]) -> Result<Vec<Node>> {
    let root = doc.root()?;
    let content = doc
        .child(root, "Content")
        .context("IPC-2581 document has no Content element")?;
    let dictionary_spans = DICTIONARY_REFERENCES
        .iter()
        .filter_map(|kind| doc.child(content, kind.dictionary))
        .map(|node| doc.span(node))
        .collect::<Vec<_>>();

    let mut references = DICTIONARY_REFERENCES
        .iter()
        .map(|kind| (kind.entry, HashSet::<&str>::new()))
        .collect::<HashMap<_, _>>();

    collect_dictionary_references(
        doc,
        all_nodes.iter().copied().filter(|node| {
            let span = doc.span(*node);
            !dictionary_spans
                .iter()
                .any(|dictionary| contains(dictionary, &span))
        }),
        &mut references,
    );

    loop {
        let selected_entry_spans = DICTIONARY_REFERENCES
            .iter()
            .flat_map(|kind| {
                let referenced = &references[kind.entry];
                doc.find_all(kind.entry)
                    .into_iter()
                    .filter(move |entry| {
                        doc.attr(*entry, "id")
                            .is_some_and(|id| referenced.contains(id))
                    })
                    .map(|entry| doc.span(entry))
            })
            .collect::<Vec<_>>();
        let before = references.values().map(HashSet::len).sum::<usize>();
        collect_dictionary_references(
            doc,
            all_nodes.iter().copied().filter(|node| {
                let span = doc.span(*node);
                selected_entry_spans
                    .iter()
                    .any(|entry| contains(entry, &span))
            }),
            &mut references,
        );
        let after = references.values().map(HashSet::len).sum::<usize>();
        if after == before {
            break;
        }
    }

    Ok(DICTIONARY_REFERENCES
        .iter()
        .flat_map(|kind| {
            let referenced = &references[kind.entry];
            doc.find_all(kind.entry).into_iter().filter(move |entry| {
                doc.attr(*entry, "id")
                    .is_none_or(|id| !referenced.contains(id))
            })
        })
        .collect())
}

fn collect_dictionary_references<'a>(
    doc: &'a Doc<'a>,
    nodes: impl Iterator<Item = Node>,
    references: &mut HashMap<&'static str, HashSet<&'a str>>,
) {
    for node in nodes {
        for kind in DICTIONARY_REFERENCES {
            if doc.name(node) == kind.reference
                && let Some(id) = doc.attr(node, "id")
            {
                references
                    .get_mut(kind.entry)
                    .expect("all dictionary entry kinds are initialized")
                    .insert(id);
            }
            if let Some(attribute) = kind.attribute
                && let Some(id) = doc.attr(node, attribute)
            {
                references
                    .get_mut(kind.entry)
                    .expect("all dictionary entry kinds are initialized")
                    .insert(id);
            }
        }
    }
}

fn rewrite_function_mode(xml: &str) -> Result<String> {
    let section_key = fabrication_section_key(xml)?;
    let doc = Doc::parse(xml)?;
    let mut edits = Vec::new();
    for function_mode in doc.find_all("FunctionMode") {
        let mut attrs = vec![
            ("mode".to_string(), "FABRICATION".to_string()),
            ("sectionKey".to_string(), section_key.clone()),
        ];
        attrs.extend(
            doc.attrs(function_mode)
                .filter(|(name, _)| !matches!(*name, "mode" | "sectionKey"))
                .map(|(name, value)| (name.to_string(), value.to_string())),
        );
        let mut writer = XmlWriter::new();
        writer.empty_element_with("FunctionMode", attrs);
        edits.push(doc.replace(function_mode, writer.into_string()));
    }
    Ok(edit::apply(xml, edits)?)
}

fn fabrication_section_key(xml: &str) -> Result<String> {
    Ok(fabrication_section_key_union(std::slice::from_ref(
        &Doc::parse(xml)?,
    )))
}

/// The IPC-2581 fabrication `sectionKey` implied by the union of the given
/// documents' physical content. Element presence is monotone under document
/// composition, so a composed document's key is the union of its sources'.
pub(crate) fn fabrication_section_key_union(docs: &[Doc<'_>]) -> String {
    let mut keys = HashSet::new();
    for doc in docs {
        collect_section_keys(doc, &mut keys);
    }
    "KSUMLRDOIEFY"
        .chars()
        .filter(|key| keys.contains(key))
        .collect()
}

fn collect_section_keys(doc: &Doc<'_>, keys: &mut HashSet<char>) {
    if !doc.find_all("PadStackDef").is_empty() {
        keys.insert('K');
    }
    if !doc.find_all("Stackup").is_empty() {
        keys.insert('S');
    }
    if !doc.find_all("Profile").is_empty() {
        keys.insert('U');
    }
    if !doc.find_all("PhyNetGroup").is_empty() {
        keys.insert('Y');
    }
    for layer in doc.find_all("Layer") {
        let Some(function) = doc.attr(layer, "layerFunction") else {
            continue;
        };
        match function {
            "SOLDERMASK" => {
                keys.insert('M');
            }
            "SILKSCREEN" | "LEGEND" => {
                keys.insert('L');
            }
            "DRILL" | "ROUT" | "ROUTE" | "V_CUT" | "SCORE" => {
                keys.insert('R');
            }
            "BOARD_OUTLINE" => {
                keys.insert('D');
            }
            "EDGE_CHAMFER" => {
                keys.extend(['R', 'F']);
            }
            "CONDUCTOR" | "CONDFILM" | "CONDFOIL" | "PLANE" | "SIGNAL" | "MIXED" => {
                match doc.attr(layer, "side") {
                    Some("INTERNAL") => {
                        keys.insert('I');
                    }
                    _ => {
                        keys.insert('O');
                    }
                }
            }
            "DIELBASE" | "DIELCORE" | "DIELPREG" | "DIELADHV" | "DIELBONDPLY" | "DIELCOVERLAY" => {
                keys.insert('E');
            }
            "COATINGCOND"
            | "COATINGNONCOND"
            | "CONDUCTIVE_ADHESIVE"
            | "GLUE"
            | "HOLEFILL"
            | "SOLDERBUMP"
            | "THIEVING_KEEP_INOUT"
            | "EDGE_PLATING"
            | "STIFFENER"
            | "CAPACITIVE"
            | "RESISTIVE" => {
                keys.insert('F');
            }
            _ => {}
        }
    }
}

fn is_manufacturing_layer(function: &str) -> bool {
    matches!(
        function,
        "CONDUCTOR"
            | "CONDFILM"
            | "CONDFOIL"
            | "PLANE"
            | "SIGNAL"
            | "MIXED"
            | "COATINGCOND"
            | "COATINGNONCOND"
            | "SOLDERMASK"
            | "SILKSCREEN"
            | "LEGEND"
            | "DRILL"
            | "ROUT"
            | "ROUTE"
            | "V_CUT"
            | "SCORE"
            | "BOARD_OUTLINE"
            | "EDGE_CHAMFER"
            | "EDGE_PLATING"
            | "DIELBASE"
            | "DIELCORE"
            | "DIELPREG"
            | "DIELADHV"
            | "DIELBONDPLY"
            | "DIELCOVERLAY"
            | "CONDUCTIVE_ADHESIVE"
            | "GLUE"
            | "HOLEFILL"
            | "SOLDERBUMP"
            | "STIFFENER"
            | "CAPACITIVE"
            | "RESISTIVE"
            | "THIEVING_KEEP_INOUT"
            | "STACKUP_COMPOSITE"
    )
}

fn descendants(doc: &Doc<'_>, node: Node) -> Vec<Node> {
    fn visit(doc: &Doc<'_>, node: Node, nodes: &mut Vec<Node>) {
        nodes.push(node);
        for child in doc.children(node) {
            visit(doc, child, nodes);
        }
    }

    let mut nodes = Vec::new();
    visit(doc, node, &mut nodes);
    nodes
}

fn apply_deletions(xml: &str, doc: &Doc<'_>, nodes: Vec<Node>) -> Result<String> {
    let mut spans = nodes
        .into_iter()
        .map(|node| (doc.span(node), node))
        .collect::<Vec<_>>();
    spans.sort_by_key(|(span, _)| span.start);

    let mut edits = Vec::<Edit>::new();
    let mut deleted_end = 0;
    for (span, node) in spans {
        if span.start < deleted_end {
            continue;
        }
        deleted_end = span.end;
        edits.push(doc.delete(node));
    }
    Ok(edit::apply(xml, edits)?)
}

fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <LayerRef name="SCORE"/>
    <LayerRef name="OUTLINE"/>
    <LayerRef name="PASTE"/>
    <LayerRef name="COURTYARD"/>
    <BomRef name="bom"/>
    <AvlRef name="avl"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="used"><Circle diameter="1"/></EntryStandard>
      <EntryStandard id="package-only"><Circle diameter="2"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <LogisticHeader>
    <Role id="owner" roleFunction="DESIGNER"/>
    <Enterprise id="enterprise" code="EXAMPLE"/>
    <Person name="designer" enterpriseRef="enterprise" roleRef="owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-07-24T00:00:00Z" software="test" lastChange="2026-07-24T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="fixture">
      <SoftwarePackage name="test" vendor="test" revision="1">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom">
    <BomHeader assembly="panel" revision="1">
      <StepRef name="panel"/>
    </BomHeader>
    <BomItem OEMDesignNumberRef="material" quantity="1" category="ELECTRICAL">
      <MatDes name="FR4" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Ecad name="assembly">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="SCORE" layerFunction="SCORE" side="ALL" polarity="POSITIVE"/>
      <Layer name="OUTLINE" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>
      <Layer name="PASTE" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Layer name="COURTYARD" layerFunction="COURTYARD" side="TOP" polarity="POSITIVE"/>
      <Stackup name="stackup" overallThickness="0.035" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED" matDes="FR4">
        <StackupGroup name="group" thickness="0.035" tolPlus="0" tolMinus="0" matDes="FR4">
          <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0" matDes="FR4"/>
          <StackupLayer layerOrGroupRef="PASTE" thickness="0" tolPlus="0" tolMinus="0" sequence="1"/>
        </StackupGroup>
      </Stackup>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
        </Profile>
        <Package name="pkg" type="ELECTRICAL">
          <Outline>
            <Polygon><PolyBegin x="0" y="0"/></Polygon>
            <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
          </Outline>
        </Package>
        <Component refDes="U1" packageRef="pkg" part="part" layerRef="TOP" mountType="SMT">
          <Location x="1" y="1"/>
        </Component>
        <LogicalNet name="N1"/>
        <PhyNetGroup name="physical-nets">
          <PhyNet name="N1">
            <PhyNetPoint x="1" y="1" layerRef="TOP" netNode="END" exposure="EXPOSED">
              <StandardPrimitiveRef id="used"/>
              <PortRef portName="P1"/>
            </PhyNetPoint>
          </PhyNet>
        </PhyNetGroup>
        <LayerFeature layerRef="TOP">
          <Set componentRef="U1">
            <Pad>
              <Location x="1" y="1"/>
              <StandardPrimitiveRef id="used"/>
              <PinRef componentRef="U1" pin="1"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="SCORE">
          <Set>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="OUTLINE">
          <Set>
            <Features>
              <Line startX="0" startY="0" endX="10" endY="0">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="PASTE">
          <Set><Features><StandardPrimitiveRef id="package-only"/></Features></Set>
        </LayerFeature>
        <LayerFeature layerRef="COURTYARD">
          <Set><Features><StandardPrimitiveRef id="package-only"/></Features></Set>
        </LayerFeature>
        <Port name="P1">
          <ComponentPad/>
          <PortConnect portName="P1"/>
        </Port>
      </Step>
    </CadData>
  </Ecad>
  <Avl name="avl"/>
</IPC-2581>"#;

    #[test]
    fn keeps_only_fabrication_sections_and_referenced_definitions() {
        let filtered = strip_non_manufacturing(XML).unwrap();

        for removed in [
            "<BomRef",
            "<AvlRef",
            "<Bom ",
            "<Avl ",
            "<Package",
            "<Component",
            "<LogicalNet",
            "<PinRef",
            "<PortRef",
            "componentRef=",
            "packageRef=",
            "pinRef=",
            "bomRef=",
            "matDes=",
            "name=\"PASTE\"",
            "name=\"COURTYARD\"",
            "id=\"package-only\"",
        ] {
            assert!(!filtered.contains(removed), "{removed} was not removed");
        }
        assert!(filtered.contains(r#"<FunctionMode mode="FABRICATION" sectionKey="SURDOY"/>"#));
        assert!(filtered.contains(r#"<Layer name="TOP""#));
        assert!(filtered.contains(r#"<Layer name="SCORE" layerFunction="V_CUT""#));
        assert!(filtered.contains(r#"<Layer name="OUTLINE" layerFunction="BOARD_OUTLINE""#));
        assert!(filtered.contains(r#"<LayerFeature layerRef="SCORE">"#));
        assert!(filtered.contains(r#"<LayerFeature layerRef="OUTLINE">"#));
        assert!(filtered.contains(r#"<EntryStandard id="used">"#));
        assert!(filtered.contains(r#"<Stackup name="stackup""#));
        assert!(filtered.contains("<Profile>"));
        ipc2581::Ipc2581::validate(&filtered)
            .expect("fabrication projection should validate against IPC-2581C");
    }
}
