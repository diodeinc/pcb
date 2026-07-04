//! IPC-2581 XML patching and generated-element serialization.
//!
//! Generated fragments (specs, layers, steps) are serialized with
//! [`ipc2581::write`] and spliced into the source document as byte-range
//! edits via [`ipc2581::edit`], leaving the rest of the file untouched.

use super::*;
use ipc2581::edit::{self, Doc};
use ipc2581::write;
use ipc2581::write::fmt_units;

/// Splice all board-array changes into the source document in one pass:
/// Content step/layer refs, generated CadHeader specs, generated layers,
/// board-outline removal, and the generated board-cell/array steps.
pub(super) fn patch_board_xml(
    xml: &str,
    spec: &BoardArraySpec,
    generated_spec_xml: &str,
    generated_layer_xml: Option<&str>,
    array_step_xml: &str,
) -> Result<String> {
    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let mut edits = Vec::new();

    // Content: drop existing StepRef/LayerRef entries and write the array's
    // refs right after FunctionMode (or at the end of Content).
    if let Some(content) = doc.child(root, "Content") {
        let refs_xml = write_content_refs_xml(spec)?;
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

    Ok(edit::apply(xml, edits)?)
}

fn write_content_refs_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    for step_ref in &spec.content_step_refs {
        write::step_ref(&mut writer, step_ref)?;
    }
    for layer_ref in &spec.content_layer_refs {
        write::layer_ref(&mut writer, layer_ref)?;
    }
    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn write_generated_specs_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut spec_elem = BytesStart::new("Spec");
    spec_elem.push_attribute(("name", spec.vcut_spec_name.as_str()));
    writer.write_event(Event::Start(spec_elem))?;

    let mut vcut = BytesStart::new("V_Cut");
    vcut.push_attribute(("type", "OFFSET"));
    writer.write_event(Event::Start(vcut))?;

    let mut property = BytesStart::new("Property");
    property.push_attribute(("value", "0"));
    property.push_attribute(("unit", "MM"));
    writer.write_event(Event::Empty(property))?;

    writer.write_event(Event::End(BytesStart::new("V_Cut").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("Spec").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn write_generated_layers_xml(
    geometry: &BoardArrayGeneratedGeometry,
) -> Result<Option<String>> {
    if geometry.layers.is_empty() {
        return Ok(None);
    }

    let mut writer = Writer::new(Cursor::new(Vec::new()));
    for generated_layer in &geometry.layers {
        write_generated_layer_xml(&mut writer, generated_layer)?;
    }
    Ok(Some(String::from_utf8(writer.into_inner().into_inner())?))
}

pub(super) fn write_generated_layer_xml(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    generated_layer: &GeneratedLayer,
) -> Result<()> {
    let mut layer = BytesStart::new("Layer");
    layer.push_attribute(("name", generated_layer.name.as_str()));
    layer.push_attribute(("layerFunction", generated_layer.layer_function.as_str()));
    if let Some(side) = generated_layer.side {
        layer.push_attribute(("side", write::side_attr(side)));
    }
    if let Some(polarity) = generated_layer.polarity {
        layer.push_attribute(("polarity", write::polarity_attr(polarity)));
    }
    writer.write_event(Event::Empty(layer))?;
    Ok(())
}

pub(super) fn write_generated_steps_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut xml = write_board_cell_step_xml(spec)?;
    xml.push_str(&write_array_step_xml(spec)?);
    Ok(xml)
}

pub(super) fn write_board_cell_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.board_cell_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write::location(&mut writer, "Datum", 0.0, 0.0, spec.units)?;
    write::profile(
        &mut writer,
        spec.units,
        &rectangle_polygon(spec.pitch_x_mm, spec.pitch_y_mm),
    )?;
    write_board_cell_step_repeat(&mut writer, spec)?;
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::BoardCell)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn write_array_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.array_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write_panelization_metadata(&mut writer, spec)?;
    write::location(&mut writer, "Datum", 0.0, 0.0, spec.units)?;

    write::profile(
        &mut writer,
        spec.units,
        &rounded_rectangle_polygon(
            spec.array_width_mm,
            spec.array_height_mm,
            ARRAY_CORNER_RADIUS_MM,
        ),
    )?;

    write_array_step_repeat(&mut writer, spec)?;
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::Array)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn write_panelization_metadata(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let metadata = spec.panelization;

    write_metadata_integer(writer, "diode.panelize.schema_version", 1)?;
    write_metadata_string(writer, "diode.panelize.mode", metadata.mode.as_str())?;
    if let Some(sheet) = metadata.sheet {
        write_metadata_string(writer, "diode.panelize.sheet", sheet.name())?;
    }
    if let Some(target) = metadata.sheet_target_mm {
        write_metadata_double(writer, "diode.panelize.sheet_width_mm", target.width)?;
        write_metadata_double(writer, "diode.panelize.sheet_height_mm", target.height)?;
    }

    write_metadata_integer(writer, "diode.panelize.columns", spec.columns)?;
    write_metadata_integer(writer, "diode.panelize.rows", spec.rows)?;
    write_margin_metadata(writer, "diode.panelize.board_margin", spec.board_margin_mm)?;
    write_margin_metadata(writer, "diode.panelize.edge_rail", spec.edge_rail_mm)?;

    Ok(())
}

pub(super) fn write_margin_metadata(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    prefix: &str,
    margin: BoardMarginMm,
) -> Result<()> {
    write_metadata_double(writer, &format!("{prefix}_top_mm"), margin.top)?;
    write_metadata_double(writer, &format!("{prefix}_right_mm"), margin.right)?;
    write_metadata_double(writer, &format!("{prefix}_bottom_mm"), margin.bottom)?;
    write_metadata_double(writer, &format!("{prefix}_left_mm"), margin.left)?;
    Ok(())
}

pub(super) fn write_metadata_integer(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    value: u32,
) -> Result<()> {
    let value = value.to_string();
    write_metadata_attribute(writer, name, "INTEGER", &value)
}

pub(super) fn write_metadata_double(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    value: f64,
) -> Result<()> {
    let value = fmt_num(value);
    write_metadata_attribute(writer, name, "DOUBLE", &value)
}

pub(super) fn write_metadata_string(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    value: &str,
) -> Result<()> {
    write_metadata_attribute(writer, name, "STRING", value)
}

pub(super) fn write_metadata_attribute(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    property_type: &str,
    value: &str,
) -> Result<()> {
    let mut elem = BytesStart::new("NonstandardAttribute");
    elem.push_attribute(("name", name));
    elem.push_attribute(("type", property_type));
    elem.push_attribute(("value", value));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub(super) fn write_generated_layer_features(
    writer: &mut Writer<Cursor<Vec<u8>>>,
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
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    layer_feature: &GeneratedLayerFeature,
    names: &mut GeneratedNameState,
) -> Result<()> {
    if layer_feature.features.is_empty() {
        return Ok(());
    }

    let mut elem = BytesStart::new("LayerFeature");
    elem.push_attribute(("layerRef", layer_feature.layer_name.as_str()));
    writer.write_event(Event::Start(elem))?;

    let mut set = BytesStart::new("Set");
    set.push_attribute(("polarity", write::polarity_attr(layer_feature.polarity)));
    writer.write_event(Event::Start(set))?;
    for spec_ref in &layer_feature.spec_refs {
        write::spec_ref(writer, spec_ref)?;
    }
    write_set_features(writer, units, &layer_feature.features, names)?;
    writer.write_event(Event::End(BytesStart::new("Set").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("LayerFeature").to_end()))?;
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
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    features: &[SetFeature],
    names: &mut GeneratedNameState,
) -> Result<()> {
    let mut features_open = false;
    for feature in features {
        match feature {
            SetFeature::Line(line) => {
                if !features_open {
                    writer.write_event(Event::Start(BytesStart::new("Features")))?;
                    features_open = true;
                }
                write::line(writer, units, line)?;
            }
            SetFeature::Fiducial(fiducial) => {
                close_features_element(writer, &mut features_open)?;
                write::fiducial(writer, units, fiducial)?;
            }
            SetFeature::Hole(hole) => {
                close_features_element(writer, &mut features_open)?;
                write::hole(writer, units, hole, &names.next_hole_name())?;
            }
            _ => bail!("generated board array layer feature has unsupported feature kind"),
        }
    }
    close_features_element(writer, &mut features_open)?;
    Ok(())
}

pub(super) fn close_features_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    features_open: &mut bool,
) -> Result<()> {
    if *features_open {
        writer.write_event(Event::End(BytesStart::new("Features").to_end()))?;
        *features_open = false;
    }
    Ok(())
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

pub(super) fn write_array_step_repeat(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let x = fmt_units(spec.array_repeat_x_mm, spec.units);
    let y = fmt_units(spec.array_repeat_y_mm, spec.units);
    let dx = fmt_units(spec.pitch_x_mm, spec.units);
    let dy = fmt_units(spec.pitch_y_mm, spec.units);
    let nx = spec.columns.to_string();
    let ny = spec.rows.to_string();

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_cell_name.as_str()));
    repeat.push_attribute(("x", x.as_str()));
    repeat.push_attribute(("y", y.as_str()));
    repeat.push_attribute(("nx", nx.as_str()));
    repeat.push_attribute(("ny", ny.as_str()));
    repeat.push_attribute(("dx", dx.as_str()));
    repeat.push_attribute(("dy", dy.as_str()));
    repeat.push_attribute(("angle", "0.00"));
    repeat.push_attribute(("mirror", "false"));
    writer.write_event(Event::Empty(repeat))?;
    Ok(())
}

pub(super) fn write_board_cell_step_repeat(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let x = fmt_units(spec.board_repeat_x_mm, spec.units);
    let y = fmt_units(spec.board_repeat_y_mm, spec.units);

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_name.as_str()));
    repeat.push_attribute(("x", x.as_str()));
    repeat.push_attribute(("y", y.as_str()));
    repeat.push_attribute(("nx", "1"));
    repeat.push_attribute(("ny", "1"));
    repeat.push_attribute(("dx", "0"));
    repeat.push_attribute(("dy", "0"));
    repeat.push_attribute(("angle", "0.00"));
    repeat.push_attribute(("mirror", "false"));
    writer.write_event(Event::Empty(repeat))?;
    Ok(())
}
