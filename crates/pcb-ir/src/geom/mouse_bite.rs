//! One experimental mouse-bite attachment, not a panelizer or fracture model.
//!
//! See `mouse_bite/README.md` for dimensional provenance and coupon protocol.
//! All results inherit `attachment`'s bounded polygon-model guarantees. No
//! source-curve topology, manufacturing yield, or physical release is certified.

use super::attachment::{
    BoundaryId, BoundaryQuery, PolygonTopology, QueryError, QueryTolerance, material_after_break,
    transform_region,
};
use super::{
    Affine2, ContourBuf, ContourSet, LineCap, LineJoin, PathCmd, Point, StrokeToFillStyle,
};

/// Opinionated, experimental shallow-intrusion adaptation of SparkFun's pattern.
/// Constants are millimeters. There are deliberately no strength-tuning knobs.
pub struct SparkFunShallow;

impl SparkFunShallow {
    pub const HOLE_DIAMETER_MM: f64 = 0.015 * 25.4;
    pub const PITCH_MM: f64 = 0.025 * 25.4;
    pub const NOMINAL_LIGAMENT_MM: f64 = Self::PITCH_MM - Self::HOLE_DIAMETER_MM;
    /// Our experimental adaptation, NOT a tested SparkFun recommendation.
    pub const OUTWARD_OFFSET_MM: f64 = 0.005 * 25.4;
    pub const NOMINAL_INTRUSION_MM: f64 = Self::HOLE_DIAMETER_MM / 2.0 - Self::OUTWARD_OFFSET_MM;
    pub const HOLE_COUNT: usize = 5;
    pub const NECK_WIDTH_MM: f64 = 2.0;
    pub const CUTTER_RADIUS_MM: f64 = 0.5;
}

/// Fully specified single attachment. `stock` is the material before routing;
/// `board` and `support` are disjoint connected subsets of it. The straight neck
/// joins the supplied boundary station to `support_anchor`, which must lie in
/// support. It is not a placement search. Witnesses must be interior material.
pub struct Attachment<'a> {
    pub stock: &'a ContourSet,
    pub board: &'a ContourSet,
    pub support: &'a ContourSet,
    pub boundary: BoundaryId,
    pub station_mm: f64,
    pub support_anchor: Point,
    pub board_witness: Point,
    pub tolerance: QueryTolerance,
}

/// Analytic circular unplated drill, independent of its flattened boolean mask.
#[derive(Debug, Clone, Copy)]
pub struct Npth {
    pub center: Point,
    pub diameter_mm: f64,
}

#[derive(Debug, Clone)]
pub struct TabGeometry {
    /// Final material after BOTH routing and drilling, including board/support.
    pub retained_substrate: ContourSet,
    /// Router removal alone; do not union drill masks into routed profiles.
    pub routed_removal: ContourSet,
    pub npth: Vec<Npth>,
    /// Full circular masks, including portions overlapping routed void.
    pub perforations: ContourSet,
    /// Open polygon path following the offset boundary, through every drill.
    /// This is a proposed fracture locus, NOT a predicted crack trajectory.
    pub break_path: ContourBuf,
    /// Complete unperforated footprint for downstream obstacle clearance checks.
    pub attachment_footprint: ContourSet,
    /// Rounded material added by disk-opening the ideal routing void.
    pub shoulders: ContourSet,
    /// Minimum all-pairs chord clearance, not arc pitch minus diameter on curves.
    pub minimum_ligament_mm: f64,
}

impl TabGeometry {
    /// Virtual removal along the entire supplied row. Width is an explicit
    /// geometric test probe, not a fracture-process parameter or router kerf.
    pub fn after_break(
        &self,
        probe_width_mm: f64,
        witnesses: &[Point],
        tolerance: QueryTolerance,
    ) -> Result<PolygonTopology, QueryError> {
        if !probe_width_mm.is_finite() || probe_width_mm <= 0.0 {
            return Err(QueryError::InvalidInput(
                "expected positive break probe width",
            ));
        }
        material_after_break(
            &self.retained_substrate,
            &stroke(&self.break_path, probe_width_mm),
            witnesses,
            tolerance,
        )
    }
}

fn stroke(path: &ContourBuf, width: f64) -> ContourSet {
    let contours = super::path::stroke_to_fill(
        std::slice::from_ref(path),
        StrokeToFillStyle::new(width, LineCap::Round, LineJoin::Round),
    )
    .expect("positive finite stroke width");
    ContourSet::from_filled_contours(&contours, 0.0)
}

/// Extract an exact portion of the supplied polygon boundary, including its
/// vertices and requested drill stations, even when it crosses the cyclic seam.
fn row(
    region: &ContourSet,
    query: &BoundaryQuery<'_>,
    boundary: BoundaryId,
    center: f64,
) -> Result<(ContourBuf, Vec<Point>), QueryError> {
    let half = 2.0 * SparkFunShallow::PITCH_MM + SparkFunShallow::CUTTER_RADIUS_MM;
    let perimeter = query.perimeter(boundary)?;
    if perimeter <= 2.0 * half {
        return Err(QueryError::InvalidInput("attachment boundary too short"));
    }
    let start = center - half;
    let mut stations = vec![0.0, 2.0 * half];
    let mut cumulative = 0.0;
    for (a, b) in super::region::ring_edges(&region.rings[boundary.ring]) {
        let s = (cumulative - start).rem_euclid(perimeter);
        if s > 0.0 && s < 2.0 * half {
            stations.push(s);
        }
        cumulative += a.distance_to(b);
    }
    let drill_stations = (0..SparkFunShallow::HOLE_COUNT)
        .map(|i| half + (i as f64 - 2.0) * SparkFunShallow::PITCH_MM)
        .collect::<Vec<_>>();
    stations.extend(&drill_stations);
    stations.sort_by(f64::total_cmp);
    stations.dedup();
    let points = stations
        .into_iter()
        .map(|s| query.site(boundary, start + s).map(|site| site.point))
        .collect::<Result<Vec<_>, _>>()?;
    let mut cmds = vec![PathCmd::move_to(points[0])];
    cmds.extend(points[1..].iter().copied().map(PathCmd::line_to));
    let centers = drill_stations
        .into_iter()
        .map(|s| query.site(boundary, start + s).map(|site| site.point))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((ContourBuf::new(cmds), centers))
}

/// Build one tab. Unsupported geometry is an error, never a fallback pattern.
/// The caller remains responsible for complete obstacle/cutter access checks
/// in its actual panel workspace, not just this local construction.
pub fn build(input: Attachment<'_>) -> Result<TabGeometry, QueryError> {
    let query = BoundaryQuery::new(input.board, input.tolerance)?;
    BoundaryQuery::new(input.stock, input.tolerance)?;
    BoundaryQuery::new(input.support, input.tolerance)?;
    let site = query.site(input.boundary, input.station_mm)?;
    if input.board.connected_components().len() != 1
        || input.support.connected_components().len() != 1
        || !input.board.intersection(input.support).is_empty()
        || !input
            .board
            .union(input.support)
            .difference(input.stock)
            .is_empty()
        || !input.support.contains_point(input.support_anchor)
        || !input.board.contains_point(input.board_witness)
    {
        return Err(QueryError::InvalidInput(
            "expected disjoint connected board/support inside stock with interior witnesses",
        ));
    }
    let radius = SparkFunShallow::CUTTER_RADIUS_MM;
    let neck = stroke(
        &ContourBuf::new(vec![
            PathCmd::move_to(site.point - site.outward_normal * radius),
            PathCmd::line_to(input.support_anchor),
        ]),
        SparkFunShallow::NECK_WIDTH_MM,
    );
    let protected = input.board.union(input.support).union(&neck);
    // Open the void with the actual cutter disk: its circular sweeps leave
    // rounded concave shoulders instead of demanding a square inside corner.
    // Extend beyond stock so stock-edge corners do not create retained islands.
    let routed_removal = input
        .stock
        .disk_dilate(2.0 * radius)
        .difference(&protected)
        .disk_open(radius)
        .intersection(input.stock);
    let undrilled = input.stock.difference(&routed_removal);
    let shoulders = undrilled.difference(&protected);
    let attachment_footprint = undrilled.difference(&input.board.union(input.support));

    // Offset the whole region first, rather than guessing normals on a curved
    // row or assigning straight-line pitch to the source curve's arc length.
    let offset = input.board.disk_dilate(SparkFunShallow::OUTWARD_OFFSET_MM);
    let offset_query = BoundaryQuery::new(&offset, input.tolerance)?;
    let target = site.point + site.outward_normal * SparkFunShallow::OUTWARD_OFFSET_MM;
    let projection = offset_query
        .boundaries()
        .map(|id| offset_query.project(id, target))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min_by(|a, b| a.distance.mm.total_cmp(&b.distance.mm))
        .ok_or(QueryError::InvalidInput("empty offset boundary"))?;
    let (break_path, centers) = row(
        &offset,
        &offset_query,
        projection.site.boundary,
        projection.site.station_mm,
    )?;
    let diameter = SparkFunShallow::HOLE_DIAMETER_MM;
    let minimum_ligament_mm = centers
        .iter()
        .enumerate()
        .flat_map(|(i, a)| {
            centers[i + 1..]
                .iter()
                .map(move |b| a.distance_to(*b) - diameter)
        })
        .fold(f64::INFINITY, f64::min);
    if minimum_ligament_mm <= 2.0 * input.tolerance.boundary_mm + input.tolerance.numerical_mm {
        return Err(QueryError::InvalidInput(
            "drill ligaments unresolved or overlapping",
        ));
    }
    let circle = ContourSet::from_filled_contours(&[super::shapes::circle(diameter).unwrap()], 0.0);
    let mut perforations = ContourSet::empty(0.0);
    let npth = centers
        .iter()
        .map(|&center| Npth {
            center,
            diameter_mm: diameter,
        })
        .collect();
    for &center in &centers {
        perforations =
            perforations.union(&transform_region(&circle, Affine2::translation(center))?);
    }
    let result = TabGeometry {
        retained_substrate: undrilled.difference(&perforations),
        routed_removal,
        npth,
        perforations,
        break_path,
        attachment_footprint,
        shoulders,
        minimum_ligament_mm,
    };
    // Check every inter-hole ligament as material, not merely positive spacing.
    let mut witnesses = vec![input.board_witness, input.support_anchor];
    witnesses.extend(centers.windows(2).map(|p| (p[0] + p[1]) / 2.0));
    let before = material_after_break(
        &result.retained_substrate,
        &ContourSet::empty(0.0),
        &witnesses,
        input.tolerance,
    )?;
    if before.components.len() != 1
        || (1..witnesses.len()).any(|i| before.connected(0, i) != Ok(Some(true)))
    {
        return Err(QueryError::InvalidInput(
            "attachment does not retain connected ligaments",
        ));
    }
    // Reject wraparound/oblique necks that bypass the finite row. This 2µm
    // virtual removal is only a polygon topology probe, not a fracture kerf.
    let after = result.after_break(0.002, &witnesses[..2], input.tolerance)?;
    if after.components.len() != 2 || after.connected(0, 1)? != Some(false) {
        return Err(QueryError::InvalidInput(
            "break row does not completely separate board and support in the polygon model",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
