use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use ipc2581::edit::{self, Doc, Edit, Node};
use ipc2581::types::Units;
use ipc2581::{Ipc2581, XmlWriter};

use super::{FabPanelSpec, SourcePanel};
use crate::commands::fab_panel::packing::Placement;
use crate::generated::{GeneratedLayerFeature, GeneratedNameState, write_generated_layer_feature};
use crate::steps::FAB_PANEL_STEP_NAME;

const FAB_ROLE_ID: &str = "fab_panel_role";
const FAB_ENTERPRISE_ID: &str = "fab_panel_enterprise";
const FAB_PERSON_NAME: &str = "pcb";

pub(super) fn namespace_source(
    xml: &str,
    prefix: &str,
    shared_stackup_layers: &HashSet<String>,
) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let mut edits = Vec::new();
    collect_namespace_edits(&doc, root, prefix, shared_stackup_layers, &mut edits);
    Ok(edit::apply(xml, edits)?)
}

fn collect_namespace_edits(
    doc: &Doc,
    node: Node,
    prefix: &str,
    shared_stackup_layers: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    let element = doc.name(node);
    let mut changed = false;
    let attrs = doc
        .attrs(node)
        .map(|(name, value)| {
            let value = if should_namespace_attr(element, name, value, shared_stackup_layers) {
                changed = true;
                format!("{prefix}{value}")
            } else {
                value.to_string()
            };
            (name.to_string(), value)
        })
        .collect::<Vec<_>>();

    if changed {
        let mut writer = XmlWriter::new();
        if doc.source(node).ends_with("/>") {
            writer.empty_element_with(element, attrs);
        } else {
            writer.start_element_with(element, attrs);
        }
        edits.push(doc.replace_start_tag(node, writer.into_string()));
    }

    for child in doc.children(node) {
        collect_namespace_edits(doc, child, prefix, shared_stackup_layers, edits);
    }
}

fn should_namespace_attr(
    element: &str,
    attr: &str,
    value: &str,
    shared_stackup_layers: &HashSet<String>,
) -> bool {
    let is_layer_reference = matches!(
        attr,
        "layerRef"
            | "layerRefTopside"
            | "secondaryLayerRef"
            | "fromLayer"
            | "toLayer"
            | "startCutLayer"
            | "layerId"
            | "layerOrGroupRef"
    ) || (attr == "name" && matches!(element, "Layer" | "LayerRef"));
    if is_layer_reference && shared_stackup_layers.contains(value) {
        return false;
    }

    if matches!(
        attr,
        "enterpriseRef"
            | "personRef"
            | "roleRef"
            | "stepRef"
            | "packageRef"
            | "layerRef"
            | "layerRefTopside"
            | "secondaryLayerRef"
            | "fromLayer"
            | "toLayer"
            | "startCutLayer"
            | "layerId"
            | "modelRef"
            | "componentRef"
            | "compRef"
            | "specRef"
            | "firmwareRef"
            | "padstackDefRef"
            | "netRef"
            | "stackupRef"
            | "bomRef"
            | "layerOrGroupRef"
            | "portName"
            | "matDes"
            | "refDes"
            | "net"
            | "netPair"
            | "part"
            | "OEMDesignNumber"
            | "OEMDesignNumberRef"
            | "dfxMeasurementRef"
    ) {
        return true;
    }

    match attr {
        "name" => matches!(
            element,
            "Step"
                | "StepRef"
                | "Package"
                | "Bom"
                | "BomRef"
                | "Person"
                | "Avl"
                | "AvlRef"
                | "Layer"
                | "LayerRef"
                | "RefDes"
                | "MatDes"
                | "Stackup"
                | "StackupGroup"
                | "Spec"
                | "Port"
                | "NetRef"
                | "LogicalNet"
                | "PhyNet"
                | "PhyNetGroup"
                | "PadStackDef"
                | "Model"
                | "SlotCavity"
                | "StackupZone"
        ),
        "id" => matches!(
            element,
            "Role"
                | "Enterprise"
                | "EntryColor"
                | "ColorRef"
                | "EntryStandard"
                | "StandardPrimitiveRef"
                | "EntryUser"
                | "UserPrimitiveRef"
                | "EntryFirmware"
                | "FirmwareRef"
                | "EntryFont"
                | "FontRef"
                | "EntryLineDesc"
                | "LineDescRef"
                | "EntryFillDesc"
                | "FillDescRef"
                | "SpecRef"
                | "SlotCavityRef"
                | "StackupZoneRef"
                | "DfxMeasurement"
        ),
        _ => false,
    }
}

/// Render the fabrication-panel document from already-stripped sources. The
/// provisional balancing pass consumes this directly; only the final document
/// pays for reformatting via [`write_fab_panel_xml`].
pub(super) fn render_fab_panel_xml(
    sources: &[SourcePanel],
    occurrences: &[usize],
    placements: &[Placement],
    shared_stackup_layers: &HashSet<String>,
    spec: FabPanelSpec,
) -> Result<String> {
    let docs = sources
        .iter()
        .map(|source| Doc::parse(&source.namespaced_xml))
        .collect::<ipc2581::Result<Vec<_>>>()?;
    let first = sources
        .first()
        .context("at least one assembly panel source is required")?;
    let mut writer = XmlWriter::new();
    writer.write_declaration();
    writer.start_element(
        "IPC-2581",
        &[
            ("revision", first.revision.as_str()),
            ("xmlns", "http://webstds.ipc.org/2581"),
        ],
    );
    write_content(&mut writer, &docs, shared_stackup_layers)?;
    write_logistic_header(&mut writer);
    write_history_record(&mut writer);
    write_ecad(
        &mut writer,
        sources,
        &docs,
        occurrences,
        placements,
        shared_stackup_layers,
        spec,
    )?;
    writer.end_element("IPC-2581");

    Ok(writer.into_string())
}

/// Finalize the provisional document: splice the balance layer features into
/// the fabrication-panel step, then reformat and re-parse. Splicing avoids a
/// second full composition of the source documents.
pub(super) fn write_fab_panel_xml(
    provisional: &str,
    units: Units,
    balance_features: &[GeneratedLayerFeature],
    templates: &[crate::copper_balance::BalanceVoidTemplate],
) -> Result<String> {
    let mut features_xml = XmlWriter::new();
    let mut names = GeneratedNameState::default();
    for layer_feature in balance_features {
        write_generated_layer_feature(&mut features_xml, units, layer_feature, &mut names)?;
    }
    let features_xml = features_xml.into_string();

    let xml = if features_xml.is_empty() && templates.is_empty() {
        provisional.to_string()
    } else {
        let doc = Doc::parse(provisional)?;
        let mut edits = Vec::new();
        if !features_xml.is_empty() {
            let fab_step = doc
                .find_all("Step")
                .into_iter()
                .find(|step| doc.attr(*step, "name") == Some(FAB_PANEL_STEP_NAME))
                .context("provisional fabrication panel has no fab step")?;
            edits.push(doc.append_inside(fab_step, features_xml));
        }
        edits.extend(crate::generated::user_dictionary_edit(
            &doc, units, templates,
        )?);
        edit::apply(provisional, edits)?
    };
    let xml = crate::utils::format::reformat_xml(&xml)?;
    Ipc2581::validate(&xml)
        .context("Generated IPC-2581 fabrication panel XML failed schema validation")?;
    Ipc2581::parse(&xml).context("Generated IPC-2581 fabrication panel XML did not parse")?;
    Ok(xml)
}

fn write_content(
    writer: &mut XmlWriter,
    docs: &[Doc<'_>],
    shared_stackup_layers: &HashSet<String>,
) -> Result<()> {
    let section_key = super::super::fabrication::fabrication_section_key_union(docs);
    writer.start_element("Content", &[("roleRef", FAB_ROLE_ID)]);
    writer.empty_element(
        "FunctionMode",
        &[
            ("mode", "FABRICATION"),
            ("sectionKey", section_key.as_str()),
        ],
    );
    writer.empty_element("StepRef", &[("name", FAB_PANEL_STEP_NAME)]);

    for (doc_index, doc) in docs.iter().enumerate() {
        let cad_data = cad_data(doc)?;
        for layer in children_named(doc, cad_data, "Layer") {
            let name = doc
                .attr(layer, "name")
                .context("IPC-2581 Layer has no name")?;
            if doc_index > 0 && shared_stackup_layers.contains(name) {
                continue;
            }
            writer.empty_element("LayerRef", &[("name", name)]);
        }
    }
    for dictionary in [
        "DictionaryColor",
        "DictionaryLineDesc",
        "DictionaryFillDesc",
        "DictionaryFont",
        "DictionaryStandard",
        "DictionaryUser",
        "DictionaryFirmware",
    ] {
        write_merged_container(writer, docs, dictionary)?;
    }
    writer.end_element("Content");
    Ok(())
}

fn write_merged_container(
    writer: &mut XmlWriter,
    docs: &[Doc<'_>],
    element_name: &str,
) -> Result<()> {
    let mut attrs: Option<Vec<(String, String)>> = None;
    let mut children = Vec::new();
    for doc in docs {
        let root = doc.root()?;
        let Some(content) = doc.child(root, "Content") else {
            continue;
        };
        let Some(container) = doc.child(content, element_name) else {
            continue;
        };
        let container_attrs = doc
            .attrs(container)
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<Vec<_>>();
        match &attrs {
            Some(first_attrs) if first_attrs != &container_attrs => {
                bail!("assembly panel inputs use incompatible {element_name} attributes")
            }
            None => attrs = Some(container_attrs),
            _ => {}
        }
        children.extend(
            doc.children(container)
                .into_iter()
                .map(|child| doc.source(child).to_string()),
        );
    }
    if children.is_empty() {
        return Ok(());
    }

    writer.start_element_with(element_name, attrs.unwrap_or_default());
    for child in children {
        writer.raw(&child);
    }
    writer.end_element(element_name);
    Ok(())
}

fn write_logistic_header(writer: &mut XmlWriter) {
    writer.start_element("LogisticHeader", &[]);
    writer.empty_element("Role", &[("id", FAB_ROLE_ID), ("roleFunction", "DESIGNER")]);

    writer.empty_element(
        "Enterprise",
        &[
            ("id", FAB_ENTERPRISE_ID),
            ("name", "Diode"),
            ("code", "DIODE"),
        ],
    );

    writer.empty_element(
        "Person",
        &[
            ("name", FAB_PERSON_NAME),
            ("enterpriseRef", FAB_ENTERPRISE_ID),
            ("roleRef", FAB_ROLE_ID),
        ],
    );
    writer.end_element("LogisticHeader");
}

fn write_history_record(writer: &mut XmlWriter) {
    let now = jiff::Timestamp::now().to_string();
    writer.start_element(
        "HistoryRecord",
        &[
            ("number", "1"),
            ("origination", now.as_str()),
            ("software", "pcb"),
            ("lastChange", now.as_str()),
        ],
    );
    writer.start_element(
        "FileRevision",
        &[
            ("fileRevisionId", "1"),
            ("comment", "Created fabrication panel"),
            ("label", ""),
        ],
    );
    writer.start_element(
        "SoftwarePackage",
        &[
            ("name", "pcb"),
            ("vendor", "Diode"),
            ("revision", env!("CARGO_PKG_VERSION")),
        ],
    );
    writer.empty_element("Certification", &[("certificationStatus", "SELFTEST")]);
    writer.end_element("SoftwarePackage");
    writer.end_element("FileRevision");
    writer.end_element("HistoryRecord");
}

fn write_ecad(
    writer: &mut XmlWriter,
    sources: &[SourcePanel],
    docs: &[Doc<'_>],
    occurrences: &[usize],
    placements: &[Placement],
    shared_stackup_layers: &HashSet<String>,
    spec: FabPanelSpec,
) -> Result<()> {
    let units = sources
        .first()
        .context("at least one assembly panel source is required")?
        .units;
    writer.start_element("Ecad", &[("name", FAB_PANEL_STEP_NAME)]);
    writer.start_element("CadHeader", &[("units", units_attr(units))]);
    for doc in docs {
        let cad_header = cad_header(doc)?;
        for spec in children_named(doc, cad_header, "Spec") {
            writer.raw(doc.source(spec));
        }
    }
    writer.end_element("CadHeader");

    writer.start_element("CadData", &[]);
    for (doc_index, doc) in docs.iter().enumerate() {
        let cad_data = cad_data(doc)?;
        for layer in children_named(doc, cad_data, "Layer") {
            let name = doc
                .attr(layer, "name")
                .context("IPC-2581 Layer has no name")?;
            if doc_index > 0 && shared_stackup_layers.contains(name) {
                continue;
            }
            writer.raw(doc.source(layer));
        }
    }
    let first_doc = docs
        .first()
        .context("at least one assembly panel source is required")?;
    let first_cad_data = cad_data(first_doc)?;
    for stackup in children_named(first_doc, first_cad_data, "Stackup") {
        writer.raw(first_doc.source(stackup));
    }
    for doc in docs {
        let cad_data = cad_data(doc)?;
        for step in children_named(doc, cad_data, "Step") {
            writer.raw(doc.source(step));
        }
    }
    write_fab_step(writer, sources, occurrences, placements, units, spec)?;
    writer.end_element("CadData");
    writer.end_element("Ecad");
    Ok(())
}

fn write_fab_step(
    writer: &mut XmlWriter,
    sources: &[SourcePanel],
    occurrences: &[usize],
    placements: &[Placement],
    units: Units,
    spec: FabPanelSpec,
) -> Result<()> {
    let usable = spec.usable_bbox()?;
    writer.start_element("Step", &[("name", FAB_PANEL_STEP_NAME), ("type", "PALLET")]);
    write_metadata(writer, "diode.fab_panel.schema_version", "INTEGER", "2");
    write_metadata(
        writer,
        "diode.fab_panel.width_mm",
        "DOUBLE",
        &ipc2581::write::fmt_num(spec.width_mm()),
    );
    write_metadata(
        writer,
        "diode.fab_panel.height_mm",
        "DOUBLE",
        &ipc2581::write::fmt_num(spec.height_mm()),
    );
    write_metadata(
        writer,
        "diode.fab_panel.usable_width_mm",
        "DOUBLE",
        &ipc2581::write::fmt_num(usable.width()),
    );
    write_metadata(
        writer,
        "diode.fab_panel.usable_height_mm",
        "DOUBLE",
        &ipc2581::write::fmt_num(usable.height()),
    );
    for (name, value) in [
        (
            "diode.fab_panel.edge_margin_top_mm",
            spec.edge_margin_mm.top,
        ),
        (
            "diode.fab_panel.edge_margin_right_mm",
            spec.edge_margin_mm.right,
        ),
        (
            "diode.fab_panel.edge_margin_bottom_mm",
            spec.edge_margin_mm.bottom,
        ),
        (
            "diode.fab_panel.edge_margin_left_mm",
            spec.edge_margin_mm.left,
        ),
    ] {
        write_metadata(writer, name, "DOUBLE", &ipc2581::write::fmt_num(value));
    }
    write_metadata(
        writer,
        "diode.fab_panel.gap_mm",
        "DOUBLE",
        &ipc2581::write::fmt_num(spec.panel_gap_mm),
    );
    write_metadata(
        writer,
        "diode.fab_panel.panel_count",
        "INTEGER",
        &occurrences.len().to_string(),
    );

    ipc2581::write::location(writer, "Datum", 0.0, 0.0, units);
    writer.start_element("Profile", &[]);
    writer.start_element("Polygon", &[]);
    ipc2581::write::location(writer, "PolyBegin", 0.0, 0.0, units);
    ipc2581::write::location(writer, "PolyStepSegment", spec.width_mm(), 0.0, units);
    ipc2581::write::location(
        writer,
        "PolyStepSegment",
        spec.width_mm(),
        spec.height_mm(),
        units,
    );
    ipc2581::write::location(writer, "PolyStepSegment", 0.0, spec.height_mm(), units);
    writer.end_element("Polygon");
    writer.end_element("Profile");

    for placement in placements {
        let source_index = occurrences[placement.item_index];
        let source = &sources[source_index];
        let target_x_mm = usable.min.x + f64::from(placement.x) / 1_000.0;
        let target_y_mm = usable.min.y + f64::from(placement.y) / 1_000.0;
        let (x_mm, y_mm, angle) = if placement.rotated {
            (
                target_x_mm + source.bbox.max.y,
                target_y_mm - source.bbox.min.x,
                "90",
            )
        } else {
            (
                target_x_mm - source.bbox.min.x,
                target_y_mm - source.bbox.min.y,
                "0",
            )
        };
        let x = ipc2581::write::fmt_units(x_mm, units);
        let y = ipc2581::write::fmt_units(y_mm, units);
        writer.empty_element(
            "StepRepeat",
            &[
                ("stepRef", source.root_step_name.as_str()),
                ("x", x.as_str()),
                ("y", y.as_str()),
                ("nx", "1"),
                ("ny", "1"),
                ("dx", "0"),
                ("dy", "0"),
                ("angle", angle),
                ("mirror", "false"),
            ],
        );
    }
    writer.end_element("Step");
    Ok(())
}

fn write_metadata(writer: &mut XmlWriter, name: &str, property_type: &str, value: &str) {
    writer.empty_element(
        "NonstandardAttribute",
        &[("name", name), ("type", property_type), ("value", value)],
    );
}

fn cad_header<'a>(doc: &'a Doc<'a>) -> Result<Node> {
    let root = doc.root()?;
    let ecad = doc
        .child(root, "Ecad")
        .context("assembly panel IPC-2581 has no Ecad element")?;
    doc.child(ecad, "CadHeader")
        .context("assembly panel IPC-2581 has no CadHeader element")
}

fn cad_data<'a>(doc: &'a Doc<'a>) -> Result<Node> {
    let root = doc.root()?;
    let ecad = doc
        .child(root, "Ecad")
        .context("assembly panel IPC-2581 has no Ecad element")?;
    doc.child(ecad, "CadData")
        .context("assembly panel IPC-2581 has no CadData element")
}

fn children_named<'a>(doc: &'a Doc<'a>, parent: Node, name: &'a str) -> Vec<Node> {
    doc.children(parent)
        .into_iter()
        .filter(|child| doc.name(*child) == name)
        .collect()
}

fn units_attr(units: Units) -> &'static str {
    match units {
        Units::Millimeter => "MILLIMETER",
        Units::Inch => "INCH",
        Units::Micron => "MICRON",
        Units::Mils => "MILS",
    }
}
