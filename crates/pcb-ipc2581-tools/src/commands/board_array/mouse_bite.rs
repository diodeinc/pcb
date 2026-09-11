//! Routed slots and perforated tabs for a board array: the material the
//! router removes around every board instance, the tabs left bridging each
//! slot, and their break holes. Pure geometry in panel coordinates; the
//! array spec turns the removal into profile cutouts and the holes into
//! non-plated drills.
//!
//! Each tab comes from the single-tab builder in pcb-ir, given the board
//! instance, the frame around it and a local window of stock. A board's
//! removal is its slot minus every tab footprint on it, so tabs never erase
//! one another; the holes are the builder's. The retained panel is then
//! checked as a whole: every board connected to the frame before the break
//! rows are cut, every board free of the frame and of each other after.

use std::collections::HashSet;

use anyhow::{Context, Result, bail, ensure};
use ipc2581::types::Polygon;
use pcb_ir::geom::{
    Affine2, ContourBuf, ContourSet, LineCap, LineJoin, Point, Resolution, StrokeToFillStyle,
    attachment::{BoundaryQuery, QueryTolerance, material_after_break, transform_region},
    mouse_bite::{Attachment, Npth, SparkFunShallow, build},
    path::stroke_to_fill,
    region::ring_signed_area,
};

use super::placement::{Placement, Preset, select};
use super::xml::poly_segment;

pub struct Tabs {
    /// Connected routed voids in panel coordinates, one profile cutout each.
    pub cutouts: Vec<ContourSet>,
    /// Break perforations in panel coordinates.
    pub holes: Vec<Npth>,
    pub per_board: usize,
    /// Candidate sites the builder could not turn into a tab, with the reason.
    pub dropped: Vec<String>,
}

/// Tab and slot geometry for `placement`'s board repeated at `offsets`
/// inside `stock`. Sites the builder rejects are dropped and the placement
/// is re-solved without them, so the result always holds the board or fails
/// with the placement's own reason.
pub(super) fn generate(
    placement: &Placement,
    stock: &ContourSet,
    offsets: &[Point],
    preset: &Preset,
    resolution: Resolution,
) -> Result<Tabs> {
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
    };
    // Slots follow the outer boundary; a board's own holes stay the board's.
    let substrate = &placement.prepared.substrate;
    let outer = ContourSet::from_regularized(
        substrate
            .rings
            .iter()
            .filter(|ring| ring_signed_area(ring) > 0.0)
            .cloned()
            .collect(),
        substrate.resolution,
        substrate.uncertainty_mm,
    );
    let boards = offsets
        .iter()
        .map(|&offset| transform_region(&outer, Affine2::translation(offset)))
        .collect::<Result<Vec<_>, _>>()?;
    let grown = boards
        .iter()
        .map(|board| board.disk_dilate(preset.routing_gap_mm))
        .collect::<Result<Vec<_>, _>>()?;
    let frame = grown
        .iter()
        .try_fold(stock.clone(), |frame, slot| frame.difference(slot))?;
    ensure!(
        significant_components(&frame, resolution) == 1,
        "the rails between routed slots do not form one connected frame"
    );
    let candidates = &placement.sites.candidates;
    let reach = preset.routing_gap_mm + preset.frame_landing_mm;
    let mut excluded = HashSet::new();
    let mut dropped = Vec::new();
    loop {
        let kept: Vec<usize> = (0..candidates.len())
            .filter(|i| !excluded.contains(i))
            .collect();
        let sites: Vec<_> = kept.iter().map(|&i| candidates[i].site).collect();
        let selection = select::select(&sites, &placement.loads, &placement.model);
        if !selection.satisfied() {
            bail!(
                "tabs cannot hold the board: {}{}",
                selection
                    .violations
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
                if dropped.is_empty() {
                    String::new()
                } else {
                    format!("; sites dropped: {}", dropped.join("; "))
                }
            );
        }
        let chosen: Vec<usize> = selection.chosen.iter().map(|&k| kept[k]).collect();

        let mut cutouts = Vec::new();
        let mut holes = Vec::new();
        let mut perforations = ContourSet::empty(resolution);
        let mut break_rows = Vec::new();
        let mut witnesses = vec![frame_witness(&frame)?];
        let mut failure = None;
        'boards: for ((board, slot), offset) in boards.iter().zip(&grown).zip(offsets) {
            // The builder wants stock well beyond the support it lands on, so
            // the support ring stops short of the local stock window.
            let support = frame.intersection(&slot.disk_dilate(preset.frame_landing_mm + 1.0)?)?;
            let window = slot
                .disk_dilate(preset.frame_landing_mm + 4.0)?
                .intersection(stock)?;
            let query = BoundaryQuery::new(board, tolerance)?;
            let mut footprints = ContourSet::empty(resolution);
            for &c in &chosen {
                let site = candidates[c].site;
                let point = site.point + *offset;
                let normal = site.outward_normal;
                let projection = query
                    .boundaries()
                    .map(|id| query.project(id, point))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .min_by(|a, b| a.distance.mm.total_cmp(&b.distance.mm))
                    .context("board instance has no boundary")?;
                let attachment = Attachment {
                    stock: &window,
                    board,
                    support: &support,
                    boundary: projection.site.boundary,
                    station_mm: projection.site.station_mm,
                    support_anchor: point + normal * (preset.routing_gap_mm + reach) / 2.0,
                    board_witness: point - normal * WITNESS_DEPTH_MM,
                    tolerance,
                };
                match build(attachment) {
                    Ok(tab) => {
                        footprints = footprints.union(&tab.attachment_footprint)?;
                        perforations = perforations.union(&tab.perforations)?;
                        holes.extend(tab.npth);
                        break_rows.push(tab.break_path);
                        if witnesses.len() == cutouts.len() + 1 {
                            witnesses.push(attachment_witness(point, normal));
                        }
                    }
                    Err(error) => {
                        failure = Some(format!(
                            "site {c} at ({:.2}, {:.2}): {error}",
                            site.point.x, site.point.y
                        ));
                        break 'boards;
                    }
                }
            }
            let removal = slot.difference(board)?.difference(&footprints)?;
            cutouts.push(removal);
        }
        if let Some(failure) = failure {
            let c = failure
                .split(' ')
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok())
                .expect("failure names its site");
            excluded.insert(c);
            dropped.push(failure);
            continue;
        }

        let cutouts: Vec<ContourSet> = cutouts
            .iter()
            .flat_map(|removal| significant(removal, resolution))
            .collect();
        check_release(
            stock,
            &cutouts,
            &perforations,
            &break_rows,
            &witnesses,
            tolerance,
        )?;
        return Ok(Tabs {
            cutouts,
            holes,
            per_board: chosen.len(),
            dropped,
        });
    }
}

/// How far inside the board the builder's witness point sits.
const WITNESS_DEPTH_MM: f64 = 1.0;

fn attachment_witness(point: Point, normal: Point) -> Point {
    point - normal * WITNESS_DEPTH_MM
}

/// A point on the bottom rail, away from the rounded corners.
fn frame_witness(frame: &ContourSet) -> Result<Point> {
    let bbox = frame.bbox();
    let point = Point::new(bbox.center().x, bbox.min.y + 1.0);
    ensure!(
        frame.contains_point(point),
        "the bottom rail is not frame material at its middle"
    );
    Ok(point)
}

/// Connected pieces of `region` at the cutter's own significance: a void or
/// an island smaller than the cutter radius squared is not something a router
/// cuts or leaves, only residue of booleans along coincident edges.
fn significant(region: &ContourSet, resolution: Resolution) -> Vec<ContourSet> {
    let routable = resolution.with_tolerance(SparkFunShallow::CUTTER_RADIUS_MM);
    region
        .connected_components()
        .into_iter()
        .map(|piece| {
            ContourSet::from_regularized(piece.rings.clone(), routable, piece.uncertainty_mm)
        })
        .filter(|piece| !piece.is_empty())
        .collect()
}

fn significant_components(region: &ContourSet, resolution: Resolution) -> usize {
    significant(region, resolution).len()
}

/// Every board must connect to the frame through its tabs, and cutting every
/// break row must free every board from the frame and from each other.
fn check_release(
    stock: &ContourSet,
    cutouts: &[ContourSet],
    perforations: &ContourSet,
    break_rows: &[ContourBuf],
    witnesses: &[Point],
    tolerance: QueryTolerance,
) -> Result<()> {
    let resolution = stock.resolution.strict();
    let retained = cutouts
        .iter()
        .try_fold(stock.clone(), |kept, cutout| kept.difference(cutout))?
        .difference(perforations)?;
    let boards = 1..witnesses.len();
    let held = material_after_break(
        &retained,
        &ContourSet::empty(resolution),
        witnesses,
        tolerance,
    )?;
    ensure!(
        boards
            .clone()
            .all(|i| held.connected(0, i) == Ok(Some(true))),
        "not every board is connected to the frame through its tabs"
    );
    let rows = stroke_to_fill(
        break_rows,
        StrokeToFillStyle::new(BREAK_PROBE_MM, LineCap::Round, LineJoin::Round),
        resolution.accuracy,
    )?
    .context("break rows have no width")?;
    let rows = ContourSet::from_filled_contours(&rows, resolution)?;
    let released = material_after_break(&retained, &rows, witnesses, tolerance)?;
    ensure!(
        boards
            .clone()
            .all(|i| released.connected(0, i) == Ok(Some(false)))
            && boards
                .clone()
                .flat_map(|i| boards.clone().filter(move |&j| j > i).map(move |j| (i, j)))
                .all(|(i, j)| released.connected(i, j) == Ok(Some(false))),
        "breaking every tab row does not separate every board from the frame and from each other"
    );
    Ok(())
}

/// Width of the virtual removal along a break row: a polygon topology probe,
/// not a kerf.
const BREAK_PROBE_MM: f64 = 0.002;

/// One simply connected routed void as an IPC-2581 polygon.
pub fn cutout_polygon(cutout: &ContourSet) -> Result<Polygon> {
    let [ring] = cutout.rings.as_slice() else {
        bail!(
            "a routed void has {} rings; each must be a single loop (areas {:?}, bbox {:?})",
            cutout.rings.len(),
            cutout
                .rings
                .iter()
                .map(ring_signed_area)
                .collect::<Vec<_>>(),
            cutout.bbox()
        );
    };
    ensure!(
        ring_signed_area(ring) > 0.0 && ring.len() >= 3,
        "a routed void has no area (signed area {}, {} vertices)",
        ring_signed_area(ring),
        ring.len()
    );
    Ok(Polygon {
        begin: ipc2581::types::Point {
            x: ring[0][0],
            y: ring[0][1],
        },
        steps: ring[1..]
            .iter()
            .chain(std::iter::once(&ring[0]))
            .map(|p| poly_segment(p[0], p[1]))
            .collect(),
    })
}
