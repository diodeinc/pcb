//! Serialization of generated IPC-2581 layer features, shared by the
//! board-array and fab-panel panelizers.

use anyhow::{Result, bail};
use ipc2581::XmlWriter;
use ipc2581::types::{
    Units,
    ecad::{FeatureUserPrimitive, Polarity, SetFeature},
    primitives::{Contour, UserPrimitive, UserShapeType},
};
use ipc2581::write;

use crate::copper_balance::{BalanceVoidInstance, BalanceVoidTemplate};

/// Only board-array tooling generates named holes today; the prefix stays
/// stable so re-panelization produces identical names.
const GENERATED_HOLE_NAME_PREFIX: &str = "array_tooling_hole";

/// One generated `LayerFeature` element: a single `Set` of features on one
/// layer.
#[derive(Debug, Clone)]
pub(crate) struct GeneratedLayerFeature {
    pub layer_name: String,
    pub polarity: Polarity,
    pub spec_refs: Vec<String>,
    pub features: Vec<SetFeature>,
    /// Flashed dictionary-instance references, emitted after `features`.
    pub instance_refs: Vec<BalanceVoidInstance>,
}

/// Sequential names for generated holes, unique within one Step.
#[derive(Debug, Default)]
pub(crate) struct GeneratedNameState {
    hole_index: usize,
}

impl GeneratedNameState {
    fn next_hole_name(&mut self) -> String {
        let name = format!("{GENERATED_HOLE_NAME_PREFIX}_{}", self.hole_index);
        self.hole_index += 1;
        name
    }
}

pub(crate) fn write_generated_layer_feature(
    writer: &mut XmlWriter,
    units: Units,
    layer_feature: &GeneratedLayerFeature,
    names: &mut GeneratedNameState,
) -> Result<()> {
    if layer_feature.features.is_empty() && layer_feature.instance_refs.is_empty() {
        return Ok(());
    }

    writer.start_element(
        "LayerFeature",
        &[("layerRef", layer_feature.layer_name.as_str())],
    );
    writer.start_element(
        "Set",
        &[("polarity", write::polarity_attr(layer_feature.polarity))],
    );
    for spec_ref in &layer_feature.spec_refs {
        write::spec_ref(writer, spec_ref);
    }
    write_set_features(writer, units, &layer_feature.features, names)?;
    for instance in &layer_feature.instance_refs {
        writer.start_element("Features", &[]);
        write::location(writer, "Location", instance.x, instance.y, units);
        writer.empty_element("UserPrimitiveRef", &[("id", instance.template.as_str())]);
        writer.end_element("Features");
    }
    writer.end_element("Set");
    writer.end_element("LayerFeature");
    Ok(())
}

fn write_set_features(
    writer: &mut XmlWriter,
    units: Units,
    features: &[SetFeature],
    names: &mut GeneratedNameState,
) -> Result<()> {
    for feature in features {
        match feature {
            SetFeature::Line(line) => {
                writer.start_element("Features", &[]);
                write::line(writer, units, line)?;
                writer.end_element("Features");
            }
            SetFeature::Polygon(polygon) => {
                writer.start_element("Features", &[]);
                writer.start_element("Contour", &[]);
                write::polygon(writer, units, polygon);
                writer.end_element("Contour");
                writer.end_element("Features");
            }
            SetFeature::UserPrimitive(feature) => {
                write_generated_user_primitive(writer, units, feature)?;
            }
            SetFeature::Fiducial(fiducial) => {
                write::fiducial(writer, units, fiducial)?;
            }
            SetFeature::Hole(hole) => {
                write::hole(writer, units, hole, &names.next_hole_name());
            }
            _ => bail!("generated layer feature has unsupported feature kind"),
        }
    }
    Ok(())
}

/// Splice shared void templates into the document's `DictionaryUser`,
/// creating the dictionary in schema position when absent.
pub(crate) fn user_dictionary_edit(
    doc: &ipc2581::edit::Doc<'_>,
    units: Units,
    templates: &[BalanceVoidTemplate],
) -> Result<Option<ipc2581::edit::Edit>> {
    if templates.is_empty() {
        return Ok(None);
    }
    let root = doc.root()?;
    let content = doc
        .child(root, "Content")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 document has no Content section"))?;
    if let Some(dictionary) = doc.child(content, "DictionaryUser") {
        // An existing dictionary's declared units govern its entries; the
        // parser defaults an absent attribute to millimeters.
        let dictionary_units = match doc.attr(dictionary, "units") {
            Some("INCH") => Units::Inch,
            Some("MICRON") => Units::Micron,
            Some("MILS") => Units::Mils,
            _ => Units::Millimeter,
        };
        let mut writer = XmlWriter::new();
        write_user_dictionary_entries(&mut writer, dictionary_units, templates);
        return Ok(Some(doc.append_inside(dictionary, writer.into_string())));
    }
    let mut writer = XmlWriter::new();
    write_user_dictionary_entries(&mut writer, units, templates);
    let entries = writer.into_string();
    let mut container = XmlWriter::new();
    container.start_element("DictionaryUser", &[("units", units_attr(units))]);
    container.raw(&entries);
    container.end_element("DictionaryUser");
    let xml = container.into_string();
    Ok(Some(
        match doc
            .children(content)
            .into_iter()
            .find(|child| doc.name(*child) == "DictionaryFirmware")
        {
            Some(firmware) => doc.insert_before(firmware, xml),
            None => doc.append_inside(content, xml),
        },
    ))
}

fn units_attr(units: Units) -> &'static str {
    match units {
        Units::Millimeter => "MILLIMETER",
        Units::Inch => "INCH",
        Units::Micron => "MICRON",
        Units::Mils => "MILS",
    }
}

/// Serialize shared void templates as `DictionaryUser` entries.
fn write_user_dictionary_entries(
    writer: &mut XmlWriter,
    units: Units,
    templates: &[BalanceVoidTemplate],
) {
    for template in templates {
        writer.start_element("EntryUser", &[("id", template.id.as_str())]);
        writer.start_element("UserSpecial", &[]);
        write::contour(
            writer,
            units,
            &Contour {
                polygon: template.polygon.clone(),
                cutouts: Vec::new(),
            },
        );
        writer.end_element("UserSpecial");
        writer.end_element("EntryUser");
    }
}

fn write_generated_user_primitive(
    writer: &mut XmlWriter,
    units: Units,
    feature: &FeatureUserPrimitive,
) -> Result<()> {
    let UserPrimitive::UserSpecial(user_special) = &feature.primitive;
    let [shape] = user_special.shapes.as_slice() else {
        bail!("generated user primitive must contain exactly one shape");
    };
    let UserShapeType::Contour(contour) = &shape.shape else {
        bail!("generated user primitive must be a contour");
    };
    if shape.line_desc.is_some() || shape.line_desc_ref.is_some() || shape.fill_desc.is_some() {
        bail!("generated contour cannot carry explicit style descriptors");
    }

    writer.start_element("Features", &[]);
    if feature.x != 0.0 || feature.y != 0.0 {
        write::location(writer, "Location", feature.x, feature.y, units);
    }
    write::contour(writer, units, contour);
    writer.end_element("Features");
    Ok(())
}
