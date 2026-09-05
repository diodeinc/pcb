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

use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::geom::dfm::{circular_region, disk_clearance};

use crate::commands::dfm::design::{Design, HoleClass};
use crate::commands::dfm::report::{DisplayCircle, Evidence, EvidenceDisplay, MeasurementKind};

use super::{COMPARISON_EPSILON_MM, Evaluation, Measured, MeasuredSite, hole_subject, layers};

pub(super) fn evaluate(
    limit_mm: f64,
    first_class: HoleClass,
    second_class: HoleClass,
    design: &Design,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Evaluation> {
    let holes = design
        .holes
        .iter()
        .filter(|hole| hole.class == first_class || hole.class == second_class)
        .collect::<Vec<_>>();
    let reach = limit_mm - COMPARISON_EPSILON_MM;
    let measured = holes
        .iter()
        .enumerate()
        .map(|(index, first)| {
            Ok::<_, anyhow::Error>(
                holes[index + 1..]
                    .iter()
                    .take_while(move |second| second.bbox.min.x - first.bbox.max.x < reach)
                    .filter(move |second| {
                        let classes_match = (first.class == first_class
                            && second.class == second_class)
                            || (first.class == second_class && second.class == first_class);
                        let y_gap = (second.bbox.min.y - first.bbox.max.y)
                            .max(first.bbox.min.y - second.bbox.max.y)
                            .max(0.0);
                        classes_match
                            && y_gap < reach
                            && first.drill_span.overlaps(&second.drill_span)
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
                                circular_region(first.center, first.diameter_mm / 2.0, accuracy)?
                                    .intersection(&circular_region(
                                        second.center,
                                        second.diameter_mm / 2.0,
                                        accuracy,
                                    )?);
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
                            site.note = Some(
                                "Drilled regions overlap; the edge clearance is zero.".to_owned(),
                            );
                        } else if distance.mm == 0.0 {
                            site.note = Some(
                                "Drilled regions touch; the edge clearance is zero.".to_owned(),
                            );
                        }
                        Ok::<_, anyhow::Error>(Measured {
                            distance,
                            bbox: first.bbox.union(second.bbox),
                            layers: layers([&first.layer, &second.layer]),
                            subjects: vec![
                                hole_subject(design, first, "first"),
                                hole_subject(design, second, "second"),
                            ],
                            evidence,
                            sites: vec![site],
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
                    .into_iter(),
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(Evaluation {
        checked: holes.len(),
        measured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dfm::{pdk::Pdk, rules};
    use crate::ipc2581::Ipc2581;
    use pcb_ir::dialects::ipc::ArtworkScope;

    #[test]
    fn physical_overlap_is_independent_of_copper_declaration_order() {
        for order in [[0, 1, 2, 3], [0, 2, 1, 3], [0, 3, 1, 2]] {
            // Disjoint blind spans must not interact; nested spans must.
            for (first, second, findings) in [((0, 1), (2, 3), 0), ((0, 3), (1, 2), 1)] {
                let layers = order.map(|i| format!(r#"<Layer name="L{i}" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>"#)).join("");
                let stackup = [0, 1, 2, 3].map(|i| format!(r#"<StackupLayer layerOrGroupRef="L{i}" thickness="0.035" tolPlus="0" tolMinus="0" sequence="{i}"/>"#)).join("");
                let ipc = Ipc2581::parse(&format!(r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
                  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
                    <LayerRef name="L0"/><LayerRef name="L1"/><LayerRef name="L2"/><LayerRef name="L3"/><LayerRef name="D1"/><LayerRef name="D2"/>
                  </Content><Ecad><CadHeader units="MILLIMETER"/><CadData>{layers}
                    <Layer name="D1" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="L{}" toLayer="L{}"/></Layer>
                    <Layer name="D2" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="L{}" toLayer="L{}"/></Layer>
                    <Stackup name="Primary" overallThickness="0.14" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
                      <StackupGroup name="Group" thickness="0.14" tolPlus="0" tolMinus="0">{stackup}</StackupGroup>
                    </Stackup>
                    <Step name="board" type="BOARD">
                      <LayerFeature layerRef="D1"><Set><Hole name="H1" diameter="1" platingStatus="PLATED" x="0" y="0"/></Set></LayerFeature>
                      <LayerFeature layerRef="D2"><Set><Hole name="H2" diameter="1" platingStatus="PLATED" x="0.1" y="0"/></Set></LayerFeature>
                    </Step>
                  </CadData></Ecad></IPC-2581>"#, first.0, first.1, second.0, second.1)).unwrap();
                let pdk = Pdk::parse(
                    r#"schema_version = 2
                    default_profile = "test"
                    [pdk]
                    id = "test"
                    name = "Test"
                    revision = "1"
                    [profiles.test]
                    name = "Test"
                    [[rules.drilling.hole_to_hole_clearance]]
                    id = "hole-clearance"
                    select = { first_hole = "pth", second_hole = "pth" }
                    limit = { minimum = "0.2 mm" }
                "#,
                )
                .unwrap();
                let rules = rules::lower(&pdk, None).unwrap();
                let imported =
                    pcb_ir::import::ipc2581::import_design(&ipc, GeometryAccuracy::default())
                        .unwrap();
                let design = Design::extract(
                    &imported,
                    ArtworkScope::Board,
                    &rules,
                    GeometryAccuracy::default(),
                )
                .unwrap();
                let evaluation = evaluate(
                    0.2,
                    HoleClass::Pth,
                    HoleClass::Pth,
                    &design,
                    GeometryAccuracy::default(),
                )
                .unwrap();
                assert_eq!(evaluation.checked, 2);
                assert_eq!(
                    evaluation.measured.len(),
                    findings,
                    "declarations {order:?}, spans {first:?}, {second:?}"
                );
            }
        }
    }

    #[test]
    fn overlapping_drills_retain_exact_circle_intersection_parameters() {
        let accuracy = GeometryAccuracy::default();

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
            r#"schema_version = 2
          default_profile = "test"
          [pdk]
          id = "test"
          name = "Test"
          revision = "1"
          [profiles.test]
          name = "Test"
          [[rules.drilling.hole_to_hole_clearance]]
          id = "hole-clearance"
          select = { first_hole = "pth", second_hole = "pth" }
          limit = { minimum = "0.2 mm" }
        "#,
        )
        .unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, accuracy).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules, accuracy).unwrap();
        let evaluation = evaluate(0.2, HoleClass::Pth, HoleClass::Pth, &design, accuracy).unwrap();
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
