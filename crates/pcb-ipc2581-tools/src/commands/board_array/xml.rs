//! IPC-2581 XML patching and generated-element serialization.
//!
//! Generated fragments (specs, layers, steps) are serialized with
//! [`ipc2581::write`] and spliced into the source document as byte-range
//! edits via [`ipc2581::edit`], leaving the rest of the file untouched.

use super::*;
use crate::generated::{
    GeneratedNameState, write_double_attribute, write_generated_layer_feature,
    write_nonstandard_attribute,
};
use ipc2581::XmlWriter;
use ipc2581::edit::{Doc, Edit};
use ipc2581::write;
use ipc2581::write::fmt_units;

/// The board-array changes as byte-range edits against the source document:
/// Content step/layer refs, generated CadHeader specs, generated layers,
/// board-outline removal, and the generated board-cell/array steps.
pub(super) fn board_array_edits(doc: &Doc, spec: &BoardArraySpec) -> Result<Vec<Edit>> {
    let generated_steps_xml = write_generated_steps_xml(spec)?;
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
    edits.push(doc.append_inside(cad_header, write_generated_specs_xml(spec)));

    let cad_data = doc
        .child(ecad, "CadData")
        .ok_or_else(|| anyhow::anyhow!("IPC-2581 file has no CadData section"))?;
    let children = doc.children(cad_data);

    // Generated layers join the end of the leading Layer block.
    if !spec.generated_geometry.layers.is_empty() {
        let layer_xml = write_generated_layers_xml(&spec.generated_geometry);
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

    edits.push(doc.append_inside(cad_data, generated_steps_xml));
    edits.extend(crate::generated::user_dictionary_edit(
        doc,
        spec.units,
        &spec.generated_geometry.user_entries,
    )?);

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

fn write_generated_specs_xml(spec: &BoardArraySpec) -> String {
    let mut writer = XmlWriter::new();
    if let Some(vcut_spec_name) = &spec.vcut_spec_name {
        writer.start_element("Spec", &[("name", vcut_spec_name.as_str())]);
        writer.start_element("V_Cut", &[("type", "OFFSET")]);
        writer.empty_element("Property", &[("value", "0"), ("unit", "MM")]);
        writer.end_element("V_Cut");
        writer.end_element("Spec");
    }
    writer.into_string()
}

fn write_generated_layers_xml(geometry: &BoardArrayGeneratedGeometry) -> String {
    let mut writer = XmlWriter::new();
    for layer in &geometry.layers {
        let attrs = [
            ("name", layer.name.as_str()),
            ("layerFunction", layer.layer_function.as_str()),
            ("side", layer.side.as_str()),
            ("polarity", Polarity::Positive.as_str()),
        ];
        match &layer.span {
            Some((from, to)) => {
                writer.start_element("Layer", &attrs);
                writer.empty_element("Span", &[("fromLayer", from), ("toLayer", to)]);
                writer.end_element("Layer");
            }
            None => writer.empty_element("Layer", &attrs),
        }
    }
    writer.into_string()
}

/// The board-cell Step, one board in its margin, then the array Step that
/// repeats it.
fn write_generated_steps_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = XmlWriter::new();
    let (units, grid) = (spec.units, &spec.grid);

    writer.start_element(
        "Step",
        &[("name", spec.board_cell_name.as_str()), ("type", "PALLET")],
    );
    write::location(&mut writer, "Datum", 0.0, 0.0, units);
    write::profile(
        &mut writer,
        units,
        &rectangle_polygon(grid.pitch_x_mm, grid.pitch_y_mm),
    );
    write_step_repeat(
        &mut writer,
        units,
        &spec.board_name,
        (spec.board_repeat_x_mm, spec.board_repeat_y_mm),
        (1, 1),
        (0.0, 0.0),
        "0.00",
    );
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::BoardCell)?;
    writer.end_element("Step");

    writer.start_element(
        "Step",
        &[("name", spec.array_name.as_str()), ("type", "PALLET")],
    );
    write_panelization_metadata(&mut writer, spec);
    write::location(&mut writer, "Datum", 0.0, 0.0, units);
    write::profile_with_cutouts(
        &mut writer,
        units,
        &rounded_rectangle_polygon(
            grid.array_width_mm,
            grid.array_height_mm,
            ARRAY_CORNER_RADIUS_MM,
        ),
        &spec.profile_cutouts,
    );
    write_step_repeat(
        &mut writer,
        units,
        &spec.board_cell_name,
        (spec.edge_rail_mm.left, spec.edge_rail_mm.bottom),
        (grid.columns, grid.rows),
        (grid.pitch_x_mm, grid.pitch_y_mm),
        "0.00",
    );
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::Array)?;
    writer.end_element("Step");

    Ok(writer.into_string())
}

fn write_panelization_metadata(writer: &mut XmlWriter, spec: &BoardArraySpec) {
    let string = |writer: &mut XmlWriter, name: &str, value: &str| {
        write_nonstandard_attribute(writer, name, "STRING", value)
    };
    let integer = |writer: &mut XmlWriter, name: &str, value: u32| {
        write_nonstandard_attribute(writer, name, "INTEGER", &value.to_string())
    };

    integer(writer, "diode.panelize.schema_version", 1);
    string(
        writer,
        "diode.panelize.mode",
        spec.panelization.mode.as_str(),
    );
    if let Some((sheet, target)) = spec.panelization.sheet {
        string(writer, "diode.panelize.sheet", sheet.name());
        write_double_attribute(writer, "diode.panelize.sheet_width_mm", target.width);
        write_double_attribute(writer, "diode.panelize.sheet_height_mm", target.height);
    }
    string(
        writer,
        "diode.panelize.separation",
        spec.separation.as_str(),
    );
    if spec.separation == Separation::MouseBite {
        let tabs = spec.tabs_per_board as u32;
        integer(writer, "diode.panelize.tabs_per_board", tabs);
    }
    integer(writer, "diode.panelize.columns", spec.grid.columns);
    integer(writer, "diode.panelize.rows", spec.grid.rows);
    for (prefix, margin) in [
        ("board_margin", spec.board_margin_mm),
        ("edge_rail", spec.edge_rail_mm),
    ] {
        for (side, value) in margin.sides() {
            write_double_attribute(writer, &format!("diode.panelize.{prefix}_{side}_mm"), value);
        }
    }
}

fn write_generated_layer_features(
    writer: &mut XmlWriter,
    spec: &BoardArraySpec,
    scope: GeneratedFeatureScope,
) -> Result<()> {
    let mut names = GeneratedNameState::default();
    for (_, layer_feature) in spec
        .generated_geometry
        .layer_features
        .iter()
        .filter(|(feature_scope, _)| *feature_scope == scope)
    {
        write_generated_layer_feature(writer, spec.units, layer_feature, &mut names)?;
    }
    Ok(())
}

pub(crate) fn rectangle_polygon(width_mm: f64, height_mm: f64) -> Polygon {
    Polygon::new(
        IpcPoint { x: 0.0, y: 0.0 },
        [
            poly_segment(width_mm, 0.0),
            poly_segment(width_mm, height_mm),
            poly_segment(0.0, height_mm),
        ],
    )
}

pub(super) fn rounded_rectangle_polygon(width_mm: f64, height_mm: f64, radius_mm: f64) -> Polygon {
    let radius = radius_mm.min(width_mm / 2.0).min(height_mm / 2.0);
    let begin = IpcPoint { x: 0.0, y: radius };
    Polygon::new(
        begin,
        [
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
    )
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

pub(super) fn round_fiducial_features(
    kind: IpcFiducialKind,
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    let fiducial = |(x, y)| Fiducial {
        kind,
        location: Location { x, y },
        xform: None,
        shape: FiducialShape::Primitive(StandardPrimitive::Circle(Styled {
            shape: Circle {
                diameter: diameter_mm,
            },
            fill_property: None,
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        })),
        pin_ref: None,
    };
    points
        .into_iter()
        .map(|point| SetFeature::Fiducial(Box::new(fiducial(point))))
        .collect()
}

pub(super) fn round_nonplated_hole_features(
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    let hole = |(x, y)| Hole {
        name: None,
        shape: ipc2581::types::HoleShape::Circle,
        diameter: diameter_mm,
        plating_status: PlatingStatus::NonPlated,
        xform: None,
        spec_refs: Vec::new(),
        x,
        y,
    };
    points
        .into_iter()
        .map(|point| SetFeature::Hole(hole(point)))
        .collect()
}

/// One `StepRepeat` of `step_ref`: `count` placements from `origin_mm`, a
/// `pitch_mm` apart, unmirrored.
pub(crate) fn write_step_repeat(
    writer: &mut XmlWriter,
    units: Units,
    step_ref: &str,
    origin_mm: (f64, f64),
    count: (u32, u32),
    pitch_mm: (f64, f64),
    angle: &str,
) {
    writer.empty_element(
        "StepRepeat",
        &[
            ("stepRef", step_ref),
            ("x", fmt_units(origin_mm.0, units).as_str()),
            ("y", fmt_units(origin_mm.1, units).as_str()),
            ("nx", count.0.to_string().as_str()),
            ("ny", count.1.to_string().as_str()),
            ("dx", fmt_units(pitch_mm.0, units).as_str()),
            ("dy", fmt_units(pitch_mm.1, units).as_str()),
            ("angle", angle),
            ("mirror", "false"),
        ],
    );
}
