//! Where mouse-bite tabs go on one canonical board: candidate sites along the
//! Eligible outline, then the fewest tabs that hold the board rigidly.
//! Analysis only: no tab geometry, routing, frame, or panel export.
//!
//! The panel is assumed to route a slot of `routing_gap_mm` that follows the
//! board outline, with frame material beyond it. A candidate is a straight
//! strip of the tab width from the outline across the slot into that frame.

pub mod select;

use anyhow::Result;
use pcb_ir::geom::{
    ContourSet, FillRule, Point, Resolution,
    attachment::{
        BoundaryId, BoundaryQuery, QueryTolerance,
        outline::{OutlineFootprint, OutlineState},
    },
    region::{ring_edges, ring_signed_area},
};
use serde_json::{Value, json};

use super::eligibility::{self, Prepared};
use pcb_ir::geom::mouse_bite::SparkFunShallow;
use select::{Laminate, Model, Process, Rail, Site, TabBeam};

/// Opinionated placement preset, in millimeters and newtons. Not user knobs.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Preset {
    /// Tab width along the outline, neck plus router shoulders.
    pub tab_width_mm: f64,
    /// How far the perforations reach into the board.
    pub inward_mm: f64,
    /// Growth applied to courtyards before they count as obstacles.
    pub courtyard_clearance_mm: f64,
    /// Routed slot between the board outline and the frame.
    pub routing_gap_mm: f64,
    /// Frame material a tab must reach beyond the slot.
    pub frame_landing_mm: f64,
    /// Candidate spacing along usable outline runs.
    pub candidate_pitch_mm: f64,
    /// Outline may turn at most this much within the tab's own width.
    pub tab_bend_degrees: f64,
    /// Outline may turn at most this much within the tab plus the keep-out on
    /// either side, which keeps tabs clear of corners without excluding round
    /// boards.
    pub corner_bend_degrees: f64,
    pub corner_keepout_mm: f64,
    /// Closest two tabs may sit.
    pub min_separation_mm: f64,
    /// Spacing of the outline points the load may act at.
    pub load_point_spacing_mm: f64,
    /// Assumed when the source states no stackup thickness.
    pub default_thickness_mm: f64,
    pub laminate: Laminate,
    pub tab: TabBeam,
    pub rail: Rail,
    pub process: Process,
}

pub const PRESET: Preset = Preset {
    tab_width_mm: SparkFunShallow::NECK_WIDTH_MM + 2.0 * SparkFunShallow::CUTTER_RADIUS_MM,
    inward_mm: 0.2,
    courtyard_clearance_mm: 0.0,
    routing_gap_mm: 2.0,
    frame_landing_mm: 1.0,
    candidate_pitch_mm: 2.5,
    tab_bend_degrees: 15.0,
    corner_bend_degrees: 60.0,
    corner_keepout_mm: 5.0,
    min_separation_mm: 10.0,
    load_point_spacing_mm: 2.0,
    default_thickness_mm: 1.6,
    // Typical rigid FR-4, in-plane; the laminate is treated as isotropic.
    laminate: Laminate {
        youngs_modulus: 22_000.0,
        shear_modulus: 4_500.0,
        poisson_ratio: 0.13,
    },
    // The neck spans the routing gap; perforation halves its section.
    tab: TabBeam {
        width_mm: SparkFunShallow::NECK_WIDTH_MM,
        length_mm: 2.0,
        perforation_factor: 0.5,
    },
    // Two default 5 mm board margins minus a slot on each side.
    rail: Rail { width_mm: 6.0 },
    // A finger or placement nozzle anywhere on the board, and the sag that
    // still leaves the surface flat enough for placement.
    process: Process {
        load_n: 5.0,
        deflection_limit_mm: 0.25,
    },
};

impl Preset {
    /// The band checked for obstacles: the tab from its perforations to its
    /// frame landing.
    pub fn footprint(&self) -> OutlineFootprint {
        OutlineFootprint {
            width_mm: self.tab_width_mm,
            inward_mm: self.inward_mm,
            outward_mm: self.routing_gap_mm + self.frame_landing_mm,
        }
    }

    /// Cross rails sit one board span apart, so the rail a tab lands on is
    /// held that far apart at worst.
    pub fn model(&self, thickness_mm: f64, board_span_mm: f64) -> Model {
        Model::new(
            thickness_mm,
            board_span_mm,
            self.laminate,
            self.tab,
            self.rail,
            self.process,
            self.min_separation_mm,
        )
    }
}

/// Analyze one canonical board: eligibility plus tab placement.
pub fn analyze(xml: &str, preset: &Preset, resolution: Resolution) -> Result<Value> {
    let prepared = eligibility::prepare(
        xml,
        preset.footprint(),
        preset.courtyard_clearance_mm,
        &[],
        resolution,
    )?;
    let mut report = prepared.report.clone();
    report["phase"] = json!("tab-placement-only");
    report["placement"] = place(&prepared, preset, resolution.strict())?;
    Ok(report)
}

struct Candidate {
    ring: usize,
    station_mm: f64,
    site: Site,
}

fn place(prepared: &Prepared, preset: &Preset, resolution: Resolution) -> Result<Value> {
    let substrate = &prepared.substrate;
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
    };
    let boundary = BoundaryQuery::new(substrate, tolerance)?;
    let reach = preset.routing_gap_mm + preset.frame_landing_mm;
    // Frame material beyond an outline-following slot, locally to this board.
    let frame = ContourSet::rectangle(substrate.bbox().expand(reach + 1.0), resolution)
        .difference(&substrate.disk_dilate(preset.routing_gap_mm)?)?;

    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for id in boundary.boundaries() {
        let ring = &substrate.rings[id.ring];
        if ring_signed_area(ring) <= 0.0 {
            continue; // holes cannot reach the frame
        }
        let perimeter = boundary.perimeter(id)?;
        let turns = turning_angles(ring);
        let half = preset.tab_width_mm / 2.0;
        for (lo, hi) in eligible_runs(&prepared.intervals, id, perimeter) {
            let length = hi - lo;
            let bins = (length / preset.candidate_pitch_mm).ceil().max(1.0) as usize;
            for k in 0..bins {
                let station_mm = (lo + length * (k as f64 + 0.5) / bins as f64) % perimeter;
                let site = boundary.site(id, station_mm)?;
                let tab_bend = bend_within(&turns, perimeter, station_mm, half);
                let corner_bend = bend_within(
                    &turns,
                    perimeter,
                    station_mm,
                    half + preset.corner_keepout_mm,
                );
                if tab_bend > preset.tab_bend_degrees || corner_bend > preset.corner_bend_degrees {
                    let p = site.point;
                    rejected.push(json!({
                        "ring": id.ring, "station_mm": station_mm, "point": [p.x, p.y],
                        "reason": if tab_bend > preset.tab_bend_degrees {
                            format!("outline turns {tab_bend:.0}° within the tab")
                        } else {
                            format!("outline turns {corner_bend:.0}° within the corner keep-out")
                        },
                    }));
                    continue;
                }
                let (p, t, n) = (site.point, site.tangent, site.outward_normal);
                let width = preset.tab_width_mm;
                let across = strip(p, t, n, width, 0.0, preset.routing_gap_mm, resolution)?;
                let landing = strip(p, t, n, width, preset.routing_gap_mm, reach, resolution)?;
                let reason = if across.intersection(substrate)?.area() > 0.0 {
                    Some("tab crosses back into the board")
                } else if !landing.difference(&frame)?.is_empty() {
                    Some("no frame material beyond the slot")
                } else {
                    None
                };
                match reason {
                    Some(reason) => rejected.push(json!({
                        "ring": id.ring, "station_mm": station_mm, "point": [p.x, p.y], "reason": reason,
                    })),
                    None => candidates.push(Candidate {
                        ring: id.ring,
                        station_mm,
                        site: Site {
                            point: p,
                            outward_normal: n,
                        },
                    }),
                }
            }
        }
    }

    let loads = outline_samples(substrate, preset.load_point_spacing_mm);
    let bbox = substrate.bbox();
    let thickness_mm = prepared.thickness_mm.unwrap_or(preset.default_thickness_mm);
    let model = preset.model(thickness_mm, bbox.width().max(bbox.height()));
    let sites: Vec<_> = candidates.iter().map(|c| c.site).collect();
    let selection = select::select(&sites, &loads, &model);
    let rings = |region: &ContourSet| {
        region
            .rings
            .iter()
            .map(|ring| ring.iter().map(|p| json!([p[0], p[1]])).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "preset": preset,
        "model": model,
        "thickness_source": if prepared.thickness_mm.is_some() { "stackup" } else { "default" },
        "board": {"bbox": [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y]},
        "substrate": rings(substrate),
        "frame": rings(&frame),
        "obstacles": prepared.evidence.iter().filter_map(|e| {
            e.region.as_ref().map(|r| json!({"id": e.id, "rings": rings(r)}))
        }).collect::<Vec<_>>(),
        "candidates": candidates.iter().enumerate().map(|(i, c)| json!({
            "id": i, "ring": c.ring, "station_mm": c.station_mm,
            "point": [c.site.point.x, c.site.point.y],
            "normal": [c.site.outward_normal.x, c.site.outward_normal.y],
        })).collect::<Vec<_>>(),
        "rejected": rejected,
        "selected": selection.chosen,
        "tab_count": selection.chosen.len(),
        "deflection_mm": selection.deflection_mm.is_finite().then_some(selection.deflection_mm),
        "worst_point": selection.worst_point.map(|j| [loads[j].x, loads[j].y]),
        "satisfied": selection.satisfied(),
        "violations": selection.violations.iter().map(ToString::to_string).collect::<Vec<_>>(),
    }))
}

/// Contiguous Eligible arclength runs on one ring, joined across the seam.
/// A run through the seam is returned with `hi` beyond the perimeter.
fn eligible_runs(
    intervals: &[pcb_ir::geom::attachment::outline::OutlineInterval],
    id: BoundaryId,
    perimeter: f64,
) -> Vec<(f64, f64)> {
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for interval in intervals
        .iter()
        .filter(|i| i.boundary == id && i.state == OutlineState::Eligible)
    {
        match runs.last_mut() {
            Some(last) if last.1 == interval.start_mm => last.1 = interval.end_mm,
            _ => runs.push((interval.start_mm, interval.end_mm)),
        }
    }
    if runs.len() > 1 && runs[0].0 == 0.0 && runs.last().unwrap().1 == perimeter {
        let first = runs.remove(0);
        runs.last_mut().unwrap().1 = perimeter + first.1;
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

/// Rectangle of `width` across the tangent, spanning `lo..hi` along the normal.
fn strip(
    p: Point,
    tangent: Point,
    normal: Point,
    width: f64,
    lo: f64,
    hi: f64,
    resolution: Resolution,
) -> Result<ContourSet> {
    let corners = [
        p - tangent * (width / 2.0) + normal * lo,
        p + tangent * (width / 2.0) + normal * lo,
        p + tangent * (width / 2.0) + normal * hi,
        p - tangent * (width / 2.0) + normal * hi,
    ];
    Ok(ContourSet::from_rings(
        vec![corners.map(|c| [c.x, c.y]).to_vec()],
        FillRule::EvenOdd,
        resolution,
    )?)
}

/// Points every `spacing` of arclength along the outer rings. Loads act
/// here; the rigid response is convex, so the outline is the worst case.
fn outline_samples(region: &ContourSet, spacing: f64) -> Vec<Point> {
    let mut samples = Vec::new();
    for ring in region.rings.iter().filter(|r| ring_signed_area(r) > 0.0) {
        let mut next = 0.0;
        let mut station = 0.0;
        for (a, b) in ring_edges(ring) {
            let length = a.distance_to(b);
            while next < station + length {
                samples.push(a + (b - a) * ((next - station) / length));
                next += spacing;
            }
            station += length;
        }
    }
    samples
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &std::path::Path,
    output: &std::path::Path,
    resolution: Resolution,
) -> Result<()> {
    let xml = crate::utils::file::load_ipc_file(input)?;
    let report = analyze(&xml, &PRESET, resolution)?;
    let mut json = serde_json::to_vec_pretty(&report)?;
    json.push(b'\n');
    if output.as_os_str() == "-" {
        pcb_ui::write_stdout(|stdout| stdout.write_all(&json))?;
    } else {
        std::fs::write(output, json)?;
    }
    anstream::eprintln!("Tab placement analysis only; no mouse-bite panel generated.");
    Ok(())
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
}
