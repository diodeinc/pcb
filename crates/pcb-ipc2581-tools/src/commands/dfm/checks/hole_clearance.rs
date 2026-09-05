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

use crate::commands::dfm::design::{
    ConductorId, CopperLayer, Design, Hole, HoleClass, HoleLand, Land,
};
use crate::commands::dfm::report::{Evidence, MeasurementKind};
use crate::commands::dfm::rules::Conditions;

use super::copper_clearance::conductor_subject;
use super::{
    Evaluation, Measured, MeasuredSite, hole_subject, holes_of_class, layers, linework_clearance,
    violates,
};

pub(super) fn evaluate(
    limit_mm: f64,
    class: HoleClass,
    conditions: &Conditions,
    design: &Design,
) -> Evaluation {
    let mut checked = 0;
    let mut measured = Vec::new();

    for (hole_index, hole) in holes_of_class(design, class) {
        let radius_mm = hole.diameter_mm / 2.0;
        for (copper_index, copper) in design.copper_layers.iter().enumerate() {
            if !hole.spans_copper(copper_index) || !conditions.applies_to_layer(copper) {
                continue;
            }
            checked += 1;
            let own_lands = design.hole_lands[hole_index]
                .iter()
                .filter(|land| land.copper_index as usize == copper_index)
                .collect::<Vec<_>>();
            let nearest = copper
                .conductors
                .iter()
                .zip(&design.conductor_boundaries[copper_index])
                .filter(|(conductor, _)| !owned_by_hole(hole, copper, &own_lands, conductor.id))
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
            let evidence = vec![
                Evidence::circle("drilled_hole", hole.center, hole.diameter_mm),
                Evidence::bounds("offending_copper", offender.image.bbox),
            ];
            let sites = if violates(&distance, limit_mm) {
                let drill = circular_region(hole.center, radius_mm);
                let mut sites = region_clearance_sites(&drill, &offender.image, limit_mm)
                    .into_iter()
                    .map(|geometry| {
                        let mut site = linework_clearance::report_site(
                            geometry,
                            finding_layers.clone(),
                            limit_mm,
                        );
                        site.subjects = subjects.clone();
                        site.evidence.push(Evidence::circle(
                            "drilled_hole",
                            hole.center,
                            hole.diameter_mm,
                        ));
                        site.evidence.push(Evidence::circle(
                            "required_copper_keepout",
                            hole.center,
                            hole.diameter_mm + 2.0 * limit_mm,
                        ));
                        site
                    })
                    .collect::<Vec<_>>();
                if !sites.iter().any(|site| violates(&site.distance, limit_mm)) {
                    sites.push(fallback_site(
                        distance,
                        hole,
                        limit_mm,
                        finding_layers.clone(),
                        subjects.clone(),
                    ));
                }
                sites
            } else {
                Vec::new()
            };
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

    Evaluation { checked, measured }
}

fn disk_to_copper_clearance(
    center: Point,
    radius_mm: f64,
    copper: &pcb_ir::geom::ContourSet,
    boundary: &pcb_ir::geom::PreparedRegion,
    limit_mm: f64,
) -> Option<Distance> {
    if copper.contains_point(center) {
        return Some(Distance::flattened(0.0, center, center, 1));
    }
    let nearest = boundary.nearest_within(center, radius_mm + limit_mm)?;
    let direction = nearest.second - center;
    let direction = if direction.length() <= f64::EPSILON {
        Point::new(1.0, 0.0)
    } else {
        direction / direction.length()
    };
    if nearest.mm <= radius_mm {
        // The copper boundary lies inside the drill disk, so this point is
        // shared by both closed regions even when neither center is contained.
        return Some(Distance::flattened(0.0, nearest.second, nearest.second, 1));
    }
    Some(Distance::flattened(
        nearest.mm - radius_mm,
        center + direction * radius_mm,
        nearest.second,
        1,
    ))
}

fn owned_by_hole(
    hole: &Hole,
    copper: &CopperLayer,
    links: &[&HoleLand],
    conductor: ConductorId,
) -> bool {
    if hole.class == HoleClass::Npth {
        return false;
    }
    let hole_owner = hole.net.map(|net| ConductorId::Net {
        step: hole.step,
        instance: hole.provenance.instance_index,
        net,
    });
    if hole_owner == Some(conductor) {
        return true;
    }
    links.iter().any(|link| {
        let land = &copper.lands[link.land_index as usize];
        land_owns_conductor(land, conductor)
    })
}

fn land_owns_conductor(land: &Land, conductor: ConductorId) -> bool {
    match conductor {
        ConductorId::Net {
            step,
            instance,
            net,
        } => {
            land.net == Some(net) && step == land.step && instance == land.provenance.instance_index
        }
        ConductorId::Auxiliary { .. } | ConductorId::Unattributed { .. } => false,
        ConductorId::Isolated { occurrence, .. } => land.id.0 == occurrence,
    }
}

fn fallback_site(
    distance: Distance,
    hole: &Hole,
    limit_mm: f64,
    layers: Vec<crate::commands::dfm::report::LayerRef>,
    subjects: Vec<crate::commands::dfm::report::Subject>,
) -> MeasuredSite {
    let mut bbox = BBox::from_point(distance.first);
    bbox.include_point(distance.second);
    bbox = bbox.union(hole.bbox.expand(limit_mm));
    let mut site = MeasuredSite::new(
        distance,
        bbox,
        layers,
        vec![
            Evidence::circle("drilled_hole", hole.center, hole.diameter_mm),
            Evidence::circle(
                "required_copper_keepout",
                hole.center,
                hole.diameter_mm + 2.0 * limit_mm,
            ),
        ],
        if distance.mm == 0.0 {
            MeasurementKind::Overlap
        } else {
            MeasurementKind::Clearance
        },
    );
    site.subjects = subjects;
    site.note = Some("The analytic drill clearance is below the configured limit.".to_owned());
    site
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pcb_ir::dialects::ipc::ArtworkScope;

    use crate::commands::dfm::{checks, design::Design, pdk::Pdk, rules};
    use crate::ipc2581::Ipc2581;

    fn pdk(hole: &str) -> String {
        format!(
            r#"schema_version = 2
default_profile = "test"

[pdk]
id = "hole-clearance-test"
name = "Hole clearance test"
revision = "1"

[profiles.test]
name = "Test"

[[rules.copper.hole_clearance]]
id = "hole-clearance"
select = {{ hole = "{hole}" }}
limit = {{ minimum = "0.20 mm" }}
"#
        )
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
        let ipc = Ipc2581::parse(xml).unwrap();
        let pdk = Pdk::parse(&pdk(hole)).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules).unwrap();
        checks::run(
            &rules,
            &design,
            None,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        )
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
        use crate::LayoutTarget;
        use crate::commands::dfm::{CheckRequest, PdkSource, TextSource, report};

        let xml = board("VIA", Some((0, 2)), &[copper(0, Some("N2"), 0.65)]);
        let source = pdk("via");
        let ipc = Ipc2581::parse(&xml).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let checked = crate::commands::dfm::check(
            &imported,
            CheckRequest {
                input: report::FileIdentity::new("board.xml", xml.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "pdk.toml",
                    source: &source,
                }),
                waivers: None,
                layout_target: LayoutTarget::Board,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
        .unwrap();

        assert!(matches!(checked.verdict, report::Verdict::Fail));
        assert_eq!(checked.rules[0].view.kind, "hole_to_copper_clearance");
        assert!(checked.rules[0].view.spatial);
        assert_eq!(
            checked.rules[0].view.features,
            ["copper", "drills", "board_outlines"]
        );
        assert!(
            checked
                .scene
                .passes
                .iter()
                .any(|pass| { pass.feature == "copper" && pass.layer.as_deref() == Some("L0") })
        );
        assert!(
            checked
                .scene
                .passes
                .iter()
                .any(|pass| { pass.feature == "drills" && pass.layer.as_deref() == Some("DRILL") })
        );
    }

    #[test]
    fn excludes_own_net_for_vias_and_pths_but_not_unattributed_copper() {
        for (hole, plating) in [("via", "VIA"), ("pth", "PLATED")] {
            let own_net = run(
                &board(plating, Some((0, 2)), &[copper(0, Some("N1"), 0.55)]),
                hole,
            );
            assert!(own_net.findings.is_empty(), "{hole} own net");

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
    fn npth_treats_same_net_copper_as_an_offender() {
        let results = run(
            &board("NONPLATED", Some((0, 2)), &[copper(0, Some("N1"), 0.55)]),
            "npth",
        );
        assert_eq!(results.findings.len(), 1);
        assert_eq!(results.findings[0].subjects[1].net.as_deref(), Some("N1"));
    }

    #[test]
    fn checks_only_copper_layers_in_the_declared_drill_span() {
        let outside = run(
            &board("VIA", Some((0, 1)), &[copper(2, Some("N2"), 0.55)]),
            "via",
        );
        assert_eq!(outside.rules[0].checked, 2);
        assert!(outside.findings.is_empty());

        let inside = run(
            &board("VIA", Some((0, 1)), &[copper(1, Some("N2"), 0.55)]),
            "via",
        );
        assert_eq!(inside.findings.len(), 1);
        assert_eq!(inside.findings[0].layers[1].name, "L1");
    }

    #[test]
    fn rejects_a_hole_without_a_resolvable_drill_span() {
        let xml = board("VIA", None, &[copper(0, Some("N2"), 0.8)]);
        let ipc = Ipc2581::parse(&xml).unwrap();
        let pdk = Pdk::parse(&pdk("via")).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let error = Design::extract(&imported, ArtworkScope::Board, &rules)
            .err()
            .expect("an unknown span must fail closed");
        assert!(error.to_string().contains("no resolvable drill span"));
    }

    #[test]
    fn does_not_require_a_span_for_a_nonapplicable_named_case() {
        let xml = board("VIA", None, &[copper(0, Some("N2"), 0.55)]);
        let source = pdk("via").replace(
            "limit = { minimum = \"0.20 mm\" }",
            "cases = [{ id = \"two-layer\", when = { copper_layers = { exact = 2 } }, limit = { minimum = \"0.20 mm\" } }]",
        );
        let ipc = Ipc2581::parse(&xml).unwrap();
        let pdk = Pdk::parse(&source).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules).unwrap();
        let results = checks::run(
            &rules,
            &design,
            None,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        );

        assert!(matches!(
            results.rules[0].status,
            crate::commands::dfm::report::RuleStatus::Skipped
        ));
        assert_eq!(
            results.rules[0].skip_reason.as_deref(),
            Some("rule conditions do not apply to this stackup")
        );
    }
}
