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

use pcb_ir::geom::dfm::{circular_region, disk_clearance};

use crate::commands::dfm::design::Design;
use crate::commands::dfm::report::{DisplayCircle, Evidence, EvidenceDisplay, MeasurementKind};

use super::{COMPARISON_EPSILON_MM, Evaluation, Measured, MeasuredSite, hole_subject, layers};

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
                .map(move |second| {
                    let distance = disk_clearance(
                        first.center,
                        first.diameter_mm / 2.0,
                        second.center,
                        second.diameter_mm / 2.0,
                    );
                    let evidence = vec![
                        Evidence::circle("first_hole", first.center, first.diameter_mm),
                        Evidence::circle("second_hole", second.center, second.diameter_mm),
                    ];
                    let mut site_evidence = evidence.clone();
                    site_evidence.push(Evidence::circle(
                        "required_hole_separation",
                        first.center,
                        first.diameter_mm + 2.0 * limit_mm,
                    ));
                    let overlap = first.center.distance_to(second.center)
                        < (first.diameter_mm + second.diameter_mm) / 2.0;
                    if overlap {
                        let overlap_region =
                            circular_region(first.center, first.diameter_mm / 2.0).intersection(
                                &circular_region(second.center, second.diameter_mm / 2.0),
                            );
                        site_evidence.push(Evidence {
                            display: Some(EvidenceDisplay::CircleIntersection {
                                first: DisplayCircle {
                                    center: first.center.into(),
                                    diameter: first.diameter_mm,
                                },
                                second: DisplayCircle {
                                    center: second.center.into(),
                                    diameter: second.diameter_mm,
                                },
                            }),
                            ..Evidence::region("overlap_region", &overlap_region)
                        });
                    }
                    let mut site = MeasuredSite::new(
                        distance,
                        first.bbox.expand(limit_mm).union(second.bbox),
                        layers([&first.layer, &second.layer]),
                        site_evidence,
                        if distance.mm == 0.0 {
                            MeasurementKind::Overlap
                        } else {
                            MeasurementKind::Clearance
                        },
                    );
                    if overlap {
                        site.note =
                            Some("Drilled regions overlap; the edge clearance is zero.".to_owned());
                    } else if distance.mm == 0.0 {
                        site.note =
                            Some("Drilled regions touch; the edge clearance is zero.".to_owned());
                    }
                    Measured {
                        distance,
                        bbox: first.bbox.union(second.bbox),
                        layers: layers([&first.layer, &second.layer]),
                        subjects: vec![
                            hole_subject(design, first, "first"),
                            hole_subject(design, second, "second"),
                        ],
                        evidence,
                        sites: vec![site],
                    }
                })
        })
        .collect();
    Evaluation {
        checked: holes.len(),
        measured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dfm::{pdk::Pdk, rules};
    use crate::ipc2581::Ipc2581;
    use pcb_ir::dialects::ipc::ArtworkScope;

    #[test]
    fn overlapping_drills_retain_exact_circle_intersection_parameters() {
        let ipc = Ipc2581::parse(r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="DRILL"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
            <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
            <Step name="board" type="BOARD"><LayerFeature layerRef="DRILL"><Set>
              <Hole name="H1" diameter="1.2" platingStatus="PLATED" x="10" y="-20"/>
              <Hole name="H2" diameter="0.8" platingStatus="PLATED" x="10.5" y="-20"/>
            </Set></LayerFeature></Step>
          </CadData></Ecad>
        </IPC-2581>"#).unwrap();
        let pdk = Pdk::parse(
            r#"schema_version = 1
          [pdk]
          id = "test"
          name = "Test"
          revision = "1"
          [capabilities.drilling]
          minimum_hole_to_hole_clearance = "0.2 mm"
        "#,
        )
        .unwrap();
        let rules = rules::lower(&pdk).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules).unwrap();
        let evaluation = evaluate(0.2, &design);
        assert_eq!(evaluation.measured.len(), 1);
        let site = &evaluation.measured[0].sites[0];
        let overlap = site
            .evidence
            .iter()
            .find(|evidence| evidence.role == "overlap_region")
            .unwrap();
        assert!(
            !overlap.paths.is_empty(),
            "the measured polygon remains in the report"
        );
        assert_eq!(site.distance.mm, 0.0);
        assert_eq!(
            site.distance.uncertainty_mm, 0.0,
            "disk separation is exact"
        );
        assert_eq!(
            serde_json::to_value(&overlap.display).unwrap(),
            serde_json::json!({
                "kind": "circle_intersection",
                "first": {"center": {"x": 10.0, "y": -20.0}, "diameter": 1.2},
                "second": {"center": {"x": 10.5, "y": -20.0}, "diameter": 0.8},
            })
        );
    }
}
