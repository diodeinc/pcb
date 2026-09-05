//! Filled materialized slot to attributed final copper, on its declared span.
//! Nonplated slots exempt nothing. Plated slots exempt only occurrence-scoped
//! net ownership or canonical physical lands; proximity never implies ownership.

use pcb_ir::geom::dfm::{region_clearance_sites_with_index, region_clearance_within};

use crate::commands::dfm::design::{ConductorId, Design};
use crate::commands::dfm::pdk::SlotPlating;
use crate::commands::dfm::report::Evidence;
use crate::commands::dfm::rules::Conditions;

use super::copper_clearance::conductor_subject;
use super::hole_clearance::land_owns_conductor;
use super::{Evaluation, Measured, layers, linework_clearance, slot_matches, slot_subject};

pub(super) fn evaluate(
    limit_mm: f64,
    plating: SlotPlating,
    conditions: &Conditions,
    design: &Design,
) -> Evaluation {
    let mut checked = 0;
    let mut measured = Vec::new();
    for (slot_index, slot) in design
        .slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot_matches(slot.plating, plating))
    {
        let boundary = slot.outline.prepare_query();
        let owner = slot.net.map(|net| ConductorId::Net {
            step: slot.step,
            instance: slot.provenance.instance_index,
            net,
        });
        for (copper_index, copper) in design.copper_layers.iter().enumerate() {
            // Extraction orders copper layers and drill spans by the same
            // validated physical stackup.
            if !slot.drill_span.contains_copper(copper_index)
                || !conditions.applies_to_layer(copper)
            {
                continue;
            }
            checked += 1;
            // A canonical link can mean unique overlap, not identity. Exempt
            // a land only with stated padstack identity and no conflicting net.
            let own_lands = design.slot_lands[slot_index]
                .iter()
                .filter(|link| link.copper_index as usize == copper_index)
                .map(|link| &copper.lands[link.land_index as usize])
                .filter(|land| slot.padstack == Some(land.padstack))
                .filter(|land| {
                    slot.net
                        .zip(land.net)
                        .is_none_or(|(slot_net, land_net)| slot_net == land_net)
                })
                .collect::<Vec<_>>();
            let nearest = copper
                .conductors
                .iter()
                .zip(&design.conductor_boundaries[copper_index])
                .filter(|(conductor, _)| {
                    plating == SlotPlating::Nonplated
                        || !(owner == Some(conductor.id)
                            || own_lands
                                .iter()
                                .any(|land| land_owns_conductor(land, conductor.id)))
                })
                .filter_map(|(conductor, copper_boundary)| {
                    region_clearance_within(
                        &slot.outline,
                        &boundary,
                        &conductor.image,
                        copper_boundary,
                        limit_mm,
                    )
                    .map(|distance| (conductor, copper_boundary, distance))
                })
                .min_by(|(_, _, left), (_, _, right)| left.mm.total_cmp(&right.mm));
            let Some((offender, copper_boundary, distance)) = nearest else {
                continue;
            };
            let finding_layers = layers([&slot.layer, &copper.layer]);
            let subjects = vec![
                slot_subject(design, slot, "slot"),
                conductor_subject(design, offender.id, "offender", &copper.layer.name),
            ];
            let evidence = vec![
                Evidence::region("routed_slot", &slot.outline),
                Evidence::bounds("offending_copper", offender.image.bbox),
            ];
            let sites = region_clearance_sites_with_index(
                &slot.outline,
                &offender.image,
                copper_boundary,
                limit_mm,
            )
            .into_iter()
            .map(|geometry| {
                let mut site =
                    linework_clearance::report_site(geometry, finding_layers.clone(), limit_mm);
                site.subjects = subjects.clone();
                site.evidence.extend(evidence.clone());
                site
            })
            .collect();
            measured.push(Measured {
                distance,
                bbox: slot
                    .bbox
                    .union(pcb_ir::geom::BBox::from_point(distance.second)),
                layers: finding_layers,
                subjects,
                evidence,
                sites,
            });
        }
    }
    Evaluation { checked, measured }
}

#[cfg(test)]
mod tests {
    use crate::LayoutTarget;
    use crate::commands::dfm::{self, CheckRequest, PdkSource, TextSource, report};
    use crate::ipc2581::Ipc2581;

    fn pdk(plating: &str) -> String {
        format!(
            r#"schema_version = 2
default_profile = "test"
[pdk]
id = "slot-test"
name = "Slot test"
revision = "1"
[profiles.test]
name = "Test"
[[rules.copper.slot_clearance]]
id = "slot-clearance"
select = {{ plating = "{plating}" }}
limit = {{ minimum = "0.20 mm" }}
"#
        )
    }

    // Declaration order deliberately differs from physical copper order.
    fn board(plating: &str, slot_set: &str, copper: &str) -> String {
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
<Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
<LayerRef name="L0"/><LayerRef name="L1"/><LayerRef name="L2"/><LayerRef name="ROUT"/>
<DictionaryStandard units="MILLIMETER"><EntryStandard id="land"><Oval width="2.4" height="1"/></EntryStandard></DictionaryStandard>
</Content><Ecad><CadHeader units="MILLIMETER"/><CadData>
<Layer name="L0" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
<Layer name="L2" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
<Layer name="L1" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
<Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"><Span fromLayer="L0" toLayer="L1"/></Layer>
<Stackup name="primary" overallThickness="1.6" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
<StackupGroup name="group" thickness="1.6" tolPlus="0" tolMinus="0">
<StackupLayer layerOrGroupRef="L0" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
<StackupLayer layerOrGroupRef="L1" thickness="0.035" tolPlus="0" tolMinus="0" sequence="1"/>
<StackupLayer layerOrGroupRef="L2" thickness="0.035" tolPlus="0" tolMinus="0" sequence="2"/>
</StackupGroup></Stackup>
<Step name="board" type="BOARD"><Datum x="0" y="0"/>
<PadStackDef name="land-stack"><PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef></PadStackDef>
{copper}
<LayerFeature layerRef="ROUT"><Set {slot_set}>
<SlotCavity name="S1" platingStatus="{plating}"><Location x="0" y="0"/><Oval width="2" height="0.6"/></SlotCavity>
</Set></LayerFeature></Step></CadData></Ecad></IPC-2581>"#
        )
    }

    fn copper(net: &str) -> String {
        format!(
            r#"<LayerFeature layerRef="L0"><Set {net}><Features><Contour><Polygon>
<PolyBegin x="1.1" y="-0.5"/><PolyStepSegment x="2" y="-0.5"/>
<PolyStepSegment x="2" y="0.5"/><PolyStepSegment x="1.1" y="0.5"/><PolyStepSegment x="1.1" y="-0.5"/>
</Polygon></Contour></Features></Set></LayerFeature>"#
        )
    }

    fn check(xml: &str, source: &str, target: LayoutTarget) -> anyhow::Result<report::DfmReport> {
        let imported = pcb_ir::import::ipc2581::import_design(&Ipc2581::parse(xml)?)?;
        dfm::check(
            &imported,
            CheckRequest {
                input: report::FileIdentity::new("slot.xml", xml.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "slot.toml",
                    source,
                }),
                waivers: None,
                layout_target: target,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
    }

    #[test]
    fn width_only_report_omits_unproven_span_with_shuffled_layers() {
        let source = pdk("plated")
            .replace("rules.copper.slot_clearance", "rules.drilling.slot_width")
            .replace("0.20 mm", "0.80 mm");
        let result = check(&board("PLATED", "", ""), &source, LayoutTarget::Board).unwrap();
        let finding = &result.findings[0];
        assert!((finding.measurement.actual_mm().unwrap() - 0.6).abs() < 1e-8);
        assert!(finding.subjects[0].drill_span.is_none());
    }

    #[test]
    fn standard_slot_copper_checks_warn_without_failing_but_require_a_span() {
        let standard = dfm::builtin_pdks()
            .iter()
            .find(|pdk| pdk.name == "standard")
            .unwrap();
        for plating in ["PLATED", "NONPLATED"] {
            let xml = board(plating, "net=\"N1\"", &copper("net=\"N2\""))
                .replace("height=\"0.6\"", "height=\"0.8\"")
                .replace(
                    "<Datum x=\"0\" y=\"0\"/>",
                    r#"<Datum x="0" y="0"/><Profile><Polygon>
<PolyBegin x="-10" y="-10"/><PolyStepSegment x="10" y="-10"/>
<PolyStepSegment x="10" y="10"/><PolyStepSegment x="-10" y="10"/>
<PolyStepSegment x="-10" y="-10"/></Polygon></Profile>"#,
                );
            let result = check(&xml, standard.source, LayoutTarget::Board).unwrap();
            assert!(matches!(result.verdict, report::Verdict::Pass));
            assert_eq!(result.summary.errors, 0);
            let finding = result
                .findings
                .iter()
                .find(|finding| finding.rule_id.contains("slot_clearance"))
                .unwrap();
            assert_eq!(finding.severity, report::Severity::Warning);
            assert!((finding.measurement.actual_mm().unwrap() - 0.1).abs() < 1e-8);
            assert_eq!(finding.subjects[0].kind, "routed_slot");

            let enclosure = result
                .findings
                .iter()
                .find(|finding| finding.rule_id.contains("plated_slot_enclosure"));
            if plating == "PLATED" {
                let enclosure = enclosure.unwrap();
                assert_eq!(enclosure.severity, report::Severity::Warning);
                assert_eq!(enclosure.measurement.actual_mm(), Some(0.0));
            } else {
                assert!(enclosure.is_none(), "nonplated slots need no copper land");
            }

            let missing = xml.replace("<Span fromLayer=\"L0\" toLayer=\"L1\"/>", "");
            assert!(
                check(&missing, standard.source, LayoutTarget::Board)
                    .unwrap_err()
                    .to_string()
                    .contains("no resolvable drill span")
            );
        }
    }

    #[test]
    fn plated_ownership_and_nonplated_clearance_use_the_slot_ends_not_its_width() {
        for (plating, selector, net, fails) in [
            ("PLATED", "plated", "net=\"N1\"", false),
            ("PLATED", "plated", "net=\"N2\"", true),
            ("PLATED", "plated", "", true),
            ("NONPLATED", "nonplated", "net=\"N1\"", true),
        ] {
            let result = check(
                &board(plating, "net=\"N1\"", &copper(net)),
                &pdk(selector),
                LayoutTarget::Board,
            )
            .unwrap();
            assert_eq!(result.rules[0].checked, 2);
            assert_eq!(result.findings.len(), usize::from(fails), "{result:#?}");
            if fails {
                let finding = &result.findings[0];
                assert!((finding.measurement.actual_mm().unwrap() - 0.1).abs() < 1e-8);
                assert_eq!(
                    finding.subjects[0]
                        .drill_span
                        .as_ref()
                        .unwrap()
                        .last_copper_index,
                    1
                );
                assert!(!finding.sites.is_empty());
                assert!(
                    result
                        .scene
                        .passes
                        .iter()
                        .any(|pass| pass.feature == "drills"
                            && pass.layer.as_deref() == Some("ROUT"))
                );
            }
        }
    }

    #[test]
    fn only_the_proven_physical_land_is_exempt_not_other_netless_copper() {
        let land = r#"<LayerFeature layerRef="L0"><Set><Pad padstackDefRef="land-stack"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pad></Set></LayerFeature>"#;
        let own = check(
            &board("PLATED", "geometry=\"land-stack\"", land),
            &pdk("plated"),
            LayoutTarget::Board,
        )
        .unwrap();
        assert!(own.findings.is_empty());
        for (identity, copper_land) in [
            ("", land.to_owned()),
            (
                "geometry=\"land-stack\" net=\"N1\"",
                land.replace("<Set>", "<Set net=\"N2\">"),
            ),
        ] {
            let unproven = check(
                &board("PLATED", identity, &copper_land),
                &pdk("plated"),
                LayoutTarget::Board,
            )
            .unwrap();
            assert_eq!(
                unproven.findings.len(),
                1,
                "overlap or conflicting net is not ownership"
            );
        }
        let foreign = check(
            &board(
                "PLATED",
                "geometry=\"land-stack\"",
                &format!("{land}{}", copper("")),
            ),
            &pdk("plated"),
            LayoutTarget::Board,
        )
        .unwrap();
        assert_eq!(foreign.findings.len(), 1);
        assert_eq!(foreign.findings[0].subjects[1].kind, "unattributed_copper");
        let nonplated = check(
            &board("NONPLATED", "geometry=\"land-stack\"", land),
            &pdk("nonplated"),
            LayoutTarget::Board,
        )
        .unwrap();
        assert_eq!(nonplated.findings[0].measurement.actual_mm(), Some(0.0));
    }

    #[test]
    fn physical_span_survives_mirrored_repeats_and_missing_span_fails_closed() {
        let source = pdk("plated");
        let outside = board(
            "PLATED",
            "net=\"N1\"",
            &copper("net=\"N2\"").replace("layerRef=\"L0\"", "layerRef=\"L2\""),
        );
        assert!(
            check(&outside, &source, LayoutTarget::Board)
                .unwrap()
                .findings
                .is_empty()
        );
        let inside = outside.replace("layerRef=\"L2\"", "layerRef=\"L1\"");
        let panel = inside.replace("<StepRef name=\"board\"/>", "<StepRef name=\"panel\"/>")
            .replace("</CadData>", r#"<Step name="panel" type="PALLET"><StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="20" dy="0" mirror="true"/></Step></CadData>"#);
        let repeated = check(&panel, &source, LayoutTarget::BoardArray).unwrap();
        assert_eq!(repeated.rules[0].checked, 4);
        assert_eq!(repeated.findings.len(), 2);
        for finding in &repeated.findings {
            assert!((finding.measurement.actual_mm().unwrap() - 0.1).abs() < 1e-8);
            assert_eq!(
                finding.subjects[0]
                    .provenance
                    .as_ref()
                    .unwrap()
                    .instance_index,
                finding.subjects[1]
                    .provenance
                    .as_ref()
                    .unwrap()
                    .instance_index
            );
        }
        let missing = inside.replace("<Span fromLayer=\"L0\" toLayer=\"L1\"/>", "");
        assert!(
            check(&missing, &source, LayoutTarget::Board)
                .unwrap_err()
                .to_string()
                .contains("no resolvable drill span")
        );
        let inactive = source.replace("limit = { minimum = \"0.20 mm\" }", "cases = [{ id = \"two\", when = { copper_layers = { exact = 2 } }, limit = { minimum = \"0.20 mm\" } }]");
        assert!(matches!(
            check(&missing, &inactive, LayoutTarget::Board)
                .unwrap()
                .rules[0]
                .status,
            report::RuleStatus::Skipped
        ));
    }
}
