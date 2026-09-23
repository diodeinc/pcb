//! Minimum clearance between electrically distinct final copper conductors.
//!
//! Copper ownership is resolved while ordered artwork is composed. Each
//! owner therefore carries exactly the material that survives polarity,
//! clear features, and final cutouts. Clearance compares connected regions
//! belonging to different owners; self-notches and disconnected islands of
//! one owner are deliberately outside this quantity. Touching, overlapping,
//! or contained distinct-owner regions measure zero.

#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use pcb_ir::dialects::ipc::FeatureKind;
use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::{region_clearance_sites_with_index, region_clearance_within};
use std::collections::HashSet;

use crate::commands::dfm::design::{
    ConductorId, CopperLayer, Design, NET_SHORT_LOCATION_TOLERANCE_MM, component_copper, spans,
};
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
    let measure = |layer: &CopperLayer| {
        let conductors = &layer.conductors;
        // Like KiCad, permit a tied graphic to meet the group's nets, but
        // never merge those nets. Pad-only ties permit just the pad pair.
        let mut permissions = HashSet::new();
        let mut pad_pairs = HashSet::new();
        for short in &layer.net_shorts {
            let objects = short.nets.map(|net| {
                conductors
                    .iter()
                    .filter_map(|conductor| {
                        let ConductorId::Net {
                            object: Some(object),
                            ..
                        } = conductor.id
                        else {
                            return None;
                        };
                        if conductor.id.electrical() != net {
                            return None;
                        }
                        let distance = conductor
                            .image
                            .prepare_query()
                            .signed_distance(short.location)?;
                        if distance.mm
                            > conductor.image.uncertainty_mm + NET_SHORT_LOCATION_TOLERANCE_MM
                        {
                            return None;
                        }
                        let feature = design.imported.feature_definition(object.feature)?;
                        Some((
                            conductor.id,
                            component_copper(&design.imported.geometry, feature)?,
                            feature.kind,
                        ))
                    })
                    .collect::<Vec<_>>()
            });
            let mut matched = false;
            for &(first, component, first_kind) in &objects[0] {
                for &(second, other_component, second_kind) in &objects[1] {
                    if component != other_component {
                        continue;
                    }
                    match (
                        first_kind == FeatureKind::Padstack,
                        second_kind == FeatureKind::Padstack,
                    ) {
                        (false, true) => {
                            permissions.insert((first, second.electrical()));
                        }
                        (true, false) => {
                            permissions.insert((second, first.electrical()));
                        }
                        (true, true) => {
                            pad_pairs.insert((first, second));
                        }
                        (false, false) => continue,
                    }
                    matched = true;
                }
            }
            anyhow::ensure!(
                matched,
                "{} has no matching component graphic/pad or pad/pad contact",
                short.description
            );
        }
        let permitted = |first: ConductorId, second: ConductorId| {
            first.electrical() == second.electrical()
                || permissions.contains(&(first, second.electrical()))
                || permissions.contains(&(second, first.electrical()))
                || pad_pairs.contains(&(first, second))
                || pad_pairs.contains(&(second, first))
        };
        // A conductor whose bounds come within the limit of no other's is
        // proven clear whole; only the rest are taken apart into pieces.
        let mut by_x = (0..conductors.len()).collect::<Vec<_>>();
        by_x.sort_by(|&left, &right| {
            let bounds = |index: usize| conductors[index].image.bbox;
            bounds(left).min.x.total_cmp(&bounds(right).min.x)
        });
        let mut near = vec![false; conductors.len()];
        for (position, &left_index) in by_x.iter().enumerate() {
            let left = &conductors[left_index];
            for &right_index in by_x[position + 1..].iter().take_while(|&&right_index| {
                conductors[right_index].image.bbox.min.x - left.image.bbox.max.x < limit_mm
            }) {
                let right = &conductors[right_index];
                if spans(left.branch, right.branch)
                    && left.image.bbox.distance_to(right.image.bbox) < limit_mm
                {
                    near[left_index] = true;
                    near[right_index] = true;
                }
            }
        }
        let components = conductors
            .iter()
            .zip(near)
            .map(|(conductor, near)| match near {
                true => conductor.image.connected_components(),
                false => Vec::new(),
            })
            .collect::<Vec<_>>();
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
                        !permitted(
                            conductors[left.conductor_index].id,
                            conductors[right.conductor_index].id,
                        ) && spans(
                            conductors[left.conductor_index].branch,
                            conductors[right.conductor_index].branch,
                        ) && left.region.bbox.distance_to(right.region.bbox) < limit_mm
                    })
                    .map(move |(offset, _)| (left_index, left_index + 1 + offset))
            })
            .collect::<Vec<_>>();

        let measure_pair = |(left_index, right_index): (usize, usize)| {
            let (left, right) = (&pieces[left_index], &pieces[right_index]);
            let right_boundary = &boundaries[right_index];
            let Some(distance) = region_clearance_within(
                &left.region,
                &boundaries[left_index],
                &right.region,
                right_boundary,
                limit_mm,
            ) else {
                return Ok(None);
            };

            let left_id = conductors[left.conductor_index].id;
            let right_id = conductors[right.conductor_index].id;
            // A route may enter a tied pad where that pad meets its partner.
            // This is a pad-position exception, not permission for the route's net.
            let enters_pad = |target, incoming: ConductorId, point| {
                pad_pairs.iter().any(|&(first, second)| {
                    let entry = if first == target {
                        second
                    } else if second == target {
                        first
                    } else {
                        return false;
                    };
                    entry.electrical() == incoming.electrical()
                        && conductors
                            .iter()
                            .find(|conductor| conductor.id == entry)
                            .and_then(|conductor| {
                                conductor.image.prepare_query().signed_distance(point).map(
                                    |distance| {
                                        distance.mm
                                            <= conductor.image.uncertainty_mm
                                                + NET_SHORT_LOCATION_TOLERANCE_MM
                                    },
                                )
                            })
                            .unwrap_or(false)
                })
            };
            if enters_pad(left_id, right_id, distance.first)
                || enters_pad(right_id, left_id, distance.second)
            {
                return Ok(None);
            }
            Ok::<_, anyhow::Error>(Some(Measured {
                distance,
                bbox: BBox::spanning(distance.first, distance.second),
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
                    let sites = region_clearance_sites_with_index(
                        &left.region,
                        &right.region,
                        right_boundary,
                        limit_mm,
                    )?;
                    linework_clearance::report_sites(
                        sites,
                        std::slice::from_ref(&layer.layer),
                        limit_mm,
                        design.resolution,
                    )?
                } else {
                    Vec::new()
                },
            }))
        };
        #[cfg(not(target_family = "wasm"))]
        let pairs = pairs.into_par_iter();
        #[cfg(target_family = "wasm")]
        let pairs = pairs.into_iter();
        pairs.map(measure_pair).collect::<anyhow::Result<Vec<_>>>()
    };

    // Layers are independent, and so are the pairs on one.
    #[cfg(not(target_family = "wasm"))]
    let layers = design.copper_layers.par_iter();
    #[cfg(target_family = "wasm")]
    let layers = design.copper_layers.iter();
    let measured = layers
        .filter(|layer| conditions.applies_to_layer(layer))
        .map(measure)
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    Ok(Evaluation {
        // Every pair of connected pieces of two conductors is decided, those
        // inside one placement in that placement's own design.
        checked: design
            .copper_layers
            .iter()
            .filter(|layer| conditions.applies_to_layer(layer))
            .map(|layer| layer.piece_pairs)
            .sum(),
        measured,
    })
}

pub(super) fn conductor_subject(
    design: &Design,
    id: ConductorId,
    role: &'static str,
    layer: &str,
) -> Subject {
    let (kind, name, set_index, feature_index) = match id {
        ConductorId::Net {
            object: Some(object),
            ..
        } => {
            let source = design
                .imported
                .feature_definition(object.feature)
                .expect("net-tie object must reference its imported definition")
                .source;
            (
                "electrical_net",
                None,
                Some(source.set_index),
                Some(source.feature_index),
            )
        }
        ConductorId::Net { .. } => ("electrical_net", None, None, None),
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
            set_index,
            feature_index,
            instance_index: id.instance(),
        }),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::commands::dfm::{checks, design::Design, fixtures, report::RuleStatus};
    use ipc2581::Ipc2581;
    use pcb_ir::dialects::ipc::ArtworkScope;
    use pcb_ir::geom::Resolution;

    const BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="0.1"/></EntryStandard>
      <EntryStandard id="tie-pad"><RectCenter width="1" height="1"/></EntryStandard>
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
          <Set net="N4" componentRef="NT1"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="8" y="0"/><PolyStepSegment x="9" y="0"/>
            <PolyStepSegment x="9" y="1"/><PolyStepSegment x="8" y="1"/>
            <PolyStepSegment x="8" y="0"/>
          </Polygon></Contour></UserSpecial></Features></Set>
          <Set net="N4"><Pad padstackDefRef="padstack">
            <Location x="7.75" y="0.5"/><StandardPrimitiveRef id="tie-pad"/>
            <PinRef componentRef="NT1" pin="1"/>
          </Pad></Set>
          <Set net="N5"><Pad padstackDefRef="padstack">
            <Location x="9" y="0.5"/><StandardPrimitiveRef id="tie-pad"/>
            <PinRef componentRef="NT1" pin="2"/>
          </Pad></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    fn run(xml: &str) -> checks::Results {
        let rule = "[[rules.copper.clearance]]\nid = \"copper-clearance\"\nlimit = { minimum = \"0.15 mm\" }";
        fixtures::run_board(xml, &fixtures::pdk(rule))
    }

    #[test]
    fn graphic_permission_does_not_spread_to_other_objects_in_the_same_set() {
        let declared = BOARD.replace(
            "</Step>",
            r#"
          <LayerFeature layerRef="TOP"><Set><NetShort>
            <NetRef name="N4"/><NetRef name="N5"/>
            <Location x="8.75" y="0.5"/><LayerRef name="TOP"/>
          </NetShort></Set></LayerFeature></Step>"#,
        );
        // This graphic touches the first graphic but is 0.05 mm from the pad.
        // Neither shared Set/component/net nor copper continuity grants permission.
        let extra = r#"<Features><UserSpecial><Contour><Polygon>
          <PolyBegin x="8.2" y="0.75"/><PolyStepSegment x="8.45" y="0.75"/>
          <PolyStepSegment x="8.45" y="1.5"/><PolyStepSegment x="8.2" y="1.5"/>
          <PolyStepSegment x="8.2" y="0.75"/>
        </Polygon></Contour></UserSpecial></Features>"#;
        let xml = declared.replace(
            r#"<Set net="N4" componentRef="NT1">"#,
            &format!(r#"<Set net="N4" componentRef="NT1">{extra}"#),
        );
        let plain = run(&declared);
        assert_eq!(plain.findings.len(), 1, "only the N2/N3 gap remains");
        let results = run(&xml);
        assert_eq!(results.findings.len(), 2);
        assert!(results.findings.iter().any(|finding| {
            finding
                .subjects
                .iter()
                .any(|s| s.net.as_deref() == Some("N4"))
                && (finding.measurement.actual_mm().unwrap() - 0.05).abs() < 1e-6
        }));
    }

    #[test]
    fn pad_tie_allows_pad_entry_but_not_remote_shorts_on_the_same_nets() {
        let pad = r#"<Set net="A"><Pad padstackDefRef="padstack">
          <Location x="12" y="0.5"/><StandardPrimitiveRef id="tie-pad"/>
          <PinRef componentRef="NT2" pin="1"/></Pad></Set>"#;
        let other = pad
            .replace("net=\"A\"", "net=\"B\"")
            .replace("pin=\"1\"", "pin=\"2\"")
            .replace("x=\"12\"", "x=\"12.4\"");
        let tie = r#"<Set><NetShort><NetRef name="A"/><NetRef name="B"/>
          <Location x="12.2" y="0.5"/><LayerRef name="TOP"/></NetShort></Set>"#;
        let board = BOARD.replace(
            "</Step>",
            &format!(
                r#"
          <LayerFeature layerRef="TOP">{pad}{other}{tie}</LayerFeature></Step>"#
            ),
        );
        assert_eq!(
            run(&board).findings.len(),
            2,
            "only the original violations remain"
        );
        // Entry through the tied pad is allowed, as in KiCad.
        let track = r#"<Set net="A"><Features><UserSpecial>
          <Line startX="12" startY="0.5" endX="11" endY="0.5"><LineDesc lineWidth="0.2" lineEnd="ROUND"/></Line>
        </UserSpecial></Features></Set>"#;
        let board = board.replace(
            "</Step>",
            &format!(
                r#"
          <LayerFeature layerRef="TOP">{track}</LayerFeature></Step>"#
            ),
        );
        assert_eq!(run(&board).findings.len(), 2);
        let remote = other.replace("x=\"12.4\"", "x=\"10.75\"");
        let board = board.replace(
            "</Step>",
            &format!(
                r#"
          <LayerFeature layerRef="TOP">{remote}</LayerFeature></Step>"#
            ),
        );
        let results = run(&board);
        assert_eq!(results.findings.len(), 3);
        assert!(
            results.findings.iter().any(|f| f
                .subjects
                .iter()
                .any(|s| s.net.as_deref() == Some("B"))
                && f.measurement.actual_mm() == Some(0.0))
        );
    }

    const ANTENNA: &str = include_str!("../fixtures/antenna.xml");
    const TIE: &str = r#"<NetShort><NetRef name="GND"/><NetRef name="WIFI.RF_ANT"/><Location x="168.9" y="-100.439392"/><LayerRef name="F.Cu"/></NetShort>"#;

    fn annotated_antenna() -> String {
        let set = r#"<Set geometryUsage="GRAPHIC" componentRef="E1">"#;
        ANTENNA.replacen(
            set,
            &format!(r#"<Set net="GND" geometryUsage="GRAPHIC" componentRef="E1">{TIE}"#),
            1,
        )
    }

    fn antenna_check(xml: &str, pdk_name: &str) -> anyhow::Result<crate::commands::dfm::DfmReport> {
        use crate::commands::dfm::{self, CheckRequest, PdkSource, report::FileIdentity};
        let ipc = Ipc2581::parse(xml)?;
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default())?;
        let report = dfm::check(
            &imported,
            CheckRequest {
                input: FileIdentity::new("antenna.xml", xml.as_bytes()),
                pdk: PdkSource::Builtin(pdk_name),
                waivers: None,
                layout_target: crate::LayoutTarget::Board,
                generated_at: "2026-09-06T00:00:00Z".parse().unwrap(),
            },
            Resolution::default(),
        )?;
        if let Some(rule) = report
            .rules
            .iter()
            .find(|rule| rule.status == RuleStatus::Incomplete)
        {
            anyhow::bail!("{}", rule.skip_reason.as_deref().unwrap());
        }
        Ok(report)
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
                results
                    .findings
                    .iter()
                    .all(|finding| !finding.subjects.iter().any(|s| s.role == "first_conductor")),
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
            let third_net_short = results
                .findings
                .iter()
                .find(|finding| {
                    finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("GND"))
                        && finding
                            .subjects
                            .iter()
                            .any(|s| s.net.as_deref() == Some("UNRELATED"))
                        && finding.measurement.actual_mm() == Some(0.0)
                })
                .expect("missing third-net short");
            let graphic = third_net_short
                .subjects
                .iter()
                .find(|s| s.net.as_deref() == Some("GND"))
                .unwrap();
            for locator in [graphic.source.as_ref(), graphic.provenance.as_ref()] {
                let locator = locator.unwrap();
                assert_eq!(locator.step.as_deref(), Some("antenna"));
                assert_eq!(locator.layer.as_deref(), Some("F.Cu"));
                assert_eq!(locator.set_index, Some(0));
                assert_eq!(locator.feature_index, Some(0));
            }
        }
    }

    #[test]
    fn antenna_declaration_requires_an_actual_contact_not_component_metadata() {
        let annotated = annotated_antenna();
        for xml in [
            annotated.replace(r#"<NetRef name="GND"/>"#, r#"<NetRef name="UNKNOWN"/>"#),
            annotated.replace(r#"<NetRef name="GND"/>"#, r#"<NetRef name="WIFI.RF_ANT"/>"#),
            annotated.replace("</NetShort>", r#"<NetRef name="THIRD"/></NetShort>"#),
            annotated.replace(r#"<Location x="168.9" y="-100.439392"/>"#, ""),
            annotated.replace(TIE, &TIE.replace("168.9", "150")),
            annotated.replace(
                r#"<LayerRef name="F.Cu"/></NetShort>"#,
                r#"<LayerRef name="B.Cu"/></NetShort>"#,
            ),
            annotated.replace("net=\"GND\"", "net=\"WIFI.RF_ANT\""),
            annotated.replace("net=\"GND\"", ""),
            annotated.replace(r#" componentRef="E1""#, ""),
            annotated.replace(
                r#"<PinRef componentRef="E1" pin="1""#,
                r#"<PinRef componentRef="OTHER" pin="1""#,
            ),
        ] {
            let error = antenna_check(&xml, "standard").err().unwrap().to_string();
            assert!(error.contains("NetShort"), "{error}");
        }
        for xml in [
            annotated.replace("</NetShort>", r#"<NetRef name="THIRD"/></NetShort>"#),
            annotated.replace(TIE, &TIE.replace("168.9", "150")),
        ] {
            let xml = xml.replace("<NetShort>", r#"<NetShort id="tie-1">"#);
            let error = antenna_check(&xml, "standard").unwrap_err().to_string();
            for detail in [
                "tie-1",
                "GND",
                "WIFI.RF_ANT",
                "-100.439392",
                "antenna",
                "F.Cu",
            ] {
                assert!(error.contains(detail), "missing {detail}: {error}");
            }
        }
        for xml in [
            annotated.replace(TIE, &format!("{TIE}{TIE}")),
            annotated.replace(TIE, "").replace(
                "</Step>",
                &format!(r#"<LayerFeature layerRef="F.Cu"><Set>{TIE}</Set></LayerFeature></Step>"#),
            ),
        ] {
            // NetShort need not live on the graphic's Set.
            antenna_check(&xml, "standard").unwrap();
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
    fn net_tie_import_warnings_are_scoped_to_the_checked_step_and_layer() {
        let board = BOARD.replace(
            "</Step>",
            r#"
          <LayerFeature layerRef="TOP"><Set><NetShort>
            <NetRef name="N4"/><NetRef name="N5"/>
            <Location x="8.75" y="0.5"/><LayerRef name="TOP"/>
          </NetShort></Set></LayerFeature></Step>"#,
        );
        let pdk = fixtures::pdk(
            r#"
[[rules.copper.clearance]]
id = "clearance"
limit = { minimum = "0.15 mm" }
[[rules.copper.feature_width]]
id = "width"
limit = { minimum = "0.15 mm" }
"#,
        );
        let missing = r#"<Set><Features><StandardPrimitiveRef id="absent"/></Features></Set>"#;
        let legend = board
            .replace(
                "<CadData>",
                r#"<CadData><Layer name="LEGEND" layerFunction="SILKSCREEN" side="TOP"/>"#,
            )
            .replace(
                "</Step>",
                &format!(r#"<LayerFeature layerRef="LEGEND">{missing}</LayerFeature></Step>"#,),
            );
        let other_step = board.replace("</CadData>", &format!(
            r#"<Step name="unused"><LayerFeature layerRef="TOP">{missing}</LayerFeature></Step></CadData>"#,
        ));
        let missing_copper = board.replace(
            "</Step>",
            &format!(r#"<LayerFeature layerRef="TOP">{missing}</LayerFeature></Step>"#,),
        );
        let patterned_copper = board.replace(
            r#"<RectCenter width="1" height="1"/>"#,
            r#"<RectCenter width="1" height="1"><FillDesc fillProperty="HATCH"/></RectCenter>"#,
        );
        for (xml, status) in [
            (legend, RuleStatus::Fail),
            (other_step, RuleStatus::Fail),
            (missing_copper, RuleStatus::Incomplete),
            (patterned_copper, RuleStatus::Incomplete),
        ] {
            assert!(!fixtures::import(&xml).geometry.diagnostics.is_empty());
            let report = fixtures::report(&xml, &pdk, crate::LayoutTarget::Board);
            let clearance = report
                .rules
                .iter()
                .find(|rule| rule.id == "clearance")
                .unwrap();
            assert_eq!(clearance.status, status, "{clearance:?}");
            let width = report.rules.iter().find(|rule| rule.id == "width").unwrap();
            assert_eq!(width.status, RuleStatus::Pass);
            if status == RuleStatus::Fail {
                assert_eq!(
                    report.findings.len(),
                    1,
                    "only the unrelated N2/N3 gap remains"
                );
            }
        }
    }

    #[test]
    fn parent_net_tie_respects_placed_clear_copper() {
        let pdk = fixtures::pdk(
            "[[rules.copper.clearance]]\nid = \"clearance\"\nlimit = { minimum = \"0.15 mm\" }",
        );
        let xml = BOARD
            .replace(r#"<StepRef name="board"/>"#, r#"<StepRef name="panel"/>"#)
            .replace(
                r#"<Step name="board" type="BOARD">"#,
                r#"<Step name="panel" type="PALLET">
                <StepRepeat stepRef="clear-child" x="0" y="0" nx="1" ny="1" dx="0" dy="0"/>"#,
            )
            .replace(
                "</Step>",
                r#"
              <LayerFeature layerRef="TOP"><Set><NetShort>
                <NetRef name="N4"/><NetRef name="N5"/>
                <Location x="8.75" y="0.5"/><LayerRef name="TOP"/>
              </NetShort></Set></LayerFeature></Step>"#,
            )
            .replace(
                "</CadData>",
                r#"
              <Step name="clear-child" type="BOARD"><LayerFeature layerRef="TOP">
                <Set polarity="NEGATIVE"><Features><UserSpecial><Contour><Polygon>
                  <PolyBegin x="8.65" y="0"/><PolyStepSegment x="8.85" y="0"/>
                  <PolyStepSegment x="8.85" y="1"/><PolyStepSegment x="8.65" y="1"/>
                  <PolyStepSegment x="8.65" y="0"/>
                </Polygon></Contour></UserSpecial></Features></Set>
              </LayerFeature></Step></CadData>"#,
            );
        for (xml, expected) in [
            (xml.clone(), RuleStatus::Incomplete),
            (
                xml.replace(
                    r#"stepRef="clear-child" x="0""#,
                    r#"stepRef="clear-child" x="0.9""#,
                ),
                RuleStatus::Fail,
            ),
        ] {
            let report = fixtures::report(&xml, &pdk, crate::LayoutTarget::BoardArray);
            assert_eq!(report.rules[0].status, expected, "{:?}", report.rules[0]);
            if expected == RuleStatus::Incomplete {
                assert!(
                    report.rules[0]
                        .skip_reason
                        .as_deref()
                        .unwrap()
                        .contains("contact")
                );
            } else {
                assert_eq!(
                    report.findings.len(),
                    1,
                    "only the unrelated N2/N3 gap remains"
                );
            }
        }
    }

    #[test]
    fn unsupported_net_shorts_only_block_checks_requiring_ownership() {
        let pdk = fixtures::pdk(
            r#"
[[rules.copper.clearance]]
id = "clearance"
limit = { minimum = "0.15 mm" }
[[rules.copper.feature_width]]
id = "width"
limit = { minimum = "0.15 mm" }
"#,
        );
        let annotated = annotated_antenna();
        for xml in [
            annotated.replace("</NetShort>", r#"<NetRef name="THIRD"/></NetShort>"#),
            annotated.replace("</NetShort>", r#"<LayerRef name="B.Cu"/></NetShort>"#),
            // A declaration on a layer with no copper must not disappear
            // while importing the layer's empty artwork.
            annotated.replace(TIE, "").replace(
                "</Step>",
                &format!(
                    r#"<LayerFeature layerRef="B.Cu"><Set>{}</Set></LayerFeature></Step>"#,
                    TIE.replace("F.Cu", "B.Cu")
                ),
            ),
        ] {
            let report = fixtures::report(&xml, &pdk, crate::LayoutTarget::Board);
            let clearance = report
                .rules
                .iter()
                .find(|rule| rule.id == "clearance")
                .unwrap();
            assert_eq!(clearance.status, RuleStatus::Incomplete);
            assert!(
                clearance
                    .skip_reason
                    .as_deref()
                    .unwrap()
                    .contains("NetShort")
            );
            let width = report.rules.iter().find(|rule| rule.id == "width").unwrap();
            assert_eq!(width.status, RuleStatus::Pass);
        }
    }

    #[test]
    fn antenna_bridge_still_requires_third_net_spacing_and_drill_clearance() {
        // A 0.5 mm pad is 0.05 mm from the end of a 0.5 mm radiator arm.
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="UNRELATED">
            <Pad padstackDefRef="PADSTACK_1"><Location x="169.31" y="-112.039392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
          </Set></LayerFeature>
          <LayerFeature layerRef="F.Cu_B.Cu"><Set net="WIFI.RF_ANT">
            <Hole name="UNRELATED_DRILL" diameter="0.30" platingStatus="PLATED" plusTol="0" minusTol="0" x="173.8" y="-100.439392"/>
          </Set></LayerFeature>
        </Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report.findings.iter().any(|finding| finding
                    .subjects
                    .iter()
                    .any(|s| s.net.as_deref() == Some("GND"))
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
                .filter(|finding| finding.rule_id.contains("pth_hole_clearance")
                    && finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("GND"))
                    && finding.subjects.iter().any(|s| s.kind == "plated_hole"))
                .count(),
            1,
            "{:?}",
            report.findings
        );
        let clean = antenna_check(&annotated_antenna(), "ipc").unwrap();
        assert!(
            clean
                .findings
                .iter()
                .all(|finding| !finding.rule_id.contains("pth_hole_clearance"))
        );
    }

    #[test]
    fn antenna_net_tie_permissions_do_not_leak_between_layout_occurrences() {
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
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap();
        let rules = fixtures::rules(&fixtures::pdk(
            "[[rules.copper.clearance]]\nid = \"clearance\"\nlimit = { minimum = \"0.15 mm\" }",
        ));
        let designs = Design::frames(
            &imported,
            ArtworkScope::ArrayFlattened,
            &rules,
            Resolution::default(),
        )
        .unwrap();
        assert_eq!(designs.len(), 2);
        assert!(
            designs[0]
                .copper_layers
                .iter()
                .all(|layer| layer.net_shorts.is_empty())
        );
        assert_eq!(designs[1].placements.len(), 2);
        let layer = &designs[1]
            .copper_layers
            .iter()
            .find(|layer| layer.layer.name == "F.Cu")
            .unwrap();
        let ties = &layer.net_shorts;
        assert_eq!(ties.len(), 1);
        assert_eq!(ties[0].nets[0].instance(), None);
        for design in &designs {
            assert!(
                super::evaluate(0.15, &super::Conditions::default(), design)
                    .unwrap()
                    .measured
                    .is_empty()
            );
        }

        // A declaration inside one board cannot authorize contact with a
        // different placement, even when both boards declare the same tie.
        let xml = xml.replace(r#"dx="40""#, r#"dx="0""#);
        let ipc = Ipc2581::parse(&xml).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap();
        let designs = Design::frames(
            &imported,
            ArtworkScope::ArrayFlattened,
            &rules,
            Resolution::default(),
        )
        .unwrap();
        assert!(
            super::evaluate(0.15, &super::Conditions::default(), &designs[0])
                .unwrap()
                .measured
                .iter()
                .any(|measurement| measurement.distance.mm == 0.0)
        );
    }

    #[test]
    fn declared_graphic_permits_group_nets_at_other_contacts_and_gaps() {
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT">
            <Pad padstackDefRef="PADSTACK_1"><Location x="173.8" y="-100.439392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
          </Set></LayerFeature></Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
            );

            let gap = xml.replace("</Step>", r#"
              <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT">
                <Pad padstackDefRef="PADSTACK_1"><Location x="169.31" y="-112.039392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
              </Set></LayerFeature></Step>"#);
            let report = antenna_check(&gap, pdk).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
            );
        }
    }

    #[test]
    fn net_short_accepts_partial_and_curved_pad_contacts() {
        for width in ["0.50001", "0.60"] {
            let xml = annotated_antenna().replacen(
                r#"<RectCenter width="0.50" height="0.50"/>"#,
                &format!(r#"<RectCenter width="{width}" height="0.50"/>"#),
                1,
            );
            let report = antenna_check(&xml, "standard").unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
            );
        }
        let xml = annotated_antenna().replacen(
            r#"<RectCenter width="0.50" height="0.50"/>"#,
            r#"<Circle diameter="0.50"/>"#,
            1,
        );
        antenna_check(&xml, "standard").unwrap();
        // A clear away from the declared point does not change object permission.
        let xml = annotated_antenna().replace(
            "</Step>",
            r#"
          <LayerFeature layerRef="F.Cu"><Set polarity="NEGATIVE"><Features>
            <Location x="169.0" y="-100.439392"/>
            <UserSpecial><Circle diameter="0.05"/></UserSpecial>
          </Features></Set></LayerFeature></Step>"#,
        );
        antenna_check(&xml, "standard").unwrap();
        let xml = annotated_antenna().replace(TIE, "");
        let report = antenna_check(&xml, "standard").unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.subjects.iter().any(|s| s.role == "first_conductor")
                    && f.measurement.actual_mm() == Some(0.0))
        );
    }

    #[test]
    fn net_short_in_a_gap_does_not_authorize_nearby_copper() {
        let pdk = fixtures::pdk(
            "[[rules.copper.clearance]]\nid = \"clearance\"\nlimit = { minimum = \"0.15 mm\" }",
        );
        // The graphic ends at x=9. The pad starts at its centre minus 0.5.
        for (pad_x, short_x, status) in [
            ("9.5", "9", RuleStatus::Fail),
            ("9.5015", "9.00075", RuleStatus::Incomplete),
        ] {
            let board = BOARD.replace(
                r#"<Location x="9" y="0.5"/>"#,
                &format!(r#"<Location x="{pad_x}" y="0.5"/>"#),
            );
            let xml = board.replace(
                "</Step>",
                &format!(
                    r#"
              <LayerFeature layerRef="TOP"><Set><NetShort>
                <NetRef name="N4"/><NetRef name="N5"/>
                <Location x="{short_x}" y="0.5"/><LayerRef name="TOP"/>
              </NetShort></Set></LayerFeature></Step>"#
                ),
            );
            let report = fixtures::report(&xml, &pdk, crate::LayoutTarget::Board);
            assert_eq!(report.rules[0].status, status);
            if status == RuleStatus::Incomplete {
                assert!(
                    report.rules[0]
                        .skip_reason
                        .as_deref()
                        .unwrap()
                        .contains("contact")
                );
                assert!(run(&board).findings.iter().any(|finding| {
                    finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("N5"))
                        && (finding.measurement.actual_mm().unwrap() - 0.0015).abs() < 1e-8
                }));
            } else {
                assert_eq!(report.findings.len(), 1, "only the N2/N3 gap remains");
            }
        }
    }

    #[test]
    fn net_short_matches_rounded_boundary_locations() {
        for (x, y, accepted) in [
            ("168.650000", "-100.439392", true),
            ("168.6499995", "-100.439392", true),
            ("168.649997", "-100.439392", false),
            ("168.6495", "-100.439392", false),
            ("168.6485", "-100.439392", false),
            // The fork's exporter chooses this pad/graphic corner.
            ("169.150", "-100.689392", true),
            ("169.1500005", "-100.6893925", true),
            ("169.150003", "-100.689395", false),
            ("169.1505", "-100.689892", false),
            ("169.1515", "-100.690892", false),
        ] {
            let xml = annotated_antenna()
                .replace(TIE, &TIE.replace("168.9", x).replace("-100.439392", y));
            for pdk in ["standard", "jlcpcb-1oz"] {
                let result = antenna_check(&xml, pdk);
                if accepted {
                    let report = result.unwrap();
                    assert!(
                        report
                            .findings
                            .iter()
                            .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
                    );
                } else {
                    assert!(result.unwrap_err().to_string().contains("contact"));
                }
            }
        }
    }

    #[test]
    fn net_ties_do_not_change_the_netless_copper_significance_threshold() {
        let pdk = fixtures::pdk(
            "[[rules.copper.clearance]]\nid = \"clearance\"\nlimit = { minimum = \"0.15 mm\" }",
        );
        let tie = r#"<Set><NetShort><NetRef name="N4"/><NetRef name="N5"/>
          <Location x="8.75" y="0.5"/><LayerRef name="TOP"/></NetShort></Set>"#;
        // Both rectangles are 10 µm wide. Their areas straddle the normal
        // 1 µm² significance threshold, despite both having a long edge.
        for (height, status) in [
            (0.00005, RuleStatus::Fail),
            (0.0002, RuleStatus::Incomplete),
        ] {
            for declaration in ["", tie] {
                let xml = BOARD.replace("</Step>", &format!(r#"
                  <LayerFeature layerRef="TOP">
                    <Set geometryUsage="GRAPHIC"><Features><Location x="20" y="0"/>
                      <UserSpecial><Contour><Polygon>
                        <PolyBegin x="0" y="0"/><PolyStepSegment x="0.01" y="0"/>
                        <PolyStepSegment x="0.01" y="{height}"/><PolyStepSegment x="0" y="{height}"/>
                        <PolyStepSegment x="0" y="0"/>
                      </Polygon></Contour></UserSpecial>
                    </Features></Set>{declaration}
                  </LayerFeature></Step>"#));
                let report = fixtures::report(&xml, &pdk, crate::LayoutTarget::Board);
                assert_eq!(
                    report.rules[0].status,
                    status,
                    "height {height}, tie {}: {:?}",
                    !declaration.is_empty(),
                    report.rules[0]
                );
                if status == RuleStatus::Incomplete {
                    assert!(
                        report.rules[0]
                            .skip_reason
                            .as_deref()
                            .unwrap()
                            .contains("without net attribution")
                    );
                } else {
                    // The N2/N3 gap remains; the N4/N5 short is exempt only
                    // when declared. The remote sliver contributes neither.
                    assert_eq!(
                        report.findings.len(),
                        if declaration.is_empty() { 2 } else { 1 }
                    );
                }
            }
        }
    }

    #[test]
    fn antenna_feed_arc_has_the_same_permission_as_its_narrower_trace() {
        // The saved board's 0.26 mm arc ends in a 0.20 mm trace entering the
        // tied pad. Both can approach the entire declared graphic.
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT"><Features><UserSpecial>
            <Line startX="168.850" startY="-100.439392" endX="168.498427" endY="-100.439392">
              <LineDesc lineWidth="0.20" lineEnd="ROUND"/>
            </Line>
            <Arc startX="167.084213" startY="-101.025179" endX="168.498427" endY="-100.439392" centerX="168.49840" centerY="-102.43940" clockwise="true">
              <LineDesc lineWidth="0.260" lineEnd="ROUND"/>
            </Arc>
          </UserSpecial></Features></Set></LayerFeature></Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor")),
                "{pdk}: unexpected electrical clearance: {:?}",
                report.findings
            );
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
    fn surviving_functional_copper_without_net_ownership_leaves_the_rule_incomplete() {
        let results = run(&BOARD.replace("<Set net=\"N2\">", "<Set>"));
        let rule = &results.rules[0];
        assert!(matches!(
            rule.status,
            crate::commands::dfm::report::RuleStatus::Incomplete
        ));
        assert!(
            rule.blocks_verdict(),
            "unattributed copper must fail closed"
        );
        assert!(
            rule.skip_reason
                .as_deref()
                .unwrap()
                .contains("final functional copper without net attribution")
        );
        assert!(results.findings.is_empty());
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
