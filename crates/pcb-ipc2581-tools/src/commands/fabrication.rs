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
    let doc = Doc::parse(xml)?;
    Ok(edit::apply(xml, fabrication_edits(&doc)?)?)
}

/// The edits behind [`strip_non_manufacturing`], against one parsed document.
///
/// The projection runs in stages, each deciding from what the stages before
/// it left alive: excluded sections and layers go first, then the pad stacks
/// and specs nothing surviving references, then the dictionary entries
/// nothing surviving reaches. Deletions only mark elements; the splices are
/// emitted once at the end, so nested removals never overlap.
pub(crate) fn fabrication_edits(doc: &Doc<'_>) -> Result<Vec<Edit>> {
    let root = doc.root()?;
    let elements = descendants(doc, root)
        .into_iter()
        .map(|node| (node, doc.span(node)))
        .collect::<Vec<_>>();
    let mut deleted = vec![false; elements.len()];

    mark_excluded_sections_and_layers(doc, root, &elements, &mut deleted);
    let alive = alive_elements(&elements, &deleted);
    mark_unreferenced_definitions(doc, &elements, &alive, &mut deleted);
    let alive = alive_elements(&elements, &deleted);
    mark_unreferenced_dictionary_entries(doc, root, &elements, &alive, &mut deleted)?;
    let alive = alive_elements(&elements, &deleted);

    let survivors = || {
        elements
            .iter()
            .zip(&alive)
            .filter_map(|((node, _), alive)| alive.then_some(*node))
    };
    let section_key = section_key(doc, survivors());

    let mut edits = Vec::new();
    let mut deleted_end = 0;
    for (index, (node, span)) in elements.iter().enumerate() {
        if span.start < deleted_end {
            continue;
        }
        if deleted[index] {
            deleted_end = span.end;
            edits.push(doc.delete(*node));
        } else if let Some(edit) = rewritten_start_tag(doc, *node, &section_key) {
            edits.push(edit);
        }
    }
    Ok(edits)
}

/// Elements that are neither deleted nor inside a deleted element. Elements
/// are in document order, so a deleted subtree is one contiguous run.
fn alive_elements(elements: &[(Node, Range<usize>)], deleted: &[bool]) -> Vec<bool> {
    let mut deleted_end = 0;
    elements
        .iter()
        .zip(deleted)
        .map(|((_, span), &deleted)| {
            if span.start >= deleted_end && deleted {
                deleted_end = span.end;
            }
            span.start >= deleted_end
        })
        .collect()
}

fn mark_excluded_sections_and_layers(
    doc: &Doc<'_>,
    root: Node,
    elements: &[(Node, Range<usize>)],
    deleted: &mut [bool],
) {
    let layers = elements
        .iter()
        .filter(|(node, _)| doc.name(*node) == "Layer")
        .filter_map(|(node, _)| Some((doc.attr(*node, "name")?, doc.attr(*node, "layerFunction")?)))
        .collect::<Vec<_>>();
    let known_layers = layers.iter().map(|(name, _)| *name).collect::<HashSet<_>>();
    let retained_layers = layers
        .iter()
        .filter(|(_, function)| is_manufacturing_layer(function))
        .map(|(name, _)| *name)
        .collect::<HashSet<_>>();
    let dropped = |name: Option<&str>| name.is_some_and(|name| !retained_layers.contains(name));
    let known_dropped = |name: Option<&str>| {
        name.is_some_and(|name| known_layers.contains(name) && !retained_layers.contains(name))
    };
    let root_sections = doc
        .children(root)
        .into_iter()
        .filter(|child| matches!(doc.name(*child), "Bom" | "Avl"))
        .map(|child| doc.span(child).start)
        .collect::<HashSet<_>>();

    for ((node, span), deleted) in elements.iter().zip(deleted) {
        let node = *node;
        *deleted = match doc.name(node) {
            "Bom" | "Avl" => root_sections.contains(&span.start),
            "BomRef" | "AvlRef" | "PinRef" | "PortRef" => true,
            name if EXCLUDED_STEP_CHILDREN.contains(&name) => true,
            "Layer" => doc.attr(node, "layerFunction").is_some() && dropped(doc.attr(node, "name")),
            "StackupLayer" => known_dropped(doc.attr(node, "layerOrGroupRef")),
            "CADDataLayerRef" => known_dropped(doc.attr(node, "layerId")),
            "LayerRef" => dropped(doc.attr(node, "name")),
            "LayerFeature" => dropped(doc.attr(node, "layerRef")),
            _ => false,
        };
    }
}

/// Pad stacks and specs that no surviving element references.
fn mark_unreferenced_definitions(
    doc: &Doc<'_>,
    elements: &[(Node, Range<usize>)],
    alive: &[bool],
    deleted: &mut [bool],
) {
    let survivors = || {
        elements
            .iter()
            .zip(alive)
            .filter_map(|((node, _), alive)| alive.then_some(*node))
    };
    let padstack_refs = survivors()
        .filter_map(|node| doc.attr(node, "padstackDefRef"))
        .collect::<HashSet<_>>();
    let spec_refs = survivors()
        .flat_map(|node| {
            let element_ref = (doc.name(node) == "SpecRef")
                .then(|| doc.attr(node, "id"))
                .flatten();
            [element_ref, doc.attr(node, "specRef")]
                .into_iter()
                .flatten()
        })
        .collect::<HashSet<_>>();

    for (index, (node, _)) in elements
        .iter()
        .enumerate()
        .filter(|(index, _)| alive[*index])
    {
        let referenced = match doc.name(*node) {
            "PadStackDef" => &padstack_refs,
            "Spec" => &spec_refs,
            _ => continue,
        };
        deleted[index] = doc
            .attr(*node, "name")
            .is_none_or(|name| !referenced.contains(name));
    }
}

/// Dictionary entries that nothing outside the dictionaries reaches, directly
/// or through another reachable entry.
fn mark_unreferenced_dictionary_entries<'a>(
    doc: &'a Doc<'a>,
    root: Node,
    elements: &[(Node, Range<usize>)],
    alive: &[bool],
    deleted: &mut [bool],
) -> Result<()> {
    let content = doc
        .child(root, "Content")
        .context("IPC-2581 document has no Content element")?;
    let dictionary_spans = DICTIONARY_REFERENCES
        .iter()
        .filter_map(|kind| doc.child(content, kind.dictionary))
        .map(|node| doc.span(node))
        .collect::<Vec<_>>();

    // One walk splits every surviving reference by where it sits: inside a
    // dictionary entry, where it counts only once that entry is reached, or
    // outside the dictionaries, where it seeds the search.
    let mut entries = Vec::<(usize, usize, &str)>::new();
    let mut entry_references = Vec::<Vec<(usize, &str)>>::new();
    let mut open_entries = Vec::<(usize, usize)>::new();
    let mut pending = Vec::<(usize, &str)>::new();
    for (index, (node, span)) in elements
        .iter()
        .enumerate()
        .filter(|(index, _)| alive[*index])
    {
        while open_entries
            .last()
            .is_some_and(|(_, end)| span.start >= *end)
        {
            open_entries.pop();
        }
        if let Some(kind) = DICTIONARY_REFERENCES
            .iter()
            .position(|kind| kind.entry == doc.name(*node))
        {
            open_entries.push((entries.len(), span.end));
            entries.push((index, kind, doc.attr(*node, "id").unwrap_or_default()));
            entry_references.push(Vec::new());
        }
        let in_dictionary = dictionary_spans
            .iter()
            .any(|dictionary| contains(dictionary, span));
        for reference in dictionary_references(doc, *node) {
            for (entry, _) in &open_entries {
                entry_references[*entry].push(reference);
            }
            if !in_dictionary {
                pending.push(reference);
            }
        }
    }

    let mut entries_by_id = HashMap::<(usize, &str), Vec<usize>>::new();
    for (entry, (index, kind, id)) in entries.iter().enumerate() {
        if doc.attr(elements[*index].0, "id").is_some() {
            entries_by_id.entry((*kind, *id)).or_default().push(entry);
        }
    }
    let mut referenced = HashSet::new();
    while let Some(reference) = pending.pop() {
        if referenced.insert(reference) {
            for &entry in entries_by_id.get(&reference).into_iter().flatten() {
                pending.extend(entry_references[entry].iter().copied());
            }
        }
    }

    for (index, kind, id) in entries {
        deleted[index] =
            doc.attr(elements[index].0, "id").is_none() || !referenced.contains(&(kind, id));
    }
    Ok(())
}

/// The dictionary entries one element names, as (kind, id).
fn dictionary_references<'a>(
    doc: &'a Doc<'a>,
    node: Node,
) -> impl Iterator<Item = (usize, &'a str)> {
    DICTIONARY_REFERENCES
        .iter()
        .enumerate()
        .flat_map(move |(kind_index, kind)| {
            let element_ref = (doc.name(node) == kind.reference)
                .then(|| doc.attr(node, "id"))
                .flatten();
            let attribute_ref = kind
                .attribute
                .and_then(|attribute| doc.attr(node, attribute));
            [element_ref, attribute_ref]
                .into_iter()
                .flatten()
                .map(move |id| (kind_index, id))
        })
}

/// The start tag a surviving element keeps: without the attributes that tie
/// it to components, with schema layer-function names, and for FunctionMode
/// with the fabrication mode and its section key.
fn rewritten_start_tag(doc: &Doc<'_>, node: Node, section_key: &str) -> Option<Edit> {
    let name = doc.name(node);
    let kept = || {
        doc.attrs(node).filter(|(name, _)| {
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
    };
    let mut writer = XmlWriter::new();
    if name == "FunctionMode" {
        let attrs = [("mode", "FABRICATION"), ("sectionKey", section_key)]
            .into_iter()
            .chain(kept().filter(|(name, _)| !matches!(*name, "mode" | "sectionKey")))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<Vec<_>>();
        writer.empty_element_with("FunctionMode", attrs);
        return Some(doc.replace(node, writer.into_string()));
    }

    // These aliases are accepted by the internal parser, but IPC-2581C names
    // the schema values ROUT and V_CUT.
    let normalized = |attribute: &str, value: &str| match (name, attribute, value) {
        ("Layer", "layerFunction", "ROUTE") => "ROUT".to_string(),
        ("Layer", "layerFunction", "SCORE") => "V_CUT".to_string(),
        _ => value.to_string(),
    };
    let attrs = kept()
        .map(|(attribute, value)| (attribute.to_string(), normalized(attribute, value)))
        .collect::<Vec<_>>();
    if attrs
        .iter()
        .map(|(attribute, value)| (attribute.as_str(), value.as_str()))
        .eq(doc.attrs(node))
    {
        return None;
    }
    if doc.source(node).ends_with("/>") {
        writer.empty_element_with(name, attrs);
    } else {
        writer.start_element_with(name, attrs);
    }
    Some(doc.replace_start_tag(node, writer.into_string()))
}

/// The IPC-2581 fabrication `sectionKey` implied by the union of the given
/// documents' physical content. Element presence is monotone under document
/// composition, so a composed document's key is the union of its sources'.
pub(crate) fn fabrication_section_key_union(docs: &[Doc<'_>]) -> String {
    ordered_section_key(docs.iter().flat_map(|doc| {
        doc.root()
            .map(|root| descendants(doc, root))
            .unwrap_or_default()
            .into_iter()
            .flat_map(move |node| section_keys(doc, node))
    }))
}

fn section_key(doc: &Doc<'_>, elements: impl Iterator<Item = Node>) -> String {
    ordered_section_key(elements.flat_map(|node| section_keys(doc, node)))
}

fn ordered_section_key(keys: impl Iterator<Item = &'static char>) -> String {
    let keys = keys.collect::<HashSet<_>>();
    "KSUMLRDOIEFY"
        .chars()
        .filter(|key| keys.contains(key))
        .collect()
}

/// The section keys one element's presence implies.
fn section_keys(doc: &Doc<'_>, node: Node) -> &'static [char] {
    match doc.name(node) {
        "PadStackDef" => &['K'],
        "Stackup" => &['S'],
        "Profile" => &['U'],
        "PhyNetGroup" => &['Y'],
        "Layer" => match doc.attr(node, "layerFunction").unwrap_or_default() {
            "SOLDERMASK" => &['M'],
            "SILKSCREEN" | "LEGEND" => &['L'],
            "DRILL" | "ROUT" | "ROUTE" | "V_CUT" | "SCORE" => &['R'],
            "BOARD_OUTLINE" => &['D'],
            "EDGE_CHAMFER" => &['R', 'F'],
            "CONDUCTOR" | "CONDFILM" | "CONDFOIL" | "PLANE" | "SIGNAL" | "MIXED" => {
                match doc.attr(node, "side") {
                    Some("INTERNAL") => &['I'],
                    _ => &['O'],
                }
            }
            "DIELBASE" | "DIELCORE" | "DIELPREG" | "DIELADHV" | "DIELBONDPLY" | "DIELCOVERLAY" => {
                &['E']
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
            | "RESISTIVE" => &['F'],
            _ => &[],
        },
        _ => &[],
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
