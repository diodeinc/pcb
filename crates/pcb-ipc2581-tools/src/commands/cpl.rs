use std::cmp::Ordering;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::ValueEnum;
use ipc2581::Ipc2581;
use pcb_ir::dialects::placement::{Document as PlacementDocument, Placement, PlacementSide};

use crate::accessors::IpcAccessor;
use crate::placement::extract_single_board_placements;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CplSideFilter {
    Both,
    Top,
    Bottom,
}

#[derive(Debug, Clone)]
pub struct CplOptions {
    pub output: Option<PathBuf>,
    pub side: CplSideFilter,
    pub exclude_dnp: bool,
}

pub fn execute(file: &Path, options: &CplOptions) -> Result<()> {
    let ipc = Ipc2581::parse_file(file)?;
    let accessor = IpcAccessor::new(&ipc);
    let placements = extract_single_board_placements(&accessor)?;
    let cpl = emit_cpl_csv(&placements, options);

    if let Some(output) = &options.output {
        fs::write(output, cpl)?;
    } else {
        io::stdout().write_all(cpl.as_bytes())?;
    }

    Ok(())
}

pub fn emit_cpl_csv(document: &PlacementDocument, options: &CplOptions) -> String {
    let mut rows = document
        .components
        .iter()
        .filter(|component| include_component(component, options))
        .collect::<Vec<_>>();
    rows.sort_by(compare_components);

    let mut output = String::from("Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n");
    for component in rows {
        write_csv_row(
            &mut output,
            &[
                component.designator.as_str(),
                component.value.as_deref().unwrap_or_default(),
                component.package.as_deref().unwrap_or_default(),
                &format_number(component.at.x),
                &format_number(component.at.y),
                &format_number(normalize_rotation(component.rotation_degrees)),
                cpl_layer(component.side),
            ],
        );
    }

    output
}

fn cpl_layer(side: PlacementSide) -> &'static str {
    match side {
        PlacementSide::Top => "top",
        PlacementSide::Bottom => "bottom",
        PlacementSide::Internal => "internal",
        PlacementSide::Unknown => "unknown",
    }
}

fn include_component(component: &Placement, options: &CplOptions) -> bool {
    if options.exclude_dnp && component.populate == Some(false) {
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
        PlacementSide::Unknown => 3,
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
    use pcb_ir::dialects::placement::{PlacementMount, PlacementSide};
    use pcb_ir::geom::Point;

    use super::*;

    #[test]
    fn emits_release_cpl_header_and_rows() {
        let document = PlacementDocument {
            components: vec![
                Placement {
                    designator: "R10".to_string(),
                    value: Some("10k".to_string()),
                    package: Some("R_0603".to_string()),
                    part: "R10k".to_string(),
                    layer_ref: "F.Cu".to_string(),
                    side: PlacementSide::Top,
                    mount: PlacementMount::Smt,
                    at: Point::new(1.0, -2.5),
                    rotation_degrees: 270.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    mirror: false,
                    face_up: false,
                    scale: 1.0,
                    populate: Some(true),
                },
                Placement {
                    designator: "R2".to_string(),
                    value: Some("1k".to_string()),
                    package: Some("R_0603".to_string()),
                    part: "R1k".to_string(),
                    layer_ref: "B.Cu".to_string(),
                    side: PlacementSide::Bottom,
                    mount: PlacementMount::Smt,
                    at: Point::new(3.0, 4.0),
                    rotation_degrees: 90.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    mirror: true,
                    face_up: false,
                    scale: 1.0,
                    populate: Some(false),
                },
            ],
        };

        let csv = emit_cpl_csv(
            &document,
            &CplOptions {
                output: None,
                side: CplSideFilter::Both,
                exclude_dnp: false,
            },
        );

        assert_eq!(
            csv,
            "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n\
R10,10k,R_0603,1.000000,-2.500000,-90.000000,top\n\
R2,1k,R_0603,3.000000,4.000000,90.000000,bottom\n"
        );
    }
}
