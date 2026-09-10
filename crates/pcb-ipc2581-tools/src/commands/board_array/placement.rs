//! Frame-only site selection and bending analysis. No fabrication geometry.
use anyhow::{Context, Result, ensure};
use pcb_elastic::Tolerances;
use pcb_ir::geom::{
    Affine2, BBox, ContourSet, FillRule, Point, Resolution,
    attachment::{
        BoundaryQuery, Decision, Obstacle, QueryTolerance, check_footprints,
        outline::{OutlineFootprint, OutlineInterval, OutlineState},
        transform_region,
    },
    mesh::MeshOptions,
};
use pcb_mechanics::planning::{self, LoadCase, Policy, Site};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{BoardArrayCreateOptions, eligibility};

/// All physical and search inputs are explicit. Matrices use global axes;
/// connection stiffness is referenced at each candidate's board boundary point.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub routing_gap_mm: f64,
    pub frame_landing_mm: f64,
    pub max_span_mm: f64,
    pub candidate_pitch_mm: f64,
    pub max_candidates: usize,
    pub bending: [[f64; 3]; 3],
    pub connection_stiffness: [[f64; 3]; 3],
    pub scales: [f64; 2],
    pub mesh_max_area_mm2: f64,
    pub mesh_min_angle_degrees: f64,
    pub mesh_max_additional_vertices: usize,
    pub max_dofs: usize,
    pub max_subsets: usize,
    /// Top, right, bottom, left exterior frame sides; homogeneous clamps.
    pub clamp_sides: [bool; 4],
    pub load_cases: Vec<Case>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Applied to every board, at its bbox center via a whole-area fitted plane.
    /// This is a generalized resultant, not a uniform pressure distribution.
    pub resultant_per_board: [f64; 3],
    pub compliance_limit: f64,
}

impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(
            [
                self.routing_gap_mm,
                self.frame_landing_mm,
                self.max_span_mm,
                self.candidate_pitch_mm,
                self.mesh_max_area_mm2,
            ]
            .iter()
            .all(|x| x.is_finite() && *x > 0.0),
            "placement distances and mesh area must be positive and finite"
        );
        ensure!(
            self.max_candidates > 0 && self.max_dofs > 0 && self.max_subsets > 0,
            "placement budgets must be positive"
        );
        ensure!(
            !self.load_cases.is_empty()
                && self.load_cases.iter().all(|c| c
                    .resultant_per_board
                    .iter()
                    .all(|x| x.is_finite())
                    && c.compliance_limit.is_finite()
                    && c.compliance_limit > 0.0),
            "invalid load cases"
        );
        ensure!(
            self.clamp_sides.iter().any(|x| *x),
            "at least one frame side must be clamped"
        );
        ensure!(
            self.scales.iter().all(|x| x.is_finite() && *x > 0.0)
                && self.mesh_min_angle_degrees.is_finite()
                && (0.0..=30.0).contains(&self.mesh_min_angle_degrees),
            "invalid mesh/scales"
        );
        // Validate physical matrices even when missing evidence leaves no sites.
        let matrix = |a: [[f64; 3]; 3]| pcb_elastic::DMatrix::from_fn(3, 3, |r, c| a[r][c]);
        pcb_elastic::Model::new(
            vec![1.0; 3],
            &[pcb_elastic::Contribution {
                dofs: vec![0, 1, 2],
                stiffness: matrix(self.connection_stiffness),
            }],
            tolerances(),
        )?;
        let bending = matrix(self.bending);
        ensure!(
            bending.iter().all(|x| x.is_finite())
                && (&bending - bending.transpose()).norm() <= 1e-12 * bending.norm()
                && bending.cholesky().is_some(),
            "bending tensor must be symmetric positive definite"
        );
        Ok(())
    }
}

fn tolerances() -> Tolerances {
    // Numerical acceptance only, never physical stiffness/compliance defaults.
    Tolerances {
        rank_relative: 1e-10,
        rank_absolute: 0.0,
        residual_relative: 1e-8,
        residual_absolute: 1e-10,
    }
}

struct Candidate {
    site: Site,
    ring: usize,
    station: f64,
    perimeter: f64,
    frame_point: Point,
    span: f64,
    envelope: ContourSet,
}

// Eligibility is split at every polygon edge for provenance. Sample connected
// arclength runs, not each tessellation fragment, so a finer arc does not force
// hundreds of extra candidates. Resolve each sample back to its source interval.
fn sample_intervals(
    intervals: &[OutlineInterval],
    pitch: f64,
) -> Result<impl Iterator<Item = (&OutlineInterval, f64)>> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, interval) in intervals.iter().enumerate() {
        if interval.state != OutlineState::Eligible {
            continue;
        }
        if let Some((_, last)) = runs.last_mut()
            && *last + 1 == index
            && intervals[*last].boundary == interval.boundary
            && intervals[*last].end_mm == interval.start_mm
        {
            *last = index;
        } else {
            runs.push((index, index));
        }
    }
    let runs = runs
        .into_iter()
        .map(|(first, last)| {
            let length = intervals[last].end_mm - intervals[first].start_mm;
            let count = (length / pitch).ceil().max(1.0);
            ensure!(
                count < usize::MAX as f64,
                "candidate pitch is too small to enumerate"
            );
            Ok((first, last, count as usize))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(runs.into_iter().flat_map(move |(first, last, count)| {
        let lo = intervals[first].start_mm;
        let length = intervals[last].end_mm - lo;
        (0..count).filter_map(move |k| {
            let station = lo + length * (k as f64 + 0.5) / count as f64;
            intervals[first..=last]
                .iter()
                .find(|i| i.start_mm < station && station < i.end_mm)
                .map(|interval| (interval, station))
        })
    }))
}

fn rectangle(bbox: BBox, resolution: Resolution) -> ContourSet {
    ContourSet::rectangle(bbox, resolution.strict())
}

fn strip(
    p: Point,
    tangent: Point,
    normal: Point,
    width: f64,
    lo: f64,
    hi: f64,
    resolution: Resolution,
) -> Result<ContourSet> {
    let points = [
        p - tangent * (width / 2.0) + normal * lo,
        p + tangent * (width / 2.0) + normal * lo,
        p + tangent * (width / 2.0) + normal * hi,
        p - tangent * (width / 2.0) + normal * hi,
    ];
    Ok(ContourSet::from_rings(
        vec![points.map(|v| [v.x, v.y]).to_vec()],
        FillRule::EvenOdd,
        resolution.strict(),
    )?)
}

/// A full-width landing occupies [s, s + depth] along the travel axis.
/// Each connected void in its swept corridor projects to one blocked interval:
/// linear projection of a connected set is an interval, with extrema at vertices.
/// Search the complement of their union, expanded by the uncertainty guard.
fn landing_start(
    frame: &ContourSet,
    corridor: &ContourSet,
    origin: Point,
    normal: Point,
    depth: f64,
    guard: f64,
) -> Result<f64> {
    let mut blocked = corridor
        .difference(frame)?
        .connected_components()
        .iter()
        .map(|component| {
            component.rings.iter().flatten().fold(
                (f64::INFINITY, f64::NEG_INFINITY),
                |(lo, hi), p| {
                    let s = (p[0] - origin.x) * normal.x + (p[1] - origin.y) * normal.y;
                    (lo.min(s), hi.max(s))
                },
            )
        })
        .collect::<Vec<_>>();
    blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut start = guard;
    for (lo, hi) in blocked {
        if start + depth + guard <= lo {
            break;
        }
        start = start.max(hi + guard);
    }
    Ok(start)
}

/// Analyze repeated boards in the existing array layout. The modeled frame is
/// panel stock minus rectangular board apertures with an explicit routing gap.
/// These are analysis domains, not tab masks, routes, perforations or export.
pub fn analyze(
    xml: &str,
    footprint: OutlineFootprint,
    clearance_mm: f64,
    options: &BoardArrayCreateOptions,
    config: &Config,
    resolution: Resolution,
) -> Result<Value> {
    config.validate()?;
    ensure!(
        footprint.inward_mm > 0.0,
        "mechanics requires positive inward landing depth"
    );
    let prepared = eligibility::prepare(xml, footprint, clearance_mm, &[], resolution)?;
    plan(prepared, footprint, options, config, resolution.strict())
}

fn plan(
    prepared: eligibility::Prepared,
    footprint: OutlineFootprint,
    options: &BoardArrayCreateOptions,
    config: &Config,
    resolution: Resolution,
) -> Result<Value> {
    ensure!(
        (1..=10).contains(&options.columns) && (1..=10).contains(&options.rows),
        "array counts must be 1..=10"
    );
    let m = options.board_margin_mm;
    let r = options.edge_rail_mm;
    ensure!(
        [m.top, m.right, m.bottom, m.left]
            .iter()
            .all(|x| x.is_finite() && *x > config.routing_gap_mm),
        "board margins must exceed routing_gap_mm to retain a connected internal frame; no board-to-board fallback"
    );
    ensure!(
        [r.top, r.right, r.bottom, r.left]
            .iter()
            .all(|x| x.is_finite() && *x > 0.0),
        "edge rails must be positive"
    );
    let bbox = prepared.substrate.bbox();
    let pitch = Point::new(
        bbox.width() + m.left + m.right,
        bbox.height() + m.top + m.bottom,
    );
    let size = Point::new(
        pitch.x * f64::from(options.columns) + r.left + r.right,
        pitch.y * f64::from(options.rows) + r.bottom + r.top,
    );
    // Match the existing array's outer stock profile, including its rounded
    // corners. Apertures below define only the analysis rail network.
    let stock = pcb_ir::geom::shapes::rounded_rect(
        size.x,
        size.y,
        super::ARRAY_CORNER_RADIUS_MM,
        pcb_ir::geom::shapes::ALL_CORNERS,
    )
    .context("invalid panel dimensions")?;
    let stock = ContourSet::from_contours(&[stock], FillRule::EvenOdd, resolution)?;
    let mut frame = transform_region(&stock, Affine2::translation(size * 0.5))?;
    let mut boards = Vec::new();
    let mut offsets = Vec::new();
    let mut obstacles = Vec::new();
    for row in 0..options.rows {
        for column in 0..options.columns {
            let offset = Point::new(
                r.left + m.left + f64::from(column) * pitch.x - bbox.min.x,
                r.bottom + m.bottom + f64::from(row) * pitch.y - bbox.min.y,
            );
            let transform = Affine2::translation(offset);
            let board = transform_region(&prepared.substrate, transform)?;
            let b = board.bbox();
            let gap = Point::new(config.routing_gap_mm, config.routing_gap_mm);
            frame =
                frame.difference(&rectangle(BBox::new(b.min - gap, b.max + gap), resolution))?;
            for e in &prepared.evidence {
                if let Some(region) = &e.region {
                    obstacles.push((
                        format!("board-{}:{}", boards.len(), e.id),
                        transform_region(region, transform)?,
                    ));
                }
            }
            boards.push(board);
            offsets.push(offset);
        }
    }
    ensure!(
        frame.connected_components().len() == 1,
        "retained rails are not one connected frame"
    );
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
    };
    let boundary = BoundaryQuery::new(&prepared.substrate, tolerance)?;
    let empty = ContourSet::empty(resolution);
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for (board_index, offset) in offsets.iter().enumerate() {
        for (interval, station) in sample_intervals(&prepared.intervals, config.candidate_pitch_mm)?
        {
            let local = boundary.site(interval.boundary, station)?;
            let p = local.point + *offset;
            let n = local.outward_normal;
            let t = local.tangent;
            let attempt = (|| -> Result<std::result::Result<Candidate, String>> {
                let guard =
                    interval.uncertainty_mm.max(frame.uncertainty_mm) + tolerance.numerical_mm;
                let corridor = strip(
                    p,
                    t,
                    n,
                    footprint.width_mm,
                    0.0,
                    config.max_span_mm + config.frame_landing_mm + 2.0 * guard,
                    resolution,
                )?;
                let start = landing_start(&frame, &corridor, p, n, config.frame_landing_mm, guard)?;
                let span = start - guard;
                if span > config.max_span_mm {
                    return Ok(Err("no full-width frame landing within max_span_mm".into()));
                }
                let landing = strip(
                    p,
                    t,
                    n,
                    footprint.width_mm,
                    start,
                    start + config.frame_landing_mm,
                    resolution,
                )?;
                if !landing.difference(&frame)?.is_empty() {
                    return Ok(Err("full frame landing is not on retained rails".into()));
                }
                let envelope = strip(
                    p,
                    t,
                    n,
                    footprint.width_mm,
                    -footprint.inward_mm,
                    start + config.frame_landing_mm,
                    resolution,
                )?;
                let outward = strip(p, t, n, footprint.width_mm, guard, start, resolution)?;
                if !outward.intersection(&boards[board_index])?.is_empty() {
                    return Ok(Err("connection re-enters its board".into()));
                }
                let other_boards = boards
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != board_index)
                    .map(|(_, b)| Obstacle {
                        id: "other-board",
                        region: b,
                    });
                let refs = obstacles
                    .iter()
                    .map(|(id, region)| Obstacle { id, region })
                    .chain(other_boards)
                    .collect::<Vec<_>>();
                let checks = check_footprints(&envelope, &empty, &refs, 0.0, tolerance)?;
                if let Some(check) = checks.iter().find(|c| c.decision != Decision::Admissible) {
                    return Ok(Err(format!("{}: {:?}", check.obstacle, check.decision)));
                }
                let board_landing = strip(
                    p,
                    t,
                    n,
                    footprint.width_mm,
                    -footprint.inward_mm,
                    0.0,
                    resolution,
                )?
                .intersection(&boards[board_index])?;
                if board_landing.is_empty() {
                    return Ok(Err("no finite board landing".into()));
                }
                Ok(Ok(Candidate {
                    site: Site {
                        id: candidates.len(),
                        board: board_index,
                        board_landing,
                        frame_landing: landing,
                        reference: [p.x, p.y],
                    },
                    ring: interval.boundary.ring,
                    station,
                    perimeter: boundary.perimeter(interval.boundary)?,
                    frame_point: p + n * span,
                    span,
                    envelope,
                }))
            })()?;
            match attempt {
                    Ok(candidate) => {
                        ensure!(candidates.len() < config.max_candidates,
                            "accepted frame candidate budget exceeded; increase candidate_pitch_mm or max_candidates");
                        candidates.push(candidate);
                    },
                    Err(reason) => rejected.push(json!({"board":board_index,"ring":interval.boundary.ring,"station_mm":station,"reason":reason})),
                }
        }
    }
    let mut conflicts = Vec::new();
    for (i, a) in candidates.iter().enumerate() {
        for b in &candidates[i + 1..] {
            let distance = (a.station - b.station).abs();
            let same_band = a.site.board == b.site.board
                && a.ring == b.ring
                && distance.min(a.perimeter - distance) <= footprint.width_mm;
            let check = check_footprints(
                &a.envelope,
                &empty,
                &[Obstacle {
                    id: "candidate",
                    region: &b.envelope,
                }],
                0.0,
                tolerance,
            )?;
            if same_band || check.iter().any(|c| c.decision != Decision::Admissible) {
                conflicts.push((a.site.id, b.site.id));
            }
        }
    }
    let candidate_json = candidates.iter().map(|c| json!({"id":c.site.id,"board":c.site.board,"ring":c.ring,
        "station_mm":c.station,"board_point":c.site.reference,"frame_point":[c.frame_point.x,c.frame_point.y],
        "span_mm":c.span,"board_landing_area_mm2":c.site.board_landing.area(),"frame_landing_area_mm2":c.site.frame_landing.area()})).collect::<Vec<_>>();
    let unsupported = (0..boards.len())
        .filter(|i| !candidates.iter().any(|c| c.site.board == *i))
        .collect::<Vec<_>>();
    let mechanics = if !unsupported.is_empty() {
        json!({"status":"no-proven-frame-candidates","boards_without_candidates":unsupported,"selected_ids":null})
    } else {
        let sites = candidates.into_iter().map(|c| c.site).collect::<Vec<_>>();
        let cases = config
            .load_cases
            .iter()
            .map(|c| LoadCase {
                board_resultants: vec![c.resultant_per_board; boards.len()],
                compliance_limit: c.compliance_limit,
            })
            .collect::<Vec<_>>();
        let evaluation = planning::evaluate(
            &boards,
            &frame,
            &sites,
            &conflicts,
            &cases,
            &Policy {
                bending: config.bending,
                connection_stiffness: config.connection_stiffness,
                scales: config.scales,
                mesh_options: MeshOptions {
                    max_area_mm2: config.mesh_max_area_mm2,
                    min_angle_degrees: config.mesh_min_angle_degrees,
                    max_additional_vertices: config.mesh_max_additional_vertices,
                },
                max_dofs: config.max_dofs,
                max_subsets: config.max_subsets,
                tolerances: tolerances(),
                clamp_sides: config.clamp_sides,
            },
        );
        match evaluation {
            Err(error) => {
                json!({"status":"analysis-failed","error":error.to_string(),"selected_ids":null})
            }
            Ok(evaluation) => {
                let report = evaluation.report;
                json!({"status":format!("{:?}",report.proof),"dofs":evaluation.dofs,
            "visited_subsets":report.visited_subsets,"count_lower_bound":report.count_lower_bound,
            "selected_ids":report.selected.as_ref().map(|s| &s.ids),
            "tab_count":report.selected.as_ref().map(|s| s.ids.len()),
            "worst_normalized_compliance":report.selected.as_ref().map(|s| s.objective),
            "responses":report.selected.as_ref().map(|s| s.analyses.iter().zip(&evaluation.board_responses).map(|(a,boards)| json!({
                "status":format!("{:?}",a.status),"compliance_n_mm":a.compliance,
                "strain_energy_n_mm":a.strain_energy,"relative_residual":a.relative_residual,
                "board_fitted_w_theta_x_theta_y":boards,
            })).collect::<Vec<_>>()),
            "mesh":evaluation.mesh_quality.iter().zip(evaluation.mesh_refinement).map(|(q,r)| json!({
                "refinement":format!("{:?}",r),"area_mm2":q.area_mm2,"max_element_area_mm2":q.max_area_mm2,
                "min_angle_degrees":q.min_angle_degrees})).collect::<Vec<_>>(),
            "unresolved":report.unresolved.iter().map(|u| json!({"ids":u.ids,"case":u.load_case,"reason":format!("{:?}",u.failure)})).collect::<Vec<_>>()})
            }
        }
    };
    Ok(
        json!({"phase":"frame-only-placement-analysis","manufacturing_ready":false,
        "eligibility":prepared.report,"config":config,"units":"N, mm",
        "layout":{"columns":options.columns,"rows":options.rows,"size_mm":[size.x,size.y],
            "board_offsets":offsets.iter().map(|p|[p.x,p.y]).collect::<Vec<_>>(),"frame_area_mm2":frame.area()},
        "candidates":candidate_json,"rejected":rejected,"conflicts":conflicts,"mechanics":mechanics,
        "limitations":["Frame-only: no board-to-board fallback. Rail domain excludes board bounding boxes plus routing_gap_mm.",
            "Sites discretize open Eligible intervals; Unknown evidence is never accepted. Search optimum applies only to these candidates and supplied mechanics.",
            "Finite tangent landing regions are analysis footprints, not generated tabs; curved board landings are clipped to substrate.",
            "Supplied connection stiffness is at each board_point in global axes; no material or fracture strength is inferred.",
            "No tab/perforation/route geometry, router-access certification, generated rail tooling clearance, manufacturing export or physical qualification."]}),
    )
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &std::path::Path,
    output: &std::path::Path,
    config_path: &std::path::Path,
    footprint_policy: (OutlineFootprint, f64),
    layout: Option<BoardArrayCreateOptions>,
    sheet: Option<super::AutoSheetSize>,
    resolution: Resolution,
) -> Result<()> {
    let (footprint, clearance_mm) = footprint_policy;
    let xml = crate::utils::file::load_ipc_file(input)?;
    let config: Config = serde_json::from_slice(&std::fs::read(config_path)?)
        .context("invalid mouse-bite placement configuration")?;
    let options = match layout {
        Some(options) => options,
        None => {
            super::auto_board_array_options(&ipc2581::Ipc2581::parse(&xml)?, sheet, resolution)?.0
        }
    };
    let report = analyze(&xml, footprint, clearance_mm, &options, &config, resolution)?;
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    if output.as_os_str() == "-" {
        pcb_ui::write_stdout(|w| w.write_all(&bytes))?;
    } else {
        std::fs::write(output, bytes)?;
    }
    anstream::eprintln!(
        "Frame-only placement and mechanics analysis; no panel geometry generated."
    );
    Ok(())
}

#[cfg(test)]
mod tests;
