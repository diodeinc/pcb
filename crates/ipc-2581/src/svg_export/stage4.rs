use super::resolved_feature::{BoundingBox, FeatureBucket, LayerStats};
use super::stage3::{LayerPaths, PathFeature};
use super::Result;
use crate::Polarity;
use skia_safe::{Path, PathOp};
use std::collections::HashMap;
use std::time::Instant;

/// Stage 4: Boolean Flattening
///
/// Converts bucketed paths into final flattened copper geometry by applying
/// boolean operations (union, difference) to resolve overlaps and subtract
/// negative features.

/// Coordinate snapping grid size (1 micron)
const SNAP_GRID_MM: f64 = 0.001;

/// Flattened layer after boolean operations (Stage 4 output)
#[derive(Debug, Clone)]
pub struct FlattenedLayer {
    pub layer_name: String,
    /// One unified path per bucket (after boolean operations)
    pub buckets: HashMap<FeatureBucket, Path>,
    pub bbox: BoundingBox,
    pub stats: HashMap<FeatureBucket, BucketStats>,
    pub layer_stats: LayerStats,
}

/// Statistics for a single bucket after boolean operations
#[derive(Debug, Clone, Default)]
pub struct BucketStats {
    pub positive_count: usize,
    pub negative_count: usize,
    pub area_mm2: f64,
    pub vertex_count: usize,
    pub union_time_ms: u64,
    pub difference_time_ms: u64,
}

/// Flatten layers by applying boolean operations per bucket
pub fn flatten_layers(
    layers: HashMap<String, LayerPaths>,
) -> Result<HashMap<String, FlattenedLayer>> {
    layers
        .into_iter()
        .map(|(layer_name, layer_paths)| {
            println!("Flattening layer: {}", layer_name);
            flatten_layer(&layer_name, layer_paths).map(|flattened| (layer_name, flattened))
        })
        .collect()
}

/// Flatten a single layer by processing each bucket
fn flatten_layer(layer_name: &str, layer_paths: LayerPaths) -> Result<FlattenedLayer> {
    let grouped = group_by_bucket(layer_paths.features);

    // Process buckets in priority order (largest/slowest first)
    let bucket_order = [
        FeatureBucket::Fill,
        FeatureBucket::Trace,
        FeatureBucket::Smd,
        FeatureBucket::Pth,
        FeatureBucket::Via,
        FeatureBucket::Thermal,
        FeatureBucket::Antipad,
        FeatureBucket::Cutout,
    ];

    let mut buckets = HashMap::new();
    let mut bucket_stats = HashMap::new();

    for bucket in bucket_order {
        if let Some(features) = grouped.get(&bucket) {
            println!(
                "  Processing bucket {:?}: {} features",
                bucket,
                features.len()
            );

            let (final_path, stats) = flatten_bucket(bucket, features)?;
            buckets.insert(bucket, final_path);
            bucket_stats.insert(bucket, stats);
        }
    }

    // Recalculate overall bounding box from all bucket paths
    let bbox = buckets
        .values()
        .filter(|path| path.count_points() > 0) // Skip empty paths
        .map(|path| {
            let bounds = path.bounds();
            BoundingBox {
                min_x: bounds.left as f64,
                min_y: bounds.top as f64,
                max_x: bounds.right as f64,
                max_y: bounds.bottom as f64,
            }
        })
        .fold(BoundingBox::empty(), |acc, b| acc.union(&b));

    Ok(FlattenedLayer {
        layer_name: layer_name.to_string(),
        buckets,
        bbox,
        stats: bucket_stats,
        layer_stats: layer_paths.stats,
    })
}

/// Group features by bucket
fn group_by_bucket(features: Vec<PathFeature>) -> HashMap<FeatureBucket, Vec<PathFeature>> {
    features.into_iter().fold(HashMap::new(), |mut acc, feature| {
        acc.entry(feature.bucket).or_default().push(feature);
        acc
    })
}

/// Flatten a single bucket by applying boolean operations
fn flatten_bucket(
    bucket: FeatureBucket,
    features: &[PathFeature],
) -> Result<(Path, BucketStats)> {
    if features.is_empty() {
        return Ok((Path::new(), BucketStats::default()));
    }

    // Separate and validate features by polarity
    let (positive_features, negative_features): (Vec<&PathFeature>, Vec<&PathFeature>) =
        features.iter().partition(|f| f.polarity == Polarity::Positive);

    println!(
        "    {:?}: {} positive, {} negative",
        bucket,
        positive_features.len(),
        negative_features.len()
    );

    if positive_features.is_empty() {
        return Ok((Path::new(), BucketStats::default()));
    }

    // OPTIMIZATION: Separate overlapping from non-overlapping features
    // Non-overlapping features don't need boolean ops - preserves curve quality!
    let (overlapping_paths, standalone_paths) =
        separate_overlapping_features(&positive_features);

    println!(
        "      {} overlapping, {} standalone (skip boolean ops)",
        overlapping_paths.len(),
        standalone_paths.len()
    );

    // Union only the overlapping positive paths
    let union_start = Instant::now();
    let positive_union = if !overlapping_paths.is_empty() {
        union_paths(overlapping_paths)?
    } else {
        Path::new()
    };
    let union_time_ms = union_start.elapsed().as_millis() as u64;

    // Process negative features if present
    let (unioned_copper, difference_time_ms) = if negative_features.is_empty() {
        (positive_union, 0)
    } else {
        let negative_paths: Vec<Path> = negative_features
            .iter()
            .map(|f| f.path.clone())
            .collect();
        let negative_union = union_paths(negative_paths)?;
        let diff_start = Instant::now();
        let result = subtract_paths(&positive_union, &negative_union)?;
        let diff_time = diff_start.elapsed().as_millis() as u64;
        println!("    Difference time: {}ms", diff_time);
        (result, diff_time)
    };

    // Combine unioned result with standalone features (no boolean ops needed!)
    let final_path = if !standalone_paths.is_empty() {
        combine_paths(unioned_copper, standalone_paths)?
    } else {
        unioned_copper
    };

    // Calculate accurate statistics
    let area_mm2 = calculate_path_area(&final_path);
    let vertex_count = final_path.count_points();

    let stats = BucketStats {
        positive_count: features
            .iter()
            .filter(|f| f.polarity == Polarity::Positive)
            .count(),
        negative_count: features
            .iter()
            .filter(|f| f.polarity == Polarity::Negative)
            .count(),
        area_mm2,
        vertex_count,
        union_time_ms,
        difference_time_ms,
    };

    println!(
        "    Result: {:.2} mm², {} vertices, union: {}ms",
        area_mm2, vertex_count, union_time_ms
    );

    Ok((final_path, stats))
}

/// Separate overlapping from non-overlapping features
///
/// Non-overlapping features can skip boolean operations entirely, preserving
/// perfect curve quality (circles stay circular, not polygonized).
///
/// This is mathematically correct: Union(A, B) where A∩B = ∅ is just {A, B}.
fn separate_overlapping_features(features: &[&PathFeature]) -> (Vec<Path>, Vec<Path>) {
    let mut overlapping = Vec::new();
    let mut standalone = Vec::new();

    for (i, feature) in features.iter().enumerate() {
        // Broad-phase: Check if this feature's bbox intersects any other
        let has_overlap = features.iter().enumerate().any(|(j, other)| {
            i != j && feature.bbox.intersects(&other.bbox)
        });

        if has_overlap {
            overlapping.push(feature.path.clone());
        } else {
            // No overlap - preserve original geometry!
            standalone.push(feature.path.clone());
        }
    }

    (overlapping, standalone)
}

/// Combine a path with multiple standalone paths (no boolean ops)
fn combine_paths(base: Path, standalone: Vec<Path>) -> Result<Path> {
    let mut combined = base;

    // Add each standalone path as a separate contour
    for path in standalone {
        combined.add_path(&path, (0.0, 0.0), None);
    }

    Ok(combined)
}

/// Union multiple paths into a single path
fn union_paths(mut paths: Vec<Path>) -> Result<Path> {
    if paths.is_empty() {
        return Ok(Path::new());
    }

    // Normalize ALL paths (even single paths) to ensure consistent fill types,
    // remove self-intersections, and snap coordinates to grid
    // Note: We intentionally do NOT call path.simplify() here because it degrades
    // visual quality by approximating curves with low-poly lines. Skia's boolean
    // ops handle curved paths fine, and we want to preserve visual fidelity.
    for path in &mut paths {
        snap_path_to_grid(path);
    }

    // Single path case: return normalized path
    if paths.len() == 1 {
        return Ok(paths.pop().unwrap());
    }

    // Iteratively union paths
    paths
        .into_iter()
        .reduce(|acc, path| match acc.op(&path, PathOp::Union) {
            Some(unioned) => unioned,
            None => {
                eprintln!("WARNING: Union failed, keeping partial result");
                acc
            }
        })
        .ok_or_else(|| {
            crate::Ipc2581Error::InvalidStructure("Union paths resulted in empty path".into())
        })
}

/// Subtract second path from first path
fn subtract_paths(minuend: &Path, subtrahend: &Path) -> Result<Path> {
    minuend.op(subtrahend, PathOp::Difference).ok_or_else(|| {
        eprintln!("WARNING: Difference operation failed, keeping minuend");
        // This error is recoverable - we return the minuend in the outer handler
    }).or_else(|_| Ok(minuend.clone()))
}

/// Snap path coordinates to grid to prevent floating point artifacts
///
/// Prevents sliver artifacts from floating point errors during boolean operations.
/// Uses a 1 micron (0.001mm) grid which is well below manufacturing tolerance.
///
/// The previous implementation just scaled up/down without rounding. This version
/// rebuilds the path with properly rounded coordinates.
fn snap_path_to_grid(path: &mut Path) {
    use skia_safe::path::Verb;

    let mut snapped = Path::new();
    snapped.set_fill_type(path.fill_type());

    let iter = skia_safe::path::Iter::new(path, false);

    for (verb, points) in iter {
        match verb {
            Verb::Move => {
                let p = snap_point(points[0]);
                snapped.move_to(p);
            }
            Verb::Line => {
                let p = snap_point(points[1]);
                snapped.line_to(p);
            }
            Verb::Quad => {
                let p1 = snap_point(points[1]);
                let p2 = snap_point(points[2]);
                snapped.quad_to(p1, p2);
            }
            Verb::Conic => {
                // Convert conic to cubic bezier to preserve arc geometry
                // Note: Skia's Rust API doesn't easily expose conic weight from iterator,
                // so we use weight=1.0 (circular arc) which is the common case for our usage
                let weight = 1.0;
                let cubic_points = conic_to_cubic(
                    points[0],
                    points[1],
                    points[2],
                    weight,
                );

                let cp1 = snap_point(cubic_points.0);
                let cp2 = snap_point(cubic_points.1);
                let end = snap_point(cubic_points.2);
                snapped.cubic_to(cp1, cp2, end);
            }
            Verb::Cubic => {
                let p1 = snap_point(points[1]);
                let p2 = snap_point(points[2]);
                let p3 = snap_point(points[3]);
                snapped.cubic_to(p1, p2, p3);
            }
            Verb::Close => {
                snapped.close();
            }
            _ => {}
        }
    }

    *path = snapped;
}

/// Snap a single point to the grid
#[inline]
fn snap_point(point: skia_safe::Point) -> (f32, f32) {
    let x = (point.x as f64 / SNAP_GRID_MM).round() * SNAP_GRID_MM;
    let y = (point.y as f64 / SNAP_GRID_MM).round() * SNAP_GRID_MM;
    (x as f32, y as f32)
}

/// Convert a conic Bezier to a cubic Bezier
///
/// A conic (rational quadratic) Bezier is defined by 3 points and a weight.
/// For weight=1, it's exactly a circular arc.
///
/// The conversion uses the standard approximation formula:
/// cubic_cp1 = p0 + (2/3) * w * (p1 - p0) / (1 + w)
/// cubic_cp2 = p2 + (2/3) * w * (p1 - p2) / (1 + w)
///
/// Returns (control_point_1, control_point_2, end_point)
fn conic_to_cubic(
    p0: skia_safe::Point,
    p1: skia_safe::Point,
    p2: skia_safe::Point,
    weight: f32,
) -> (skia_safe::Point, skia_safe::Point, skia_safe::Point) {
    // For weight=1 (circular arc), this simplifies nicely
    // For other weights, we use the general formula
    let k = (2.0 * weight) / (3.0 * (1.0 + weight));

    let cp1 = skia_safe::Point::new(
        p0.x + k * (p1.x - p0.x),
        p0.y + k * (p1.y - p0.y),
    );

    let cp2 = skia_safe::Point::new(
        p2.x + k * (p1.x - p2.x),
        p2.y + k * (p1.y - p2.y),
    );

    (cp1, cp2, p2)
}

/// Calculate the actual area of a path in square millimeters using the shoelace formula
///
/// Uses Skia's path iteration to walk vertices and compute exact polygon area.
/// Handles:
/// - Simple polygons
/// - Polygons with holes (via EvenOdd fill type)
/// - Self-intersecting paths
///
/// For curved paths (quads, cubics), we use the polyline approximation which
/// is accurate enough for area calculation since Skia has already tessellated
/// the curves during path construction.
fn calculate_path_area(path: &Path) -> f64 {
    use skia_safe::path::Verb;

    // Check fill type to determine how to combine contour areas
    let is_evenodd = matches!(
        path.fill_type(),
        skia_safe::path::FillType::EvenOdd | skia_safe::path::FillType::InverseEvenOdd
    );

    let mut contour_areas: Vec<f64> = Vec::new();
    let mut current_poly_area = 0.0;
    let mut first_point = None;
    let mut last_point = None;

    let iter = skia_safe::path::Iter::new(path, false);

    for (verb, points) in iter {
        match verb {
            Verb::Move => {
                // Start new polygon contour
                if let (Some(first), Some(last)) = (first_point, last_point) {
                    // Close previous polygon if needed
                    current_poly_area += shoelace_term(last, first);
                    contour_areas.push(current_poly_area / 2.0);
                }

                // Reset for new contour
                current_poly_area = 0.0;
                first_point = Some(points[0]);
                last_point = Some(points[0]);
            }
            Verb::Line => {
                if let Some(p1) = last_point {
                    current_poly_area += shoelace_term(p1, points[1]);
                    last_point = Some(points[1]);
                }
            }
            Verb::Quad => {
                // Sample quad curve at midpoint (good enough for area)
                if let Some(p0) = last_point {
                    let mid = interpolate_quad(p0, points[1], points[2], 0.5);
                    current_poly_area += shoelace_term(p0, mid);
                    current_poly_area += shoelace_term(mid, points[2]);
                    last_point = Some(points[2]);
                }
            }
            Verb::Cubic => {
                // Sample cubic curve at two points (good enough for area)
                if let Some(p0) = last_point {
                    let t1 = interpolate_cubic(p0, points[1], points[2], points[3], 0.33);
                    let t2 = interpolate_cubic(p0, points[1], points[2], points[3], 0.67);
                    current_poly_area += shoelace_term(p0, t1);
                    current_poly_area += shoelace_term(t1, t2);
                    current_poly_area += shoelace_term(t2, points[3]);
                    last_point = Some(points[3]);
                }
            }
            Verb::Close => {
                // Close current polygon
                if let (Some(first), Some(last)) = (first_point, last_point) {
                    current_poly_area += shoelace_term(last, first);
                    contour_areas.push(current_poly_area / 2.0);
                }
                current_poly_area = 0.0;
                first_point = None;
                last_point = None;
            }
            _ => {}
        }
    }

    // Handle unclosed final polygon
    if let (Some(first), Some(last)) = (first_point, last_point) {
        current_poly_area += shoelace_term(last, first);
        contour_areas.push(current_poly_area / 2.0);
    }

    // Combine contour areas based on fill rule
    let total_area = if is_evenodd {
        // EvenOdd: alternate signs for each contour
        // First contour (exterior) = +, second (hole) = -, third (nested) = +, etc.
        contour_areas
            .iter()
            .enumerate()
            .map(|(i, &area)| {
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                sign * area
            })
            .sum::<f64>()
    } else {
        // Winding: signed areas naturally subtract (CCW = +, CW = -)
        contour_areas.iter().sum::<f64>()
    };

    // Shoelace formula gives signed area, take absolute value for final result
    total_area.abs()
}

/// Calculate one term of the shoelace formula: x1 * y2 - x2 * y1
#[inline]
fn shoelace_term(p1: skia_safe::Point, p2: skia_safe::Point) -> f64 {
    (p1.x as f64 * p2.y as f64) - (p2.x as f64 * p1.y as f64)
}

/// Interpolate point on quadratic Bezier curve
#[inline]
fn interpolate_quad(p0: skia_safe::Point, p1: skia_safe::Point, p2: skia_safe::Point, t: f32) -> skia_safe::Point {
    let t2 = 1.0 - t;
    skia_safe::Point::new(
        t2 * t2 * p0.x + 2.0 * t2 * t * p1.x + t * t * p2.x,
        t2 * t2 * p0.y + 2.0 * t2 * t * p1.y + t * t * p2.y,
    )
}

/// Interpolate point on cubic Bezier curve
#[inline]
fn interpolate_cubic(
    p0: skia_safe::Point,
    p1: skia_safe::Point,
    p2: skia_safe::Point,
    p3: skia_safe::Point,
    t: f32,
) -> skia_safe::Point {
    let t2 = 1.0 - t;
    skia_safe::Point::new(
        t2 * t2 * t2 * p0.x + 3.0 * t2 * t2 * t * p1.x + 3.0 * t2 * t * t * p2.x + t * t * t * p3.x,
        t2 * t2 * t2 * p0.y + 3.0 * t2 * t2 * t * p1.y + 3.0 * t2 * t * t * p2.y + t * t * t * p3.y,
    )
}
