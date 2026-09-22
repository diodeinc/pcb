//! V-cut score lines, callouts, and stroke-font labels.

use super::*;

/// Score the array along every board edge, through the rails, and call each
/// line out below or to the right of the array.
pub(super) fn add_vcut_lines(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    vcut_spec_name: &str,
    grid: &ArrayGrid,
) {
    let xs = board_edge_positions(
        grid.columns,
        grid.margin_x_mm,
        grid.pitch_x_mm,
        grid.board_width_mm,
        grid.array_width_mm,
    );
    let ys = board_edge_positions(
        grid.rows,
        grid.margin_y_mm,
        grid.pitch_y_mm,
        grid.board_height_mm,
        grid.array_height_mm,
    );
    if xs.is_empty() && ys.is_empty() {
        return;
    }

    let layer_name = reserve_unique_name(used_layer_names, VCUT_LAYER_BASE_NAME);
    generated_geometry.layers.push(GeneratedLayer {
        name: layer_name.clone(),
        layer_function: LayerFunction::VCut,
        side: Side::None,
        span: None,
    });
    let (width, height) = (grid.array_width_mm, grid.array_height_mm);
    let score = |start, end| solid_line_feature(start, end, VCUT_MARKER_STROKE_MM);
    let vertical = xs
        .iter()
        .map(|&x| score(Point::new(x, 0.0), Point::new(x, height)));
    let horizontal = ys
        .iter()
        .map(|&y| score(Point::new(0.0, y), Point::new(width, y)));
    generated_geometry
        .add_layer_feature(
            GeneratedFeatureScope::Array,
            &layer_name,
            vertical.chain(horizontal).collect(),
        )
        .spec_refs = vec![vcut_spec_name.to_string()];

    let label = vcut_label_geometry();
    let mut callouts = Vec::new();
    for &x in &xs {
        add_bottom_vcut_callout(&mut callouts, x, &label);
    }
    for &y in &ys {
        add_right_vcut_callout(&mut callouts, width, y, &label);
    }
    generated_geometry.add_layer_feature(GeneratedFeatureScope::Array, &layer_name, callouts);
}

fn solid_line_feature(start: Point, end: Point, line_width: f64) -> SetFeature {
    let point = |point: Point| IpcPoint {
        x: point.x,
        y: point.y,
    };
    SetFeature::Stroke(Stroke {
        path: StrokePath::Line(Line {
            start: point(start),
            end: point(end),
        }),
        line_desc: Some(LineDescGroup::Inline(LineDesc {
            line_width,
            line_end: LineEnd::Round,
            line_property: Some(LineProperty::Solid),
        })),
    })
}

fn add_bottom_vcut_callout(features: &mut Vec<SetFeature>, x: f64, label: &VcutLabelGeometry) {
    let arrow_tip = Point::new(x, -VCUT_CALLOUT_ARROW_CLEARANCE_MM);
    let arrow_start = Point::new(
        x,
        -(VCUT_CALLOUT_ARROW_CLEARANCE_MM + VCUT_CALLOUT_ARROW_LENGTH_MM),
    );
    let head_y = arrow_tip.y - VCUT_CALLOUT_ARROW_HEAD_MM;
    add_vcut_callout(
        features,
        label,
        [arrow_start, arrow_tip],
        [
            Point::new(x - VCUT_CALLOUT_ARROW_HEAD_MM, head_y),
            Point::new(x + VCUT_CALLOUT_ARROW_HEAD_MM, head_y),
        ],
        Point::new(
            x - 0.5 * label.width_mm,
            arrow_start.y - VCUT_CALLOUT_TEXT_GAP_MM - label.height_mm,
        ),
    );
}

fn add_right_vcut_callout(
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
    let head_x = arrow_tip.x + VCUT_CALLOUT_ARROW_HEAD_MM;
    add_vcut_callout(
        features,
        label,
        [arrow_start, arrow_tip],
        [
            Point::new(head_x, y - VCUT_CALLOUT_ARROW_HEAD_MM),
            Point::new(head_x, y + VCUT_CALLOUT_ARROW_HEAD_MM),
        ],
        Point::new(
            arrow_start.x + VCUT_CALLOUT_TEXT_GAP_MM,
            y - 0.5 * label.height_mm,
        ),
    );
}

/// An arrow from `shaft[0]` to its tip at `shaft[1]`, the two barbs of its
/// head, and the label with its lower-left corner at `label_at`.
fn add_vcut_callout(
    features: &mut Vec<SetFeature>,
    label: &VcutLabelGeometry,
    shaft: [Point; 2],
    barbs: [Point; 2],
    label_at: Point,
) {
    let marker = |start, end| solid_line_feature(start, end, VCUT_MARKER_STROKE_MM);
    features.push(marker(shaft[0], shaft[1]));
    features.extend(barbs.map(|barb| marker(shaft[1], barb)));
    features.extend(label.lines.iter().map(|[start, end]| {
        solid_line_feature(
            Point::new(label_at.x + start.x, label_at.y + start.y),
            Point::new(label_at.x + end.x, label_at.y + end.y),
            VCUT_CALLOUT_TEXT_STROKE_MM,
        )
    }));
}

/// The "V-CUT" label as stroke-font segments from its lower-left corner.
struct VcutLabelGeometry {
    lines: Vec<[Point; 2]>,
    width_mm: f64,
    height_mm: f64,
}

fn vcut_label_geometry() -> VcutLabelGeometry {
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
    let place = |point: Point| Point::new((point.x - min_x) * scale, (max_y - point.y) * scale);
    let lines = strokes
        .iter()
        .flat_map(|stroke| stroke.windows(2))
        .map(|points| [place(points[0]), place(points[1])])
        .collect();

    VcutLabelGeometry {
        lines,
        width_mm: (max_x - min_x) * scale,
        height_mm: VCUT_CALLOUT_TEXT_HEIGHT_MM,
    }
}

#[derive(Debug)]
struct KiCadStrokeGlyph {
    strokes: Vec<Vec<Point>>,
    width: f64,
}

fn parse_kicad_stroke_glyph(raw: &str) -> KiCadStrokeGlyph {
    let bytes = raw.as_bytes();
    let glyph_start_x = f64::from(kicad_font_coord(bytes[0])) * KICAD_STROKE_FONT_SCALE;
    let glyph_end_x = f64::from(kicad_font_coord(bytes[1])) * KICAD_STROKE_FONT_SCALE;
    let mut strokes = Vec::new();
    let mut stroke = Vec::new();

    for pair in bytes[2..].as_chunks::<2>().0 {
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

fn kicad_font_coord(value: u8) -> i32 {
    i32::from(value) - i32::from(b'R')
}

fn board_edge_positions(
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
