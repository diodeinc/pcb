//! Minimum clearance between electrically distinct final copper conductors.
//!
//! Copper ownership is resolved while ordered artwork is composed. Each
//! owner therefore carries exactly the material that survives polarity,
//! clear features, and final cutouts. Clearance compares connected regions
//! belonging to different owners; self-notches and disconnected islands of
//! one owner are deliberately outside this quantity. Touching, overlapping,
//! or contained distinct-owner regions measure zero.

use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::{region_clearance_sites_with_index, region_clearance_within};

use crate::commands::dfm::design::{ConductorId, Design};
use crate::commands::dfm::report::{Evidence, SourceLocator, Subject};
use crate::commands::dfm::rules::Conditions;

use super::{Evaluation, Measured, linework_clearance, violates};

struct Piece {
    conductor_index: usize,
    region: pcb_ir::geom::ContourSet,
}

pub(super) fn evaluate(
    limit_mm: f64,
    conditions: &Conditions,
    design: &Design,
) -> anyhow::Result<Evaluation> {
    let mut checked = 0;
    let mut measured = Vec::new();

    for layer in &design.copper_layers {
        if !conditions.applies_to_layer(layer) {
            continue;
        }
        let components = layer
            .conductors
            .iter()
            .map(|conductor| conductor.image.connected_components())
            .collect::<Vec<_>>();
        let mut earlier_components = 0;
        for conductor_components in &components {
            checked += earlier_components * conductor_components.len();
            earlier_components += conductor_components.len();
        }
        // Only ties add exemptions. Preserve the linear count for ordinary
        // designs rather than enumerating every pair of nets.
        for (index, conductor) in layer.conductors.iter().enumerate() {
            if !matches!(conductor.id, ConductorId::NetTie { .. }) {
                continue;
            }
            for (other, net) in layer.conductors.iter().enumerate() {
                if matches!(net.id, ConductorId::Net { .. }) && conductor.id.permits_contact(net.id)
                {
                    checked -= components[index].len() * components[other].len();
                }
            }
        }

        let mut pieces = components
            .into_iter()
            .enumerate()
            .flat_map(|(conductor_index, components)| {
                components.into_iter().map(move |region| Piece {
                    conductor_index,
                    region,
                })
            })
            .collect::<Vec<_>>();
        pieces.sort_by(|left, right| {
            left.region
                .bbox
                .min
                .x
                .total_cmp(&right.region.bbox.min.x)
                .then_with(|| left.region.bbox.min.y.total_cmp(&right.region.bbox.min.y))
        });

        let boundaries = pieces
            .iter()
            .map(|piece| piece.region.prepare_query())
            .collect::<Vec<_>>();
        // The pairs the bounds cannot separate, in sweep order along x.
        let pairs = pieces
            .iter()
            .enumerate()
            .flat_map(|(left_index, left)| {
                pieces[left_index + 1..]
                    .iter()
                    .enumerate()
                    .take_while(move |(_, right)| {
                        right.region.bbox.min.x - left.region.bbox.max.x < limit_mm
                    })
                    .filter(move |(_, right)| {
                        !layer.conductors[left.conductor_index]
                            .id
                            .permits_contact(layer.conductors[right.conductor_index].id)
                            && left.region.bbox.distance_to(right.region.bbox) < limit_mm
                    })
                    .map(move |(offset, _)| (left_index, left_index + 1 + offset))
            })
            .collect::<Vec<_>>();

        for (left_index, right_index) in pairs {
            let (left, right) = (&pieces[left_index], &pieces[right_index]);
            let right_boundary = &boundaries[right_index];
            let Some(distance) = region_clearance_within(
                &left.region,
                &boundaries[left_index],
                &right.region,
                right_boundary,
                limit_mm,
            ) else {
                continue;
            };

            let left_id = layer.conductors[left.conductor_index].id;
            let right_id = layer.conductors[right.conductor_index].id;
            let mut bbox = BBox::from_point(distance.first);
            bbox.include_point(distance.second);
            measured.push(Measured {
                distance,
                bbox,
                layers: vec![layer.layer.clone()],
                subjects: vec![
                    conductor_subject(design, left_id, "first_conductor", &layer.layer.name),
                    conductor_subject(design, right_id, "second_conductor", &layer.layer.name),
                ],
                evidence: vec![
                    Evidence::bounds("first_conductor_component", left.region.bbox),
                    Evidence::bounds("second_conductor_component", right.region.bbox),
                ],
                sites: if violates(&distance, limit_mm) {
                    region_clearance_sites_with_index(
                        &left.region,
                        &right.region,
                        right_boundary,
                        limit_mm,
                    )?
                    .into_iter()
                    .map(|site| {
                        linework_clearance::report_site(
                            site,
                            vec![layer.layer.clone()],
                            limit_mm,
                            design.resolution,
                        )
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
                    .into_iter()
                    .collect()
                } else {
                    Vec::new()
                },
            });
        }
    }

    Ok(Evaluation { checked, measured })
}

pub(super) fn conductor_subject(
    design: &Design,
    id: ConductorId,
    role: &'static str,
    layer: &str,
) -> Subject {
    let (kind, name, set_index, feature_index) = match id {
        ConductorId::Net { .. } => ("electrical_net", None, None, None),
        ConductorId::NetTie {
            component,
            first_net,
            second_net,
            ..
        } => (
            "net_tie",
            Some(format!(
                "{} ({} ↔ {})",
                design.imported.resolve(component),
                design.imported.resolve(first_net),
                design.imported.resolve(second_net)
            )),
            None,
            None,
        ),
        ConductorId::Isolated { occurrence, .. } => {
            let source = design
                .imported
                .feature_definition(occurrence.feature)
                .expect("isolated pad must reference its imported definition")
                .source;
            (
                "auxiliary_copper",
                Some("isolated pad".to_owned()),
                Some(source.set_index),
                Some(source.feature_index),
            )
        }
        ConductorId::Auxiliary {
            source_set_index, ..
        } => (
            "auxiliary_copper",
            Some("auxiliary copper".to_owned()),
            Some(source_set_index),
            None,
        ),
        ConductorId::Unattributed {
            source_set_index,
            source_feature_index,
            ..
        } => (
            "unattributed_copper",
            Some("functional copper without net attribution".to_owned()),
            Some(source_set_index),
            Some(source_feature_index),
        ),
    };
    Subject {
        role,
        kind,
        name,
        net: design.resolve(id.net()),
        source: Some(SourceLocator {
            step: design.resolve(id.step()),
            layer: Some(layer.to_owned()),
            set_index,
            feature_index,
            instance_index: id.instance(),
        }),
        provenance: matches!(id, ConductorId::Net { .. }).then(|| SourceLocator {
            step: design.resolve(id.step()),
            layer: Some(layer.to_owned()),
            set_index: None,
            feature_index: None,
            instance_index: id.instance(),
        }),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
    use pcb_ir::geom::Resolution;
    use std::collections::BTreeSet;

    use chrono::NaiveDate;
    use pcb_ir::dialects::ipc::ArtworkScope;

    use crate::commands::dfm::{checks, design::Design, pdk::Pdk, rules};
    use crate::ipc2581::Ipc2581;

    const PDK: &str = r#"schema_version = 2
default_profile = "test"

[pdk]
id = "clearance-test"
name = "Clearance test"
revision = "1"

[profiles.test]
name = "Test"

[[rules.copper.clearance]]
id = "copper-clearance"
limit = { minimum = "0.15 mm" }
"#;

    const BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="0.1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="pad"/></PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="2" y="0"/>
            <PolyStepSegment x="2" y="2"/>
            <PolyStepSegment x="1.05" y="2"/>
            <PolyStepSegment x="1.05" y="0.5"/>
            <PolyStepSegment x="0.95" y="0.5"/>
            <PolyStepSegment x="0.95" y="2"/>
            <PolyStepSegment x="0" y="2"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
          <Set net="N2"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="4" y="0"/><PolyStepSegment x="5" y="0"/>
            <PolyStepSegment x="5" y="1"/><PolyStepSegment x="4" y="1"/>
            <PolyStepSegment x="4" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
          <Set net="N3"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="5.05" y="0"/><PolyStepSegment x="6.05" y="0"/>
            <PolyStepSegment x="6.05" y="1"/><PolyStepSegment x="5.05" y="1"/>
            <PolyStepSegment x="5.05" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
          <Set net="N4"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="8" y="0"/><PolyStepSegment x="9" y="0"/>
            <PolyStepSegment x="9" y="1"/><PolyStepSegment x="8" y="1"/>
            <PolyStepSegment x="8" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
          <Set net="N5"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="8.5" y="0"/><PolyStepSegment x="9.5" y="0"/>
            <PolyStepSegment x="9.5" y="1"/><PolyStepSegment x="8.5" y="1"/>
            <PolyStepSegment x="8.5" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    fn run(xml: &str) -> checks::Results {
        let ipc = Ipc2581::parse(xml).unwrap();
        let pdk = Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap();
        let design = Design::extract(
            &imported,
            ArtworkScope::Board,
            &rules,
            Resolution::default(),
        )
        .unwrap();
        checks::run(
            &rules,
            &design,
            None,
            NaiveDate::from_ymd_opt(2026, 8, 25).unwrap(),
        )
        .unwrap()
    }

    const ANTENNA: &str = include_str!("../fixtures/mockingbird-antenna.xml");
    const TIE: &str = r#"<NetShort><NetRef name="GND"/><NetRef name="WIFI.RF_ANT"/><Location x="168.9" y="-98.339392"/><LayerRef name="F.Cu"/></NetShort>"#;

    fn annotated_antenna() -> String {
        let set = r#"<Set geometryUsage="GRAPHIC" componentRef="E1">"#;
        ANTENNA.replacen(set, &format!("{set}{TIE}"), 1)
    }

    fn antenna_check(xml: &str, pdk_name: &str) -> anyhow::Result<crate::commands::dfm::DfmReport> {
        use crate::commands::dfm::{self, CheckRequest, PdkSource, report::FileIdentity};
        let ipc = Ipc2581::parse(xml)?;
        let imported = pcb_ir::import::ipc2581::import_design(&ipc)?;
        dfm::check(
            &imported,
            CheckRequest {
                input: FileIdentity {
                    path: "mockingbird-antenna.xml".to_owned(),
                    sha256: dfm::sha256(xml.as_bytes()),
                    size_bytes: xml.len() as u64,
                },
                pdk: PdkSource::Builtin(pdk_name),
                waivers: None,
                layout_target: crate::LayoutTarget::Board,
                generated_at: "2026-09-06T00:00:00Z".parse().unwrap(),
            },
        )
    }

    #[test]
    fn antenna_requires_explicit_intent_for_both_pdks() {
        for pdk in ["standard", "jlcpcb-1oz"] {
            let error = antenna_check(ANTENNA, pdk).err().unwrap().to_string();
            for detail in [
                "F.Cu",
                "antenna",
                "component 'E1'",
                "Set 0",
                "feature 0",
                "NetShort",
            ] {
                assert!(error.contains(detail), "missing {detail}: {error}");
            }
            let results = antenna_check(&annotated_antenna(), pdk).unwrap();
            assert!(
                results.findings.iter().all(|finding| !finding
                    .subjects
                    .iter()
                    .any(|subject| subject.kind == "net_tie")),
                "{pdk}: {:?}",
                results.findings
            );
            // Complete is not synonymous with passing: the faithful 0.5 mm
            // ground pad around a 0.3 mm drill still has a 0.1 mm annular ring.
            assert!(
                results
                    .findings
                    .iter()
                    .any(|finding| finding.rule_id.contains("pth_annular_ring")
                        && finding
                            .measurement
                            .actual_mm()
                            .is_some_and(|mm| (mm - 0.1).abs() < 1e-6)),
                "{pdk}: missing real annular-ring violation"
            );
        }
    }

    #[test]
    fn antenna_tie_does_not_merge_nets_or_hide_third_net_clearance() {
        // The same two nets short elsewhere; a third-net pad overlaps the
        // radiator, far from either antenna pad. Both must still be reported.
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu">
            <Set net="WIFI.RF_ANT"><Pad padstackDefRef="PADSTACK_1"><Location x="162" y="-110"/><StandardPrimitiveRef id="RECT_1"/></Pad></Set>
            <Set net="GND"><Pad padstackDefRef="PADSTACK_1"><Location x="162" y="-110"/><StandardPrimitiveRef id="RECT_1"/></Pad></Set>
            <Set net="UNRELATED"><Pad padstackDefRef="PADSTACK_1"><Location x="173.8" y="-100.439392"/><StandardPrimitiveRef id="RECT_1"/></Pad></Set>
          </LayerFeature>
        </Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let results = antenna_check(&xml, pdk).unwrap();
            assert!(
                results.findings.iter().any(|finding| finding
                    .subjects
                    .iter()
                    .any(|s| s.net.as_deref() == Some("WIFI.RF_ANT"))
                    && finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("GND"))
                    && finding.measurement.actual_mm() == Some(0.0)),
                "{pdk}: missing remote short"
            );
            assert!(
                results.findings.iter().any(|finding| finding
                    .subjects
                    .iter()
                    .any(|s| s.kind == "net_tie")
                    && finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("UNRELATED"))
                    && finding.measurement.actual_mm() == Some(0.0)),
                "{pdk}: missing third-net short"
            );
        }
    }

    #[test]
    fn antenna_declaration_is_strict_and_component_scoped() {
        let annotated = annotated_antenna();
        for xml in [
            annotated.replace(r#"<NetRef name="GND"/>"#, r#"<NetRef name="UNKNOWN"/>"#),
            annotated.replace(r#"<NetRef name="GND"/>"#, r#"<NetRef name="WIFI.RF_ANT"/>"#),
            annotated.replace("</NetShort>", r#"<NetRef name="THIRD"/></NetShort>"#),
            annotated.replace(r#"<Location x="168.9" y="-98.339392"/>"#, ""),
            annotated.replace(
                r#"<LayerRef name="F.Cu"/></NetShort>"#,
                r#"<LayerRef name="B.Cu"/></NetShort>"#,
            ),
            annotated.replace(TIE, &format!("{TIE}{TIE}")),
            annotated.replace("net=\"GND\"", "net=\"WIFI.RF_ANT\""),
            annotated.replace("net=\"GND\"", ""),
        ] {
            let error = antenna_check(&xml, "standard").err().unwrap().to_string();
            assert!(error.contains("NetShort"), "{error}");
        }
        for graphic in [
            r#"<Set geometryUsage="GRAPHIC">"#,
            r#"<Set geometryUsage="GRAPHIC" componentRef="E2">"#,
            r#"<Set geometryUsage="TEXT" componentRef="E1">"#,
        ] {
            let xml = annotated.replacen(
                r#"<Set geometryUsage="GRAPHIC" componentRef="E1">"#,
                graphic,
                1,
            );
            let error = antenna_check(&xml, "standard").err().unwrap().to_string();
            assert!(error.contains("NetShort"), "{error}");
        }
        // Source-local Set indices repeat across LayerFeature blocks. A NetShort
        // on one Set must not authorize unrelated copper with the same refdes.
        let xml = annotated.replace(
            "</Step>",
            r#"<LayerFeature layerRef="F.Cu">
          <Set geometryUsage="GRAPHIC" componentRef="E1"><Features><Location x="150" y="-100"/>
            <UserPrimitiveRef id="UPOLY_1"/></Features></Set></LayerFeature></Step>"#,
        );
        assert!(
            antenna_check(&xml, "standard")
                .err()
                .unwrap()
                .to_string()
                .contains("final functional copper without net attribution")
        );
        let xml = xml.replace(
            r#"componentRef="E1"><Features><Location x="150""#,
            r#"componentRef="E2"><Features><Location x="150""#,
        );
        let error = antenna_check(&xml, "standard").err().unwrap().to_string();
        assert!(error.contains("component 'E2'"), "{error}");
    }

    #[test]
    fn antenna_bridge_still_requires_third_net_spacing_and_drill_clearance() {
        // A 0.5 mm pad is 0.05 mm from the end of a 0.5 mm radiator arm.
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="UNRELATED">
            <Pad padstackDefRef="PADSTACK_1"><Location x="169.31" y="-112.039392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
          </Set></LayerFeature>
          <LayerFeature layerRef="F.Cu_B.Cu"><Set net="GND">
            <Hole name="UNRELATED_DRILL" diameter="0.30" platingStatus="PLATED" plusTol="0" minusTol="0" x="173.8" y="-100.439392"/>
          </Set></LayerFeature>
        </Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report.findings.iter().any(|finding| finding
                    .subjects
                    .iter()
                    .any(|s| s.kind == "net_tie")
                    && finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("UNRELATED"))
                    && finding
                        .measurement
                        .actual_mm()
                        .is_some_and(|mm| (mm - 0.05).abs() < 1e-6)),
                "{pdk}: missing 0.05 mm gap"
            );
        }
        // The IPC profile, unlike standard/JLC, also enables hole-to-copper
        // clearance. Only the added drill, not E1's ground pad, may hit the tie.
        let report = antenna_check(&xml, "ipc").unwrap();
        assert_eq!(
            report
                .findings
                .iter()
                .filter(
                    |finding| finding.subjects.iter().any(|s| s.kind == "net_tie")
                        && finding.subjects.iter().any(|s| s.kind == "plated_hole")
                )
                .count(),
            1
        );
        let clean = antenna_check(&annotated_antenna(), "ipc").unwrap();
        assert!(
            clean
                .findings
                .iter()
                .all(|finding| !finding.subjects.iter().any(|s| s.kind == "net_tie"))
        );
    }

    #[test]
    fn antenna_net_tie_permissions_do_not_leak_between_layout_occurrences() {
        use crate::commands::dfm::design::ConductorId;
        let xml = annotated_antenna()
            .replacen(
                r#"<StepRef name="antenna"/>"#,
                r#"<StepRef name="panel"/>"#,
                1,
            )
            .replace(
                "</CadData>",
                r#"<Step name="panel" type="PALLET">
              <StepRepeat stepRef="antenna" x="0" y="0" nx="2" ny="1" dx="40" dy="0"/>
            </Step></CadData>"#,
            );
        let ipc = Ipc2581::parse(&xml).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let rules = rules::lower(&Pdk::parse(PDK).unwrap(), None).unwrap();
        let design = Design::extract(&imported, ArtworkScope::ArrayFlattened, &rules).unwrap();
        let conductors = &design
            .copper_layers
            .iter()
            .find(|layer| layer.layer.name == "F.Cu")
            .unwrap()
            .conductors;
        let ties = conductors
            .iter()
            .filter(|c| matches!(c.id, ConductorId::NetTie { .. }))
            .collect::<Vec<_>>();
        assert_eq!(ties.len(), 2);
        assert!(!ties[0].id.permits_contact(ties[1].id));
        for tie in ties {
            for net in conductors
                .iter()
                .filter(|c| matches!(c.id, ConductorId::Net { .. }))
            {
                assert_eq!(
                    tie.id.permits_contact(net.id),
                    tie.id.instance() == net.id.instance()
                );
            }
        }
    }

    #[test]
    fn ignores_same_net_notches_but_reports_distinct_gaps_and_overlaps() {
        let results = run(BOARD);

        assert_eq!(results.findings.len(), 2);
        let pairs = results
            .findings
            .iter()
            .map(|finding| {
                let mut pair = finding
                    .subjects
                    .iter()
                    .map(|subject| subject.net.clone().unwrap())
                    .collect::<Vec<_>>();
                pair.sort();
                pair
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            pairs,
            BTreeSet::from([
                vec!["N2".to_owned(), "N3".to_owned()],
                vec!["N4".to_owned(), "N5".to_owned()],
            ])
        );
        assert!(results.findings.iter().all(|finding| {
            finding
                .subjects
                .iter()
                .all(|subject| subject.net.as_deref() != Some("N1"))
        }));
        let actual = results
            .findings
            .iter()
            .map(|finding| finding.measurement.actual_mm().unwrap())
            .collect::<Vec<_>>();
        assert!(actual.iter().any(|value| value.abs() < 1e-9));
        assert!(actual.iter().any(|value| (value - 0.05).abs() < 1e-9));
    }

    #[test]
    fn rejects_surviving_functional_copper_without_net_ownership() {
        let resolution = Resolution::default();

        let xml = BOARD.replace("<Set net=\"N2\">", "<Set>");
        let ipc = Ipc2581::parse(&xml).unwrap();
        let pdk = Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();

        let error = Design::extract(&imported, ArtworkScope::Board, &rules, resolution)
            .err()
            .expect("unattributed copper must fail closed");
        assert!(
            error
                .to_string()
                .contains("final functional copper without net attribution"),
            "{error:#}"
        );
    }

    #[test]
    fn checks_netless_pads_as_isolated_copper() {
        let xml = BOARD.replace(
            "</Step>",
            r#"<LayerFeature layerRef="TOP"><Set>
          <Pad padstackDefRef="padstack"><Location x="3.85" y="0.5"/><PinRef componentRef="FID1" pin="PAD0"/></Pad>
        </Set></LayerFeature>
        <LayerFeature layerRef="TOP"><Set>
          <Pad padstackDefRef="padstack"><Location x="3.65" y="0.5"/><PinRef componentRef="FID2" pin="PAD0"/></Pad>
        </Set></LayerFeature>
      </Step>"#,
        );

        let results = run(&xml);

        assert_eq!(results.findings.len(), 4);
        assert_eq!(
            results
                .findings
                .iter()
                .filter(|finding| {
                    finding
                        .subjects
                        .iter()
                        .any(|subject| subject.name.as_deref() == Some("isolated pad"))
                })
                .count(),
            2
        );
    }
}
