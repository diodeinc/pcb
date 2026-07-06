//! IPC-2581 XML patching and generated-element serialization.
//!
//! Generated fragments (specs, layers, steps) are serialized with
//! [`ipc2581::write`] and spliced into the source document as byte-range
//! edits via [`ipc2581::edit`], leaving the rest of the file untouched.

use super::*;
use ipc2581::XmlWriter;
use ipc2581::edit::{Doc, Edit};
use ipc2581::write;
use ipc2581::write::{fmt_num, fmt_units};

/// The board-array changes as byte-range edits against the source document:
/// Content step/layer refs, generated CadHeader specs, generated layers,
/// board-outline removal, and the generated board-cell/array steps.
pub(super) fn board_array_edits(
    doc: &Doc,
    spec: &BoardArraySpec,
    generated_spec_xml: &str,
    generated_layer_xml: Option<&str>,
    array_step_xml: &str,
) -> Result<Vec<Edit>> {
    let root = doc.root()?;
    let mut edits = Vec::new();

    // Content: drop existing StepRef/LayerRef entries and write the array's
    // refs right after FunctionMode (or at the end of Content).
    if let Some(content) = doc.child(root, "Content") {
        let refs_xml = write_content_refs_xml(spec);
        let mut function_mode = None;
        for child in doc.children(content) {
            match doc.name(child) {
                "StepRef" | "LayerRef" => edits.push(doc.delete(child)),
                "FunctionMode" if function_mode.is_none() => function_mode = Some(child),
                _ => {}
            }
        }
        match function_mode {
            Some(anchor) => edits.push(doc.insert_after(anchor, refs_xml)),
            None => edits.push(doc.append_inside(content, refs_xml)),
        }
    }

    let ecad = doc
        .child(root, "Ecad")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 file has no CadHeader section"))?;

    let cad_header = doc
        .child(ecad, "CadHeader")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 file has no CadHeader section"))?;
    edits.push(doc.append_inside(cad_header, generated_spec_xml));

    let cad_data = doc
        .child(ecad, "CadData")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 file has no CadData section"))?;
    let children = doc.children(cad_data);

    // Generated layers join the end of the leading Layer block.
    if let Some(layer_xml) = generated_layer_xml {
        match children.iter().find(|&&child| doc.name(child) != "Layer") {
            Some(&first_non_layer) => edits.push(doc.insert_before(first_non_layer, layer_xml)),
            None => edits.push(doc.append_inside(cad_data, layer_xml)),
        }
    }

    let is_outline = |name: Option<&str>| {
        name.is_some_and(|name| spec.board_outline_layer_names.iter().any(|n| n == name))
    };
    for &child in &children {
        // The array re-expresses the board outline, so the source outline
        // layer and its features are removed.
        if doc.name(child) == "Layer" && is_outline(doc.attr(child, "name")) {
            edits.push(doc.delete(child));
        }
        if doc.name(child) == "Step" && doc.attr(child, "name") == Some(spec.board_name.as_str()) {
            for feature in doc.children(child) {
                if doc.name(feature) == "LayerFeature" && is_outline(doc.attr(feature, "layerRef"))
                {
                    edits.push(doc.delete(feature));
                }
            }
        }
    }

    edits.push(doc.append_inside(cad_data, array_step_xml));

    Ok(edits)
}

fn write_content_refs_xml(spec: &BoardArraySpec) -> String {
    let mut writer = XmlWriter::new();
    for step_ref in &spec.content_step_refs {
        write::step_ref(&mut writer, step_ref);
    }
    for layer_ref in &spec.content_layer_refs {
        write::layer_ref(&mut writer, layer_ref);
    }
    writer.into_string()
}

pub(super) fn write_generated_specs_xml(spec: &BoardArraySpec) -> String {
    let mut writer = XmlWriter::new();
    writer.start_element("Spec", &[("name", spec.vcut_spec_name.as_str())]);
    writer.start_element("V_Cut", &[("type", "OFFSET")]);
    writer.empty_element("Property", &[("value", "0"), ("unit", "MM")]);
    writer.end_element("V_Cut");
    writer.end_element("Spec");
    writer.into_string()
}

pub(super) fn write_generated_layers_xml(geometry: &BoardArrayGeneratedGeometry) -> Option<String> {
    if geometry.layers.is_empty() {
        return None;
    }

    let mut writer = XmlWriter::new();
    for generated_layer in &geometry.layers {
        write_generated_layer_xml(&mut writer, generated_layer);
    }
    Some(writer.into_string())
}

pub(super) fn write_generated_layer_xml(writer: &mut XmlWriter, generated_layer: &GeneratedLayer) {
    let mut attrs = vec![
        ("name", generated_layer.name.as_str()),
        ("layerFunction", generated_layer.layer_function.as_str()),
    ];
    if let Some(side) = generated_layer.side {
        attrs.push(("side", write::side_attr(side)));
    }
    if let Some(polarity) = generated_layer.polarity {
        attrs.push(("polarity", write::polarity_attr(polarity)));
    }
    writer.empty_element("Layer", &attrs);
}

pub(super) fn write_generated_steps_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut xml = write_board_cell_step_xml(spec)?;
    xml.push_str(&write_array_step_xml(spec)?);
    Ok(xml)
}

pub(super) fn write_board_cell_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = XmlWriter::new();

    writer.start_element(
        "Step",
        &[("name", spec.board_cell_name.as_str()), ("type", "PALLET")],
    );

    write::location(&mut writer, "Datum", 0.0, 0.0, spec.units);
    write::profile(
        &mut writer,
        spec.units,
        &rectangle_polygon(spec.pitch_x_mm, spec.pitch_y_mm),
    );
    write_board_cell_step_repeat(&mut writer, spec);
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::BoardCell)?;

    writer.end_element("Step");

    Ok(writer.into_string())
}

pub(super) fn write_array_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = XmlWriter::new();

    writer.start_element(
        "Step",
        &[("name", spec.array_name.as_str()), ("type", "PALLET")],
    );

    write_panelization_metadata(&mut writer, spec);
    write::location(&mut writer, "Datum", 0.0, 0.0, spec.units);

    write::profile(
        &mut writer,
        spec.units,
        &rounded_rectangle_polygon(
            spec.array_width_mm,
            spec.array_height_mm,
            ARRAY_CORNER_RADIUS_MM,
        ),
    );

    write_array_step_repeat(&mut writer, spec);
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::Array)?;

    writer.end_element("Step");

    Ok(writer.into_string())
}

pub(super) fn write_panelization_metadata(writer: &mut XmlWriter, spec: &BoardArraySpec) {
    let metadata = spec.panelization;

    write_metadata_integer(writer, "diode.panelize.schema_version", 1);
    write_metadata_string(writer, "diode.panelize.mode", metadata.mode.as_str());
    if let Some(sheet) = metadata.sheet {
        write_metadata_string(writer, "diode.panelize.sheet", sheet.name());
    }
    if let Some(target) = metadata.sheet_target_mm {
        write_metadata_double(writer, "diode.panelize.sheet_width_mm", target.width);
        write_metadata_double(writer, "diode.panelize.sheet_height_mm", target.height);
    }

    write_metadata_integer(writer, "diode.panelize.columns", spec.columns);
    write_metadata_integer(writer, "diode.panelize.rows", spec.rows);
    write_margin_metadata(writer, "diode.panelize.board_margin", spec.board_margin_mm);
    write_margin_metadata(writer, "diode.panelize.edge_rail", spec.edge_rail_mm);
}

pub(super) fn write_margin_metadata(writer: &mut XmlWriter, prefix: &str, margin: BoardMarginMm) {
    write_metadata_double(writer, &format!("{prefix}_top_mm"), margin.top);
    write_metadata_double(writer, &format!("{prefix}_right_mm"), margin.right);
    write_metadata_double(writer, &format!("{prefix}_bottom_mm"), margin.bottom);
    write_metadata_double(writer, &format!("{prefix}_left_mm"), margin.left);
}

pub(super) fn write_metadata_integer(writer: &mut XmlWriter, name: &str, value: u32) {
    write_metadata_attribute(writer, name, "INTEGER", &value.to_string());
}

pub(super) fn write_metadata_double(writer: &mut XmlWriter, name: &str, value: f64) {
    write_metadata_attribute(writer, name, "DOUBLE", &fmt_num(value));
}

pub(super) fn write_metadata_string(writer: &mut XmlWriter, name: &str, value: &str) {
    write_metadata_attribute(writer, name, "STRING", value);
}

pub(super) fn write_metadata_attribute(
    writer: &mut XmlWriter,
    name: &str,
    property_type: &str,
    value: &str,
) {
    writer.empty_element(
        "NonstandardAttribute",
        &[("name", name), ("type", property_type), ("value", value)],
    );
}

pub(super) fn write_generated_layer_features(
    writer: &mut XmlWriter,
    spec: &BoardArraySpec,
    scope: GeneratedFeatureScope,
) -> Result<()> {
    let mut names = GeneratedNameState::default();
    for layer_feature in spec
        .generated_geometry
        .layer_features
        .iter()
        .filter(|layer_feature| layer_feature.scope == scope)
    {
        write_generated_layer_feature(writer, spec.units, layer_feature, &mut names)?;
    }
    Ok(())
}

pub(super) fn write_generated_layer_feature(
    writer: &mut XmlWriter,
    units: Units,
    layer_feature: &GeneratedLayerFeature,
    names: &mut GeneratedNameState,
) -> Result<()> {
    if layer_feature.features.is_empty() {
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
    writer.end_element("Set");
    writer.end_element("LayerFeature");
    Ok(())
}

/// Sequential names for generated holes, unique within one Step.
#[derive(Debug, Default)]
pub(super) struct GeneratedNameState {
    hole_index: usize,
}

impl GeneratedNameState {
    fn next_hole_name(&mut self) -> String {
        let name = format!("{GENERATED_HOLE_NAME_PREFIX}_{}", self.hole_index);
        self.hole_index += 1;
        name
    }
}

pub(super) fn write_set_features(
    writer: &mut XmlWriter,
    units: Units,
    features: &[SetFeature],
    names: &mut GeneratedNameState,
) -> Result<()> {
    let mut features_open = false;
    for feature in features {
        match feature {
            SetFeature::Line(line) => {
                if !features_open {
                    writer.start_element("Features", &[]);
                    features_open = true;
                }
                write::line(writer, units, line)?;
            }
            SetFeature::Fiducial(fiducial) => {
                close_features_element(writer, &mut features_open);
                write::fiducial(writer, units, fiducial)?;
            }
            SetFeature::Hole(hole) => {
                close_features_element(writer, &mut features_open);
                write::hole(writer, units, hole, &names.next_hole_name());
            }
            _ => bail!("generated board array layer feature has unsupported feature kind"),
        }
    }
    close_features_element(writer, &mut features_open);
    Ok(())
}

pub(super) fn close_features_element(writer: &mut XmlWriter, features_open: &mut bool) {
    if *features_open {
        writer.end_element("Features");
        *features_open = false;
    }
}

pub(super) fn rectangle_polygon(width_mm: f64, height_mm: f64) -> Polygon {
    Polygon {
        begin: IpcPoint { x: 0.0, y: 0.0 },
        steps: vec![
            poly_segment(width_mm, 0.0),
            poly_segment(width_mm, height_mm),
            poly_segment(0.0, height_mm),
        ],
    }
}

pub(super) fn rounded_rectangle_polygon(width_mm: f64, height_mm: f64, radius_mm: f64) -> Polygon {
    let radius = radius_mm.min(width_mm / 2.0).min(height_mm / 2.0);
    let begin = IpcPoint { x: 0.0, y: radius };
    Polygon {
        begin,
        steps: vec![
            poly_segment(0.0, height_mm - radius),
            poly_curve(radius, height_mm, radius, height_mm - radius),
            poly_segment(width_mm - radius, height_mm),
            poly_curve(
                width_mm,
                height_mm - radius,
                width_mm - radius,
                height_mm - radius,
            ),
            poly_segment(width_mm, radius),
            poly_curve(width_mm - radius, 0.0, width_mm - radius, radius),
            poly_segment(radius, 0.0),
            poly_curve(0.0, radius, radius, radius),
        ],
    }
}

pub(super) fn poly_segment(x: f64, y: f64) -> PolyStep {
    PolyStep::Segment(PolyStepSegment {
        point: IpcPoint { x, y },
    })
}

pub(super) fn poly_curve(x: f64, y: f64, center_x: f64, center_y: f64) -> PolyStep {
    PolyStep::Curve(PolyStepCurve {
        point: IpcPoint { x, y },
        center: IpcPoint {
            x: center_x,
            y: center_y,
        },
        clockwise: true,
    })
}

pub(super) fn round_fiducial(
    kind: IpcFiducialKind,
    x_mm: f64,
    y_mm: f64,
    diameter_mm: f64,
) -> Fiducial {
    Fiducial {
        kind,
        location: Location { x: x_mm, y: y_mm },
        xform: None,
        shape: FiducialShape::Primitive(StandardPrimitive::Circle(Styled {
            shape: Circle {
                diameter: diameter_mm,
            },
            fill_property: None,
            line_desc_ref: None,
        })),
        pin_ref: None,
    }
}

pub(super) fn round_fiducial_features(
    kind: IpcFiducialKind,
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    points
        .into_iter()
        .map(|(x, y)| SetFeature::Fiducial(round_fiducial(kind, x, y, diameter_mm)))
        .collect()
}

pub(super) fn round_nonplated_hole(x_mm: f64, y_mm: f64, diameter_mm: f64) -> Hole {
    Hole {
        name: None,
        diameter: diameter_mm,
        plating_status: PlatingStatus::NonPlated,
        x: x_mm,
        y: y_mm,
    }
}

pub(super) fn round_nonplated_hole_features(
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    points
        .into_iter()
        .map(|(x, y)| SetFeature::Hole(round_nonplated_hole(x, y, diameter_mm)))
        .collect()
}

pub(super) fn write_array_step_repeat(writer: &mut XmlWriter, spec: &BoardArraySpec) {
    writer.empty_element(
        "StepRepeat",
        &[
            ("stepRef", spec.board_cell_name.as_str()),
            ("x", fmt_units(spec.array_repeat_x_mm, spec.units).as_str()),
            ("y", fmt_units(spec.array_repeat_y_mm, spec.units).as_str()),
            ("nx", spec.columns.to_string().as_str()),
            ("ny", spec.rows.to_string().as_str()),
            ("dx", fmt_units(spec.pitch_x_mm, spec.units).as_str()),
            ("dy", fmt_units(spec.pitch_y_mm, spec.units).as_str()),
            ("angle", "0.00"),
            ("mirror", "false"),
        ],
    );
}

pub(super) fn write_board_cell_step_repeat(writer: &mut XmlWriter, spec: &BoardArraySpec) {
    writer.empty_element(
        "StepRepeat",
        &[
            ("stepRef", spec.board_name.as_str()),
            ("x", fmt_units(spec.board_repeat_x_mm, spec.units).as_str()),
            ("y", fmt_units(spec.board_repeat_y_mm, spec.units).as_str()),
            ("nx", "1"),
            ("ny", "1"),
            ("dx", "0"),
            ("dy", "0"),
            ("angle", "0.00"),
            ("mirror", "false"),
        ],
    );
}
