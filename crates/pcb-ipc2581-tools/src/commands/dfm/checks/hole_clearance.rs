//! Minimum clearance from a circular drilled hole to unrelated final copper.
//!
//! For a drill disk `D(c, r)` and one attributed final copper image `M`, the
//! clearance is `dist(D, M) = max(0, dist(c, M) - r)`. The drill is analytic;
//! only the composed copper boundary contributes geometric uncertainty. Via
//! and PTH checks exclude copper proven to belong to the hole's scoped net or
//! a physically associated land. NPTH checks exclude nothing. Measurements
//! are made only on copper layers in the declared drill span.

use pcb_ir::geom::dfm::{Distance, circular_region, region_clearance_sites};
use pcb_ir::geom::{BBox, Point};

use crate::commands::dfm::design::{Design, HoleClass, spans};
use crate::commands::dfm::report::{Evidence, MeasurementKind};
use crate::commands::dfm::rules::Conditions;

use super::copper_clearance::conductor_subject;
use super::{
    Evaluation, Measured, MeasuredSite, Ownership, hole_subject, layers, linework_clearance,
    spanned_layers, violates,
};

pub(super) fn evaluate(
    limit_mm: f64,
    class: HoleClass,
    conditions: &Conditions,
    design: &Design,
) -> anyhow::Result<Evaluation> {
    let mut checked = 0;
    let mut measured = Vec::new();

    // A placed hole is measured here too, against what its own Step's design
    // does not hold: this Step's copper and that of the other placements.
    for (hole_index, hole) in design
        .holes
        .iter()
        .enumerate()
        .filter(|(_, hole)| hole.class == class)
    {
        let radius_mm = hole.diameter_mm / 2.0;
        let owner = Ownership::of(
            design,
            hole.net,
            hole.padstack,
            hole.step,
            hole.provenance.instance_index,
            &design.hole_lands[hole_index],
        );
        for (copper_index, copper) in spanned_layers(design, &hole.drill_span, conditions) {
            checked += usize::from(hole.branch.is_none());
            // Only a conductor whose bounds reach the keepout can enter it.
            let nearest = design.conductors_near[copper_index]
                .query(hole.bbox.expand(limit_mm))
                .into_iter()
                .map(|index| {
                    (
                        &copper.conductors[index],
                        &design.conductor_boundaries[copper_index][index],
                    )
                })
                .filter(|(conductor, _)| spans(hole.branch, conductor.branch))
                .filter(|(conductor, _)| class == HoleClass::Npth || !owner.owns(conductor.id))
                .filter_map(|(conductor, boundary)| {
                    disk_to_copper_clearance(
                        hole.center,
                        radius_mm,
                        &conductor.image,
                        boundary,
                        limit_mm,
                    )
                    .map(|distance| (conductor, distance))
                })
                .min_by(|(_, left), (_, right)| left.mm.total_cmp(&right.mm));

            let Some((offender, distance)) = nearest else {
                continue;
            };
            let finding_layers = layers([&hole.layer, &copper.layer]);
            let subjects = vec![
                hole_subject(design, hole, "hole"),
                conductor_subject(design, offender.id, "offender", &copper.layer.name),
            ];
            let drilled = Evidence::circle("drilled_hole", hole.center, hole.diameter_mm);
            let keepout = Evidence::circle(
                "required_copper_keepout",
                hole.center,
                hole.diameter_mm + 2.0 * limit_mm,
            );
            let evidence = vec![
                drilled.clone(),
                Evidence::bounds("offending_copper", offender.image.bbox),
            ];
            let mut sites = Vec::new();
            if violates(&distance, limit_mm) {
                let drill = circular_region(hole.center, radius_mm, design.resolution)?;
                sites = linework_clearance::report_sites(
                    region_clearance_sites(&drill, &offender.image, limit_mm)?,
                    &finding_layers,
                    limit_mm,
                    design.resolution,
                )?;
                for site in &mut sites {
                    site.evidence.extend([drilled.clone(), keepout.clone()]);
                }
                // The flattened drill can clear what the analytic disk does not.
                if !sites.iter().any(|site| violates(&site.distance, limit_mm)) {
                    let mut site = MeasuredSite::new(
                        distance,
                        BBox::spanning(distance.first, distance.second)
                            .union(hole.bbox.expand(limit_mm)),
                        finding_layers.clone(),
                        vec![drilled, keepout],
                        if distance.mm == 0.0 {
                            MeasurementKind::Overlap
                        } else {
                            MeasurementKind::Clearance
                        },
                    );
                    site.note = Some(
                        "The analytic drill clearance is below the configured limit.".to_owned(),
                    );
                    sites.push(site);
                }
                for site in &mut sites {
                    site.subjects = subjects.clone();
                }
            }
            let mut bbox = hole.bbox.expand(limit_mm);
            bbox.include_point(distance.second);
            measured.push(Measured {
                distance,
                bbox,
                layers: finding_layers,
                subjects,
                evidence,
                sites,
            });
        }
    }

    Ok(Evaluation { checked, measured })
}

fn disk_to_copper_clearance(
    center: Point,
    radius_mm: f64,
    copper: &pcb_ir::geom::ContourSet,
    boundary: &pcb_ir::geom::PreparedRegion,
    limit_mm: f64,
) -> Option<Distance> {
    if copper.contains_point(center) {
        return Some(Distance::with_uncertainty(
            0.0,
            center,
            center,
            copper.uncertainty_mm,
        ));
    }
    let nearest = boundary.canonical_nearest_within(center, radius_mm + limit_mm)?;
    let direction = nearest.second - center;
    let direction = if direction.length() <= f64::EPSILON {
        Point::new(1.0, 0.0)
    } else {
        direction / direction.length()
    };
    if nearest.mm <= radius_mm {
        // The copper boundary lies inside the drill disk, so this point is
        // shared by both closed regions even when neither center is contained.
        return Some(Distance::with_uncertainty(
            0.0,
            nearest.second,
            nearest.second,
            nearest.uncertainty_mm,
        ));
    }
    Some(Distance::with_uncertainty(
        nearest.mm - radius_mm,
        center + direction * radius_mm,
        nearest.second,
        nearest.uncertainty_mm,
    ))
}

#[cfg(test)]
mod tests {
    use pcb_ir::geom::Resolution;

    use crate::commands::dfm::report::RuleStatus;
    use crate::commands::dfm::{checks, design::Design, fixtures};

    fn pdk(hole: &str) -> String {
        fixtures::pdk(&format!(
            r#"[[rules.copper.hole_clearance]]
id = "hole-clearance"
select = {{ hole = "{hole}" }}
limit = {{ minimum = "0.20 mm" }}"#
        ))
    }

    fn copper(layer: usize, net: Option<&str>, x: f64) -> String {
        let net = net
            .map(|net| format!(r#" net="{net}""#))
            .unwrap_or_default();
        format!(
            r#"<LayerFeature layerRef="L{layer}"><Set{net} polarity="POSITIVE"><Features><Contour><Polygon>
              <PolyBegin x="{x}" y="-0.5"/><PolyStepSegment x="{}" y="-0.5"/>
              <PolyStepSegment x="{}" y="0.5"/><PolyStepSegment x="{x}" y="0.5"/>
              <PolyStepSegment x="{x}" y="-0.5"/>
            </Polygon></Contour></Features></Set></LayerFeature>"#,
            x + 1.0,
            x + 1.0,
        )
    }

    fn board(plating: &str, span: Option<(usize, usize)>, copper_features: &[String]) -> String {
        let span = span
            .map(|(from, to)| format!(r#"<Span fromLayer="L{from}" toLayer="L{to}"/>"#))
            .unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
    <LayerRef name="L0"/><LayerRef name="L1"/><LayerRef name="L2"/><LayerRef name="DRILL"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="L0" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
    <Layer name="L1" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
    <Layer name="L2" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">{span}</Layer>
    <Stackup name="Primary" overallThickness="0.105" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
      <StackupGroup name="Primary_Group" thickness="0.105" tolPlus="0" tolMinus="0">
        <StackupLayer layerOrGroupRef="L0" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
        <StackupLayer layerOrGroupRef="L1" thickness="0.035" tolPlus="0" tolMinus="0" sequence="1"/>
        <StackupLayer layerOrGroupRef="L2" thickness="0.035" tolPlus="0" tolMinus="0" sequence="2"/>
      </StackupGroup>
    </Stackup>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/>
      {}
      <LayerFeature layerRef="DRILL"><Set net="N1" polarity="POSITIVE">
        <Hole name="H1" diameter="1" platingStatus="{plating}" x="0" y="0"/>
      </Set></LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#,
            copper_features.join("\n")
        )
    }

    fn board_with_unowned_land(other_copper: bool) -> String {
        let other_copper = if other_copper {
            r#"<Features><Contour><Polygon>
              <PolyBegin x="0.55" y="-0.5"/><PolyStepSegment x="1.55" y="-0.5"/>
              <PolyStepSegment x="1.55" y="0.5"/><PolyStepSegment x="0.55" y="0.5"/>
              <PolyStepSegment x="0.55" y="-0.5"/>
            </Polygon></Contour></Features>"#
        } else {
            ""
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
    <LayerRef name="L0"/><LayerRef name="DRILL"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="land"><Circle diameter="1.4"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="L0" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
      <Span fromLayer="L0" toLayer="L0"/>
    </Layer>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/>
      <PadStackDef name="land-stack">
        <PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
      </PadStackDef>
      <LayerFeature layerRef="L0"><Set polarity="POSITIVE">
        <Pad padstackDefRef="land-stack"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pad>
        {other_copper}
      </Set></LayerFeature>
      <LayerFeature layerRef="DRILL"><Set geometry="land-stack" polarity="POSITIVE">
        <Hole name="H1" diameter="1" platingStatus="PLATED" x="0" y="0"/>
      </Set></LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#
        )
    }

    fn run(xml: &str, hole: &str) -> checks::Results {
        fixtures::run_board(xml, &pdk(hole))
    }

    #[test]
    fn passes_and_fails_edge_to_edge_clearance_with_report_evidence() {
        let passing = run(
            &board("VIA", Some((0, 2)), &[copper(0, Some("N2"), 0.8)]),
            "via",
        );
        assert!(passing.findings.is_empty());
        assert_eq!(passing.rules[0].checked, 3);

        let failing = run(
            &board("VIA", Some((0, 2)), &[copper(0, Some("N2"), 0.65)]),
            "via",
        );
        assert_eq!(failing.findings.len(), 1);
        let finding = &failing.findings[0];
        assert!((finding.measurement.actual_mm().unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(finding.subjects[0].role, "hole");
        assert_eq!(finding.subjects[1].role, "offender");
        assert_eq!(finding.subjects[1].net.as_deref(), Some("N2"));
        assert_eq!(finding.layers[1].name, "L0");
        assert_eq!(finding.location.witnesses.len(), 2);
        assert!(!finding.id.is_empty());
        assert!(finding.sites.iter().all(|site| {
            site.evidence
                .iter()
                .any(|evidence| evidence.role == "required_copper_keepout")
        }));
    }

    #[test]
    fn hole_clearance_report_includes_spatial_view_and_native_context() {
        let xml = board("VIA", Some((0, 2)), &[copper(0, Some("N2"), 0.65)]);
        let checked = fixtures::report(&xml, &pdk("via"), crate::LayoutTarget::Board);

        assert_eq!(checked.rules[0].view.kind, "hole_to_copper_clearance");
        assert!(checked.rules[0].view.spatial);
        assert_eq!(
            checked.rules[0].view.features,
            ["copper", "drills", "board_outlines"]
        );
        for (feature, layer) in [("copper", "L0"), ("drills", "DRILL")] {
            let mut passes = checked.scene.passes.iter();
            assert!(
                passes.any(|pass| pass.feature == feature && pass.layer.as_deref() == Some(layer))
            );
        }
    }

    #[test]
    fn excludes_own_net_for_vias_and_pths_but_not_for_npth_or_unattributed_copper() {
        for (hole, plating, own_net_offends) in [
            ("via", "VIA", false),
            ("pth", "PLATED", false),
            ("npth", "NONPLATED", true),
        ] {
            let own_net = run(
                &board(plating, Some((0, 2)), &[copper(0, Some("N1"), 0.55)]),
                hole,
            );
            assert_eq!(
                own_net.findings.len(),
                usize::from(own_net_offends),
                "{hole} own net"
            );

            let unattributed = run(
                &board(plating, Some((0, 2)), &[copper(0, None, 0.55)]),
                hole,
            );
            assert_eq!(unattributed.findings.len(), 1, "{hole} unattributed");
            assert_eq!(
                unattributed.findings[0].subjects[1].kind,
                "unattributed_copper"
            );
        }
    }

    #[test]
    fn excludes_only_the_resolved_unowned_land_not_other_copper_in_its_set() {
        let own_land = run(&board_with_unowned_land(false), "pth");
        assert!(own_land.findings.is_empty());

        let with_other_copper = run(&board_with_unowned_land(true), "pth");
        assert_eq!(with_other_copper.findings.len(), 1);
        assert_eq!(
            with_other_copper.findings[0].subjects[1].kind,
            "unattributed_copper"
        );
        assert_eq!(
            with_other_copper.findings[0].subjects[1]
                .source
                .as_ref()
                .unwrap()
                .feature_index,
            Some(1)
        );
    }

    #[test]
    fn a_drill_through_a_foreign_land_is_not_exempted_by_overlapping_it() {
        // The hole is N1 with its own padstack; the only land it overlaps is
        // N2 copper of another padstack. Overlap links them, identity does not.
        let replace = |xml: String, from: &str, to: &str| {
            assert!(xml.contains(from), "fixture no longer contains {from}");
            xml.replace(from, to)
        };
        let xml = replace(
            board_with_unowned_land(false),
            r#"<PadStackDef name="land-stack">"#,
            r#"<PadStackDef name="other-stack">
        <PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
      </PadStackDef>
      <PadStackDef name="land-stack">"#,
        );
        let xml = replace(
            xml,
            r#"<LayerFeature layerRef="L0"><Set polarity="POSITIVE">
        <Pad padstackDefRef="land-stack">"#,
            r#"<LayerFeature layerRef="L0"><Set net="N2" polarity="POSITIVE">
        <Pad padstackDefRef="other-stack">"#,
        );
        // With or without a stated padstack on the drill, which is what
        // decides whether import links the two by overlap alone.
        for drill in [
            r#"<Set geometry="land-stack" net="N1" polarity="POSITIVE">"#,
            r#"<Set net="N1" polarity="POSITIVE">"#,
        ] {
            let xml = replace(
                xml.clone(),
                r#"<Set geometry="land-stack" polarity="POSITIVE">"#,
                drill,
            );
            let results = run(&xml, "pth");
            assert_eq!(
                results.findings.len(),
                1,
                "{drill}: {:?}",
                results.rules[0].skip_reason
            );
            assert_eq!(results.findings[0].measurement.actual_mm(), Some(0.0));
            assert_eq!(results.findings[0].subjects[1].net.as_deref(), Some("N2"));
        }
    }

    #[test]
    fn checks_unattributed_copper_when_layer_features_reuse_land_source_indices() {
        // Both the owned land and the unrelated contour are set 0, feature 0,
        // but they belong to separate LayerFeatures on the same copper layer.
        let xml = board_with_unowned_land(false).replace(
            r#"<LayerFeature layerRef="DRILL">"#,
            &format!(
                "{}\n<LayerFeature layerRef=\"DRILL\">",
                copper(0, None, 0.55)
            ),
        );
        let results = run(&xml, "pth");
        assert_eq!(results.findings.len(), 1);
        let finding = &results.findings[0];
        assert!((finding.measurement.actual_mm().unwrap() - 0.05).abs() < 1e-9);
        let offender = &finding.subjects[1];
        assert_eq!(offender.kind, "unattributed_copper");
        let source = offender.source.as_ref().unwrap();
        assert_eq!(source.set_index, Some(0));
        assert_eq!(source.feature_index, Some(0));
    }

    #[test]
    fn a_layer_or_stackup_no_case_matches_is_reported_not_left_unchecked() {
        let cased = |cases: &str| {
            pdk("via").replace(
                "limit = { minimum = \"0.20 mm\" }",
                &format!("cases = [{cases}]"),
            )
        };
        let outer = r#"{ id = "outer", when = { copper = { position = "outer" } }, limit = { minimum = "0.20 mm" } }"#;
        let inner = r#"{ id = "inner", when = { copper = { position = "inner" } }, limit = { preferred = "0.20 mm" } }"#;
        let through = board("VIA", Some((0, 2)), &[copper(0, Some("N2"), 0.8)]);

        // The through via meets the inner layer, which no case limits.
        let partial = fixtures::run_board(&through, &cased(outer));
        assert_eq!(partial.rules.len(), 2);
        assert!(matches!(partial.rules[0].status, RuleStatus::Pass));
        assert_eq!(
            partial.rules[0].checked, 2,
            "both outer layers are measured"
        );
        let coverage = &partial.rules[1];
        assert_eq!(coverage.id, "hole-clearance");
        assert!(
            coverage.blocks_verdict(),
            "the outer case is a required tier"
        );
        assert_eq!(
            coverage.skip_reason.as_deref(),
            Some("no case applies to copper layer(s) 'L1' (inner, 1.01 oz)")
        );

        let complete = fixtures::run_board(&through, &cased(&format!("{outer}, {inner}")));
        assert_eq!(
            complete
                .rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            ["hole-clearance.outer", "hole-clearance.inner.preferred"]
        );

        // Without a via there is nothing the uncovered layer leaves unchecked.
        let no_vias = fixtures::run_board(&board("PLATED", Some((0, 2)), &[]), &cased(outer));
        assert_eq!(no_vias.rules.len(), 1);
        assert!(matches!(no_vias.rules[0].status, RuleStatus::NotApplicable));

        let two_layer = r#"{ id = "two", when = { copper_layers = { exact = 2 } }, limit = { minimum = "0.20 mm" } }"#;
        let count = fixtures::run_board(&through, &cased(two_layer));
        assert!(matches!(count.rules[0].status, RuleStatus::NotApplicable));
        assert_eq!(
            count.rules[1].skip_reason.as_deref(),
            Some("no case applies to a design with 3 copper layer(s)")
        );
    }

    #[test]
    fn a_drill_layer_without_a_span_is_through_board() {
        // Offending copper on the bottom layer: only a through drill meets it.
        let copper = [copper(2, Some("N2"), 0.55)];
        let undeclared = run(&board("VIA", None, &copper), "via");
        let declared = run(&board("VIA", Some((0, 2)), &copper), "via");
        assert_eq!(undeclared.rules[0].checked, 3);
        assert_eq!(undeclared.findings.len(), 1);
        assert_eq!(undeclared.findings[0].layers[1].name, "L2");
        assert_eq!(
            undeclared.findings[0].measurement.actual_mm(),
            declared.findings[0].measurement.actual_mm()
        );
    }

    #[test]
    fn rejects_a_hole_without_a_resolvable_drill_span() {
        let xml = board("VIA", Some((0, 1)), &[copper(0, Some("N2"), 0.8)]).replace(
            r#"<Span fromLayer="L0" toLayer="L1"/>"#,
            r#"<Span fromLayer="L0"/>"#,
        );
        let results = run(&xml, "via");
        let rule = &results.rules[0];
        assert!(rule.blocks_verdict(), "an unknown span must fail closed");
        assert!(
            rule.skip_reason
                .as_deref()
                .unwrap()
                .contains("no resolvable drill span")
        );
    }

    #[test]
    fn physical_drill_span_is_independent_of_copper_declaration_order() {
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            for (offender, expected_findings) in [(1, 1), (2, 0)] {
                let xml = board("VIA", Some((0, 1)), &[copper(offender, Some("N2"), 0.55)]);
                let declarations = xml
                    .lines()
                    .filter(|line| line.contains("<Layer name=\"L"))
                    .collect::<Vec<_>>();
                let xml = xml.replace(
                    &declarations.join("\n"),
                    &order.map(|i| declarations[i]).join("\n"),
                );
                let (imported, rules) = (fixtures::import(&xml), fixtures::rules(&pdk("via")));
                let design = Design::board(&imported, &rules, Resolution::default());
                let mut included = design
                    .copper_layers
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| design.holes[0].drill_span.contains_copper(*index))
                    .map(|(_, layer)| layer.layer.name.as_str())
                    .collect::<Vec<_>>();
                included.sort_unstable();
                assert_eq!(included, ["L0", "L1"], "declarations {order:?}");
                let results = run(&xml, "via");
                assert_eq!(results.rules[0].checked, 2);
                assert_eq!(
                    results.findings.len(),
                    expected_findings,
                    "declarations {order:?}, copper L{offender}"
                );
            }
        }
    }

    #[test]
    fn does_not_require_a_span_for_a_nonapplicable_named_case() {
        let xml = board("VIA", None, &[copper(0, Some("N2"), 0.55)]);
        let source = pdk("via").replace(
            "limit = { minimum = \"0.20 mm\" }",
            "cases = [{ id = \"two-layer\", when = { copper_layers = { exact = 2 } }, limit = { minimum = \"0.20 mm\" } }]",
        );
        let results = fixtures::run_board(&xml, &source);

        assert!(matches!(results.rules[0].status, RuleStatus::NotApplicable));
        assert_eq!(
            results.rules[0].skip_reason.as_deref(),
            Some("rule conditions do not apply to this stackup")
        );
    }
}
