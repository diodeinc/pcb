//! Minimum hole diameter.
//!
//! Model each drilled hole `h` as the closed disk `D(cₕ, rₕ)`. The measured
//! quantity is the finished diameter `dₕ = 2·rₕ` as the source `Hole`
//! element declares it — exact, no geometry is derived — witnessed by the
//! two boundary points on the hole's horizontal diameter. The check is the
//! pointwise predicate
//!
//! ```text
//! dₕ ≥ D_min    for every hole h of the rule's plating class.
//! ```

use pcb_ir::geom::Point;
use pcb_ir::geom::dfm::Distance;

use crate::commands::dfm::design::{Design, HoleClass};
use crate::commands::dfm::report::{Evidence, MeasurementKind};

use super::{Evaluation, Measured, MeasuredSite, hole_subject, holes_of_class};

pub(super) fn evaluate(limit_mm: f64, class: HoleClass, design: &Design) -> Evaluation {
    let holes = holes_of_class(design, class);
    let measured = holes
        .iter()
        .map(|(_, hole)| {
            let radius = Point::new(hole.diameter_mm / 2.0, 0.0);
            let distance =
                Distance::exact(hole.diameter_mm, hole.center - radius, hole.center + radius);
            let evidence = vec![Evidence::circle(
                "drilled_hole",
                hole.center,
                hole.diameter_mm,
            )];
            let mut site_evidence = evidence.clone();
            site_evidence.push(Evidence::circle(
                "required_hole_diameter",
                hole.center,
                limit_mm,
            ));
            Measured {
                distance,
                bbox: hole.bbox,
                layers: vec![hole.layer.clone()],
                subjects: vec![hole_subject(design, hole, "offender")],
                evidence,
                sites: vec![MeasuredSite::new(
                    distance,
                    pcb_ir::geom::BBox::from_point(hole.center)
                        .expand(hole.diameter_mm.max(limit_mm) / 2.0),
                    vec![hole.layer.clone()],
                    site_evidence,
                    MeasurementKind::Diameter,
                )],
            }
        })
        .collect();
    Evaluation {
        checked: holes.len(),
        measured,
    }
}
