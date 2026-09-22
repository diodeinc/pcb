//! Structural validation for IPC documents headed to artwork export.

use crate::dialects::ipc::Document;
use crate::dialects::ipc::feature::Feature;
use crate::geom::path::{PathCmd, PathOp};
use crate::geom::{Diagnostics, PaintKind, Point, tol};

/// Check that every feature is exportable as native artwork: no unresolved
/// set-void semantics, homogeneous paint per feature, and circular arcs.
/// Clear polarity is native — ordered artwork paints it with exactly IPC's
/// sequential semantics. All problems are collected.
pub fn validate_artwork_ready(doc: &Document) -> Result<(), Diagnostics> {
    let mut diagnostics = Diagnostics::default();
    validate_homogeneous_features_into(doc, &mut diagnostics);
    for (feature_index, feature) in doc.features.iter().enumerate() {
        if feature.paths.is_empty() {
            continue;
        }
        if feature.clears_previous_in_set {
            diagnostics.error(format!(
                "feature {feature_index} still has unresolved set-void clear semantics"
            ));
        }
        validate_feature_arcs(doc, feature_index, feature, &mut diagnostics);
    }
    diagnostics.into_result()
}

fn validate_homogeneous_features_into(doc: &Document, diagnostics: &mut Diagnostics) {
    let group_spans = doc
        .feature_placement_groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| {
            [
                group.features.validate(
                    "feature placement group features",
                    index,
                    doc.features.len(),
                ),
                group.placements.validate(
                    "feature placement group placements",
                    index,
                    doc.feature_placements.len(),
                ),
            ]
        });
    for error in group_spans.filter_map(Result::err) {
        diagnostics.error(error);
    }
    for (feature_index, feature) in doc.features.iter().enumerate() {
        if let Some(group) = feature.placement_group
            && group as usize >= doc.feature_placement_groups.len()
        {
            diagnostics.error(format!(
                "feature {feature_index} references missing placement group {group}"
            ));
        }
        if let Err(error) =
            feature
                .paths
                .validate("feature paths", feature_index, doc.arena.paths.len())
        {
            diagnostics.error(error);
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

fn validate_feature_arcs(
    doc: &Document,
    feature_index: usize,
    feature: &Feature,
    diagnostics: &mut Diagnostics,
) {
    for path_index in feature.paths.indices() {
        if let Err(message) = validate_path_arcs(doc, feature_index, path_index) {
            diagnostics.error(message);
        }
    }
}

fn validate_path_arcs(doc: &Document, feature_index: usize, path_index: u32) -> Result<(), String> {
    let path = &doc.arena.paths[path_index as usize];
    path.contours.validate(
        "path contours",
        path_index as usize,
        doc.arena.contours.len(),
    )?;
    for contour_index in path.contours.indices() {
        let contour = doc.arena.contours[contour_index as usize];
        contour.cmds.validate(
            "contour commands",
            contour_index as usize,
            doc.arena.cmds.len(),
        )?;
        let mut current = Point::default();
        for cmd_index in contour.cmds.indices() {
            let cmd = doc.arena.cmds[cmd_index as usize];
            match cmd.op {
                PathOp::MoveTo | PathOp::LineTo => current = cmd.p0,
                PathOp::ArcTo => {
                    validate_arc_command(feature_index, path_index, cmd_index, current, cmd)?;
                    current = cmd.p0;
                }
                PathOp::EllipseTo => current = cmd.p0,
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

fn arc_radii_nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs()
        <= tol::ARC_RADIUS_MM.max(tol::EPSILON_MM * left.abs().max(right.abs()).max(1.0))
}
