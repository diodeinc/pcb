use super::primitives::{
    add_arc_segment, add_circle_as_cubics, add_corner_arc_for_rounded_rect, add_ellipse_as_cubics,
    add_oval_as_stadium, RectCorner,
};
use super::resolved_feature::*;
use super::Result;
use crate::{Polarity, Symbol};
use skia_safe::Path;
use std::collections::HashMap;

/// Stage 3: Convert ResolvedGeometry to Skia Paths
///
/// Takes the resolved features from Stage 2 and converts each geometry
/// to a concrete Skia path, preserving all geometric accuracy including
/// arc segments, stroke styles, and parametric shapes.

/// A feature after path conversion (Stage 3 output)
#[derive(Debug, Clone)]
pub struct PathFeature {
    /// Classification bucket for styling
    pub bucket: FeatureBucket,

    /// Net name symbol (if electrical feature)
    pub net: Option<Symbol>,

    /// Polarity (add or remove copper)
    pub polarity: Polarity,

    /// Skia path representing the geometry
    pub path: Path,

    /// Accurate bounding box from path.bounds()
    pub bbox: BoundingBox,
}

/// Converted paths for a single layer
#[derive(Debug, Clone)]
pub struct LayerPaths {
    pub layer_name: String,
    pub features: Vec<PathFeature>,
    pub bbox: BoundingBox,
    pub stats: LayerStats,
}

/// Convert all layers from Stage 2 resolved geometry to Skia paths
pub fn convert_to_paths(
    layers: HashMap<String, LayerResolution>,
) -> Result<HashMap<String, LayerPaths>> {
    let mut result = HashMap::new();

    for (layer_name, resolution) in layers {
        let mut features = Vec::new();
        let mut bbox = BoundingBox::empty();
        let stats = resolution.stats.clone(); // Preserve stats from Stage 2

        for resolved_feature in resolution.features {
            let path_features = convert_geometry(&resolved_feature.geometry)?;

            for path in path_features {
                let feature_bbox = path_to_bbox(&path);
                bbox = bbox.union(&feature_bbox);

                features.push(PathFeature {
                    bucket: resolved_feature.bucket,
                    net: resolved_feature.net,
                    polarity: resolved_feature.polarity,
                    path,
                    bbox: feature_bbox,
                });
            }
        }

        result.insert(
            layer_name.clone(),
            LayerPaths {
                layer_name,
                features,
                bbox,
                stats,
            },
        );
    }

    Ok(result)
}

/// Convert a single ResolvedGeometry to one or more Skia paths
///
/// Most geometries convert to a single path, but Group variants expand
/// to multiple paths.
fn convert_geometry(geometry: &ResolvedGeometry) -> Result<Vec<Path>> {
    match geometry {
        ResolvedGeometry::Circle {
            center,
            diameter,
            filled,
            line_width,
        } => Ok(vec![convert_circle(
            *center,
            *diameter,
            *filled,
            *line_width,
        )]),

        ResolvedGeometry::Rectangle {
            center,
            width,
            height,
            filled,
            line_width,
        } => Ok(vec![convert_rectangle(
            *center,
            *width,
            *height,
            *filled,
            *line_width,
        )]),

        ResolvedGeometry::RoundedRectangle {
            center,
            width,
            height,
            radius,
            corners,
            rotation,
        } => Ok(vec![convert_rounded_rectangle(
            *center, *width, *height, *radius, *corners, *rotation,
        )]),

        ResolvedGeometry::ChamferedRectangle {
            center,
            width,
            height,
            chamfer,
            corners,
            rotation,
        } => Ok(vec![convert_chamfered_rectangle(
            *center, *width, *height, *chamfer, *corners, *rotation,
        )]),

        ResolvedGeometry::Ellipse {
            center,
            width,
            height,
            rotation,
        } => Ok(vec![convert_ellipse(*center, *width, *height, *rotation)]),

        ResolvedGeometry::Oval {
            center,
            width,
            height,
            rotation,
        } => Ok(vec![convert_oval(*center, *width, *height, *rotation)]),

        ResolvedGeometry::Donut {
            center,
            outer_diameter,
            inner_diameter,
        } => Ok(vec![convert_donut(
            *center,
            *outer_diameter,
            *inner_diameter,
        )]),

        ResolvedGeometry::Thermal {
            center,
            outer_diameter,
            inner_diameter,
            gap,
            spokes,
            rotation,
        } => Ok(vec![convert_thermal(
            *center,
            *outer_diameter,
            *inner_diameter,
            *gap,
            *spokes,
            *rotation,
        )]),

        ResolvedGeometry::Polygon {
            points,
            arc_segments,
            cutouts,
            cutout_arcs,
        } => Ok(vec![convert_polygon(
            points,
            arc_segments,
            cutouts,
            cutout_arcs,
        )]),

        ResolvedGeometry::Polyline {
            points,
            arc_segments,
            line_width,
            line_end,
        } => Ok(vec![convert_polyline(points, arc_segments, *line_width, *line_end)]),

        ResolvedGeometry::Group { geometries } => {
            // Recursively convert all geometries in the group
            let mut paths = Vec::new();
            for geom in geometries {
                paths.extend(convert_geometry(geom)?);
            }
            Ok(paths)
        }

        ResolvedGeometry::PadstackRef {
            padstack_name,
            center,
            layer,
            ..
        } => {
            // This should never happen - Stage 2 should have expanded all padstacks
            eprintln!(
                "ERROR: Unexpanded PadstackRef '{}' at ({:.3}, {:.3}) on layer {} - Stage 2 expansion failed",
                padstack_name, center.x, center.y, layer
            );
            Err(crate::Ipc2581Error::MissingElement(
                "Unexpanded PadstackRef in Stage 3",
            ))
        }
    }
}

// ============================================================================
// Geometry Conversion Functions
// ============================================================================

// Constants for geometry conversion
const ROTATION_EPSILON: f64 = 1e-6; // Rotation threshold

/// Convert Point to Skia coordinate tuple
#[inline]
fn to_skia_point(p: Point) -> (f32, f32) {
    (p.x as f32, p.y as f32)
}

/// Convert Skia path bounds to BoundingBox
#[inline]
fn path_to_bbox(path: &Path) -> BoundingBox {
    let bounds = path.bounds();
    BoundingBox {
        min_x: bounds.left as f64,
        min_y: bounds.top as f64,
        max_x: bounds.right as f64,
        max_y: bounds.bottom as f64,
    }
}

/// Apply rotation and translation transform to a path
///
/// Rotates path around origin (if rotation is significant), then translates to center position.
fn apply_transform(path: &mut Path, center: Point, rotation: f64) {
    // Apply rotation around origin if needed
    if rotation.abs() > ROTATION_EPSILON {
        let matrix = skia_safe::Matrix::rotate_deg(rotation as f32);
        path.transform(&matrix);
    }

    // Translate to center position
    let translate = skia_safe::Matrix::translate(to_skia_point(center));
    path.transform(&translate);
}

fn convert_circle(center: Point, diameter: f64, filled: bool, line_width: Option<f64>) -> Path {
    let mut path = Path::new();

    let center_pt = to_skia_point(center);

    if !filled {
        // HOLLOW circle - render as annular ring with LineDesc width
        let stroke_width = line_width.expect("HOLLOW circle missing line_width from LineDesc");
        let radius = diameter / 2.0;
        let inner_radius = ((radius - stroke_width / 2.0).max(0.0)) as f32;
        let outer_radius = (radius + stroke_width / 2.0) as f32;

        path.set_fill_type(skia_safe::path::FillType::EvenOdd);

        // Use cubic beziers for smooth circles
        add_circle_as_cubics(&mut path, center_pt, outer_radius);
        add_circle_as_cubics(&mut path, center_pt, inner_radius);
    } else {
        // Filled circle using cubic beziers for smooth quality
        let radius = (diameter / 2.0) as f32;
        add_circle_as_cubics(&mut path, center_pt, radius);
    }

    path
}

fn convert_rectangle(
    center: Point,
    width: f64,
    height: f64,
    filled: bool,
    line_width: Option<f64>,
) -> Path {
    let mut path = Path::new();

    if !filled {
        // HOLLOW rectangle - render as two rectangles with LineDesc width
        let stroke_width = line_width.expect("HOLLOW rectangle missing line_width from LineDesc");
        let half_w = width / 2.0;
        let half_h = height / 2.0;

        // Outer rectangle
        let outer_rect = skia_safe::Rect::from_xywh(
            (center.x - half_w) as f32,
            (center.y - half_h) as f32,
            width as f32,
            height as f32,
        );

        // Inner rectangle (inset by stroke width)
        let inner_half_w = ((half_w - stroke_width).max(0.0)) as f32;
        let inner_half_h = ((half_h - stroke_width).max(0.0)) as f32;
        let inner_rect = skia_safe::Rect::from_xywh(
            (center.x - inner_half_w as f64) as f32,
            (center.y - inner_half_h as f64) as f32,
            inner_half_w * 2.0,
            inner_half_h * 2.0,
        );

        path.set_fill_type(skia_safe::path::FillType::EvenOdd);
        path.add_rect(outer_rect, None);
        path.add_rect(inner_rect, None);
    } else {
        // Filled rectangle
        let half_w = width / 2.0;
        let half_h = height / 2.0;
        let rect = skia_safe::Rect::from_xywh(
            (center.x - half_w) as f32,
            (center.y - half_h) as f32,
            width as f32,
            height as f32,
        );
        path.add_rect(rect, None);
    }

    path
}

fn convert_rounded_rectangle(
    center: Point,
    width: f64,
    height: f64,
    radius: f64,
    corners: [bool; 4],
    rotation: f64,
) -> Path {
    let mut path = Path::new();

    // Build rounded rectangle with cubic bezier corners
    // corners: [upper_right, upper_left, lower_right, lower_left]
    let half_w = (width / 2.0) as f32;
    let half_h = (height / 2.0) as f32;
    let r = radius as f32;

    // Start at top-left corner (after rounding)
    let start_x = -half_w + if corners[1] { r } else { 0.0 };
    path.move_to((start_x, -half_h));

    // Top edge to where upper-right corner starts
    if corners[0] {
        path.line_to((half_w - r, -half_h));
    } else {
        path.line_to((half_w, -half_h));
    }

    // Upper-right corner arc (if rounded)
    if corners[0] {
        add_corner_arc_for_rounded_rect(&mut path, RectCorner::UpperRight, (half_w - r, -half_h + r), r);
    }

    // Right edge to where lower-right corner starts
    if corners[2] {
        path.line_to((half_w, half_h - r));
    } else {
        path.line_to((half_w, half_h));
    }

    // Lower-right corner arc (if rounded)
    if corners[2] {
        add_corner_arc_for_rounded_rect(&mut path, RectCorner::LowerRight, (half_w - r, half_h - r), r);
    }

    // Bottom edge to where lower-left corner starts
    if corners[3] {
        path.line_to((-half_w + r, half_h));
    } else {
        path.line_to((-half_w, half_h));
    }

    // Lower-left corner arc (if rounded)
    if corners[3] {
        add_corner_arc_for_rounded_rect(&mut path, RectCorner::LowerLeft, (-half_w + r, half_h - r), r);
    }

    // Left edge back to where upper-left corner starts
    if corners[1] {
        path.line_to((-half_w, -half_h + r));
    } else {
        path.line_to((-half_w, -half_h));
    }

    // Upper-left corner arc (if rounded)
    if corners[1] {
        add_corner_arc_for_rounded_rect(&mut path, RectCorner::UpperLeft, (-half_w + r, -half_h + r), r);
    }

    path.close();
    apply_transform(&mut path, center, rotation);
    path
}

fn convert_chamfered_rectangle(
    center: Point,
    width: f64,
    height: f64,
    chamfer: f64,
    corners: [bool; 4],
    rotation: f64,
) -> Path {
    let mut path = Path::new();

    // Build chamfered rectangle with per-corner chamfers
    // corners: [upper_right, upper_left, lower_right, lower_left]
    let half_w = (width / 2.0) as f32;
    let half_h = (height / 2.0) as f32;
    let c = chamfer as f32;

    // Start at top-left corner (after chamfer)
    let start_x = -half_w + if corners[1] { c } else { 0.0 };
    path.move_to((start_x, -half_h));

    // Top edge to upper-right corner
    let top_right_x = half_w - if corners[0] { c } else { 0.0 };
    path.line_to((top_right_x, -half_h));

    // Upper-right corner chamfer
    if corners[0] {
        path.line_to((half_w, -half_h + c));
    } else {
        path.line_to((half_w, -half_h));
    }

    // Right edge to lower-right corner
    let lower_right_y = half_h - if corners[2] { c } else { 0.0 };
    path.line_to((half_w, lower_right_y));

    // Lower-right corner chamfer
    if corners[2] {
        path.line_to((half_w - c, half_h));
    } else {
        path.line_to((half_w, half_h));
    }

    // Bottom edge to lower-left corner
    let lower_left_x = -half_w + if corners[3] { c } else { 0.0 };
    path.line_to((lower_left_x, half_h));

    // Lower-left corner chamfer
    if corners[3] {
        path.line_to((-half_w, half_h - c));
    } else {
        path.line_to((-half_w, half_h));
    }

    // Left edge back to top-left corner
    let top_left_y = -half_h + if corners[1] { c } else { 0.0 };
    path.line_to((-half_w, top_left_y));

    // Upper-left corner chamfer (close path)
    if corners[1] {
        path.line_to((start_x, -half_h));
    }

    path.close();
    apply_transform(&mut path, center, rotation);
    path
}

fn convert_ellipse(center: Point, width: f64, height: f64, rotation: f64) -> Path {
    let mut path = Path::new();

    // Create ellipse using cubic beziers (kappa approximation)
    let half_w = (width / 2.0) as f32;
    let half_h = (height / 2.0) as f32;
    add_ellipse_as_cubics(&mut path, (0.0, 0.0), half_w, half_h);

    apply_transform(&mut path, center, rotation);
    path
}

fn convert_oval(center: Point, width: f64, height: f64, rotation: f64) -> Path {
    let mut path = Path::new();

    // Create stadium shape at origin, then transform
    add_oval_as_stadium(&mut path, (0.0, 0.0), width, height);

    apply_transform(&mut path, center, rotation);
    path
}

fn convert_donut(center: Point, outer_diameter: f64, inner_diameter: f64) -> Path {
    let mut path = Path::new();
    path.set_fill_type(skia_safe::path::FillType::EvenOdd);

    let center_pt = to_skia_point(center);
    let outer_radius = (outer_diameter / 2.0) as f32;
    let inner_radius = (inner_diameter / 2.0) as f32;

    // Use cubic beziers for better quality through boolean ops
    add_circle_as_cubics(&mut path, center_pt, outer_radius);
    add_circle_as_cubics(&mut path, center_pt, inner_radius);
    path
}

fn convert_thermal(
    center: Point,
    outer_diameter: f64,
    inner_diameter: f64,
    gap: f64,
    spokes: u8,
    rotation: f64,
) -> Path {
    let mut path = Path::new();
    path.set_fill_type(skia_safe::path::FillType::EvenOdd);

    let outer_radius = (outer_diameter / 2.0) as f32;
    let inner_radius = (inner_diameter / 2.0) as f32;
    let half_gap = (gap / 2.0) as f32;

    // Guard against divide-by-zero
    if spokes == 0 {
        // No spokes = just a donut
        add_circle_as_cubics(&mut path, (0.0, 0.0), outer_radius);
        add_circle_as_cubics(&mut path, (0.0, 0.0), inner_radius);
        apply_transform(&mut path, center, rotation);
        return path;
    }

    // Add annular ring using cubic beziers
    add_circle_as_cubics(&mut path, (0.0, 0.0), outer_radius);
    add_circle_as_cubics(&mut path, (0.0, 0.0), inner_radius);

    // Add spoke gaps as rectangles
    let angle_step = 360.0 / spokes as f32;
    for i in 0..spokes {
        let spoke_angle = i as f32 * angle_step;

        // Create gap rectangle from center to outer edge
        let mut gap_rect = Path::new();
        gap_rect.add_rect(
            skia_safe::Rect::from_xywh(-half_gap, -outer_radius, gap as f32, outer_radius * 2.0),
            None,
        );

        // Rotate gap to spoke angle
        let matrix = skia_safe::Matrix::rotate_deg(spoke_angle);
        gap_rect.transform(&matrix);

        path.add_path(&gap_rect, (0.0, 0.0), None);
    }

    apply_transform(&mut path, center, rotation);
    path
}

fn convert_polygon(
    points: &[Point],
    arc_segments: &[Option<ArcSegment>],
    cutouts: &[Vec<Point>],
    cutout_arcs: &[Vec<Option<ArcSegment>>],
) -> Path {
    let mut path = Path::new();

    if points.is_empty() {
        return path;
    }

    // Use even-odd fill if there are cutouts
    if !cutouts.is_empty() {
        path.set_fill_type(skia_safe::path::FillType::EvenOdd);
    }

    // Build outer contour with arc segments
    add_polygon_contour(&mut path, points, arc_segments);

    // Add cutouts as additional contours (even-odd fill will subtract them)
    for (cutout_points, cutout_arc_segments) in cutouts.iter().zip(cutout_arcs.iter()) {
        add_polygon_contour(&mut path, cutout_points, cutout_arc_segments);
    }

    path
}

/// Add a polygon contour (with optional arc segments) to a path
fn add_polygon_contour(path: &mut Path, points: &[Point], arc_segments: &[Option<ArcSegment>]) {
    if points.is_empty() {
        return;
    }

    // Move to first point
    path.move_to(to_skia_point(points[0]));

    // Draw edges to subsequent points
    for i in 1..points.len() {
        if let Some(Some(arc)) = arc_segments.get(i) {
            // Arc from points[i-1] to points[i] with center and direction
            add_arc_segment(
                path,
                (points[i - 1].x, points[i - 1].y),
                (points[i].x, points[i].y),
                (arc.center.x, arc.center.y),
                arc.clockwise,
            );
        } else {
            // Straight line
            path.line_to(to_skia_point(points[i]));
        }
    }

    path.close();
}


fn convert_polyline(
    points: &[Point],
    arc_segments: &[Option<ArcSegment>],
    line_width: f64,
    line_end: LineEndStyle,
) -> Path {
    if points.is_empty() {
        return Path::new();
    }

    // Create centerline path with arc support
    let mut centerline = Path::new();
    centerline.move_to(to_skia_point(points[0]));

    for (i, point) in points[1..].iter().enumerate() {
        let segment_index = i + 1; // arc_segments[i+1] describes arc from points[i] to points[i+1]

        if let Some(Some(arc)) = arc_segments.get(segment_index) {
            // Arc segment - use add_arc_segment from primitives module
            let prev_point = points[i];
            super::primitives::add_arc_segment(
                &mut centerline,
                (prev_point.x, prev_point.y),
                (point.x, point.y),
                (arc.center.x, arc.center.y),
                arc.clockwise,
            );
        } else {
            // Straight line segment
            centerline.line_to(to_skia_point(*point));
        }
    }

    // Create paint with stroke properties
    let mut paint = skia_safe::Paint::new(skia_safe::Color4f::new(0.0, 0.0, 0.0, 1.0), None);
    paint.set_style(skia_safe::paint::Style::Stroke);
    paint.set_stroke_width(line_width as f32);

    // Map line end style to stroke cap
    let cap = match line_end {
        LineEndStyle::Round => skia_safe::paint::Cap::Round,
        LineEndStyle::Square => skia_safe::paint::Cap::Square,
        LineEndStyle::None => skia_safe::paint::Cap::Butt,
    };
    paint.set_stroke_cap(cap);

    // Set stroke join for multi-segment traces
    paint.set_stroke_join(skia_safe::paint::Join::Round);

    // Convert stroked path to filled outline
    let mut filled_path = Path::new();
    skia_safe::path_utils::fill_path_with_paint(&centerline, &paint, &mut filled_path, None, None);

    filled_path
}
