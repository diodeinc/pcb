//! IPC-2581 XML patching and generated-element serialization.

use super::*;

pub(super) fn update_content_refs(
    xml: &str,
    step_refs: &[String],
    layer_refs: &[String],
) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut in_content = false;
    let mut content_depth = 0usize;
    let mut skip_depth = 0usize;
    let mut function_mode_open = false;
    let mut refs_written = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::Empty(_) if skip_depth > 0 => {}
            Event::End(_) if skip_depth > 0 => skip_depth -= 1,
            Event::Start(ref e) if e.name().as_ref() == b"Content" => {
                in_content = true;
                content_depth = 1;
                function_mode_open = false;
                refs_written = false;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::End(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"Content" =>
            {
                if !refs_written {
                    write_content_refs(&mut writer, step_refs, layer_refs)?;
                }
                writer.write_event(Event::End(e.to_owned()))?;
                in_content = false;
                content_depth = 0;
            }
            Event::Start(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"StepRef" =>
            {
                skip_depth = 1;
            }
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"StepRef" => {}
            Event::Start(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"LayerRef" =>
            {
                skip_depth = 1;
            }
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"LayerRef" => {}
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"FunctionMode" =>
            {
                writer.write_event(Event::Empty(e.to_owned()))?;
                write_content_refs(&mut writer, step_refs, layer_refs)?;
                refs_written = true;
            }
            Event::Start(ref e) if in_content => {
                if content_depth == 1 && e.name().as_ref() == b"FunctionMode" {
                    function_mode_open = true;
                }
                writer.write_event(Event::Start(e.to_owned()))?;
                content_depth += 1;
            }
            Event::End(ref e) if in_content => {
                writer.write_event(Event::End(e.to_owned()))?;
                if function_mode_open && content_depth == 2 && e.name().as_ref() == b"FunctionMode"
                {
                    write_content_refs(&mut writer, step_refs, layer_refs)?;
                    refs_written = true;
                    function_mode_open = false;
                }
                content_depth -= 1;
            }
            Event::Empty(ref e) if in_content => {
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn write_content_refs(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    step_refs: &[String],
    layer_refs: &[String],
) -> Result<()> {
    write_step_refs(writer, step_refs)?;
    write_layer_refs(writer, layer_refs)?;
    Ok(())
}

pub(super) fn write_step_refs(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    step_refs: &[String],
) -> Result<()> {
    for step_ref in step_refs {
        let mut elem = BytesStart::new("StepRef");
        elem.push_attribute(("name", step_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

pub(super) fn write_layer_refs(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    layer_refs: &[String],
) -> Result<()> {
    for layer_ref in layer_refs {
        let mut elem = BytesStart::new("LayerRef");
        elem.push_attribute(("name", layer_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

pub(super) fn insert_generated_cad_header_specs(
    xml: &str,
    generated_spec_xml: &str,
) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut in_cad_header = false;
    let mut cad_header_depth = 0usize;
    let mut inserted = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Empty(ref e) if e.name().as_ref() == b"CadHeader" => {
                writer.write_event(Event::Start(e.to_owned()))?;
                write_raw_xml(&mut writer, Some(generated_spec_xml))?;
                writer.write_event(Event::End(BytesStart::new("CadHeader").to_end()))?;
                inserted = true;
            }
            Event::Start(ref e) if e.name().as_ref() == b"CadHeader" => {
                in_cad_header = true;
                cad_header_depth = 1;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::Start(ref e) if in_cad_header => {
                writer.write_event(Event::Start(e.to_owned()))?;
                cad_header_depth += 1;
            }
            Event::Empty(ref e) if in_cad_header => {
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::End(ref e) if in_cad_header && cad_header_depth == 1 => {
                write_raw_xml(&mut writer, Some(generated_spec_xml))?;
                writer.write_event(Event::End(e.to_owned()))?;
                in_cad_header = false;
                cad_header_depth = 0;
                inserted = true;
            }
            Event::End(ref e) if in_cad_header => {
                writer.write_event(Event::End(e.to_owned()))?;
                cad_header_depth -= 1;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    if !inserted {
        bail!("IPC-2581 file has no CadHeader section");
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn insert_array_cad_data(
    xml: &str,
    spec: &BoardArraySpec,
    generated_layer_xml: Option<&str>,
    array_step_xml: &str,
) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut in_cad_data = false;
    let mut cad_data_depth = 0usize;
    let mut panel_step_inserted = false;
    let mut generated_layers_inserted = generated_layer_xml.is_none();
    let mut source_board_step_depth = None;
    let mut skip_depth = 0usize;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => {
                    bail!("unexpected end of IPC-2581 while removing board outline layer feature")
                }
                _ => {}
            }
            buf.clear();
            continue;
        }

        match event {
            Event::Eof => break,
            Event::Start(ref e) if e.name().as_ref() == b"CadData" => {
                in_cad_data = true;
                cad_data_depth = 1;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::Start(ref e) if in_cad_data => {
                if cad_data_depth == 1
                    && !generated_layers_inserted
                    && e.name().as_ref() != b"Layer"
                {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Layer"
                    && cad_data_layer_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    skip_depth = 1;
                    buf.clear();
                    continue;
                }
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Step"
                    && start_attr_eq(e, b"name", &spec.board_name)?
                {
                    source_board_step_depth = Some(cad_data_depth + 1);
                }
                if source_board_step_depth.is_some()
                    && e.name().as_ref() == b"LayerFeature"
                    && layer_feature_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    skip_depth = 1;
                    buf.clear();
                    continue;
                }
                writer.write_event(Event::Start(e.to_owned()))?;
                cad_data_depth += 1;
            }
            Event::Empty(ref e) if in_cad_data => {
                if cad_data_depth == 1
                    && !generated_layers_inserted
                    && e.name().as_ref() != b"Layer"
                {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Layer"
                    && cad_data_layer_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    buf.clear();
                    continue;
                }
                if source_board_step_depth.is_some()
                    && e.name().as_ref() == b"LayerFeature"
                    && layer_feature_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    buf.clear();
                    continue;
                }
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::End(ref e) if e.name().as_ref() == b"CadData" => {
                if !generated_layers_inserted {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                writer.get_mut().write_all(array_step_xml.as_bytes())?;
                writer.write_event(Event::End(e.to_owned()))?;
                panel_step_inserted = true;
                in_cad_data = false;
                cad_data_depth = 0;
            }
            Event::End(ref e) if in_cad_data => {
                writer.write_event(Event::End(e.to_owned()))?;
                if source_board_step_depth == Some(cad_data_depth) && e.name().as_ref() == b"Step" {
                    source_board_step_depth = None;
                }
                cad_data_depth -= 1;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    if !panel_step_inserted {
        bail!("IPC-2581 file has no CadData section");
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

pub(super) fn cad_data_layer_is_board_outline(
    e: &BytesStart,
    board_outline_layer_names: &[String],
) -> Result<bool> {
    let Some(name) = start_attr_value(e, b"name")? else {
        return Ok(false);
    };
    Ok(board_outline_layer_names.iter().any(|layer| layer == &name))
}

pub(super) fn layer_feature_is_board_outline(
    e: &BytesStart,
    board_outline_layer_names: &[String],
) -> Result<bool> {
    let Some(layer_ref) = start_attr_value(e, b"layerRef")? else {
        return Ok(false);
    };
    Ok(board_outline_layer_names
        .iter()
        .any(|name| name == &layer_ref))
}

pub(super) fn start_attr_eq(e: &BytesStart, key: &[u8], value: &str) -> Result<bool> {
    Ok(start_attr_value(e, key)?.as_deref() == Some(value))
}

pub(super) fn start_attr_value(e: &BytesStart, key: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr?;
        if attr.key.as_ref() == key {
            return Ok(Some(String::from_utf8(attr.value.into_owned())?));
        }
    }
    Ok(None)
}

pub(super) fn write_raw_xml(writer: &mut Writer<Cursor<Vec<u8>>>, xml: Option<&str>) -> Result<()> {
    if let Some(xml) = xml {
        writer.get_mut().write_all(xml.as_bytes())?;
    }
    Ok(())
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
        layer.push_attribute(("side", side_attr(side)));
    }
    if let Some(polarity) = generated_layer.polarity {
        layer.push_attribute(("polarity", polarity_attr(polarity)));
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

    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;
    write_profile(
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
    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;

    write_profile(
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
    set.push_attribute(("polarity", polarity_attr(layer_feature.polarity)));
    writer.write_event(Event::Start(set))?;
    write_set_spec_refs(writer, &layer_feature.spec_refs)?;
    write_set_features(writer, units, &layer_feature.features, names)?;
    writer.write_event(Event::End(BytesStart::new("Set").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("LayerFeature").to_end()))?;
    Ok(())
}

pub(super) fn write_set_spec_refs(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec_refs: &[String],
) -> Result<()> {
    for spec_ref in spec_refs {
        let mut elem = BytesStart::new("SpecRef");
        elem.push_attribute(("id", spec_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct GeneratedNameState {
    hole_index: usize,
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
                write_line(writer, units, line)?;
            }
            SetFeature::Fiducial(fiducial) => {
                close_features_element(writer, &mut features_open)?;
                write_fiducial(writer, units, fiducial)?;
            }
            SetFeature::Hole(hole) => {
                close_features_element(writer, &mut features_open)?;
                write_hole(writer, units, hole, names)?;
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

pub(super) fn write_line(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    line: &Line,
) -> Result<()> {
    if line.line_desc_ref.is_some() {
        bail!("generated board array lines must carry inline LineDesc values");
    }

    let start_x = fmt_units(line.start_x, units);
    let start_y = fmt_units(line.start_y, units);
    let end_x = fmt_units(line.end_x, units);
    let end_y = fmt_units(line.end_y, units);
    let mut elem = BytesStart::new("Line");
    elem.push_attribute(("startX", start_x.as_str()));
    elem.push_attribute(("startY", start_y.as_str()));
    elem.push_attribute(("endX", end_x.as_str()));
    elem.push_attribute(("endY", end_y.as_str()));
    writer.write_event(Event::Start(elem))?;

    let line_width = fmt_units(line.line_width, units);
    let mut line_desc = BytesStart::new("LineDesc");
    line_desc.push_attribute(("lineWidth", line_width.as_str()));
    if let Some(line_end) = line.line_end {
        line_desc.push_attribute(("lineEnd", line_end_attr(line_end)));
    }
    if let Some(line_property) = line.line_property {
        line_desc.push_attribute(("lineProperty", line_property_attr(line_property)));
    }
    writer.write_event(Event::Empty(line_desc))?;
    writer.write_event(Event::End(BytesStart::new("Line").to_end()))?;
    Ok(())
}

pub(super) fn write_fiducial(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    fiducial: &Fiducial,
) -> Result<()> {
    if fiducial.xform.is_some() || fiducial.pin_ref.is_some() {
        bail!("generated board array fiducials must use location-only round geometry");
    }

    let elem_name = fiducial_element_name(fiducial.kind);
    writer.write_event(Event::Start(BytesStart::new(elem_name)))?;
    write_location_empty(
        writer,
        "Location",
        fiducial.location.x,
        fiducial.location.y,
        units,
    )?;
    match &fiducial.shape {
        FiducialShape::Primitive(StandardPrimitive::Circle(circle)) => {
            write_circle(writer, units, circle.shape.diameter)?;
        }
        _ => bail!("generated board array fiducials must use inline Circle geometry"),
    }
    writer.write_event(Event::End(BytesStart::new(elem_name).to_end()))?;
    Ok(())
}

pub(super) fn write_hole(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    hole: &Hole,
    names: &mut GeneratedNameState,
) -> Result<()> {
    let diameter = fmt_units(hole.diameter, units);
    let x = fmt_units(hole.x, units);
    let y = fmt_units(hole.y, units);
    let generated_name = format!("{GENERATED_HOLE_NAME_PREFIX}_{}", names.hole_index);
    names.hole_index += 1;

    let mut elem = BytesStart::new("Hole");
    elem.push_attribute(("name", generated_name.as_str()));
    elem.push_attribute(("type", "CIRCLE"));
    elem.push_attribute(("diameter", diameter.as_str()));
    elem.push_attribute(("platingStatus", plating_status_attr(hole.plating_status)));
    elem.push_attribute(("plusTol", "0"));
    elem.push_attribute(("minusTol", "0"));
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub(super) fn write_circle(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    diameter_mm: f64,
) -> Result<()> {
    let diameter = fmt_units(diameter_mm, units);
    let mut elem = BytesStart::new("Circle");
    elem.push_attribute(("diameter", diameter.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

pub(super) fn write_profile(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    polygon: &Polygon,
) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("Profile")))?;
    write_polygon(writer, units, polygon)?;
    writer.write_event(Event::End(BytesStart::new("Profile").to_end()))?;
    Ok(())
}

pub(super) fn write_polygon(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    polygon: &Polygon,
) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("Polygon")))?;
    write_location_empty(writer, "PolyBegin", polygon.begin.x, polygon.begin.y, units)?;
    for step in &polygon.steps {
        match step {
            PolyStep::Segment(segment) => {
                write_location_empty(
                    writer,
                    "PolyStepSegment",
                    segment.point.x,
                    segment.point.y,
                    units,
                )?;
            }
            PolyStep::Curve(curve) => write_poly_step_curve(writer, units, curve)?,
        }
    }
    writer.write_event(Event::End(BytesStart::new("Polygon").to_end()))?;
    Ok(())
}

pub(super) fn write_poly_step_curve(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    curve: &PolyStepCurve,
) -> Result<()> {
    let x = fmt_units(curve.point.x, units);
    let y = fmt_units(curve.point.y, units);
    let center_x = fmt_units(curve.center.x, units);
    let center_y = fmt_units(curve.center.y, units);
    let mut elem = BytesStart::new("PolyStepCurve");
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    elem.push_attribute(("centerX", center_x.as_str()));
    elem.push_attribute(("centerY", center_y.as_str()));
    elem.push_attribute(("clockwise", if curve.clockwise { "true" } else { "false" }));
    writer.write_event(Event::Empty(elem))?;
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

pub(super) fn fiducial_element_name(kind: IpcFiducialKind) -> &'static str {
    match kind {
        IpcFiducialKind::BadBoardMark => "BadBoardMark",
        IpcFiducialKind::Global => "GlobalFiducial",
        IpcFiducialKind::GoodPanelMark => "GoodPanelMark",
        IpcFiducialKind::Local => "LocalFiducial",
    }
}

pub(super) fn side_attr(side: Side) -> &'static str {
    match side {
        Side::Top => "TOP",
        Side::Bottom => "BOTTOM",
        Side::Both => "BOTH",
        Side::Internal => "INTERNAL",
        Side::All => "ALL",
        Side::None => "NONE",
    }
}

pub(super) fn polarity_attr(polarity: Polarity) -> &'static str {
    match polarity {
        Polarity::Positive => "POSITIVE",
        Polarity::Negative => "NEGATIVE",
    }
}

pub(super) fn line_end_attr(line_end: LineEnd) -> &'static str {
    match line_end {
        LineEnd::Round => "ROUND",
        LineEnd::Square => "SQUARE",
        LineEnd::Flat => "FLAT",
    }
}

pub(super) fn line_property_attr(line_property: LineProperty) -> &'static str {
    match line_property {
        LineProperty::Solid => "SOLID",
        LineProperty::Dotted => "DOTTED",
        LineProperty::Dashed => "DASHED",
        LineProperty::Center => "CENTER",
        LineProperty::Phantom => "PHANTOM",
        LineProperty::Erase => "ERASE",
    }
}

pub(super) fn plating_status_attr(plating_status: PlatingStatus) -> &'static str {
    match plating_status {
        PlatingStatus::Plated => "PLATED",
        PlatingStatus::NonPlated => "NONPLATED",
        PlatingStatus::Via => "VIA",
    }
}

pub(super) fn write_location_empty(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    x_mm: f64,
    y_mm: f64,
    units: Units,
) -> Result<()> {
    let x = fmt_units(x_mm, units);
    let y = fmt_units(y_mm, units);
    let mut elem = BytesStart::new(name);
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
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

pub(super) fn fmt_units(value_mm: f64, units: Units) -> String {
    crate::utils::format::fmt_num(ipc2581::units::from_mm(value_mm, units))
}
