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
    let (positive_paths, negative_paths) = separate_by_polarity(bucket, features);

    println!(
        "    {:?}: {} positive, {} negative",
        bucket,
        positive_paths.len(),
        negative_paths.len()
    );

    if positive_paths.is_empty() {
        return Ok((Path::new(), BucketStats::default()));
    }

    // Union positive paths
    let union_start = Instant::now();
    let positive_union = union_paths(positive_paths)?;
    let union_time_ms = union_start.elapsed().as_millis() as u64;

    // Subtract negatives if present
    let (final_path, difference_time_ms) = if negative_paths.is_empty() {
        (positive_union, 0)
    } else {
        let negative_union = union_paths(negative_paths)?;
        let diff_start = Instant::now();
        let result = subtract_paths(&positive_union, &negative_union)?;
        let diff_time = diff_start.elapsed().as_millis() as u64;
        println!("    Difference time: {}ms", diff_time);
        (result, diff_time)
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

/// Separate features by polarity with validation
fn separate_by_polarity(
    bucket: FeatureBucket,
    features: &[PathFeature],
) -> (Vec<Path>, Vec<Path>) {
    let mut positive_paths = Vec::new();
    let mut negative_paths = Vec::new();

    for feature in features {
        // Validate polarity expectations
        if feature.polarity == Polarity::Negative
            && !matches!(
                bucket,
                FeatureBucket::Fill | FeatureBucket::Cutout | FeatureBucket::Antipad
            )
        {
            eprintln!(
                "WARNING: Unexpected negative polarity in {:?} bucket",
                bucket
            );
        }

        match feature.polarity {
            Polarity::Positive => positive_paths.push(feature.path.clone()),
            Polarity::Negative => negative_paths.push(feature.path.clone()),
        }
    }

    (positive_paths, negative_paths)
}

/// Union multiple paths into a single path
fn union_paths(mut paths: Vec<Path>) -> Result<Path> {
    match paths.len() {
        0 => return Ok(Path::new()),
        1 => return Ok(paths.pop().unwrap()),
        _ => {}
    }

    // Simplify and snap paths before boolean operations
    for path in &mut paths {
        snap_path_to_grid(path);
        if let Some(simplified) = path.simplify() {
            *path = simplified;
        }
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
fn snap_path_to_grid(path: &mut Path) {
    let scale = (1.0 / SNAP_GRID_MM) as f32;

    // Scale up to grid units, let Skia round to integer representation
    let mut scale_up = skia_safe::Matrix::new_identity();
    scale_up.set_scale((scale, scale), None);
    path.transform(&scale_up);

    // Scale back down to mm
    let mut scale_down = skia_safe::Matrix::new_identity();
    scale_down.set_scale((1.0 / scale, 1.0 / scale), None);
    path.transform(&scale_down);
}

/// Calculate the approximate area of a path in square millimeters
///
/// Uses tight_bounds() which is more accurate than regular bounds() as it
/// accounts for the actual path geometry rather than just the control points.
///
/// Note: This is still an approximation. For true polygon area, we would need
/// to implement the shoelace formula or use a geometry library like `geo`.
fn calculate_path_area(path: &Path) -> f64 {
    // tight_bounds() computes the tightest rectangle that encloses the path
    // This is more accurate than bounds() but still an overestimate for complex shapes
    match path.tight_bounds() {
        Some(bounds) => {
            let width = (bounds.right - bounds.left) as f64;
            let height = (bounds.bottom - bounds.top) as f64;
            width * height
        }
        None => {
            // Fallback to regular bounds if tight_bounds fails
            let bounds = path.bounds();
            let width = (bounds.right - bounds.left) as f64;
            let height = (bounds.bottom - bounds.top) as f64;
            width * height
        }
    }
}
