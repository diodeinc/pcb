//! Where a tab could sit: eligible outline runs sampled at a pitch. A site is
//! kept where the outline is straight enough for the tab and its keep-out,
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

pub fn find(
    substrate: &ContourSet,
    intervals: &[OutlineInterval],
    preset: &Preset,
    resolution: Resolution,
) -> Result<Sites> {
    let boundary = BoundaryQuery::new(
        substrate,
        QueryTolerance {
            boundary_mm: 0.0,
            numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
        },
    )?;
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
            let bins = ((hi - lo) / preset.candidate_pitch_mm).ceil().max(1.0) as usize;
            for k in 0..bins {
                let station_mm = (lo + (hi - lo) * (k as f64 + 0.5) / bins as f64) % perimeter;
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
        let half = p.tab_width_mm / 2.0;
        let tab_bend = bend_within(turns, perimeter, site.station_mm, half);
        if tab_bend > p.tab_bend_degrees {
            return Ok(Some(format!("outline turns {tab_bend:.0}° within the tab")));
        }
        let corner_bend = bend_within(
            turns,
            perimeter,
            site.station_mm,
            half + p.corner_keepout_mm,
        );
        if corner_bend > p.corner_bend_degrees {
            return Ok(Some(format!(
                "outline turns {corner_bend:.0}° within the corner keep-out"
            )));
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

/// Contiguous Eligible arclength runs on one ring, joined across the seam.
/// A run through the seam is returned with `hi` beyond the perimeter.
fn eligible_runs(intervals: &[OutlineInterval], id: BoundaryId, perimeter: f64) -> Vec<(f64, f64)> {
    let touching = |a: f64, b: f64| (a - b).abs() < 1e-9;
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
        // Mid bottom edge: the collinear vertex adds no turning.
        assert!(bend_within(&turns, perimeter, 20.0, 6.5) < 1e-9);
        // Sharp bottom-right corner at station 40: 90° inside any window.
        assert!((bend_within(&turns, perimeter, 39.0, 1.5) - 90.0).abs() < 1e-9);
        assert!((bend_within(&turns, perimeter, 34.0, 6.5) - 90.0).abs() < 1e-9);
        // A 4 mm radius turns about 43° within a 3 mm tab and most of the
        // quarter turn within the keep-out window: too tight for a tab.
        let mid_arc = 40.0 + 6.0 + std::f64::consts::FRAC_PI_2 * 4.0 / 2.0;
        let within_tab = bend_within(&turns, perimeter, mid_arc, 1.5);
        assert!(within_tab > 15.0 && within_tab < 90.0, "{within_tab}");
        assert!(bend_within(&turns, perimeter, mid_arc, 6.5) > 60.0);
        // Across the seam: the top-left corner sits at station 0.
        assert!((bend_within(&turns, perimeter, perimeter - 1.0, 2.0) - 90.0).abs() < 1e-9);
        // A 40 mm radius flattened at the same angle per vertex turns about
        // 4° within a tab and 19° within the keep-out window: usable.
        let big: Vec<[f64; 2]> = (0..48)
            .map(|k| {
                let a = std::f64::consts::TAU * k as f64 / 48.0;
                [40.0 * a.cos(), 40.0 * a.sin()]
            })
            .collect();
        let turns = turning_angles(&big);
        let perimeter: f64 = ring_edges(&big).map(|(a, b)| a.distance_to(b)).sum();
        assert!(bend_within(&turns, perimeter, 10.0, 1.5) < 15.0);
        assert!(bend_within(&turns, perimeter, 10.0, 6.5) < 60.0);
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
