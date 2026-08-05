//! Laser depaneling cut lines.
//!
//! V-scores run the full width and height of the array, so cutting all of them
//! shatters the panel into a grid of boards and scrap. A laser only has to
//! follow each board's own perimeter, which is always a subset of those score
//! lines. Cutting just those segments frees every board and leaves the rails
//! and scrap webbing intact.

use super::*;

pub(super) fn add_laser_cut_lines(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    lines: Vec<VcutLine>,
) {
    if lines.is_empty() {
        return;
    }

    let layer_name = reserve_unique_name(used_layer_names, LASER_CUT_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        layer_name.clone(),
        LayerFunction::Rout,
        Some(Side::None),
        Some(Polarity::Positive),
    ));
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        layer_name,
        Polarity::Positive,
        lines.into_iter().map(laser_cut_line_feature).collect(),
    );
}

fn laser_cut_line_feature(line: VcutLine) -> SetFeature {
    SetFeature::Line(Line {
        start_x: line.start_x_mm,
        start_y: line.start_y_mm,
        end_x: line.end_x_mm,
        end_y: line.end_y_mm,
        line_desc_ref: None,
        line_width: LASER_CUT_STROKE_MM,
        line_end: Some(LineEnd::Round),
        line_property: Some(LineProperty::Solid),
    })
}

pub(super) struct LaserCutLineSpec {
    pub(super) columns: u32,
    pub(super) rows: u32,
    pub(super) board_width_mm: f64,
    pub(super) board_height_mm: f64,
    pub(super) margin_x_mm: f64,
    pub(super) margin_y_mm: f64,
    pub(super) pitch_x_mm: f64,
    pub(super) pitch_y_mm: f64,
}

/// Board-perimeter segments, merged so butted boards share a single cut.
pub(super) fn laser_cut_lines(spec: LaserCutLineSpec) -> Vec<VcutLine> {
    let mut vertical: Vec<(f64, f64, f64)> = Vec::new();
    let mut horizontal: Vec<(f64, f64, f64)> = Vec::new();

    for row in 0..spec.rows {
        for column in 0..spec.columns {
            let left = spec.margin_x_mm + f64::from(column) * spec.pitch_x_mm;
            let bottom = spec.margin_y_mm + f64::from(row) * spec.pitch_y_mm;
            let right = left + spec.board_width_mm;
            let top = bottom + spec.board_height_mm;
            vertical.push((left, bottom, top));
            vertical.push((right, bottom, top));
            horizontal.push((bottom, left, right));
            horizontal.push((top, left, right));
        }
    }

    let mut lines = Vec::new();
    for (x, start, end) in merge_collinear_spans(vertical) {
        lines.push(VcutLine {
            start_x_mm: x,
            start_y_mm: start,
            end_x_mm: x,
            end_y_mm: end,
        });
    }
    for (y, start, end) in merge_collinear_spans(horizontal) {
        lines.push(VcutLine {
            start_x_mm: start,
            start_y_mm: y,
            end_x_mm: end,
            end_y_mm: y,
        });
    }
    lines
}

/// Collapse `(offset, start, end)` spans that share an offset and touch or overlap.
fn merge_collinear_spans(mut spans: Vec<(f64, f64, f64)>) -> Vec<(f64, f64, f64)> {
    spans.sort_by(|left, right| {
        f64::total_cmp(&left.0, &right.0).then_with(|| f64::total_cmp(&left.1, &right.1))
    });

    let mut merged: Vec<(f64, f64, f64)> = Vec::new();
    for (offset, start, end) in spans {
        match merged.last_mut() {
            Some(last)
                if (last.0 - offset).abs() <= EPSILON
                    && start <= last.2 + EPSILON
                    && end > last.2 =>
            {
                last.2 = end;
            }
            Some(last) if (last.0 - offset).abs() <= EPSILON && start <= last.2 + EPSILON => {}
            _ => merged.push((offset, start, end)),
        }
    }
    merged
}
