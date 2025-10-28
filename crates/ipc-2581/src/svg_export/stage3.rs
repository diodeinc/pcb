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
                // Calculate accurate bbox from path
                let path_bounds = path.bounds();
                let feature_bbox = BoundingBox {
                    min_x: path_bounds.left as f64,
                    min_y: path_bounds.top as f64,
                    max_x: path_bounds.right as f64,
                    max_y: path_bounds.bottom as f64,
                };

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
        } => Ok(vec![convert_circle(*center, *diameter, *filled)]),

        ResolvedGeometry::Rectangle {
            center,
            width,
            height,
            filled,
        } => Ok(vec![convert_rectangle(*center, *width, *height, *filled)]),

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
            line_width,
            line_end,
        } => Ok(vec![convert_polyline(points, *line_width, *line_end)]),

        ResolvedGeometry::Group { geometries } => {
            // Recursively convert all geometries in the group
            let mut paths = Vec::new();
            for geom in geometries {
                paths.extend(convert_geometry(geom)?);
            }
            Ok(paths)
        }

        ResolvedGeometry::PadstackRef { .. } => {
            // This should never happen - Stage 2 should have expanded all padstacks
            Err(crate::Ipc2581Error::MissingElement(
                "Unexpanded PadstackRef in Stage 3",
            ))
        }
    }
}

// ============================================================================
// Geometry Conversion Functions
// ============================================================================

fn convert_circle(center: Point, diameter: f64, filled: bool) -> Path {
    let mut path = Path::new();
    let radius = diameter / 2.0;

    if filled {
        // Filled circle
        path.add_circle((center.x as f32, center.y as f32), radius as f32, None);
    } else {
        // Hollow circle - render as annular ring (donut)
        // Use even-odd fill rule: outer circle - inner circle
        // Estimate stroke width as 10% of diameter (reasonable default for hollow shapes)
        let stroke_width = (diameter * 0.1).max(0.1); // At least 0.1mm
        let inner_radius = (radius - stroke_width / 2.0).max(0.0);
        let outer_radius = radius + stroke_width / 2.0;

        path.set_fill_type(skia_safe::path::FillType::EvenOdd);
        path.add_circle(
            (center.x as f32, center.y as f32),
            outer_radius as f32,
            None,
        );
        path.add_circle(
            (center.x as f32, center.y as f32),
            inner_radius as f32,
            None,
        );
    }

    path
}

fn convert_rectangle(center: Point, width: f64, height: f64, filled: bool) -> Path {
    let mut path = Path::new();
    let half_w = width / 2.0;
    let half_h = height / 2.0;

    if filled {
        // Filled rectangle
        let rect = skia_safe::Rect::from_xywh(
            (center.x - half_w) as f32,
            (center.y - half_h) as f32,
            width as f32,
            height as f32,
        );
        path.add_rect(rect, None);
    } else {
        // Hollow rectangle - render as two rectangles with even-odd fill
        // Estimate stroke width as 10% of minimum dimension
        let stroke_width = (width.min(height) * 0.1).max(0.1); // At least 0.1mm

        // Outer rectangle
        let outer_rect = skia_safe::Rect::from_xywh(
            (center.x - half_w) as f32,
            (center.y - half_h) as f32,
            width as f32,
            height as f32,
        );

        // Inner rectangle (inset by stroke width)
        let inner_half_w = (half_w - stroke_width).max(0.0);
        let inner_half_h = (half_h - stroke_width).max(0.0);
        let inner_rect = skia_safe::Rect::from_xywh(
            (center.x - inner_half_w) as f32,
            (center.y - inner_half_h) as f32,
            (inner_half_w * 2.0) as f32,
            (inner_half_h * 2.0) as f32,
        );

        path.set_fill_type(skia_safe::path::FillType::EvenOdd);
        path.add_rect(outer_rect, None);
        path.add_rect(inner_rect, None);
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

    // Build rounded rectangle with per-corner radii
    // corners: [upper_right, upper_left, lower_right, lower_left]
    let half_w = (width / 2.0) as f32;
    let half_h = (height / 2.0) as f32;
    let r = radius as f32;

    // Start at top-left corner (after rounding)
    let start_x = -half_w + if corners[1] { r } else { 0.0 };
    let start_y = -half_h;
    path.move_to((start_x, start_y));

    // Top edge to upper-right corner
    let top_right_x = half_w - if corners[0] { r } else { 0.0 };
    path.line_to((top_right_x, -half_h));

    // Upper-right corner arc if rounded
    if corners[0] {
        path.arc_to_tangent((half_w, -half_h), (half_w, -half_h + r), r);
    } else {
        path.line_to((half_w, -half_h));
    }

    // Right edge to lower-right corner
    let lower_right_y = half_h - if corners[2] { r } else { 0.0 };
    path.line_to((half_w, lower_right_y));

    // Lower-right corner arc if rounded
    if corners[2] {
        path.arc_to_tangent((half_w, half_h), (half_w - r, half_h), r);
    } else {
        path.line_to((half_w, half_h));
    }

    // Bottom edge to lower-left corner
    let lower_left_x = -half_w + if corners[3] { r } else { 0.0 };
    path.line_to((lower_left_x, half_h));

    // Lower-left corner arc if rounded
    if corners[3] {
        path.arc_to_tangent((-half_w, half_h), (-half_w, half_h - r), r);
    } else {
        path.line_to((-half_w, half_h));
    }

    // Left edge back to top-left corner
    let top_left_y = -half_h + if corners[1] { r } else { 0.0 };
    path.line_to((-half_w, top_left_y));

    // Upper-left corner arc if rounded
    if corners[1] {
        path.arc_to_tangent((-half_w, -half_h), (start_x, -half_h), r);
    }

    path.close();

    // Apply rotation around origin if needed
    if rotation.abs() > 1e-6 {
        let matrix = skia_safe::Matrix::rotate_deg(rotation as f32);
        path.transform(&matrix);
    }

    // Translate to center position
    let translate = skia_safe::Matrix::translate((center.x as f32, center.y as f32));
    path.transform(&translate);

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

    // Apply rotation around origin if needed
    if rotation.abs() > 1e-6 {
        let matrix = skia_safe::Matrix::rotate_deg(rotation as f32);
        path.transform(&matrix);
    }

    // Translate to center position
    let translate = skia_safe::Matrix::translate((center.x as f32, center.y as f32));
    path.transform(&translate);

    path
}

fn convert_ellipse(center: Point, width: f64, height: f64, rotation: f64) -> Path {
    let mut path = Path::new();

    // Create ellipse as oval centered at origin
    let half_w = (width / 2.0) as f32;
    let half_h = (height / 2.0) as f32;
    let oval = skia_safe::Rect::from_xywh(-half_w, -half_h, width as f32, height as f32);
    path.add_oval(oval, None);

    // Apply rotation around origin if needed
    if rotation.abs() > 1e-6 {
        let matrix = skia_safe::Matrix::rotate_deg(rotation as f32);
        path.transform(&matrix);
    }

    // Translate to center position
    let translate = skia_safe::Matrix::translate((center.x as f32, center.y as f32));
    path.transform(&translate);

    path
}

fn convert_donut(center: Point, outer_diameter: f64, inner_diameter: f64) -> Path {
    let mut path = Path::new();

    // Donut (annular ring) = outer circle with inner hole
    // Use even-odd fill rule so inner circle creates a hole
    path.set_fill_type(skia_safe::path::FillType::EvenOdd);

    let outer_radius = (outer_diameter / 2.0) as f32;
    let inner_radius = (inner_diameter / 2.0) as f32;

    // Add outer circle
    path.add_circle((center.x as f32, center.y as f32), outer_radius, None);

    // Add inner circle (creates hole with even-odd fill)
    path.add_circle((center.x as f32, center.y as f32), inner_radius, None);

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

    // Thermal relief = annular ring with spoke gaps cut out
    path.set_fill_type(skia_safe::path::FillType::EvenOdd);

    let outer_radius = (outer_diameter / 2.0) as f32;
    let inner_radius = (inner_diameter / 2.0) as f32;
    let half_gap = (gap / 2.0) as f32;

    // Add annular ring (outer circle - inner circle)
    path.add_circle((0.0, 0.0), outer_radius, None);
    path.add_circle((0.0, 0.0), inner_radius, None);

    // Add spoke gaps as rectangles
    let angle_step = 360.0 / spokes as f32;
    for i in 0..spokes {
        let spoke_angle = i as f32 * angle_step + rotation as f32;

        // Create gap rectangle from center to outer edge
        // Gap width should span from -gap/2 to +gap/2 (total width = gap)
        let mut gap_rect = Path::new();
        gap_rect.add_rect(
            skia_safe::Rect::from_xywh(
                -half_gap,
                -outer_radius,
                gap as f32, // Gap width (not gap * 2.0)
                outer_radius * 2.0,
            ),
            None,
        );

        // Rotate gap to spoke angle
        let matrix = skia_safe::Matrix::rotate_deg(spoke_angle);
        gap_rect.transform(&matrix);

        // Add to path (even-odd fill will subtract it)
        path.add_path(&gap_rect, (0.0, 0.0), None);
    }

    // Translate to center position
    let translate = skia_safe::Matrix::translate((center.x as f32, center.y as f32));
    path.transform(&translate);

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
    path.move_to((points[0].x as f32, points[0].y as f32));

    // Draw edges to subsequent points
    for i in 1..points.len() {
        if let Some(Some(arc)) = arc_segments.get(i) {
            // Arc from points[i-1] to points[i] with center and direction
            add_arc_segment(path, points[i - 1], points[i], arc.center, arc.clockwise);
        } else {
            // Straight line
            path.line_to((points[i].x as f32, points[i].y as f32));
        }
    }

    path.close();
}

/// Add an arc segment from start to end, passing through an arc with given center
fn add_arc_segment(path: &mut Path, start: Point, end: Point, center: Point, clockwise: bool) {
    // Calculate arc parameters
    let start_x = (start.x - center.x) as f32;
    let start_y = (start.y - center.y) as f32;
    let end_x = (end.x - center.x) as f32;
    let end_y = (end.y - center.y) as f32;

    let start_radius = (start_x * start_x + start_y * start_y).sqrt();
    let end_radius = (end_x * end_x + end_y * end_y).sqrt();

    // Validate that start and end points are equidistant from center
    // (within 0.1% tolerance for floating point errors)
    let radius_diff = (start_radius - end_radius).abs();
    let tolerance = start_radius * 0.001;
    if radius_diff > tolerance {
        eprintln!(
            "WARNING: Arc endpoints not equidistant from center - start_r={:.6}, end_r={:.6}, diff={:.6}",
            start_radius, end_radius, radius_diff
        );
    }

    // Use average radius to minimize error if endpoints differ
    let radius = (start_radius + end_radius) / 2.0;

    // Calculate start and end angles (in degrees)
    let start_angle = start_y.atan2(start_x).to_degrees();
    let end_angle = end_y.atan2(end_x).to_degrees();

    // Calculate sweep angle (accounting for direction)
    let mut sweep_angle = end_angle - start_angle;

    // Normalize sweep angle based on direction
    if clockwise {
        // For clockwise, we want negative sweep
        if sweep_angle > 0.0 {
            sweep_angle -= 360.0;
        }
    } else {
        // For counter-clockwise, we want positive sweep
        if sweep_angle < 0.0 {
            sweep_angle += 360.0;
        }
    }

    // Create oval (bounding box for the arc)
    let oval = skia_safe::Rect::from_xywh(
        center.x as f32 - radius,
        center.y as f32 - radius,
        radius * 2.0,
        radius * 2.0,
    );

    // Add arc to path
    path.arc_to(oval, start_angle, sweep_angle, false);
}

fn convert_polyline(points: &[Point], line_width: f64, line_end: LineEndStyle) -> Path {
    // For polylines (traces), we need to create a stroked path
    // Skia can convert a stroked path to a filled outline

    if points.is_empty() {
        return Path::new();
    }

    // Create centerline path
    let mut centerline = Path::new();
    centerline.move_to((points[0].x as f32, points[0].y as f32));
    for point in &points[1..] {
        centerline.line_to((point.x as f32, point.y as f32));
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
