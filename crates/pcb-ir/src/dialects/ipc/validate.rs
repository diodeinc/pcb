//! Structural validation for IPC documents headed to artwork export.

use crate::dialects::ipc::Document;
use crate::dialects::ipc::feature::{Feature, FeatureBucket};
use crate::geom::path::{PathCmd, PathOp};
use crate::geom::{Diagnostics, PaintKind, Point, Polarity, Span, tol};

/// Check that every feature is exportable as native artwork: no unresolved
/// set-void or negative-polarity semantics, homogeneous paint per feature,
/// and circular arcs. All problems are collected.
pub fn validate_artwork_ready<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Result<(), Diagnostics> {
    let mut diagnostics = Diagnostics::default();
    validate_homogeneous_features_into(doc, &mut diagnostics);
    for (feature_index, feature) in doc.features.iter().enumerate() {
        if feature.paths.is_empty() {
            continue;
        }
        if feature.flags.clears_previous_in_set {
            diagnostics.error(format!(
                "feature {feature_index} still has unresolved set-void clear semantics"
            ));
        }
        if feature.bucket != FeatureBucket::Cutout && feature.polarity != Polarity::Dark {
            diagnostics.error(format!(
                "feature {feature_index} still has unresolved negative polarity"
            ));
        }
        validate_feature_arcs(doc, feature_index, feature, &mut diagnostics);
    }
    diagnostics.into_result()
}

/// Check that every feature's paths agree on one paint kind.
pub fn validate_homogeneous_features<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Result<(), Diagnostics> {
    let mut diagnostics = Diagnostics::default();
    validate_homogeneous_features_into(doc, &mut diagnostics);
    diagnostics.into_result()
}

fn validate_homogeneous_features_into<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    diagnostics: &mut Diagnostics,
) {
    for (feature_index, feature) in doc.features.iter().enumerate() {
        if let Err(error) = checked_span(feature.paths, "feature paths", doc.arena.paths.len()) {
            diagnostics.error(format!("feature {feature_index}: {error}"));
            continue;
        }
        let mut feature_kind = None;
        for path_index in feature.paths.indices() {
            let path_kind = doc.arena.paths[path_index as usize].paint.kind();
            if path_kind == PaintKind::None {
                diagnostics.error(format!(
                    "feature {feature_index} path {path_index} is unpainted"
                ));
                continue;
            }

            match feature_kind {
                Some(previous) if previous != path_kind => {
                    diagnostics.error(format!(
                        "feature {feature_index} mixes {previous:?} and {path_kind:?} paths"
                    ));
                }
                None => feature_kind = Some(path_kind),
                _ => {}
            }
        }
    }
}

fn validate_feature_arcs<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature_index: usize,
    feature: &Feature<Symbol>,
    diagnostics: &mut Diagnostics,
) {
    for path_index in feature.paths.indices() {
        if let Err(message) = validate_path_arcs(doc, feature_index, path_index) {
            diagnostics.error(message);
        }
    }
}

fn validate_path_arcs<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature_index: usize,
    path_index: u32,
) -> Result<(), String> {
    let path = &doc.arena.paths[path_index as usize];
    checked_span(path.contours, "path contours", doc.arena.contours.len())
        .map_err(|error| format!("feature {feature_index} path {path_index}: {error}"))?;
    for contour_index in path.contours.indices() {
        let contour = doc.arena.contours[contour_index as usize];
        checked_span(contour.cmds, "contour commands", doc.arena.cmds.len()).map_err(|error| {
            format!("feature {feature_index} path {path_index} contour {contour_index}: {error}")
        })?;
        let mut current = Point::default();
        for cmd_index in contour.cmds.indices() {
            let cmd = doc.arena.cmds[cmd_index as usize];
            match cmd.op {
                PathOp::MoveTo | PathOp::LineTo => current = cmd.p0,
                PathOp::ArcTo => {
                    validate_arc_command(feature_index, path_index, cmd_index, current, cmd)?;
                    current = cmd.p0;
                }
                PathOp::CubicTo => current = cmd.p2,
                PathOp::Close => {}
            }
        }
    }
    Ok(())
}

fn validate_arc_command(
    feature_index: usize,
    path_index: u32,
    cmd_index: u32,
    start: Point,
    cmd: PathCmd,
) -> Result<(), String> {
    let start_radius = start.distance_to(cmd.p1);
    let end_radius = cmd.p0.distance_to(cmd.p1);
    if start_radius <= 0.0 || end_radius <= 0.0 {
        return Err(format!(
            "feature {feature_index} path {path_index} command {cmd_index} has a zero-radius arc"
        ));
    }
    if !arc_radii_nearly_equal(start_radius, end_radius) {
        return Err(format!(
            "feature {feature_index} path {path_index} command {cmd_index} has non-circular arc radii {start_radius} and {end_radius}"
        ));
    }
    Ok(())
}

fn checked_span(span: Span, label: &str, len: usize) -> Result<(), String> {
    let end = span
        .start
        .checked_add(span.count)
        .ok_or_else(|| format!("{label} range overflows"))?;
    if end as usize > len {
        return Err(format!(
            "{label} range {}..{end} exceeds available length {len}",
            span.start
        ));
    }
    Ok(())
}

fn arc_radii_nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs()
        <= tol::ARC_RADIUS_MM.max(tol::EPSILON_MM * left.abs().max(right.abs()).max(1.0))
}
