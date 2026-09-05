//! Nominal enclosure of a materialized plated-slot region S by final copper M:
//! d(∂S, ∂(M ∪ S)). Filling the cavity for this query requires copper only
//! outside the routed opening, even when final artwork already cuts S out.
//! No circular proxy, bounding-box measurement, or fabrication tolerance model
//! is involved. Terminal layers and source-backed lands are required; an
//! intermediate layer with neither a land nor copper meeting S is exempt.

use pcb_ir::geom::region::ring_edges;
use pcb_ir::geom::{GeometryAccuracy, tol};

use crate::commands::dfm::design::Design;
use crate::commands::dfm::pdk::SlotPlating;
use crate::commands::dfm::report::{Evidence, MeasurementKind};
use crate::commands::dfm::rules::Conditions;

use super::drilled_board_edge_clearance::slot_evidence;
use super::{Evaluation, Measured, MeasuredSite, layers, slot_matches, slot_subject, violates};

pub(super) fn evaluate(
    limit_mm: f64,
    conditions: &Conditions,
    design: &Design,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Evaluation> {
    let mut checked = 0;
    let mut measured = Vec::new();
    for (slot_index, slot) in design
        .slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot_matches(slot.plating, SlotPlating::Plated))
    {
        let mut sites = Vec::new();
        for (index, copper) in design
            .copper_layers
            .iter()
            .enumerate()
            .filter(|(index, copper)| {
                slot.drill_span.contains_copper(*index) && conditions.applies_to_layer(copper)
            })
        {
            let required = slot.drill_span.terminates_on(index)
                || design.slot_lands[slot_index]
                    .iter()
                    .any(|land| land.copper_index as usize == index);
            let touching = slot
                .outline
                .rings
                .iter()
                .flat_map(ring_edges)
                .any(|(start, end)| {
                    design.copper_boundaries[index]
                        .segment_nearest_within(start, end, tol::REGION_MM)
                        .is_some_and(|distance| distance.mm <= tol::REGION_MM)
                });
            if !required && !touching && slot.outline.intersection(&copper.image).is_empty() {
                continue;
            }
            checked += 1;
            // Boolean composition may quantize the cutout and the extracted
            // slot on slightly different grids. Heal their shared boundary
            // within the existing flattening uncertainty, not a fab tolerance.
            let filled = copper
                .image
                .union(&slot.outline.disk_dilate(tol::REGION_MM, accuracy)?);
            let boundary = filled.prepare_query();
            let distance = slot
                .outline
                .rings
                .iter()
                .flat_map(ring_edges)
                .filter_map(|(start, end)| boundary.segment_nearest_within(start, end, limit_mm))
                .min_by(|a, b| a.mm.total_cmp(&b.mm))
                .map(|distance| {
                    let mut distance = distance.also_uncertain(slot.outline.uncertainty_mm);
                    if distance.mm <= tol::REGION_MM {
                        distance.mm = 0.0;
                        distance.second = distance.first;
                    }
                    distance
                });
            let Some(distance) = distance.filter(|distance| violates(distance, limit_mm)) else {
                continue;
            };
            let envelope = slot.outline.disk_dilate(limit_mm, accuracy)?;
            let mut site = MeasuredSite::new(
                distance,
                envelope.bbox,
                layers([&slot.layer, &copper.layer]),
                vec![
                    slot_evidence(slot),
                    Evidence::region(
                        "required_copper_envelope",
                        &envelope.difference(&slot.outline),
                    ),
                    Evidence::region("missing_copper", &envelope.difference(&filled)),
                ],
                if distance.mm <= f64::EPSILON {
                    MeasurementKind::MissingCopper
                } else {
                    MeasurementKind::Clearance
                },
            );
            site.subjects = vec![slot_subject(design, slot, "slot")];
            if distance.mm <= f64::EPSILON {
                site.note = Some("Copper does not enclose the complete slot boundary; nominal enclosure is zero.".to_owned());
            }
            sites.push(site);
        }
        if let Some(worst) = sites
            .iter()
            .min_by(|a, b| a.distance.mm.total_cmp(&b.distance.mm))
        {
            measured.push(Measured {
                distance: worst.distance,
                bbox: worst.bbox,
                layers: worst.layers.clone(),
                subjects: worst.subjects.clone(),
                evidence: worst.evidence.clone(),
                sites,
            });
        }
    }
    Ok(Evaluation { checked, measured })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayoutTarget;
    use crate::commands::dfm::report::{FileIdentity, RuleStatus, Verdict};
    use crate::commands::dfm::{self, CheckRequest, PdkSource, TextSource};
    use crate::ipc2581::Ipc2581;
    use pcb_ir::import::ipc2581::import_design;

    const PDK: &str = r#"schema_version = 2
default_profile = "test"
[pdk]
id = "test"
name = "Test"
revision = "1"
[profiles.test]
name = "Test"
[[rules.copper.plated_slot_enclosure]]
id = "slot-enclosure"
limit = { minimum = "0.2 mm", preferred = "0.3 mm" }
"#;

    fn copper(right: f64) -> String {
        format!(
            r#"<Set><Features><Contour><Polygon>
          <PolyBegin x="-2" y="-1"/><PolyStepSegment x="{right}" y="-1"/>
          <PolyStepSegment x="{right}" y="1"/><PolyStepSegment x="-2" y="1"/>
          <PolyStepSegment x="-2" y="-1"/>
        </Polygon></Contour></Features></Set>"#
        )
    }

    fn board(shape: &str, copper: [&str; 3], span: &str) -> String {
        let artwork = copper
            .iter()
            .enumerate()
            .map(|(i, copper)| format!(r#"<LayerFeature layerRef="L{i}">{copper}</LayerFeature>"#))
            .collect::<String>();
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
            <LayerRef name="L0"/><LayerRef name="L1"/><LayerRef name="L2"/><LayerRef name="ROUT"/>
          </Content><Ecad><CadHeader units="MILLIMETER"/><CadData>
            <!-- Deliberately not physical stackup order. -->
            <Layer name="L2" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
            <Layer name="L0" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
            <Layer name="L1" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
            <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE">{span}</Layer>
            <Stackup name="Primary" overallThickness="1.6" whereMeasured="METAL">
              <StackupGroup name="G" thickness="1.6">
                <StackupLayer layerOrGroupRef="L0" thickness="0.05" sequence="0"/>
                <StackupLayer layerOrGroupRef="L1" thickness="0.05" sequence="1"/>
                <StackupLayer layerOrGroupRef="L2" thickness="0.05" sequence="2"/>
              </StackupGroup>
            </Stackup>
            <Step name="board" type="BOARD">{artwork}
              <LayerFeature layerRef="ROUT"><Set>
                <SlotCavity name="S" platingStatus="PLATED">{shape}</SlotCavity>
              </Set></LayerFeature>
            </Step>
          </CadData></Ecad></IPC-2581>"#
        )
    }

    fn check(xml: &str) -> dfm::DfmReport {
        let imported =
            import_design(&Ipc2581::parse(xml).unwrap(), GeometryAccuracy::default()).unwrap();
        dfm::check(
            &imported,
            CheckRequest {
                input: FileIdentity::new("slot.xml", xml.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "slot.toml",
                    source: PDK,
                }),
                waivers: None,
                layout_target: LayoutTarget::Board,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
            GeometryAccuracy::default(),
        )
        .unwrap()
    }

    const OVAL: &str = r#"<Location x="0" y="0"/><Oval width="2" height="0.6"/>"#;
    const THROUGH: &str = r#"<Span fromLayer="L0" toLayer="L2"/>"#;

    #[test]
    fn full_boundary_enclosure_and_report_tiers_include_asymmetric_copper() {
        for (right, failures) in [(1.4, 0), (1.25, 1), (1.1, 2)] {
            let copper = copper(right);
            let report = check(&board(OVAL, [&copper, "", &copper], THROUGH));
            assert_eq!(
                report.rules[0].checked, 2,
                "intermediate anti-pad is exempt"
            );
            assert_eq!(report.findings.len(), failures);
            assert_eq!(
                matches!(report.verdict, Verdict::Fail),
                failures == 2,
                "preferred-only failure must not fail the board"
            );
            assert_eq!(
                matches!(report.rules[1].status, RuleStatus::Warning),
                failures > 0
            );
            for finding in &report.findings {
                assert!((finding.measurement.actual_mm().unwrap() - (right - 1.0)).abs() < 1e-8);
                assert_eq!(finding.sites.len(), 2);
                assert!(
                    finding
                        .sites
                        .iter()
                        .all(|site| matches!(site.measurement_kind, MeasurementKind::Clearance))
                );
            }
        }
    }

    #[test]
    fn missing_or_breached_terminal_copper_is_zero_and_span_is_respected() {
        let copper = copper(1.4);
        let breached = copper.replace("1.4", "0.9");
        for terminal in ["", breached.as_str()] {
            let report = check(&board(
                OVAL,
                ["", terminal, &copper],
                r#"<Span fromLayer="L1" toLayer="L2"/>"#,
            ));
            assert_eq!(report.rules[0].checked, 2);
            let finding = &report.findings[0];
            assert_eq!(finding.measurement.actual_mm(), Some(0.0));
            assert_eq!(finding.sites.len(), 1);
            assert_eq!(finding.sites[0].layers[1].name, "L1");
            assert!(matches!(
                finding.sites[0].measurement_kind,
                MeasurementKind::MissingCopper
            ));
            assert!(
                finding.sites[0]
                    .evidence
                    .iter()
                    .any(|e| e.role == "missing_copper" && !e.paths.is_empty())
            );
        }
    }

    #[test]
    fn cleared_intermediate_source_land_remains_required() {
        let copper = copper(1.4);
        let land = r#"<Set><Pad padstackDefRef="P"><Location x="0" y="0"/>
          <StandardPrimitiveRef id="land"/></Pad></Set>"#;
        let inner = format!(
            "{land}{}",
            copper.replace("<Set>", "<Set polarity=\"NEGATIVE\">")
        );
        let xml = board(OVAL, [&copper, &inner, &copper], THROUGH)
            .replace(
                "</Content>",
                r#"<DictionaryStandard units="MILLIMETER">
              <EntryStandard id="land"><Oval width="2.6" height="1"/></EntryStandard>
            </DictionaryStandard></Content>"#,
            )
            .replace(
                "<Step name=\"board\" type=\"BOARD\">",
                r#"<Step name="board" type="BOARD">
              <PadStackDef name="P"><PadstackPadDef layerRef="L1" padUse="REGULAR">
                <Location x="0" y="0"/><StandardPrimitiveRef id="land"/>
              </PadstackPadDef></PadStackDef>"#,
            );
        let report = check(&xml);
        assert_eq!(report.rules[0].checked, 3);
        assert_eq!(report.findings[0].sites.len(), 1);
        assert_eq!(report.findings[0].sites[0].layers[1].name, "L1");
        assert_eq!(report.findings[0].measurement.actual_mm(), Some(0.0));
    }

    #[test]
    fn outline_slot_uses_its_sloped_boundary_not_a_bounding_box() {
        // A diagonal strip inside a diagonal copper strip. Axis-aligned bounds
        // overlap, but the real perpendicular enclosure is 0.3 / sqrt(2).
        let shape = r#"<Outline><Polygon><PolyBegin x="-1" y="-1"/>
          <PolyStepSegment x="1" y="1"/><PolyStepSegment x="1" y="1.4"/>
          <PolyStepSegment x="-1" y="-0.6"/><PolyStepSegment x="-1" y="-1"/>
        </Polygon></Outline>"#;
        let copper = r#"<Set><Features><Contour><Polygon><PolyBegin x="-2" y="-2.3"/>
          <PolyStepSegment x="2" y="1.7"/><PolyStepSegment x="2" y="2.7"/>
          <PolyStepSegment x="-2" y="-1.3"/><PolyStepSegment x="-2" y="-2.3"/>
        </Polygon></Contour></Features></Set>"#;
        let report = check(&board(shape, [copper, "", copper], THROUGH));
        assert!(matches!(report.rules[0].status, RuleStatus::Pass));
        assert_eq!(report.findings.len(), 1, "preferred 0.3 mm fails");
        assert!(
            (report.findings[0].measurement.actual_mm().unwrap() - 0.3 / 2_f64.sqrt()).abs() < 1e-8
        );
    }

    #[test]
    fn missing_stackup_or_span_cannot_be_certified_even_with_adequate_copper() {
        use crate::commands::dfm::{pdk::Pdk, rules};
        use pcb_ir::dialects::ipc::ArtworkScope;

        let copper = copper(1.4);
        let rules = rules::lower(&Pdk::parse(PDK).unwrap(), None).unwrap();
        for span in ["", r#"<Span fromLayer="L0"/>"#, THROUGH] {
            let xml = board(OVAL, [&copper, "", &copper], span);
            let mut imported =
                import_design(&Ipc2581::parse(&xml).unwrap(), GeometryAccuracy::default()).unwrap();
            if span == THROUGH {
                imported.stackups.clear();
            }
            let error = Design::extract(
                &imported,
                ArtworkScope::Board,
                &rules,
                GeometryAccuracy::default(),
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains(if span == THROUGH {
                "physical stackup"
            } else {
                "no resolvable drill span"
            }));
        }
    }
}
