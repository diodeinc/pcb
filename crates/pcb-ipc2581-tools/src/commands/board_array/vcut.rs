//! V-cut score lines, callouts, and stroke-font labels.

use super::*;

pub(super) fn add_vcut_lines(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    vcut_spec_name: String,
    array_width_mm: f64,
    lines: Vec<VcutLine>,
) {
    if lines.is_empty() {
        return;
    }

    let layer_name = reserve_unique_name(used_layer_names, VCUT_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        layer_name.clone(),
        LayerFunction::VCut,
        Some(Side::None),
        Some(Polarity::Positive),
    ));
    generated_geometry.add_layer_feature_with_spec_refs(
        GeneratedFeatureScope::Array,
        layer_name.clone(),
        Polarity::Positive,
        vec![vcut_spec_name],
        lines.iter().copied().map(vcut_line_feature).collect(),
    );
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        layer_name,
        Polarity::Positive,
        vcut_callout_features(&lines, array_width_mm),
    );
}

pub(super) fn vcut_line_feature(line: VcutLine) -> SetFeature {
    SetFeature::Line(Line {
        start_x: line.start_x_mm,
        start_y: line.start_y_mm,
        end_x: line.end_x_mm,
        end_y: line.end_y_mm,
        line_desc_ref: None,
        line_width: VCUT_MARKER_STROKE_MM,
        line_end: Some(LineEnd::Round),
        line_property: Some(LineProperty::Solid),
    })
}

pub(super) fn vcut_callout_features(lines: &[VcutLine], array_width_mm: f64) -> Vec<SetFeature> {
    let mut features = Vec::new();
    let label = vcut_label_geometry();
    for line in lines {
        if (line.start_x_mm - line.end_x_mm).abs() <= EPSILON {
            add_bottom_vcut_callout(&mut features, line.start_x_mm, &label);
        } else if (line.start_y_mm - line.end_y_mm).abs() <= EPSILON {
            add_right_vcut_callout(&mut features, array_width_mm, line.start_y_mm, &label);
        }
    }
    features
}

pub(super) fn add_bottom_vcut_callout(
    features: &mut Vec<SetFeature>,
    x: f64,
    label: &VcutLabelGeometry,
) {
    let arrow_tip = Point::new(x, -VCUT_CALLOUT_ARROW_CLEARANCE_MM);
    let arrow_start = Point::new(
        x,
        -(VCUT_CALLOUT_ARROW_CLEARANCE_MM + VCUT_CALLOUT_ARROW_LENGTH_MM),
    );
    add_vcut_annotation_line(features, arrow_start, arrow_tip, VCUT_MARKER_STROKE_MM);
    add_vcut_annotation_line(
        features,
        arrow_tip,
        Point::new(
            x - VCUT_CALLOUT_ARROW_HEAD_MM,
            arrow_tip.y - VCUT_CALLOUT_ARROW_HEAD_MM,
        ),
        VCUT_MARKER_STROKE_MM,
    );
    add_vcut_annotation_line(
        features,
        arrow_tip,
        Point::new(
            x + VCUT_CALLOUT_ARROW_HEAD_MM,
            arrow_tip.y - VCUT_CALLOUT_ARROW_HEAD_MM,
        ),
        VCUT_MARKER_STROKE_MM,
    );

    add_vcut_label(
        features,
        label,
        Point::new(
            x - 0.5 * label.width_mm,
            arrow_start.y - VCUT_CALLOUT_TEXT_GAP_MM - label.height_mm,
        ),
    );
}

pub(super) fn add_right_vcut_callout(
    features: &mut Vec<SetFeature>,
    array_width_mm: f64,
    y: f64,
    label: &VcutLabelGeometry,
) {
    let arrow_tip = Point::new(array_width_mm + VCUT_CALLOUT_ARROW_CLEARANCE_MM, y);
    let arrow_start = Point::new(
        array_width_mm + VCUT_CALLOUT_ARROW_CLEARANCE_MM + VCUT_CALLOUT_ARROW_LENGTH_MM,
        y,
    );
    add_vcut_annotation_line(features, arrow_start, arrow_tip, VCUT_MARKER_STROKE_MM);
    add_vcut_annotation_line(
        features,
        arrow_tip,
        Point::new(
            arrow_tip.x + VCUT_CALLOUT_ARROW_HEAD_MM,
            y - VCUT_CALLOUT_ARROW_HEAD_MM,
        ),
        VCUT_MARKER_STROKE_MM,
    );
    add_vcut_annotation_line(
        features,
        arrow_tip,
        Point::new(
            arrow_tip.x + VCUT_CALLOUT_ARROW_HEAD_MM,
            y + VCUT_CALLOUT_ARROW_HEAD_MM,
        ),
        VCUT_MARKER_STROKE_MM,
    );

    add_vcut_label(
        features,
        label,
        Point::new(
            arrow_start.x + VCUT_CALLOUT_TEXT_GAP_MM,
            y - 0.5 * label.height_mm,
        ),
    );
}

pub(super) fn add_vcut_label(
    features: &mut Vec<SetFeature>,
    label: &VcutLabelGeometry,
    lower_left: Point,
) {
    for line in &label.lines {
        add_vcut_annotation_line(
            features,
            Point::new(lower_left.x + line.start.x, lower_left.y + line.start.y),
            Point::new(lower_left.x + line.end.x, lower_left.y + line.end.y),
            VCUT_CALLOUT_TEXT_STROKE_MM,
        );
    }
}

#[derive(Debug, Clone)]
pub(super) struct VcutLabelGeometry {
    lines: Vec<VcutLabelLine>,
    width_mm: f64,
    height_mm: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct VcutLabelLine {
    start: Point,
    end: Point,
}

pub(super) fn vcut_label_geometry() -> VcutLabelGeometry {
    let mut strokes = Vec::new();
    let mut cursor = 0.0;

    for raw_glyph in KICAD_VCUT_LABEL_GLYPHS {
        let glyph = parse_kicad_stroke_glyph(raw_glyph);
        strokes.extend(glyph.strokes.into_iter().map(|stroke| {
            stroke
                .into_iter()
                .map(|point| Point::new(cursor + point.x, point.y))
                .collect::<Vec<_>>()
        }));
        cursor += glyph.width;
    }

    let (min_x, min_y, max_x, max_y) = strokes
        .iter()
        .flatten()
        .fold(None, |bounds, point| match bounds {
            Some((min_x, min_y, max_x, max_y)) => Some((
                f64::min(min_x, point.x),
                f64::min(min_y, point.y),
                f64::max(max_x, point.x),
                f64::max(max_y, point.y),
            )),
            None => Some((point.x, point.y, point.x, point.y)),
        })
        .expect("KiCad V-cut label glyphs should produce strokes");
    let scale = VCUT_CALLOUT_TEXT_HEIGHT_MM / (max_y - min_y);
    let mut lines = Vec::new();
    for stroke in strokes {
        lines.extend(stroke.windows(2).map(|points| VcutLabelLine {
            start: Point::new((points[0].x - min_x) * scale, (max_y - points[0].y) * scale),
            end: Point::new((points[1].x - min_x) * scale, (max_y - points[1].y) * scale),
        }));
    }

    VcutLabelGeometry {
        lines,
        width_mm: (max_x - min_x) * scale,
        height_mm: VCUT_CALLOUT_TEXT_HEIGHT_MM,
    }
}

#[derive(Debug)]
pub(super) struct KiCadStrokeGlyph {
    strokes: Vec<Vec<Point>>,
    width: f64,
}

pub(super) fn parse_kicad_stroke_glyph(raw: &str) -> KiCadStrokeGlyph {
    let bytes = raw.as_bytes();
    let glyph_start_x = f64::from(kicad_font_coord(bytes[0])) * KICAD_STROKE_FONT_SCALE;
    let glyph_end_x = f64::from(kicad_font_coord(bytes[1])) * KICAD_STROKE_FONT_SCALE;
    let mut strokes = Vec::new();
    let mut stroke = Vec::new();

    for pair in bytes[2..].chunks_exact(2) {
        if pair[0] == b' ' && pair[1] == b'R' {
            if stroke.len() >= 2 {
                strokes.push(std::mem::take(&mut stroke));
            } else {
                stroke.clear();
            }
            continue;
        }

        stroke.push(Point::new(
            f64::from(kicad_font_coord(pair[0])) * KICAD_STROKE_FONT_SCALE - glyph_start_x,
            f64::from(kicad_font_coord(pair[1]) + KICAD_STROKE_FONT_OFFSET)
                * KICAD_STROKE_FONT_SCALE,
        ));
    }

    if stroke.len() >= 2 {
        strokes.push(stroke);
    }

    KiCadStrokeGlyph {
        strokes,
        width: glyph_end_x - glyph_start_x,
    }
}

pub(super) fn kicad_font_coord(value: u8) -> i32 {
    i32::from(value) - i32::from(b'R')
}

pub(super) fn add_vcut_annotation_line(
    features: &mut Vec<SetFeature>,
    start: Point,
    end: Point,
    line_width: f64,
) {
    features.push(SetFeature::Line(Line {
        start_x: start.x,
        start_y: start.y,
        end_x: end.x,
        end_y: end.y,
        line_desc_ref: None,
        line_width,
        line_end: Some(LineEnd::Round),
        line_property: Some(LineProperty::Solid),
    }));
}

pub(super) struct VcutLineSpec {
    pub(super) columns: u32,
    pub(super) rows: u32,
    pub(super) board_width_mm: f64,
    pub(super) board_height_mm: f64,
    pub(super) margin_x_mm: f64,
    pub(super) margin_y_mm: f64,
    pub(super) pitch_x_mm: f64,
    pub(super) pitch_y_mm: f64,
    pub(super) array_width_mm: f64,
    pub(super) array_height_mm: f64,
}

pub(super) fn vcut_lines(spec: VcutLineSpec) -> Result<Vec<VcutLine>> {
    let x_positions = board_edge_positions(
        spec.columns,
        spec.margin_x_mm,
        spec.pitch_x_mm,
        spec.board_width_mm,
        spec.array_width_mm,
    );
    validate_vcut_line_count("X", x_positions.len())?;

    let y_positions = board_edge_positions(
        spec.rows,
        spec.margin_y_mm,
        spec.pitch_y_mm,
        spec.board_height_mm,
        spec.array_height_mm,
    );
    validate_vcut_line_count("Y", y_positions.len())?;

    let mut lines = Vec::new();
    for x in x_positions {
        lines.push(VcutLine {
            start_x_mm: x,
            start_y_mm: 0.0,
            end_x_mm: x,
            end_y_mm: spec.array_height_mm,
        });
    }
    for y in y_positions {
        lines.push(VcutLine {
            start_x_mm: 0.0,
            start_y_mm: y,
            end_x_mm: spec.array_width_mm,
            end_y_mm: y,
        });
    }
    Ok(lines)
}

pub(super) fn validate_vcut_line_count(axis: &'static str, count: usize) -> Result<()> {
    if count <= MAX_VCUT_LINES_PER_AXIS {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::VcutLineCount {
            axis,
            count,
            max: MAX_VCUT_LINES_PER_AXIS,
        }
        .into())
    }
}

pub(super) fn board_edge_positions(
    count: u32,
    margin: f64,
    pitch: f64,
    size: f64,
    panel_size: f64,
) -> Vec<f64> {
    let mut positions = Vec::new();
    for index in 0..count {
        let start = margin + index as f64 * pitch;
        positions.push(start);
        positions.push(start + size);
    }
    positions.retain(|position| {
        position.is_finite() && *position > EPSILON && *position < panel_size - EPSILON
    });
    positions.sort_by(f64::total_cmp);
    positions.dedup_by(|left, right| (*left - *right).abs() <= EPSILON);
    positions
}
