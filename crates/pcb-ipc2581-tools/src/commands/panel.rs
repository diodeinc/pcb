use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use ipc2581::types::{StepType, Units};
use pcb_ir::dialects::ipc::{LayoutStepKind, root_step};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};

use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone)]
pub struct PanelCreateOptions {
    pub columns: u32,
    pub rows: u32,
    pub column_spacing_mm: f64,
    pub row_spacing_mm: f64,
    pub edge_rail_width_mm: f64,
}

#[derive(Debug, Clone)]
struct PanelSpec {
    panel_name: String,
    board_name: String,
    content_step_refs: Vec<String>,
    columns: u32,
    rows: u32,
    repeat_x_mm: f64,
    repeat_y_mm: f64,
    pitch_x_mm: f64,
    pitch_y_mm: f64,
    panel_width_mm: f64,
    panel_height_mm: f64,
    units: Units,
}

pub fn execute(input: &Path, output: &Path, options: &PanelCreateOptions) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    let updated_xml = create_panel_xml(&content, options)?;
    file_utils::save_ipc_file(output, &updated_xml)?;
    eprintln!("✓ Created IPC-2581 panel at {}", output.display());
    Ok(())
}

fn create_panel_xml(xml: &str, options: &PanelCreateOptions) -> Result<String> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let spec = build_panel_spec(&ipc, options)?;
    let panel_step_xml = write_panel_step_xml(&spec)?;
    let xml = update_content_step_refs(xml, &spec.content_step_refs)?;
    let xml = insert_panel_step(&xml, &panel_step_xml)?;
    let xml = crate::utils::history::append_file_revision(&xml, "Created panel array")?;
    let xml = crate::utils::format::reformat_xml(&xml)?;

    Ipc2581::parse(&xml).context("Generated IPC-2581 panel XML did not parse")?;
    Ok(xml)
}

fn build_panel_spec(ipc: &Ipc2581, options: &PanelCreateOptions) -> Result<PanelSpec> {
    validate_options(options)?;

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let primary_step = crate::steps::primary_step(ipc, &ecad.cad_data.steps)
        .context("IPC-2581 ECAD section has no Step")?;

    if is_panel_step(primary_step) {
        bail!("primary IPC-2581 step is already a panel; panel create expects a board step");
    }
    if !is_board_step(primary_step) {
        bail!("primary IPC-2581 step is not a board step");
    }

    let layout = geometry::extract_layout(ipc)?;
    let (_, root) = root_step(&layout).context("IPC-2581 board step has no layout root")?;
    if root.kind != LayoutStepKind::Board {
        bail!("primary IPC-2581 layout root is not a board step");
    }
    if root.bbox.is_empty() {
        bail!("primary IPC-2581 board step has no Profile outline");
    }

    let board_width = root.bbox.width();
    let board_height = root.bbox.height();
    if board_width <= EPSILON || board_height <= EPSILON {
        bail!("primary IPC-2581 board Profile outline has zero size");
    }

    let columns = options.columns;
    let rows = options.rows;
    let margin_x = options.column_spacing_mm + options.edge_rail_width_mm;
    let margin_y = options.row_spacing_mm + options.edge_rail_width_mm;
    let pitch_x = board_width + options.column_spacing_mm;
    let pitch_y = board_height + options.row_spacing_mm;
    let panel_width = columns as f64 * board_width
        + (columns + 1) as f64 * options.column_spacing_mm
        + 2.0 * options.edge_rail_width_mm;
    let panel_height = rows as f64 * board_height
        + (rows + 1) as f64 * options.row_spacing_mm
        + 2.0 * options.edge_rail_width_mm;

    let board_name = ipc.resolve(root.source_step_ref).to_string();
    let existing_step_names = ecad
        .cad_data
        .steps
        .iter()
        .map(|step| ipc.resolve(step.name).to_string())
        .collect::<HashSet<_>>();
    let panel_name = unique_panel_name(&existing_step_names);

    Ok(PanelSpec {
        panel_name: panel_name.clone(),
        board_name: board_name.clone(),
        content_step_refs: content_step_refs(ipc, &panel_name, &board_name),
        columns,
        rows,
        repeat_x_mm: margin_x - root.bbox.min.x,
        repeat_y_mm: margin_y - root.bbox.min.y,
        pitch_x_mm: if columns > 1 { pitch_x } else { 0.0 },
        pitch_y_mm: if rows > 1 { pitch_y } else { 0.0 },
        panel_width_mm: panel_width,
        panel_height_mm: panel_height,
        units: ecad.cad_header.units,
    })
}

fn validate_options(options: &PanelCreateOptions) -> Result<()> {
    validate_u32_range("columns", options.columns, 1, 10)?;
    validate_u32_range("rows", options.rows, 1, 10)?;
    validate_mm_range("column spacing", options.column_spacing_mm, 0.0, 20.0)?;
    validate_mm_range("row spacing", options.row_spacing_mm, 0.0, 20.0)?;
    validate_mm_range("edge rail width", options.edge_rail_width_mm, 0.0, 30.0)?;
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<()> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        bail!("{name} must be between {min} and {max}");
    }
}

fn validate_mm_range(name: &str, value: f64, min: f64, max: f64) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        bail!("{name} must be between {min} and {max} mm");
    }
}

fn is_panel_step(step: &ipc2581::types::ecad::Step) -> bool {
    step.step_type == Some(StepType::Pallet)
        || (step.step_type.is_none() && !step.step_repeats.is_empty())
}

fn is_board_step(step: &ipc2581::types::ecad::Step) -> bool {
    step.step_type == Some(StepType::Board)
        || (step.step_type.is_none() && step.step_repeats.is_empty())
}

fn unique_panel_name(existing_step_names: &HashSet<String>) -> String {
    if !existing_step_names.contains("panel") {
        return "panel".to_string();
    }

    (1..)
        .map(|index| format!("panel_{index}"))
        .find(|name| !existing_step_names.contains(name))
        .expect("unbounded panel name search should find an unused name")
}

fn content_step_refs(ipc: &Ipc2581, panel_name: &str, board_name: &str) -> Vec<String> {
    let mut refs = vec![panel_name.to_string()];
    let mut seen = HashSet::from([panel_name.to_string()]);
    for step_ref in &ipc.content().step_refs {
        let name = ipc.resolve(*step_ref).to_string();
        if seen.insert(name.clone()) {
            refs.push(name);
        }
    }
    if seen.insert(board_name.to_string()) {
        refs.push(board_name.to_string());
    }
    refs
}

fn update_content_step_refs(xml: &str, step_refs: &[String]) -> Result<String> {
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
                    write_step_refs(&mut writer, step_refs)?;
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
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"FunctionMode" =>
            {
                writer.write_event(Event::Empty(e.to_owned()))?;
                write_step_refs(&mut writer, step_refs)?;
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
                    write_step_refs(&mut writer, step_refs)?;
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

fn write_step_refs(writer: &mut Writer<Cursor<Vec<u8>>>, step_refs: &[String]) -> Result<()> {
    for step_ref in step_refs {
        let mut elem = BytesStart::new("StepRef");
        elem.push_attribute(("name", step_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

fn insert_panel_step(xml: &str, panel_step_xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut inserted = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::End(ref e) if e.name().as_ref() == b"CadData" => {
                writer.get_mut().write_all(panel_step_xml.as_bytes())?;
                writer.write_event(Event::End(e.to_owned()))?;
                inserted = true;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    if !inserted {
        bail!("IPC-2581 file has no CadData section");
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_panel_step_xml(spec: &PanelSpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.panel_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;

    writer.write_event(Event::Start(BytesStart::new("Profile")))?;
    writer.write_event(Event::Start(BytesStart::new("Polygon")))?;
    write_location_empty(&mut writer, "PolyBegin", 0.0, 0.0, spec.units)?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        0.0,
        spec.panel_height_mm,
        spec.units,
    )?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        spec.panel_width_mm,
        spec.panel_height_mm,
        spec.units,
    )?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        spec.panel_width_mm,
        0.0,
        spec.units,
    )?;
    write_location_empty(&mut writer, "PolyStepSegment", 0.0, 0.0, spec.units)?;
    writer.write_event(Event::End(BytesStart::new("Polygon").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("Profile").to_end()))?;

    write_step_repeat(&mut writer, spec)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_location_empty(
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

fn write_step_repeat(writer: &mut Writer<Cursor<Vec<u8>>>, spec: &PanelSpec) -> Result<()> {
    let x = fmt_units(spec.repeat_x_mm, spec.units);
    let y = fmt_units(spec.repeat_y_mm, spec.units);
    let dx = fmt_units(spec.pitch_x_mm, spec.units);
    let dy = fmt_units(spec.pitch_y_mm, spec.units);
    let nx = spec.columns.to_string();
    let ny = spec.rows.to_string();

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_name.as_str()));
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

fn fmt_units(value_mm: f64, units: Units) -> String {
    fmt_num(ipc2581::units::from_mm(value_mm, units))
}

fn fmt_num(value: f64) -> String {
    if value.abs() < EPSILON {
        return "0".to_string();
    }
    let mut s = format!("{value:.6}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::common::Point;

    #[test]
    fn creates_rectangular_panel_step_from_board_bbox() {
        let xml = create_panel_xml(
            board_fixture_mm(),
            &PanelCreateOptions {
                columns: 3,
                rows: 2,
                column_spacing_mm: 1.0,
                row_spacing_mm: 2.0,
                edge_rail_width_mm: 3.0,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<StepRef name="panel"/>"#));
        assert!(xml.contains(r#"<StepRef name="board"/>"#));
        assert!(xml.contains(r#"<Step name="panel" type="PALLET">"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="6" y="8" nx="3" ny="2" dx="11" dy="12" angle="0.00" mirror="false"/>"#
        ));

        let ipc = Ipc2581::parse(&xml).unwrap();
        let layout = geometry::extract_layout(&ipc).unwrap();
        let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
        assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
        assert_point_close(panel_step.bbox.max, Point::new(40.0, 32.0));
        assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 6);

        let first_instance = &layout.layout.instances[0];
        assert_point_close(first_instance.bbox.min, Point::new(4.0, 5.0));
        assert_point_close(first_instance.bbox.max, Point::new(14.0, 15.0));
    }

    #[test]
    fn writes_generated_panel_values_in_cad_header_units() {
        let xml = create_panel_xml(
            board_fixture_inch(),
            &PanelCreateOptions {
                columns: 1,
                rows: 1,
                column_spacing_mm: 2.54,
                row_spacing_mm: 2.54,
                edge_rail_width_mm: 2.54,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<PolyStepSegment x="0" y="1.4"/>"#));
        assert!(xml.contains(r#"<PolyStepSegment x="1.4" y="1.4"/>"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="0.2" y="0.2" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
        ));
    }

    #[test]
    fn rejects_primary_panel_step() {
        let error = create_panel_xml(
            panel_fixture(),
            &PanelCreateOptions {
                columns: 1,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 0.0,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("primary IPC-2581 step is already a panel")
        );
    }

    #[test]
    fn validates_simple_api_ranges() {
        let error = create_panel_xml(
            board_fixture_mm(),
            &PanelCreateOptions {
                columns: 11,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 0.0,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("columns must be between 1 and 10")
        );
    }

    fn assert_point_close(actual: Point, expected: Point) {
        assert!(
            (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn board_fixture_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="-2" y="-3"/>
            <PolyStepSegment x="8" y="-3"/>
            <PolyStepSegment x="8" y="7"/>
            <PolyStepSegment x="-2" y="7"/>
            <PolyStepSegment x="-2" y="-3"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn board_fixture_inch() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="INCH"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="1" y="0"/>
            <PolyStepSegment x="1" y="1"/>
            <PolyStepSegment x="0" y="1"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn panel_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}
