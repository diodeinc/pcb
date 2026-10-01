use std::cmp::Ordering;
#[cfg(feature = "cli")]
use std::fs;
#[cfg(feature = "cli")]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Result, bail};
#[cfg(feature = "cli")]
use ipc2581::Ipc2581;
use pcb_ir::dialects::assembly::BomCategory;
use pcb_ir::dialects::placement::{
    Document as PlacementDocument, Placement, PlacementSide, Population,
};
#[cfg(feature = "cli")]
use pcb_ir::geom::Resolution;

#[cfg(feature = "cli")]
use crate::placement::extract_single_board_placements;
#[cfg(feature = "cli")]
use pcb_ir::import::ipc2581::import_design;

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CplSideFilter {
    #[default]
    Both,
    Top,
    Bottom,
}

#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CplFormat {
    /// The release `cpl.csv` columns.
    #[default]
    Release,
    /// JLCPCB's placement columns.
    Jlc,
}

#[derive(Debug, Clone)]
pub struct CplOptions {
    pub output: Option<PathBuf>,
    pub side: CplSideFilter,
    pub exclude_dnp: bool,
    pub format: CplFormat,
}

#[cfg(feature = "cli")]
pub fn execute(file: &Path, options: &CplOptions, resolution: Resolution) -> Result<()> {
    let ipc = Ipc2581::parse(&crate::utils::file::load_ipc_file(file)?)?;
    let imported = import_design(&ipc, resolution)?;
    if let Some(defect) = &imported.flipped_rotation_defect
        && imported
            .components
            .iter()
            .any(|component| component.corrected_source_rotation.is_some())
    {
        anstream::eprintln!(
            "warning: {} wrote bottom-side rotations in a non-standard form; corrected them. Re-export with KiCad 10.0.5 or later to clear this warning.",
            defect.exporter
        );
    }
    let placements = extract_single_board_placements(&imported)?;
    let cpl = emit_cpl_csv(&placements, options)?;

    if let Some(output) = &options.output {
        fs::write(output, cpl)?;
    } else {
        pcb_ui::write_stdout(|stdout| stdout.write_all(cpl.as_bytes()))?;
    }

    Ok(())
}

pub fn emit_cpl_csv(document: &PlacementDocument, options: &CplOptions) -> Result<String> {
    let mut rows = document
        .components
        .iter()
        .filter(|component| include_component(component, options))
        .collect::<Vec<_>>();
    rows.sort_by(compare_components);

    let mut output = String::from(match options.format {
        CplFormat::Release => "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n",
        CplFormat::Jlc => "Designator,Mid X,Mid Y,Layer,Rotation\n",
    });
    for component in rows {
        match options.format {
            CplFormat::Release => write_csv_row(
                &mut output,
                &[
                    component.designator.as_str(),
                    component.value.as_deref().unwrap_or_default(),
                    component.package.as_deref().unwrap_or_default(),
                    &format_number(component.at.x),
                    &format_number(component.at.y),
                    &format_number(normalize_rotation(cpl_rotation(component))),
                    cpl_layer(component.side),
                ],
            ),
            CplFormat::Jlc => write_csv_row(
                &mut output,
                &[
                    component.designator.as_str(),
                    &format!("{:.4}mm", clean_zero(component.at.x)),
                    &format!("{:.4}mm", clean_zero(component.at.y)),
                    jlc_layer(component)?,
                    &jlc_rotation(cpl_rotation(component)).to_string(),
                ],
            ),
        }
    }

    Ok(output)
}

/// JLCPCB takes whole degrees in `0..360`.
fn jlc_rotation(degrees: f64) -> u32 {
    (degrees.round() as i64).rem_euclid(360) as u32
}

/// JLCPCB places on the top or bottom only.
fn jlc_layer(component: &Placement) -> Result<&'static str> {
    match component.side {
        PlacementSide::Top => Ok("Top"),
        PlacementSide::Bottom => Ok("Bottom"),
        side => bail!(
            "component '{}' is on side {side:?}; JLCPCB places only top and bottom parts",
            component.designator
        ),
    }
}

/// Rotation in KiCad's position-file convention, the one assembly houses
/// consume: a mirrored part reports `180° - r` for its IPC-2581 rotation `r`,
/// matching the orientation KiCad reports for a flipped footprint.
fn cpl_rotation(component: &Placement) -> f64 {
    if component.mirror {
        180.0 - component.rotation_degrees
    } else {
        component.rotation_degrees
    }
}

fn cpl_layer(side: PlacementSide) -> &'static str {
    match side {
        PlacementSide::Top => "top",
        PlacementSide::Bottom => "bottom",
        PlacementSide::Internal => "internal",
        PlacementSide::Both
        | PlacementSide::All
        | PlacementSide::None
        | PlacementSide::Unspecified => "unknown",
    }
}

fn include_component(component: &Placement, options: &CplOptions) -> bool {
    // KiCad files parts without pads under DOCUMENT; those it populates are
    // still placed.
    if component.bom_category == Some(BomCategory::Document)
        && component.population != Population::Populate
    {
        return false;
    }
    if options.exclude_dnp && component.population == Population::DoNotPopulate {
        return false;
    }

    match options.side {
        CplSideFilter::Both => true,
        CplSideFilter::Top => component.side == PlacementSide::Top,
        CplSideFilter::Bottom => component.side == PlacementSide::Bottom,
    }
}

fn compare_components(left: &&Placement, right: &&Placement) -> Ordering {
    side_sort_key(left.side)
        .cmp(&side_sort_key(right.side))
        .then_with(|| natord::compare(&left.designator, &right.designator))
}

fn side_sort_key(side: PlacementSide) -> u8 {
    match side {
        PlacementSide::Top => 0,
        PlacementSide::Bottom => 1,
        PlacementSide::Internal => 2,
        PlacementSide::Both
        | PlacementSide::All
        | PlacementSide::None
        | PlacementSide::Unspecified => 3,
    }
}

fn normalize_rotation(degrees: f64) -> f64 {
    let mut normalized = degrees % 360.0;
    if normalized < 0.0 {
        normalized += 360.0;
    }
    if normalized > 180.0 {
        normalized -= 360.0;
    }
    clean_zero(normalized)
}

fn format_number(value: f64) -> String {
    format!("{:.6}", clean_zero(value))
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() < 0.000_000_5 {
        0.0
    } else {
        value
    }
}

fn write_csv_row(output: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write_csv_field(output, field);
    }
    output.push('\n');
}

fn write_csv_field(output: &mut String, field: &str) {
    if !field.contains([',', '"', '\n', '\r']) {
        output.push_str(field);
        return;
    }

    output.push('"');
    for ch in field.chars() {
        if ch == '"' {
            output.push('"');
        }
        output.push(ch);
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use ipc2581::Ipc2581;
    use pcb_ir::dialects::assembly::{
        ComponentDefinitionId, ComponentOccurrenceId, LayoutOccurrenceId, Scope,
    };
    use pcb_ir::dialects::placement::{PlacementMount, PlacementSide};
    use pcb_ir::geom::Point;
    use pcb_ir::import::ipc2581::import_design;

    use crate::placement::extract_single_board_placements;

    use super::*;

    #[test]
    fn imported_cpl_preserves_rotation_before_mirroring() {
        let resolution = pcb_ir::geom::Resolution::default();
        let options = CplOptions {
            output: None,
            side: CplSideFilter::Both,
            exclude_dnp: false,
            format: CplFormat::Release,
        };
        // Mirrored parts report KiCad's flipped-footprint orientation, 180° - r.
        for (rotation, top_rotation, bottom_rotation, local_x, local_y) in [
            (0, "0.000000", "180.000000", 2.0, 1.0),
            (
                30,
                "30.000000",
                "150.000000",
                1.2320508075688772,
                1.8660254037844386,
            ),
            (90, "90.000000", "90.000000", -1.0, 2.0),
            (180, "180.000000", "0.000000", -2.0, -1.0),
            (270, "-90.000000", "-90.000000", 1.0, -2.0),
        ] {
            for mirror in [false, true] {
                for panel in [false, true] {
                    let side = if mirror { "BOTTOM" } else { "TOP" };
                    let layer = if mirror { "bottom" } else { "top" };
                    let csv_rotation = if mirror {
                        bottom_rotation
                    } else {
                        top_rotation
                    };
                    let root = if panel { "panel" } else { "board" };
                    let ipc = Ipc2581::parse(&format!(
                        r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="{root}"/></Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="component" layerFunction="COMPONENT_{side}" side="{side}"/>
    <Step name="board" type="BOARD">
      <Component refDes="U1" packageRef="QFN" part="ic" layerRef="component" mountType="SMT">
        <Xform rotation="{rotation}" mirror="{mirror}"/>
        <Location x="10" y="20"/>
      </Component>
    </Step>
    <Step name="panel" type="PALLET">
      <StepRepeat stepRef="board" x="50" y="60" nx="2" ny="1" dx="20" dy="0" angle="13" mirror="true"/>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#
                    ))
                    .unwrap();
                    let imported = import_design(&ipc, resolution).unwrap();
                    let placements = extract_single_board_placements(&imported).unwrap();

                    // Geometry and CPL must describe the same orientation, including
                    // when the board occurs in a rotated, mirrored panel.
                    let landmark = imported.components[0]
                        .local_from_component
                        .transform_point(Point::new(2.0, 1.0));
                    let expected_x = 10.0 + if mirror { -local_x } else { local_x };
                    assert!((landmark.x - expected_x).abs() < 1e-9);
                    assert!((landmark.y - (20.0 + local_y)).abs() < 1e-9);
                    assert_eq!(placements.components.len(), 1);
                    assert_eq!(placements.components[0].mirror, mirror);
                    assert_eq!(
                        emit_cpl_csv(&placements, &options).unwrap(),
                        format!(
                            "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n\
U1,,QFN,10.000000,20.000000,{csv_rotation},{layer}\n"
                        ),
                        "rotation={rotation}, mirror={mirror}, panel={panel}"
                    );
                }
            }
        }
    }

    #[test]
    fn emits_release_and_jlc_rows() {
        let mut document = PlacementDocument {
            scope: Scope::Board,
            step: Some("board".to_string()),
            components: vec![
                Placement {
                    id: ComponentOccurrenceId {
                        component: ComponentDefinitionId(0),
                        layout: LayoutOccurrenceId::Root,
                    },
                    designator: "R10".to_string(),
                    value: Some("10k".to_string()),
                    package: Some("R_0603".to_string()),
                    bom_category: Some(BomCategory::Electrical),
                    part: "R10k".to_string(),
                    layer_ref: "F.Cu".to_string(),
                    side: PlacementSide::Top,
                    mount: PlacementMount::Smt,
                    at: Point::new(1.0, -2.5),
                    rotation_degrees: 270.0,
                    mirror: false,
                    face_up: false,
                    scale: 1.0,
                    population: Population::Populate,
                },
                Placement {
                    id: ComponentOccurrenceId {
                        component: ComponentDefinitionId(1),
                        layout: LayoutOccurrenceId::Root,
                    },
                    designator: "R2".to_string(),
                    value: Some("1k".to_string()),
                    package: Some("R_0603".to_string()),
                    bom_category: Some(BomCategory::Electrical),
                    part: "R1k".to_string(),
                    layer_ref: "B.Cu".to_string(),
                    side: PlacementSide::Bottom,
                    mount: PlacementMount::Smt,
                    at: Point::new(3.0, 4.0),
                    rotation_degrees: 0.0,
                    mirror: true,
                    face_up: false,
                    scale: 1.0,
                    population: Population::DoNotPopulate,
                },
                Placement {
                    id: ComponentOccurrenceId {
                        component: ComponentDefinitionId(2),
                        layout: LayoutOccurrenceId::Root,
                    },
                    designator: "TP1".to_string(),
                    value: None,
                    package: Some("TestPoint_ICT".to_string()),
                    bom_category: Some(BomCategory::Document),
                    part: "TP_GND".to_string(),
                    layer_ref: "B.Cu".to_string(),
                    side: PlacementSide::Bottom,
                    mount: PlacementMount::Smt,
                    at: Point::new(5.0, 6.0),
                    rotation_degrees: 0.0,
                    mirror: true,
                    face_up: false,
                    scale: 1.0,
                    population: Population::Populate,
                },
            ],
        };

        let csv = emit_cpl_csv(
            &document,
            &CplOptions {
                output: None,
                side: CplSideFilter::Both,
                exclude_dnp: false,
                format: CplFormat::Release,
            },
        )
        .unwrap();

        // A populated DOCUMENT part (KiCad's padless parts) is still placed.
        assert_eq!(
            csv,
            "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n\
R10,10k,R_0603,1.000000,-2.500000,-90.000000,top\n\
R2,1k,R_0603,3.000000,4.000000,180.000000,bottom\n\
TP1,,TestPoint_ICT,5.000000,6.000000,180.000000,bottom\n"
        );

        document.components[0].rotation_degrees = 359.6;
        document.components[2].population = Population::DoNotPopulate;
        let jlc = emit_cpl_csv(
            &document,
            &CplOptions {
                output: None,
                side: CplSideFilter::Both,
                exclude_dnp: true,
                format: CplFormat::Jlc,
            },
        )
        .unwrap();
        assert_eq!(
            jlc,
            "Designator,Mid X,Mid Y,Layer,Rotation\n\
R10,1.0000mm,-2.5000mm,Top,0\n"
        );
    }
}
