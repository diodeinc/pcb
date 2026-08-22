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

use crate::commands::dfm::design::Design;
use crate::commands::dfm::report::Evidence;

use super::{COMPARISON_EPSILON_MM, Evaluation, Measured, hole_subject, layers};

pub(super) fn evaluate(limit_mm: f64, design: &Design) -> Evaluation {
    let holes = &design.holes;
    let reach = limit_mm - COMPARISON_EPSILON_MM;
    let measured = holes
        .iter()
        .enumerate()
        .flat_map(|(index, first)| {
            holes[index + 1..]
                .iter()
                .take_while(move |second| second.bbox.min.x - first.bbox.max.x < reach)
                .filter(move |second| {
                    let y_gap = (second.bbox.min.y - first.bbox.max.y)
                        .max(first.bbox.min.y - second.bbox.max.y)
                        .max(0.0);
                    y_gap < reach && first.span_overlaps(second)
                })
                .map(move |second| Measured {
                    distance: disk_clearance(
                        first.center,
                        first.diameter_mm / 2.0,
                        second.center,
                        second.diameter_mm / 2.0,
                    ),
                    bbox: first.bbox.union(second.bbox),
                    layers: layers([&first.layer, &second.layer]),
                    subjects: vec![
                        hole_subject(design, first, "first"),
                        hole_subject(design, second, "second"),
                    ],
                    evidence: vec![
                        Evidence::circle("first_hole", first.center, first.diameter_mm),
                        Evidence::circle("second_hole", second.center, second.diameter_mm),
                    ],
                })
        })
        .collect();
    Evaluation {
        checked: holes.len(),
        measured,
    }
}
