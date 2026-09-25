//! Where a tab could sit: eligible outline runs sampled at a pitch and at
//! their ends. A site is kept where the outline is straight enough for the tab,
//! the strip across the slot stays out of the board, and frame material lies
//! beyond the slot. Holes cannot reach the frame and are skipped.

use anyhow::Result;
use pcb_ir::geom::{
    ContourSet, FillRule, Point, Resolution,
    attachment::{
        BoundaryId, BoundaryQuery, BoundarySite, QueryTolerance,
        outline::{OutlineInterval, OutlineState},
    },
    region::{ring_edges, ring_signed_area},
};

use super::{Preset, select::Site};

pub struct Candidate {
    pub ring: usize,
    pub station_mm: f64,
    pub site: Site,
}

pub struct Rejection {
    pub ring: usize,
    pub station_mm: f64,
    pub point: Point,
    pub reason: String,
}

pub struct Sites {
    pub candidates: Vec<Candidate>,
    pub rejected: Vec<Rejection>,
    /// Frame material beyond an outline-following slot, local to this board.
    pub frame: ContourSet,
}

const TOLERANCE: QueryTolerance = QueryTolerance {
    boundary_mm: 0.0,
    numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
};

pub fn find(
    substrate: &ContourSet,
    intervals: &[OutlineInterval],
    preset: &Preset,
    resolution: Resolution,
) -> Result<Sites> {
    let boundary = BoundaryQuery::new(substrate, TOLERANCE)?;
    let reach = preset.routing_gap_mm + preset.frame_landing_mm;
    let frame = ContourSet::rectangle(substrate.bbox().expand(reach + 1.0), resolution)
        .difference(&substrate.disk_dilate(preset.routing_gap_mm)?)?;
    let checker = Checker {
        substrate,
        frame: &frame,
        preset,
        resolution,
    };
    let mut sites = Sites {
        candidates: Vec::new(),
        rejected: Vec::new(),
        frame: ContourSet::empty(resolution),
    };
    for id in boundary
        .boundaries()
        .filter(|id| ring_signed_area(&substrate.rings[id.ring]) > 0.0)
    {
        let perimeter = boundary.perimeter(id)?;
        let turns = turning_angles(&substrate.rings[id.ring]);
        for (lo, hi) in eligible_runs(intervals, id, perimeter) {
            // A site every pitch, and one at each end of the run: on a short
            // run the ends are the sites that matter. A run is open, so an end
            // site sits one geometry error inside it. A run around the whole
            // ring has no ends.
            let bins = ((hi - lo) / preset.candidate_pitch_mm).ceil().max(1.0) as usize;
            let centers = (0..bins).map(|k| lo + (hi - lo) * (k as f64 + 0.5) / bins as f64);
            let inset = resolution.accuracy.max_error_mm();
            let ends = (!touching(hi - lo, perimeter) && hi - lo > 2.0 * inset)
                .then_some([lo + inset, hi - inset]);
            for station_mm in centers.chain(ends.into_iter().flatten()) {
                let station_mm = station_mm % perimeter;
                let site = boundary.site(id, station_mm)?;
                match checker.rejection(&site, &turns, perimeter)? {
                    Some(reason) => sites.rejected.push(Rejection {
                        ring: id.ring,
                        station_mm,
                        point: site.point,
                        reason,
                    }),
                    None => sites.candidates.push(Candidate {
                        ring: id.ring,
                        station_mm,
                        site: Site {
                            point: site.point,
                            outward_normal: site.outward_normal,
                        },
                    }),
                }
            }
        }
    }
    sites.frame = frame;
    Ok(sites)
}

struct Checker<'a> {
    substrate: &'a ContourSet,
    frame: &'a ContourSet,
    preset: &'a Preset,
    /// Strips share edges with the board and the slot, so what a boolean
    /// leaves of one is judged at this significance: residue along a shared
    /// edge is neither the board crossing back nor missing frame.
    resolution: Resolution,
}

impl Checker<'_> {
    /// Why a tab cannot sit at `site`, if it cannot.
    fn rejection(
        &self,
        site: &BoundarySite,
        turns: &[(f64, f64)],
        perimeter: f64,
    ) -> Result<Option<String>> {
        let p = self.preset;
        if let Some(reason) = too_tight(turns, perimeter, site.station_mm, p) {
            return Ok(Some(reason));
        }
        let across = self.strip(site, 0.0, p.routing_gap_mm)?;
        if across.intersection(self.substrate)?.area() > 0.0 {
            return Ok(Some("tab crosses back into the board".into()));
        }
        let landing = self.strip(
            site,
            p.routing_gap_mm,
            p.routing_gap_mm + p.frame_landing_mm,
        )?;
        if !landing.difference(self.frame)?.is_empty() {
            return Ok(Some("no frame material beyond the slot".into()));
        }
        Ok(None)
    }

    /// Rectangle of the tab width across the tangent, spanning `lo..hi`
    /// outward along the normal.
    fn strip(&self, site: &BoundarySite, lo: f64, hi: f64) -> Result<ContourSet> {
        let (p, t, n) = (site.point, site.tangent, site.outward_normal);
        let half = t * (self.preset.tab_width_mm / 2.0);
        let corners = [
            p - half + n * lo,
            p + half + n * lo,
            p + half + n * hi,
            p - half + n * hi,
        ];
        Ok(ContourSet::from_rings(
            vec![corners.map(|c| [c.x, c.y]).to_vec()],
            FillRule::EvenOdd,
            self.resolution,
        )?)
    }
}

/// Why the outline at `station` is too curved for a tab, if it is: within the
/// tab it may turn no more than an arc of the minimum radius would.
fn too_tight(
    turns: &[(f64, f64)],
    perimeter: f64,
    station: f64,
    preset: &Preset,
) -> Option<String> {
    let limit = (preset.tab_width_mm / preset.min_tab_radius_mm).to_degrees();
    let bend = bend_within(turns, perimeter, station, preset.tab_width_mm / 2.0);
    (bend > limit).then(|| format!("outline turns {bend:.0}° within the tab"))
}

/// Whether two stations along a ring are the same one.
fn touching(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// Contiguous Eligible arclength runs on one ring, joined across the seam.
/// A run through the seam is returned with `hi` beyond the perimeter.
fn eligible_runs(intervals: &[OutlineInterval], id: BoundaryId, perimeter: f64) -> Vec<(f64, f64)> {
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for interval in intervals
        .iter()
        .filter(|i| i.boundary == id && i.state == OutlineState::Eligible)
    {
        match runs.last_mut() {
            Some(last) if touching(last.1, interval.start_mm) => last.1 = interval.end_mm,
            _ => runs.push((interval.start_mm, interval.end_mm)),
        }
    }
    if let [first, .., last] = runs.as_mut_slice()
        && touching(first.0, 0.0)
        && touching(last.1, perimeter)
    {
        last.1 = perimeter + first.1;
        runs.remove(0);
    }
    runs
}

/// Unsigned turning angle at each vertex, in degrees, with the vertex's
/// station. Flattened arcs turn a little at many vertices; corners a lot at one.
fn turning_angles(ring: &Vec<[f64; 2]>) -> Vec<(f64, f64)> {
    let edges: Vec<(Point, Point)> = ring_edges(ring).collect();
    let mut station = 0.0;
    let mut turns = Vec::with_capacity(edges.len());
    for (i, (a, b)) in edges.iter().enumerate() {
        let (prev_a, prev_b) = edges[(i + edges.len() - 1) % edges.len()];
        let incoming = prev_b - prev_a;
        let outgoing = *b - *a;
        let angle = (incoming.x * outgoing.y - incoming.y * outgoing.x)
            .atan2(incoming.x * outgoing.x + incoming.y * outgoing.y)
            .abs()
            .to_degrees();
        turns.push((station, angle));
        station += a.distance_to(*b);
    }
    turns
}

/// Total turning strictly within `half` of `station` along the cyclic ring.
fn bend_within(turns: &[(f64, f64)], perimeter: f64, station: f64, half: f64) -> f64 {
    turns
        .iter()
        .filter(|(s, _)| {
            let d = (s - station).rem_euclid(perimeter);
            d.min(perimeter - d) < half
        })
        .map(|(_, angle)| angle)
        .sum()
}

/// Points a load may act at: every `spacing` of arclength along the outer
/// rings. The rigid response is a convex function of position, so it peaks
/// on the outline. The local bending term is a cantilever from the nearest
/// tab, which is the right picture along a free edge; a point inside the
/// board is farther from the tabs but surrounded by supported material, and
/// the plate between supports is stiffer than any edge cantilever, so the
/// outline governs there too.
pub fn load_points(region: &ContourSet, spacing: f64) -> Vec<Point> {
    let mut points = Vec::new();
    for ring in region.rings.iter().filter(|r| ring_signed_area(r) > 0.0) {
        let (mut next, mut station) = (0.0, 0.0);
        for (a, b) in ring_edges(ring) {
            let length = a.distance_to(b);
            while next < station + length {
                points.push(a + (b - a) * ((next - station) / length));
                next += spacing;
            }
            station += length;
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bends_distinguish_corners_from_gentle_arcs() {
        // A 40 x 10 rectangle whose top-right corner is a 12-vertex arc of
        // radius 4, plus a collinear split on the bottom edge.
        let mut ring = vec![[0.0, 0.0], [15.0, 0.0], [40.0, 0.0], [40.0, 6.0]];
        for k in 1..12 {
            let a = std::f64::consts::FRAC_PI_2 * k as f64 / 12.0;
            ring.push([36.0 + 4.0 * a.cos(), 6.0 + 4.0 * a.sin()]);
        }
        ring.extend([[36.0, 10.0], [0.0, 10.0]]);
        let turns = turning_angles(&ring);
        let perimeter: f64 = ring_edges(&ring).map(|(a, b)| a.distance_to(b)).sum();
        let preset = &crate::commands::board_array::placement::PRESET;
        // Mid bottom edge: the collinear vertex adds no turning.
        assert!(bend_within(&turns, perimeter, 20.0, 1.5) < 1e-9);
        // Sharp bottom-right corner at station 40: a tab may sit beside it,
        // never over it.
        assert!((bend_within(&turns, perimeter, 39.0, 1.5) - 90.0).abs() < 1e-9);
        assert!(too_tight(&turns, perimeter, 39.0, preset).is_some());
        assert!(too_tight(&turns, perimeter, 38.5, preset).is_none());
        // A 4 mm radius turns about 43° within a 3 mm tab: too tight for one.
        let mid_arc = 40.0 + 6.0 + std::f64::consts::FRAC_PI_2 * 4.0 / 2.0;
        let within_tab = bend_within(&turns, perimeter, mid_arc, 1.5);
        assert!(within_tab > 15.0 && within_tab < 90.0, "{within_tab}");
        assert!(too_tight(&turns, perimeter, mid_arc, preset).is_some());
        // Across the seam: the top-left corner sits at station 0.
        assert!((bend_within(&turns, perimeter, perimeter - 1.0, 2.0) - 90.0).abs() < 1e-9);
        // A 40 mm radius flattened at the same angle per vertex turns about
        // 4° within a tab: usable.
        let big: Vec<[f64; 2]> = (0..48)
            .map(|k| {
                let a = std::f64::consts::TAU * k as f64 / 48.0;
                [40.0 * a.cos(), 40.0 * a.sin()]
            })
            .collect();
        let turns = turning_angles(&big);
        let perimeter: f64 = ring_edges(&big).map(|(a, b)| a.distance_to(b)).sum();
        assert!(bend_within(&turns, perimeter, 10.0, 1.5) < 15.0);
        assert!(too_tight(&turns, perimeter, 10.0, preset).is_none());
    }

    #[test]
    fn a_strip_along_a_diagonal_edge_is_not_the_board_crossing_back() {
        // A chamfered corner. A tab's strip shares its base with the diagonal
        // edge, where rounding leaves the two a hair apart or a hair
        // overlapped from one site to the next; neither is the board.
        let ring = vec![
            [100.0, -114.0],
            [100.0, -141.5],
            [100.382683, -142.42388],
            [106.889612, -149.087824],
            [108.0, -149.5],
            [125.0, -149.5],
            [125.0, -114.0],
        ];
        let resolution = Resolution::default();
        let board =
            ContourSet::from_rings(vec![ring], FillRule::NonZero, resolution.strict()).unwrap();
        let id = BoundaryId {
            component: 0,
            ring: 0,
        };
        // Sites every 10 µm wherever the tab lies within the straight chamfer.
        let chamfer =
            27.5 + Point::new(100.0, -141.5).distance_to(Point::new(100.382683, -142.42388));
        let run = OutlineInterval {
            boundary: id,
            edge: 2,
            start_mm: chamfer + 1.55,
            end_mm: chamfer + 7.75,
            start: Point::ZERO,
            end: Point::ZERO,
            state: OutlineState::Eligible,
            landing: OutlineState::Eligible,
            obstacles: Vec::new(),
            uncertainty_mm: 0.0,
        };
        let preset = Preset {
            candidate_pitch_mm: 0.01,
            min_tab_radius_mm: 1.0,
            ..crate::commands::board_array::placement::PRESET
        };
        let sites = find(&board, &[run], &preset, resolution).unwrap();
        let reasons = sites
            .rejected
            .iter()
            .map(|r| format!("{:.3}: {}", r.station_mm - chamfer, r.reason))
            .collect::<Vec<_>>();
        assert!(reasons.is_empty(), "{reasons:?}");
        assert_eq!(sites.candidates.len(), 622);
    }

    #[test]
    fn a_short_run_beside_a_corner_offers_its_ends() {
        // The clear edge between a corner and a castellated row: 4 mm of
        // sites starting a tab's half width from the corner. The ends are
        // taken just inside the open run.
        let resolution = Resolution::default();
        let board = ContourSet::rectangle(
            pcb_ir::geom::BBox::new(Point::ZERO, Point::new(20.0, 10.0)),
            resolution,
        );
        let run = OutlineInterval {
            boundary: BoundaryId {
                component: 0,
                ring: 0,
            },
            edge: 0,
            start_mm: 1.6,
            end_mm: 5.6,
            start: Point::ZERO,
            end: Point::ZERO,
            state: OutlineState::Eligible,
            landing: OutlineState::Eligible,
            obstacles: Vec::new(),
            uncertainty_mm: 0.0,
        };
        let preset = crate::commands::board_array::placement::PRESET;
        let sites = find(&board, &[run], &preset, resolution).unwrap();
        assert!(sites.rejected.is_empty());
        let mut stations = sites
            .candidates
            .iter()
            .map(|c| c.station_mm)
            .collect::<Vec<_>>();
        stations.sort_by(f64::total_cmp);
        let inset = resolution.accuracy.max_error_mm();
        let expected = [1.6 + inset, 2.6, 4.6, 5.6 - inset];
        assert!(
            stations.len() == 4
                && stations
                    .iter()
                    .zip(expected)
                    .all(|(s, e)| (s - e).abs() < 1e-9),
            "{stations:?}"
        );
    }

    #[test]
    fn eligible_runs_merge_neighbors_and_join_across_the_seam() {
        let id = BoundaryId {
            component: 0,
            ring: 0,
        };
        let interval = |start_mm: f64, end_mm: f64, state: OutlineState| OutlineInterval {
            boundary: id,
            edge: 0,
            start_mm,
            end_mm,
            start: Point::ZERO,
            end: Point::ZERO,
            state,
            landing: OutlineState::Eligible,
            obstacles: Vec::new(),
            uncertainty_mm: 0.0,
        };
        let intervals = [
            interval(0.0, 10.0, OutlineState::Eligible),
            interval(10.0, 30.0, OutlineState::Eligible),
            interval(30.0, 50.0, OutlineState::Blocked),
            interval(50.0, 100.0, OutlineState::Eligible),
        ];
        assert_eq!(eligible_runs(&intervals, id, 100.0), vec![(50.0, 130.0)]);
    }

    #[test]
    fn load_points_follow_the_outline_at_the_spacing() {
        let region = ContourSet::rectangle(
            pcb_ir::geom::BBox::new(Point::ZERO, Point::new(20.0, 10.0)),
            Resolution::default(),
        );
        let points = load_points(&region, 2.0);
        assert_eq!(points.len(), 30);
        assert!(
            points
                .iter()
                .any(|p| p.y.abs() < 1e-9 && (p.x - 10.0).abs() < 1e-9)
        );
        assert!(points.iter().all(|p| p.x.abs() < 1e-9
            || p.y.abs() < 1e-9
            || (p.x - 20.0).abs() < 1e-9
            || (p.y - 10.0).abs() < 1e-9));
    }
}
