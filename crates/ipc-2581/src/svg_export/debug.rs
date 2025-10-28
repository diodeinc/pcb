/// Debug SVG Export
///
/// Provides quick visual validation of Stage 3/4 output without needing
/// the full Stage 5/6 pipeline. Exports raw paths with color-coded buckets.

use super::resolved_feature::{BoundingBox, FeatureBucket};
use super::stage3::{LayerPaths, PathFeature};
use super::stage4::FlattenedLayer;
use skia_safe::{Path, path::Verb};
use std::fs::File;
use std::io::Write;

/// Export Stage 3 paths to SVG for visual inspection
pub fn export_layer_paths_svg(
    layer_paths: &LayerPaths,
    output_path: &str,
) -> std::io::Result<()> {
    let bounds = &layer_paths.bbox;
    let width = bounds.width();
    let height = bounds.height();

    let mut svg = String::new();

    // SVG header with viewBox
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="1200" height="1200">
"#,
        bounds.min_x, bounds.min_y, width, height
    ));

    // Add title
    svg.push_str(&format!(
        r#"  <title>{} - Stage 3 Output</title>
"#,
        layer_paths.layer_name
    ));

    // Add background
    svg.push_str(r#"  <rect x="0" y="0" width="100%" height="100%" fill="black"/>
"#);

    // Group features by bucket for cleaner SVG
    svg.push_str(r#"  <!-- Features grouped by bucket -->
"#);

    for bucket in [
        FeatureBucket::Fill,
        FeatureBucket::Trace,
        FeatureBucket::Smd,
        FeatureBucket::Pth,
        FeatureBucket::Via,
        FeatureBucket::Thermal,
    ] {
        let features: Vec<&PathFeature> = layer_paths
            .features
            .iter()
            .filter(|f| f.bucket == bucket)
            .collect();

        if features.is_empty() {
            continue;
        }

        let (color, opacity) = bucket_color(bucket);

        svg.push_str(&format!(
            r#"  <g id="{:?}" fill="{}" fill-opacity="{}" stroke="none">
"#,
            bucket, color, opacity
        ));

        for feature in &features {
            let path_data = path_to_svg_data(&feature.path);
            svg.push_str(&format!(r#"    <path d="{}"/>
"#, path_data));
        }

        svg.push_str("  </g>\n");
    }

    svg.push_str("</svg>\n");

    let mut file = File::create(output_path)?;
    file.write_all(svg.as_bytes())?;

    println!("✓ Exported debug SVG to {}", output_path);
    println!(
        "  {} features, {:.2}×{:.2}mm",
        layer_paths.features.len(),
        width,
        height
    );

    Ok(())
}

/// Export Stage 4 flattened paths to SVG for visual inspection
pub fn export_flattened_svg(
    flattened: &FlattenedLayer,
    output_path: &str,
) -> std::io::Result<()> {
    let bounds = &flattened.bbox;
    let width = bounds.width();
    let height = bounds.height();

    let mut svg = String::new();

    // SVG header
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="1200" height="1200">
"#,
        bounds.min_x, bounds.min_y, width, height
    ));

    // Add title
    svg.push_str(&format!(
        r#"  <title>{} - Stage 4 Flattened</title>
"#,
        flattened.layer_name
    ));

    // Add background
    svg.push_str(r#"  <rect x="0" y="0" width="100%" height="100%" fill="black"/>
"#);

    // Add each bucket as a group
    svg.push_str(r#"  <!-- Flattened buckets (post-boolean ops) -->
"#);

    for bucket in [
        FeatureBucket::Fill,
        FeatureBucket::Trace,
        FeatureBucket::Smd,
        FeatureBucket::Pth,
        FeatureBucket::Via,
        FeatureBucket::Thermal,
    ] {
        if let Some(path) = flattened.buckets.get(&bucket) {
            let (color, opacity) = bucket_color(bucket);
            let path_data = path_to_svg_data(path);

            svg.push_str(&format!(
                r#"  <g id="{:?}">
    <path d="{}" fill="{}" fill-opacity="{}" stroke="none"/>
"#,
                bucket, path_data, color, opacity
            ));

            if let Some(stats) = flattened.stats.get(&bucket) {
                svg.push_str(&format!(
                    "    <!-- {:.2} mm², {} vertices -->\n",
                    stats.area_mm2, stats.vertex_count
                ));
            }

            svg.push_str("  </g>\n");
        }
    }

    svg.push_str("</svg>\n");

    let mut file = File::create(output_path)?;
    file.write_all(svg.as_bytes())?;

    println!("✓ Exported flattened SVG to {}", output_path);
    println!(
        "  {} buckets, {:.2}×{:.2}mm",
        flattened.buckets.len(),
        width,
        height
    );

    Ok(())
}

/// Convert Skia Path to SVG path data string
fn path_to_svg_data(path: &Path) -> String {
    let mut data = String::new();

    // Create iterator with force_close = false
    let iter = skia_safe::path::Iter::new(path, false);

    for (verb, points) in iter {
        match verb {
            Verb::Move => {
                data.push_str(&format!("M{:.3},{:.3} ", points[0].x, points[0].y));
            }
            Verb::Line => {
                data.push_str(&format!("L{:.3},{:.3} ", points[1].x, points[1].y));
            }
            Verb::Quad => {
                data.push_str(&format!(
                    "Q{:.3},{:.3} {:.3},{:.3} ",
                    points[1].x, points[1].y, points[2].x, points[2].y
                ));
            }
            Verb::Conic => {
                // Approximate conic as quadratic (close enough for visual inspection)
                data.push_str(&format!(
                    "Q{:.3},{:.3} {:.3},{:.3} ",
                    points[1].x, points[1].y, points[2].x, points[2].y
                ));
            }
            Verb::Cubic => {
                data.push_str(&format!(
                    "C{:.3},{:.3} {:.3},{:.3} {:.3},{:.3} ",
                    points[1].x,
                    points[1].y,
                    points[2].x,
                    points[2].y,
                    points[3].x,
                    points[3].y
                ));
            }
            Verb::Close => {
                data.push('Z');
            }
            _ => {}
        }
    }

    data
}

/// Get color and opacity for a feature bucket
fn bucket_color(bucket: FeatureBucket) -> (&'static str, &'static str) {
    match bucket {
        FeatureBucket::Fill => ("#32CD32", "0.6"),      // Lime green - planes/pours
        FeatureBucket::Trace => ("#FF4500", "0.8"),     // Red-orange - traces
        FeatureBucket::Smd => ("#FFA500", "0.9"),       // Orange - SMD pads
        FeatureBucket::Pth => ("#00FF00", "0.9"),       // Green - PTH pads
        FeatureBucket::Via => ("#1E90FF", "0.9"),       // Blue - vias
        FeatureBucket::Thermal => ("#FFD700", "0.7"),   // Gold - thermal reliefs
        FeatureBucket::Antipad => ("#FF1493", "0.5"),   // Pink - antipads
        FeatureBucket::Cutout => ("#8B0000", "0.5"),    // Dark red - cutouts
    }
}

/// Export a single bucket to SVG (for detailed inspection)
pub fn export_bucket_svg(
    layer_paths: &LayerPaths,
    bucket: FeatureBucket,
    output_path: &str,
) -> std::io::Result<()> {
    let bounds = &layer_paths.bbox;
    let width = bounds.width();
    let height = bounds.height();

    let features: Vec<&PathFeature> = layer_paths
        .features
        .iter()
        .filter(|f| f.bucket == bucket)
        .collect();

    if features.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("No features found for bucket {:?}", bucket),
        ));
    }

    let mut svg = String::new();

    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="1200" height="1200">
"#,
        bounds.min_x, bounds.min_y, width, height
    ));

    svg.push_str(&format!(
        r#"  <title>{} - {:?} only</title>
"#,
        layer_paths.layer_name, bucket
    ));

    svg.push_str(r#"  <rect x="0" y="0" width="100%" height="100%" fill="black"/>
"#);

    let (color, opacity) = bucket_color(bucket);
    let feature_count = features.len();

    for feature in features {
        let path_data = path_to_svg_data(&feature.path);
        svg.push_str(&format!(
            r#"  <path d="{}" fill="{}" fill-opacity="{}" stroke="none"/>
"#,
            path_data, color, opacity
        ));
    }

    svg.push_str("</svg>\n");

    let mut file = File::create(output_path)?;
    file.write_all(svg.as_bytes())?;

    println!("✓ Exported {:?} bucket to {}", bucket, output_path);
    println!("  {} features", feature_count);

    Ok(())
}
