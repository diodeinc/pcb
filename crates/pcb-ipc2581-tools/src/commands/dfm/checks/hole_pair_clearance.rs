//! Minimum hole-to-hole clearance.
//!
//! Holes are closed disks `Dᵢ = D(cᵢ, rᵢ)`. For two disks the Euclidean set
//! distance has the closed form
//!
//! ```text
//! dist(Dᵢ, Dⱼ) = max(0, ‖cᵢ − cⱼ‖ − rᵢ − rⱼ),
//! ```
//!
//! attained along the center line, which also yields the two boundary
//! witness points ([`pcb_ir::geom::dfm::disk_clearance`]). The check
//! requires `dist(Dᵢ, Dⱼ) ≥ L` for every unordered pair whose drill spans
//! overlap in the copper stackup — holes that share no board depth cannot
//! interact, so stacked blind and buried vias on disjoint spans are exempt.
//!
//! Enumeration is a plane sweep: with holes sorted by their bounds' minimum
//! x, the inner scan stops at the first hole separated from the current one
//! by at least `L` along x, and an axis-aligned y-interval gap test prunes
//! the rest, so only genuinely close pairs are measured. A pruned pair is
//! thereby *proven* clear, not left unexamined, so `checked` counts the
//! holes entering the sweep: every hole is decided against every other.

use pcb_ir::geom::dfm::disk_clearance;

use crate::commands::dfm::report::{Evidence, Finding};
use crate::commands::dfm::rules::Rule;

use super::{
    COMPARISON_EPSILON_MM, ClearanceViolation, Context, hole_subject, unique_layers,
    violates_minimum,
};

pub(super) fn evaluate(rule: &Rule, ctx: &Context) -> (usize, Vec<Finding>) {
    let holes = &ctx.design.holes;
    let limit = rule.limit.millimeters();
    let mut findings = Vec::new();
    for first_index in 0..holes.len() {
        let first = &holes[first_index];
        for second in &holes[first_index + 1..] {
            if second.bbox.min.x - first.bbox.max.x >= limit - COMPARISON_EPSILON_MM {
                break;
            }
            let y_gap = (second.bbox.min.y - first.bbox.max.y)
                .max(first.bbox.min.y - second.bbox.max.y)
                .max(0.0);
            if y_gap >= limit - COMPARISON_EPSILON_MM {
                continue;
            }
            if !first.span_overlaps(second) {
                continue;
            }
            let clearance = disk_clearance(
                first.center,
                first.diameter_mm / 2.0,
                second.center,
                second.diameter_mm / 2.0,
            );
            if !violates_minimum(clearance.distance_mm, limit) {
                continue;
            }
            findings.push(
                ClearanceViolation {
                    rule,
                    title: "Hole-to-hole clearance is below minimum",
                    message: format!(
                        "hole edges are {:.6} mm apart; the PDK requires at least {:.6} mm",
                        clearance.distance_mm, limit
                    ),
                    witness_roles: ["first_hole_boundary", "second_hole_boundary"],
                    clearance,
                    layers: unique_layers(&first.layer, &second.layer),
                    subjects: vec![
                        hole_subject(ctx, first, "first"),
                        hole_subject(ctx, second, "second"),
                    ],
                    evidence: vec![
                        Evidence::circle("first_hole", first.center, first.diameter_mm),
                        Evidence::circle("second_hole", second.center, second.diameter_mm),
                    ],
                }
                .into_finding(),
            );
        }
    }
    (holes.len(), findings)
}
