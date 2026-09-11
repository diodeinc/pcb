//! Routed slots and perforated tabs for a board array: the material the
//! router removes around every board instance, the tabs left bridging each
//! slot, and their break holes. Pure geometry in panel coordinates; the
//! array spec turns the removal into profile cutouts and the holes into
//! non-plated drills.
//!
//! Each tab comes from the single-tab builder in pcb-ir, given the board
//! instance, the frame around it and a local window of stock; the holes and
//! break rows are the builder's. A board's removal is its slot minus every
//! tab neck, opened by the cutter around the necks so the fillets a router
//! leaves beside them are part of the void's shape. The retained panel is
//! then checked as a whole: every board connected to the frame before the
//! break rows are cut, every board free of the frame and of each other after.

use std::collections::HashSet;

use anyhow::{Context, Result, bail, ensure};
use ipc2581::types::Polygon;
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, ContourSet, LineCap, LineJoin, Point, Resolution, StrokeToFillStyle,
    attachment::{BoundaryQuery, QueryTolerance, material_after_break, transform_region},
    mouse_bite::{Attachment, Npth, SparkFunShallow, TabGeometry, build},
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
        routable_components(&frame, resolution) == 1,
        "the rails between routed slots do not form one connected frame"
    );
    let candidates = &placement.sites.candidates;
    let radius = SparkFunShallow::CUTTER_RADIUS_MM;
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
        let mut rejected = None;
        'boards: for ((board, slot), &offset) in boards.iter().zip(&grown).zip(offsets) {
            let mut necks = ContourSet::empty(resolution);
            for &c in &chosen {
                let site = candidates[c].site;
                let tab = build_tab(
                    site.point + offset,
                    site.outward_normal,
                    board,
                    slot,
                    &frame,
                    stock,
                    preset,
                    resolution,
                    tolerance,
                );
                match tab {
                    Ok(tab) => {
                        necks = necks.union(&tab.neck)?;
                        perforations = perforations.union(&tab.perforations)?;
                        holes.extend(tab.npth);
                        break_rows.push(tab.break_path);
                    }
                    Err(error) => {
                        rejected = Some((
                            c,
                            format!(
                                "site at ({:.2}, {:.2}): {error:#}",
                                site.point.x, site.point.y
                            ),
                        ));
                        break 'boards;
                    }
                }
            }
            // The router clears the slot except where necks bridge it. Its
            // disk cannot reach into the corners where a neck meets the slot
            // walls, so the void is opened by the cutter around the necks and
            // follows the outline exactly everywhere else.
            let void = slot.difference(board)?.difference(&necks)?;
            let removal = void
                .disk_open(radius)?
                .union(&void.difference(&necks.disk_dilate(2.0 * radius)?)?)?;
            cutouts.extend(removal.connected_components());
        }
        if let Some((c, reason)) = rejected {
            excluded.insert(c);
            dropped.push(reason);
            continue;
        }
        let witnesses = std::iter::once(frame_witness(&frame)?)
            .chain(offsets.iter().map(|&offset| {
                let site = candidates[chosen[0]].site;
                site.point + offset - site.outward_normal * WITNESS_DEPTH_MM
            }))
            .collect::<Vec<_>>();
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

/// How far inside the board a witness point sits.
const WITNESS_DEPTH_MM: f64 = 1.0;
/// Half-size of the window a tab is built in: the drill row, neck, shoulders
/// and frame landing all lie within a few millimetres of the site.
const LOCAL_MM: f64 = 10.0;

/// One tab at `point` on `board`, built in a window around the site so the
/// cost of a tab does not grow with the panel. The builder wants stock beyond
/// the support it lands on, so the support ring stops short of the window.
#[allow(clippy::too_many_arguments)]
fn build_tab(
    point: Point,
    normal: Point,
    board: &ContourSet,
    slot: &ContourSet,
    frame: &ContourSet,
    stock: &ContourSet,
    preset: &Preset,
    resolution: Resolution,
    tolerance: QueryTolerance,
) -> Result<TabGeometry> {
    let reach = preset.routing_gap_mm + preset.frame_landing_mm;
    let window = ContourSet::rectangle(
        BBox::new(
            point - Point::new(LOCAL_MM, LOCAL_MM),
            point + Point::new(LOCAL_MM, LOCAL_MM),
        ),
        resolution,
    );
    let local_board = board.intersection(&window)?;
    let support_anchor = point + normal * (preset.routing_gap_mm + reach) / 2.0;
    // The frame the tab lands on; a small board's window can also hold
    // frame beyond its far side or inside its notches.
    let local_support = frame
        .intersection(
            &slot
                .intersection(&window)?
                .disk_dilate(preset.frame_landing_mm + 1.0)?
                .intersection(&window)?,
        )?
        .connected_components()
        .into_iter()
        .find(|piece| piece.contains_point(support_anchor))
        .context("the tab's landing is not frame material")?;
    let local_stock = stock.intersection(&window)?;
    let query = BoundaryQuery::new(&local_board, tolerance)?;
    let projection = query
        .boundaries()
        .map(|id| query.project(id, point))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min_by(|a, b| a.distance.mm.total_cmp(&b.distance.mm))
        .context("board instance has no boundary")?;
    Ok(build(Attachment {
        stock: &local_stock,
        board: &local_board,
        support: &local_support,
        boundary: projection.site.boundary,
        station_mm: projection.site.station_mm,
        support_anchor,
        board_witness: point - normal * WITNESS_DEPTH_MM,
        tolerance,
    })?)
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

/// Connected pieces of `region` at the cutter's own significance: an island
/// smaller than the cutter radius squared is residue of booleans along
/// coincident edges, not something a router leaves.
fn routable_components(region: &ContourSet, resolution: Resolution) -> usize {
    let routable = resolution.with_tolerance(SparkFunShallow::CUTTER_RADIUS_MM);
    region
        .connected_components()
        .into_iter()
        .filter(|piece| {
            !ContourSet::from_regularized(piece.rings.clone(), routable, piece.uncertainty_mm)
                .is_empty()
        })
        .count()
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
            "a routed void has {} rings; each must be a single loop",
            cutout.rings.len()
        );
    };
    ensure!(
        ring_signed_area(ring) > 0.0 && ring.len() >= 3,
        "a routed void has no area"
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
