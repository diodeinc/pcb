//! Minimum clearance from drilled features to their physical board edge.
//!
//! A hole is an analytic closed disk and a routed slot is its materialized
//! filled outline. Each feature is paired only with a board profile from the
//! same physical Step occurrence. The profile region is its outer boundary
//! minus every cutout. For a feature fully inside that region, the measured
//! quantity is the Euclidean distance between the feature boundary and the
//! profile boundary. A feature that crosses or lies outside its board region
//! has zero clearance.

use pcb_ir::geom::dfm::{Distance, circular_region};
use pcb_ir::geom::region::ring_edges;
use pcb_ir::geom::{BBox, Point};

use crate::commands::dfm::design::{BoardOutline, Design, Hole, HoleClass, Slot};
use crate::commands::dfm::pdk::SlotPlating;
use crate::commands::dfm::report::{Evidence, EvidenceDisplay, MeasurementKind};

use super::{
    Evaluation, Measured, MeasuredSite, hole_subject, linework_clearance, slot_matches,
    slot_subject,
};

pub(super) fn evaluate_holes(limit_mm: f64, class: HoleClass, design: &Design) -> Evaluation {
    let holes = design
        .holes
        .iter()
        .filter(|hole| hole.class == class)
        .collect::<Vec<_>>();
    let measured = holes
        .iter()
        .filter_map(|&hole| {
            let outline = enclosing_outline(
                &design.board_outlines,
                hole.provenance.instance_index,
                hole.center,
            );
            let (distance, outside) = hole_clearance(hole, outline, limit_mm)?;
            Some(measured_hole(
                design, hole, outline, distance, outside, limit_mm,
            ))
        })
        .collect();
    Evaluation {
        checked: holes.len(),
        measured,
    }
}

pub(super) fn evaluate_slots(limit_mm: f64, plating: SlotPlating, design: &Design) -> Evaluation {
    let slots = design
        .slots
        .iter()
        .filter(|slot| slot_matches(slot.plating, plating))
        .collect::<Vec<_>>();
    let measured = slots
        .iter()
        .filter_map(|&slot| {
            let outline = enclosing_outline(
                &design.board_outlines,
                slot.provenance.instance_index,
                slot.bbox.center(),
            );
            let (distance, outside) = slot_clearance(slot, outline, limit_mm)?;
            Some(measured_slot(
                design, slot, outline, distance, outside, limit_mm,
            ))
        })
        .collect();
    Evaluation {
        checked: slots.len(),
        measured,
    }
}

/// Select only among profiles carrying the feature's occurrence identity.
/// Containment breaks ties between multiple physical profiles in one Step;
/// bounds distance gives an outside feature a deterministic related profile.
fn enclosing_outline(
    outlines: &[BoardOutline],
    instance_index: Option<u32>,
    point: Point,
) -> Option<&BoardOutline> {
    outlines
        .iter()
        .filter(|outline| outline.instance_index == instance_index)
        .min_by(|left, right| {
            let left_outside = !left.region.contains_point(point);
            let right_outside = !right.region.contains_point(point);
            left_outside
                .cmp(&right_outside)
                .then_with(|| {
                    left.bbox
                        .distance_to(BBox::from_point(point))
                        .total_cmp(&right.bbox.distance_to(BBox::from_point(point)))
                })
                .then_with(|| left.region.area().total_cmp(&right.region.area()))
        })
}

/// `None` proves the hole clears the limit. A returned distance is either a
/// sub-limit inner clearance or the required zero for an outside/crossing hole.
fn hole_clearance(
    hole: &Hole,
    outline: Option<&BoardOutline>,
    limit_mm: f64,
) -> Option<(Distance, bool)> {
    let Some(outline) = outline else {
        return Some((Distance::exact(0.0, hole.center, hole.center), true));
    };
    if !outline.region.contains_point(hole.center) {
        return Some((
            zero_hole_clearance(hole, outline, broad_search(hole.bbox, outline.bbox)),
            true,
        ));
    }
    let distance =
        outline
            .boundary
            .circular_enclosure(hole.center, hole.diameter_mm / 2.0, limit_mm)?;
    if distance.mm <= 0.0 {
        Some((
            Distance::flattened(0.0, distance.first, distance.second, 1),
            true,
        ))
    } else {
        Some((distance, false))
    }
}

fn zero_hole_clearance(hole: &Hole, outline: &BoardOutline, search_mm: f64) -> Distance {
    let Some(nearest) = outline.boundary.nearest_within(hole.center, search_mm) else {
        return Distance::exact(0.0, hole.center, hole.center);
    };
    let radial = nearest.second - hole.center;
    let direction = if radial.length() <= f64::EPSILON {
        Point::new(1.0, 0.0)
    } else {
        radial / radial.length()
    };
    Distance::flattened(
        0.0,
        hole.center + direction * (hole.diameter_mm / 2.0),
        nearest.second,
        1,
    )
}

fn slot_clearance(
    slot: &Slot,
    outline: Option<&BoardOutline>,
    limit_mm: f64,
) -> Option<(Distance, bool)> {
    let Some(outline) = outline else {
        let point = slot.bbox.center();
        return Some((Distance::exact(0.0, point, point), true));
    };
    let outside = !slot.outline.difference(&outline.region).is_empty();
    let search_mm = if outside {
        broad_search(slot.bbox, outline.bbox)
    } else {
        limit_mm
    };
    let distance = slot
        .outline
        .rings
        .iter()
        .flat_map(ring_edges)
        .filter_map(|(start, end)| {
            outline
                .boundary
                .segment_nearest_within(start, end, search_mm)
        })
        .min_by(|left, right| left.mm.total_cmp(&right.mm))?
        .also_flattened(1);
    if outside {
        Some((
            Distance::flattened(0.0, distance.first, distance.second, 2),
            true,
        ))
    } else {
        Some((distance, false))
    }
}

fn broad_search(feature: BBox, outline: BBox) -> f64 {
    let bounds = feature.union(outline);
    bounds.width().hypot(bounds.height()).max(1.0)
}

fn measured_hole(
    design: &Design,
    hole: &Hole,
    outline: Option<&BoardOutline>,
    distance: Distance,
    outside: bool,
    limit_mm: f64,
) -> Measured {
    let feature = Evidence::circle("drilled_hole", hole.center, hole.diameter_mm);
    let mut evidence = vec![feature.clone()];
    let mut site_evidence = vec![
        feature,
        Evidence::circle(
            "required_board_edge_clearance",
            hole.center,
            hole.diameter_mm + 2.0 * limit_mm,
        ),
    ];
    let mut subjects = vec![hole_subject(design, hole, "offender")];
    if let Some(outline) = outline {
        subjects.push(linework_clearance::outline_subject(outline, "reference"));
        evidence.push(Evidence::bounds("board_profile", outline.bbox));
        site_evidence.push(profile_evidence(outline));
        if outside {
            let outside_region =
                circular_region(hole.center, hole.diameter_mm / 2.0).difference(&outline.region);
            if !outside_region.is_empty() {
                site_evidence.push(Evidence::region("outside_board_material", &outside_region));
            }
        }
    }
    let mut site = MeasuredSite::new(
        distance,
        local_bounds(hole.bbox, distance, limit_mm),
        vec![hole.layer.clone()],
        site_evidence,
        if outside {
            MeasurementKind::OutsideBoard
        } else {
            MeasurementKind::Clearance
        },
    );
    if outside {
        site.note = Some(outside_note(outline.is_some()));
    }
    Measured {
        distance,
        bbox: hole.bbox,
        layers: vec![hole.layer.clone()],
        subjects,
        evidence,
        sites: vec![site],
    }
}

fn measured_slot(
    design: &Design,
    slot: &Slot,
    outline: Option<&BoardOutline>,
    distance: Distance,
    outside: bool,
    limit_mm: f64,
) -> Measured {
    let feature = slot_evidence(slot);
    let mut evidence = vec![Evidence::bounds("routed_slot", slot.bbox)];
    let mut site_evidence = vec![feature];
    let clearance_region = slot.outline.disk_dilate(limit_mm);
    site_evidence.push(Evidence::region(
        "required_board_edge_clearance",
        &clearance_region,
    ));
    let mut subjects = vec![slot_subject(design, slot, "offender")];
    if let Some(outline) = outline {
        subjects.push(linework_clearance::outline_subject(outline, "reference"));
        evidence.push(Evidence::bounds("board_profile", outline.bbox));
        site_evidence.push(profile_evidence(outline));
        if outside {
            let outside_region = slot.outline.difference(&outline.region);
            if !outside_region.is_empty() {
                site_evidence.push(Evidence::region("outside_board_material", &outside_region));
            }
        }
    }
    let mut site = MeasuredSite::new(
        distance,
        local_bounds(slot.bbox, distance, limit_mm),
        vec![slot.layer.clone()],
        site_evidence,
        if outside {
            MeasurementKind::OutsideBoard
        } else {
            MeasurementKind::Clearance
        },
    );
    if outside {
        site.note = Some(outside_note(outline.is_some()));
    }
    Measured {
        distance,
        bbox: slot.bbox,
        layers: vec![slot.layer.clone()],
        subjects,
        evidence,
        sites: vec![site],
    }
}

pub(super) fn slot_evidence(slot: &Slot) -> Evidence {
    Evidence {
        display: Some(EvidenceDisplay::Path {
            paths: slot
                .native_outline
                .iter()
                .map(|contour| pcb_ir::render::svg_path_data(std::slice::from_ref(contour)))
                .collect(),
            fill_rule: "evenodd",
        }),
        ..Evidence::region("routed_slot", &slot.outline)
    }
}

fn profile_evidence(outline: &BoardOutline) -> Evidence {
    Evidence {
        display: Some(EvidenceDisplay::Path {
            paths: vec![pcb_ir::render::svg_path_data(&outline.native_outline)],
            fill_rule: "evenodd",
        }),
        ..Evidence::region("board_profile", &outline.region)
    }
}

fn local_bounds(feature: BBox, distance: Distance, limit_mm: f64) -> BBox {
    feature
        .union(BBox::from_point(distance.first))
        .union(BBox::from_point(distance.second))
        .expand(limit_mm)
}

fn outside_note(has_profile: bool) -> String {
    if has_profile {
        "The drilled feature crosses or lies outside its physical board profile; its clearance is zero."
    } else {
        "The drilled feature has no physical board profile in its Step occurrence; its clearance is zero."
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayoutTarget;
    use crate::commands::dfm::report::{Measurement, RuleStatus, Verdict};
    use crate::commands::dfm::{CheckRequest, PdkSource, TextSource};
    use crate::ipc2581::Ipc2581;
    use pcb_ir::import::ipc2581::import_design;

    fn pdk(rules: &str) -> String {
        format!(
            r#"schema_version = 2
default_profile = "test"

[pdk]
id = "edge-test"
name = "Edge test"
revision = "1"

[profiles.test]
name = "Test"

{rules}
"#
        )
    }

    fn board(features: &str, cutout: &str) -> String {
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="DRILL"/><LayerRef name="ROUT"/></Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
    <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"/>
    <Step name="board" type="BOARD">
      <Profile><Polygon>
        <PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/><PolyStepSegment x="0" y="10"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon>{cutout}</Profile>
      {features}
    </Step>
  </CadData></Ecad>
</IPC-2581>"#
        )
    }

    fn check(xml: &str, pdk_source: &str, target: LayoutTarget) -> super::super::super::DfmReport {
        let imported = import_design(&Ipc2581::parse(xml).unwrap()).unwrap();
        super::super::super::check(
            &imported,
            CheckRequest {
                input: crate::commands::dfm::report::FileIdentity::new(
                    "edge-test.xml",
                    xml.as_bytes(),
                ),
                pdk: PdkSource::Toml(TextSource {
                    path: "edge-test.toml",
                    source: pdk_source,
                }),
                waivers: None,
                layout_target: target,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
        .unwrap()
    }

    fn actual_mm(measurement: &Measurement) -> f64 {
        measurement.actual_mm().unwrap()
    }

    #[test]
    fn circular_holes_pass_and_fail_on_true_edge_clearance_with_native_evidence() {
        let xml = board(
            r#"<LayerFeature layerRef="DRILL"><Set>
              <Hole name="pass" diameter="1" platingStatus="PLATED" x="5" y="2"/>
              <Hole name="fail" diameter="1" platingStatus="PLATED" x="0.7" y="2"/>
            </Set></LayerFeature>"#,
            "",
        );
        let pdk = pdk(r#"[[rules.drilling.hole_to_board_edge_clearance]]
id = "pth-edge"
select = { hole = "pth" }
limit = { minimum = "0.3 mm", preferred = "0.4 mm" }"#);
        let report = check(&xml, &pdk, LayoutTarget::Board);

        assert!(matches!(report.verdict, Verdict::Fail));
        assert_eq!(report.rules[0].checked, 2);
        assert!(matches!(report.rules[0].status, RuleStatus::Fail));
        assert_eq!(report.rules[1].checked, 2, "preferred tier is lowered");
        assert_eq!(report.findings.len(), 2);
        let required = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "pth-edge")
            .unwrap();
        assert!((actual_mm(&required.measurement) - 0.2).abs() < 1e-9);
        assert_eq!(required.layers[0].name, "DRILL");
        assert_eq!(required.subjects[0].kind, "plated_hole");
        assert_eq!(required.subjects[1].kind, "board_outline");
        assert_eq!(required.location.witnesses.len(), 2);
        let site = &required.sites[0];
        assert!(matches!(site.measurement_kind, MeasurementKind::Clearance));
        assert!(site.evidence.iter().any(|evidence| {
            evidence.role == "board_profile"
                && matches!(evidence.display, Some(EvidenceDisplay::Path { .. }))
        }));
        assert!(site.evidence.iter().any(|evidence| {
            evidence.role == "drilled_hole"
                && evidence.kind == "circle"
                && evidence.diameter == Some(1.0)
        }));
    }

    #[test]
    fn crossing_and_outside_holes_have_zero_clearance() {
        let xml = board(
            r#"<LayerFeature layerRef="DRILL"><Set>
              <Hole name="crossing" diameter="0.5" platingStatus="NONPLATED" x="0.2" y="2"/>
              <Hole name="outside" diameter="0.5" platingStatus="NONPLATED" x="-1" y="2"/>
            </Set></LayerFeature>"#,
            "",
        );
        let pdk = pdk(r#"[[rules.drilling.hole_to_board_edge_clearance]]
id = "npth-edge"
select = { hole = "npth" }
limit = { minimum = "0.3 mm" }"#);
        let report = check(&xml, &pdk, LayoutTarget::Board);

        assert_eq!(report.rules[0].checked, 2);
        assert_eq!(report.findings.len(), 2);
        assert!(
            report
                .findings
                .iter()
                .all(|finding| actual_mm(&finding.measurement) == 0.0)
        );
        assert!(report.findings.iter().all(|finding| matches!(
            finding.sites[0].measurement_kind,
            MeasurementKind::OutsideBoard
        )));
    }

    #[test]
    fn profile_cutouts_are_physical_board_edges() {
        let xml = board(
            r#"<LayerFeature layerRef="DRILL"><Set>
              <Hole name="cutout-edge" diameter="0.4" platingStatus="VIA" x="3.7" y="5"/>
            </Set></LayerFeature>"#,
            r#"<Cutout>
              <PolyBegin x="4" y="4"/><PolyStepSegment x="6" y="4"/>
              <PolyStepSegment x="6" y="6"/><PolyStepSegment x="4" y="6"/>
              <PolyStepSegment x="4" y="4"/>
            </Cutout>"#,
        );
        let pdk = pdk(r#"[[rules.drilling.hole_to_board_edge_clearance]]
id = "via-edge"
select = { hole = "via" }
limit = { minimum = "0.2 mm" }"#);
        let report = check(&xml, &pdk, LayoutTarget::Board);

        assert_eq!(report.findings.len(), 1);
        assert!((actual_mm(&report.findings[0].measurement) - 0.1).abs() < 1e-9);
        let profile = report.findings[0].sites[0]
            .evidence
            .iter()
            .find(|evidence| evidence.role == "board_profile")
            .unwrap();
        assert_eq!(profile.paths.len(), 2, "the cutout ring is retained");
    }

    #[test]
    fn materialized_slots_measure_clearance_and_crossing_as_zero() {
        let xml = board(
            r#"<LayerFeature layerRef="ROUT"><Set>
              <SlotCavity name="pass" platingStatus="PLATED"><Location x="5" y="2"/><Oval width="2" height="0.6"/></SlotCavity>
              <SlotCavity name="fail" platingStatus="PLATED"><Location x="0.8" y="2"/><Oval width="1" height="0.6"/></SlotCavity>
              <SlotCavity name="crossing" platingStatus="NONPLATED"><Location x="0.2" y="5"/><Oval width="1" height="0.6"/></SlotCavity>
            </Set></LayerFeature>"#,
            "",
        );
        let pdk = pdk(r#"[[rules.drilling.slot_to_board_edge_clearance]]
id = "plated-slot-edge"
select = { plating = "plated" }
limit = { minimum = "0.4 mm" }

[[rules.drilling.slot_to_board_edge_clearance]]
id = "nonplated-slot-edge"
select = { plating = "nonplated" }
limit = { minimum = "0.4 mm" }"#);
        let report = check(&xml, &pdk, LayoutTarget::Board);

        assert_eq!(report.rules[0].checked, 2);
        assert_eq!(report.rules[1].checked, 1);
        assert_eq!(report.findings.len(), 2);
        let plated = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "plated-slot-edge")
            .unwrap();
        assert!((actual_mm(&plated.measurement) - 0.3).abs() < 1e-8);
        assert!(matches!(
            plated.sites[0].measurement_kind,
            MeasurementKind::Clearance
        ));
        assert!(plated.sites[0].evidence.iter().any(|evidence| {
            evidence.role == "routed_slot"
                && matches!(evidence.display, Some(EvidenceDisplay::Path { .. }))
        }));
        let nonplated = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "nonplated-slot-edge")
            .unwrap();
        assert_eq!(actual_mm(&nonplated.measurement), 0.0);
        assert!(matches!(
            nonplated.sites[0].measurement_kind,
            MeasurementKind::OutsideBoard
        ));
    }

    #[test]
    fn repeated_features_select_their_own_board_occurrence_not_the_panel() {
        let xml = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/><LayerRef name="DRILL"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
            <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
            <Step name="board" type="BOARD">
              <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/><PolyStepSegment x="10" y="10"/><PolyStepSegment x="0" y="10"/><PolyStepSegment x="0" y="0"/></Polygon></Profile>
              <LayerFeature layerRef="DRILL"><Set><Hole name="edge" diameter="1" platingStatus="PLATED" x="0.7" y="5"/></Set></LayerFeature>
            </Step>
            <Step name="panel" type="PALLET">
              <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="100" y="0"/><PolyStepSegment x="100" y="50"/><PolyStepSegment x="0" y="50"/><PolyStepSegment x="0" y="0"/></Polygon></Profile>
              <StepRepeat stepRef="board" x="10" y="10" nx="2" ny="1" dx="30" dy="0"/>
            </Step>
          </CadData></Ecad>
        </IPC-2581>"#;
        let pdk = pdk(r#"[[rules.drilling.hole_to_board_edge_clearance]]
id = "pth-edge"
select = { hole = "pth" }
limit = { minimum = "0.3 mm" }"#);
        let report = check(xml, &pdk, LayoutTarget::BoardArray);

        assert_eq!(report.rules[0].checked, 2);
        assert_eq!(report.findings.len(), 2);
        let mut instances = std::collections::BTreeSet::new();
        for finding in &report.findings {
            assert!((actual_mm(&finding.measurement) - 0.2).abs() < 1e-8);
            let hole_instance = finding.subjects[0]
                .provenance
                .as_ref()
                .unwrap()
                .instance_index;
            let outline_instance = finding.subjects[1]
                .provenance
                .as_ref()
                .unwrap()
                .instance_index;
            assert_eq!(hole_instance, outline_instance);
            instances.insert(hole_instance.unwrap());
        }
        assert_eq!(instances.len(), 2);
    }
}
