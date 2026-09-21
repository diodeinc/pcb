//! Routed slots and perforated tabs for a board array: the material the
//! router removes around every board instance, the tabs left bridging each
//! slot, and their break holes. The array spec turns the removal into profile
//! cutouts and the holes into non-plated drills.
//!
//! Every board margin holds its slot and the frame its tabs land on, so one
//! board cell is all a tab ever meets: the geometry is built once, in the
//! board's own coordinates, and repeated with the cell. Each tab comes from
//! the single-tab builder in pcb-ir, given the board, the cell's frame and a
//! local window of both; the holes and break rows are the builder's, and the
//! removal is the builder's own routed void of the slot around every neck, so
//! the panel emits the construction each tab was certified with. The emitted
//! panel is then checked as a whole: no routed void reaches into any board,
//! every board is connected to the frame before the break rows are cut, and
//! every board is free of the frame and of each other after.

use anyhow::{Context, Result, bail, ensure};
use ipc2581::types::Polygon;
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, ContourSet, LineCap, LineJoin, Point, Resolution, StrokeToFillStyle,
    attachment::{BoundaryQuery, QueryTolerance, material_after_break, transform_region},
    mouse_bite::{Attachment, Npth, SparkFunShallow, TabGeometry, build, routed_void},
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
}

/// Tab and slot geometry for `placement`'s board, which sits in `cell` in its
/// own coordinates and is repeated at `offsets` inside `stock`. Sites the
/// builder rejects are dropped and the placement is re-solved without them,
/// so the result always holds the board or fails with the placement's own
/// reason.
pub(super) fn generate(
    placement: &Placement,
    cell: BBox,
    stock: &ContourSet,
    offsets: &[Point],
    preset: &Preset,
    resolution: Resolution,
) -> Result<Tabs> {
    let cell = Cell::new(&placement.prepared.substrate, cell, preset, resolution)?;
    let candidates = &placement.sites.candidates;
    // Every site is built at most once, however often the placement is
    // re-solved around the ones that fail.
    let mut tabs: Vec<Option<Result<TabGeometry>>> = candidates.iter().map(|_| None).collect();
    let mut kept: Vec<usize> = (0..candidates.len()).collect();
    let mut dropped = Vec::new();
    let mut selection = placement.selection.clone();
    let chosen = loop {
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
        for &c in &chosen {
            tabs[c].get_or_insert_with(|| cell.tab(candidates[c].site, preset, resolution));
        }
        let rejected = chosen
            .iter()
            .find_map(|&c| tabs[c].as_ref()?.as_ref().err().map(|error| (c, error)));
        let Some((c, error)) = rejected else {
            break chosen;
        };
        let site = candidates[c].site;
        dropped.push(format!(
            "site at ({:.2}, {:.2}): {error:#}",
            site.point.x, site.point.y
        ));
        kept.retain(|&k| k != c);
        let sites: Vec<_> = kept.iter().map(|&i| candidates[i].site).collect();
        selection = select::select(&sites, &placement.loads, &placement.model);
    };
    let tabs: Vec<&TabGeometry> = chosen
        .iter()
        .filter_map(|&c| tabs[c].as_ref()?.as_ref().ok())
        .collect();

    // The router clears the slot except where necks bridge it.
    let strict = resolution.strict();
    let necks = ContourSet::union_all(strict, tabs.iter().map(|tab| tab.neck.clone()))?;
    let voids = routed_void(&cell.slot.difference(&cell.board)?, &necks)?.connected_components();
    let perforations =
        ContourSet::union_all(strict, tabs.iter().map(|tab| tab.perforations.clone()))?;

    let placed = |region: &ContourSet, offset: Point| {
        Ok::<_, anyhow::Error>(transform_region(region, Affine2::translation(offset))?)
    };
    let cutouts = offsets
        .iter()
        .flat_map(|&offset| voids.iter().map(move |void| placed(void, offset)))
        .collect::<Result<Vec<_>>>()?;
    let boards = offsets
        .iter()
        .map(|&offset| placed(&cell.board, offset))
        .collect::<Result<Vec<_>>>()?;
    let perforations = ContourSet::union_all(
        strict,
        offsets
            .iter()
            .map(|&offset| placed(&perforations, offset))
            .collect::<Result<Vec<_>>>()?,
    )?;
    let break_rows = offsets
        .iter()
        .flat_map(|&offset| {
            tabs.iter().map(move |tab| {
                tab.break_path
                    .clone()
                    .transformed(Affine2::translation(offset))
            })
        })
        .collect::<Vec<_>>();
    let inside = candidates[chosen[0]].site;
    let witnesses = std::iter::once(rail_witness(stock)?)
        .chain(
            offsets
                .iter()
                .map(|&offset| inside.point + offset - inside.outward_normal * WITNESS_DEPTH_MM),
        )
        .collect::<Vec<_>>();
    check_release(
        stock,
        &boards,
        &cutouts,
        &perforations,
        &break_rows,
        &witnesses,
        resolution,
    )?;
    Ok(Tabs {
        cutouts,
        holes: offsets
            .iter()
            .flat_map(|&offset| {
                tabs.iter().flat_map(|tab| &tab.npth).map(move |hole| Npth {
                    center: hole.center + offset,
                    ..*hole
                })
            })
            .collect(),
        per_board: chosen.len(),
    })
}

/// Boards and stock are the polygon model itself, with no external
/// uncertainty beyond what their regions already carry.
const TOLERANCE: QueryTolerance = QueryTolerance {
    boundary_mm: 0.0,
    numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
};
/// How far inside the board a witness point sits.
const WITNESS_DEPTH_MM: f64 = 1.0;
/// Half-size of the window a tab is built in: the drill row, neck, shoulders
/// and frame landing all lie within a few millimetres of the site.
const LOCAL_MM: f64 = 10.0;

/// One board in its cell, in the board's own coordinates: everything a tab of
/// that board is built against.
struct Cell {
    /// The board's outer boundary, filled: slots follow it, and a board's own
    /// holes stay the board's.
    board: ContourSet,
    /// The board grown by the routing gap.
    slot: ContourSet,
    /// Cell material beyond the slot, continuous with the rails around it.
    frame: ContourSet,
    region: ContourSet,
}

impl Cell {
    fn new(
        substrate: &ContourSet,
        cell: BBox,
        preset: &Preset,
        resolution: Resolution,
    ) -> Result<Self> {
        let board = ContourSet::from_regularized(
            substrate
                .rings
                .iter()
                .filter(|ring| ring_signed_area(ring) > 0.0)
                .cloned()
                .collect(),
            substrate.resolution,
            substrate.uncertainty_mm,
        );
        let slot = board.disk_dilate(preset.routing_gap_mm)?;
        let region = ContourSet::rectangle(cell, resolution.strict());
        let frame = region.difference(&slot)?;
        ensure!(
            routable_components(&frame, resolution) == 1,
            "the routed slot cuts frame material loose from the rails around the board"
        );
        Ok(Self {
            board,
            slot,
            frame,
            region,
        })
    }

    /// One tab at `site`, built in a window around it so the cost of a tab
    /// does not grow with the board.
    fn tab(
        &self,
        site: select::Site,
        preset: &Preset,
        resolution: Resolution,
    ) -> Result<TabGeometry> {
        let (point, normal) = (site.point, site.outward_normal);
        let window = ContourSet::rectangle(
            BBox::new(
                point - Point::new(LOCAL_MM, LOCAL_MM),
                point + Point::new(LOCAL_MM, LOCAL_MM),
            ),
            resolution,
        );
        let board = self.board.intersection(&window)?;
        // The middle of the landing, beyond the slot.
        let landing_end = preset.routing_gap_mm + preset.frame_landing_mm;
        let support_anchor = point + normal * (preset.routing_gap_mm + landing_end) / 2.0;
        // The frame the tab lands on; a small board's window can also hold
        // frame beyond its far side or inside its notches.
        let support = self
            .frame
            .intersection(&window)?
            .connected_components()
            .into_iter()
            .find(|piece| piece.contains_point(support_anchor))
            .context("the tab's landing is not frame material")?;
        let stock = self.region.intersection(&window)?;
        let query = BoundaryQuery::new(&board, TOLERANCE)?;
        let projection = query
            .boundaries()
            .map(|id| query.project(id, point))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .min_by(|a, b| a.distance.mm.total_cmp(&b.distance.mm))
            .context("board has no boundary")?;
        Ok(build(Attachment {
            stock: &stock,
            board: &board,
            support: &support,
            boundary: projection.site.boundary,
            station_mm: projection.site.station_mm,
            support_anchor,
            board_witness: point - normal * WITNESS_DEPTH_MM,
            tolerance: TOLERANCE,
        })?)
    }
}

/// A point on the bottom edge rail, away from the rounded corners. No slot
/// reaches a rail: each is cut from its own board's margin.
fn rail_witness(stock: &ContourSet) -> Result<Point> {
    let bbox = stock.bbox();
    let point = Point::new(bbox.center().x, bbox.min.y + 1.0);
    ensure!(
        stock.contains_point(point),
        "the bottom rail is not stock at its middle"
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

/// The router may not remove any board's material, every board must connect
/// to the frame through its tabs, and cutting every break row must free every
/// board from the frame and from each other.
fn check_release(
    stock: &ContourSet,
    boards: &[ContourSet],
    cutouts: &[ContourSet],
    perforations: &ContourSet,
    break_rows: &[ContourBuf],
    witnesses: &[Point],
    significance: Resolution,
) -> Result<()> {
    let resolution = significance.strict();
    let routed = ContourSet::union_all(resolution, cutouts.iter().cloned())?;
    // Voids share their inner wall with the board they free, so the overlap
    // is judged at the caller's significance: coincident-edge residue is not
    // a bite, anything the board's own image would keep is.
    let bitten =
        ContourSet::union_all(resolution, boards.iter().cloned())?.intersection(&routed)?;
    let bitten = ContourSet::from_regularized(bitten.rings, significance, bitten.uncertainty_mm);
    ensure!(
        bitten.is_empty(),
        "routed slots remove {:.3} mm² of board material",
        bitten.area()
    );
    let retained = stock.difference(&routed)?.difference(perforations)?;
    let boards = 1..witnesses.len();
    let held = material_after_break(
        &retained,
        &ContourSet::empty(resolution),
        witnesses,
        TOLERANCE,
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
    let released = material_after_break(&retained, &rows, witnesses, TOLERANCE)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
        ContourSet::rectangle(
            BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
            Resolution::default().strict(),
        )
    }

    #[test]
    fn release_check_rejects_a_void_that_reaches_into_a_board() {
        let stock = rect(0.0, 0.0, 100.0, 40.0);
        let boards = [rect(10.0, 10.0, 30.0, 20.0), rect(40.0, 10.0, 30.0, 20.0)];
        // The first board's slot, cut 1.4 mm into its abutting neighbour.
        let slot = rect(40.0, 10.0, 1.4, 20.0);
        let error = check_release(
            &stock,
            &boards,
            &[slot],
            &ContourSet::empty(Resolution::default()),
            &[],
            &[Point::new(50.0, 1.0)],
            Resolution::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("routed slots remove 28.000 mm² of board material"),
            "{error}"
        );
    }
}
