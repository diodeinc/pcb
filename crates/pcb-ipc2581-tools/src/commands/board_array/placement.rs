//! Where mouse-bite tabs go on one canonical board. Analysis only: no tab
//! geometry, routing, frame, or panel export.
//!
//! [`candidates`] finds where a tab could sit on the eligible outline and
//! [`select`] chooses the fewest of them that hold the board under a process
//! load. The panel is assumed to route a slot of `routing_gap_mm` that
//! follows the outline, with frame material beyond it.

pub mod candidates;
pub mod select;

use anyhow::{Context, Result, ensure};
use ipc2581::Ipc2581;
use pcb_ir::geom::{
    ContourSet, Resolution, attachment::outline::OutlineFootprint, mouse_bite::SparkFunShallow,
};
use serde_json::{Value, json};
use sha2::Digest;

use super::eligibility;
use select::{Model, Physics};

/// Opinionated placement preset, in millimeters and newtons. Not user knobs.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Preset {
    /// Tab width along the outline: neck plus router shoulders.
    pub tab_width_mm: f64,
    /// How far the perforations reach into the board.
    pub inward_mm: f64,
    /// Growth applied to courtyards before they count as obstacles.
    pub courtyard_clearance_mm: f64,
    /// Routed slot between the board outline and the frame; the neck spans it.
    pub routing_gap_mm: f64,
    /// Frame material a tab must reach beyond the slot.
    pub frame_landing_mm: f64,
    /// Candidate spacing along eligible outline runs.
    pub candidate_pitch_mm: f64,
    /// Tightest curve a tab may sit on; the coupon-validated radius. The
    /// outline may turn no more within the tab, or within the tab plus the
    /// keep-out on either side, than an arc of this radius would, which keeps
    /// tabs off corners without excluding round boards.
    pub min_tab_radius_mm: f64,
    /// Distance kept from corners, or a quarter of the board's shorter side
    /// on boards too small for that.
    pub corner_keepout_mm: f64,
    /// Closest two tabs may sit.
    pub min_separation_mm: f64,
    /// Spacing of the outline points a load may act at.
    pub load_point_spacing_mm: f64,
    /// Assumed when the source states no stackup thickness.
    pub default_thickness_mm: f64,
    pub physics: Physics,
}

pub const PRESET: Preset = Preset {
    tab_width_mm: SparkFunShallow::NECK_WIDTH_MM + 2.0 * SparkFunShallow::CUTTER_RADIUS_MM,
    inward_mm: 0.2,
    courtyard_clearance_mm: 0.0,
    routing_gap_mm: 2.0,
    frame_landing_mm: 1.0,
    candidate_pitch_mm: 2.5,
    min_tab_radius_mm: 10.0,
    corner_keepout_mm: 5.0,
    min_separation_mm: 10.0,
    load_point_spacing_mm: 2.0,
    default_thickness_mm: 1.6,
    physics: Physics {
        // Typical rigid FR-4, in-plane, treated as isotropic.
        youngs_modulus_mpa: 22_000.0,
        shear_modulus_mpa: 4_500.0,
        poisson_ratio: 0.13,
        // Perforation halves the neck's section.
        neck_width_mm: SparkFunShallow::NECK_WIDTH_MM,
        neck_perforation_factor: 0.5,
        // Two default 5 mm board margins minus a slot on each side.
        rail_width_mm: 6.0,
        // A finger or placement nozzle anywhere on the board, and the sag that
        // still leaves the surface flat enough for placement.
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
}

/// One board's eligibility, candidate sites and chosen tabs.
pub(super) struct Placement {
    pub prepared: eligibility::Prepared,
    pub sites: candidates::Sites,
    pub loads: Vec<pcb_ir::geom::Point>,
    pub model: Model,
    pub selection: select::Selection,
}

/// Place tabs on the canonical board of `ipc`.
pub(super) fn place(ipc: &Ipc2581, preset: &Preset, resolution: Resolution) -> Result<Placement> {
    let prepared = eligibility::prepare(
        ipc,
        preset.footprint(),
        preset.courtyard_clearance_mm,
        &[],
        resolution,
    )?;
    let substrate = &prepared.substrate;
    let islands = substrate.connected_components().len();
    ensure!(
        islands == 1,
        "placement models one connected board; this substrate has {islands} separate islands"
    );
    let sites = candidates::find(substrate, &prepared.intervals, preset, resolution.strict())?;
    let loads = candidates::load_points(substrate, preset.load_point_spacing_mm);
    let bbox = substrate.bbox();
    // Cross rails sit one board span apart, so the rail a tab lands on is
    // held that far apart at worst.
    let model = Model::new(
        prepared.thickness_mm.unwrap_or(preset.default_thickness_mm),
        bbox.width().max(bbox.height()),
        preset.routing_gap_mm,
        preset.physics,
        preset.min_separation_mm,
    );
    let selection = select::select(
        &sites.candidates.iter().map(|c| c.site).collect::<Vec<_>>(),
        &loads,
        &model,
    );
    Ok(Placement {
        prepared,
        sites,
        loads,
        model,
        selection,
    })
}

/// Analyze one canonical board: eligibility plus tab placement, as JSON.
pub fn analyze(xml: &str, preset: &Preset, resolution: Resolution) -> Result<Value> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let placement = place(&ipc, preset, resolution)?;
    let Placement {
        prepared,
        sites,
        loads,
        model,
        selection,
    } = &placement;
    let substrate = &prepared.substrate;
    let bbox = substrate.bbox();
    let rings = |region: &ContourSet| {
        region
            .rings
            .iter()
            .map(|ring| ring.iter().map(|p| json!([p[0], p[1]])).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    };
    let mut report = prepared.report.clone();
    report["phase"] = json!("tab-placement-only");
    report["source_xml_sha256"] = json!(hex::encode(sha2::Sha256::digest(xml.as_bytes())));
    report["placement"] = json!({
        "preset": preset,
        "model": model,
        "thickness_source": if prepared.thickness_mm.is_some() { "stackup" } else { "default" },
        "board": {"bbox": [bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y]},
        "substrate": rings(substrate),
        "frame": rings(&sites.frame),
        "obstacles": prepared.evidence.iter().filter_map(|e| {
            e.region.as_ref().map(|r| json!({"id": e.id, "rings": rings(r)}))
        }).collect::<Vec<_>>(),
        "candidates": sites.candidates.iter().enumerate().map(|(i, c)| json!({
            "id": i, "ring": c.ring, "station_mm": c.station_mm,
            "point": [c.site.point.x, c.site.point.y],
            "normal": [c.site.outward_normal.x, c.site.outward_normal.y],
        })).collect::<Vec<_>>(),
        "rejected": sites.rejected.iter().map(|r| json!({
            "ring": r.ring, "station_mm": r.station_mm, "point": [r.point.x, r.point.y], "reason": r.reason,
        })).collect::<Vec<_>>(),
        "tight": sites.tight.iter().map(|run| run.iter().map(|p| json!([p.x, p.y])).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "selected": selection.chosen,
        "tab_count": selection.chosen.len(),
        "proven_minimal": selection.proven,
        "deflection_mm": selection.deflection_mm.is_finite().then_some(selection.deflection_mm),
        "worst_point": selection.worst_point.map(|j| [loads[j].x, loads[j].y]),
        "satisfied": selection.satisfied(),
        "violations": selection.violations.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    Ok(report)
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
