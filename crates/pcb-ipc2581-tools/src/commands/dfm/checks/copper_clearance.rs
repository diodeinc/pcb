//! Minimum clearance between electrically distinct final copper conductors.
//!
//! Copper ownership is resolved while ordered artwork is composed. Each
//! owner therefore carries exactly the material that survives polarity,
//! clear features, and final cutouts. Clearance compares connected regions
//! belonging to different owners; self-notches and disconnected islands of
//! one owner are deliberately outside this quantity. Touching, overlapping,
//! or contained distinct-owner regions measure zero.

use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::{
    RegionBoundaryIndex, region_clearance_sites_with_index, region_clearance_within,
};

use crate::commands::dfm::design::{ConductorId, Design};
use crate::commands::dfm::report::{Evidence, SourceLocator, Subject};
use crate::commands::dfm::rules::Conditions;

use super::{Evaluation, Measured, linework_clearance, violates};

struct Piece {
    conductor_index: usize,
    region: pcb_ir::geom::ContourSet,
}

pub(super) fn evaluate(limit_mm: f64, conditions: &Conditions, design: &Design) -> Evaluation {
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

        // The pairs the bounds cannot separate, in sweep order along x. Only
        // the pieces in those pairs need a boundary index.
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
                        left.conductor_index != right.conductor_index
                            && left.region.bbox.distance_to(right.region.bbox) < limit_mm
                    })
                    .map(move |(offset, _)| (left_index, left_index + 1 + offset))
            })
            .collect::<Vec<_>>();
        let mut queried = vec![false; pieces.len()];
        for &(left_index, right_index) in &pairs {
            queried[left_index] = true;
            queried[right_index] = true;
        }
        let boundaries = pieces
            .iter()
            .zip(queried)
            .map(|(piece, queried)| {
                queried.then(|| RegionBoundaryIndex::new(&piece.region, limit_mm))
            })
            .collect::<Vec<_>>();

        for (left_index, right_index) in pairs {
            let (left, right) = (&pieces[left_index], &pieces[right_index]);
            let boundary = |index: usize| {
                boundaries[index]
                    .as_ref()
                    .expect("every paired piece is indexed")
            };
            let (left_boundary, right_boundary) = (boundary(left_index), boundary(right_index));
            let Some(distance) = region_clearance_within(
                &left.region,
                left_boundary,
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
                    )
                    .into_iter()
                    .map(|site| {
                        linework_clearance::report_site(site, vec![layer.layer.clone()], limit_mm)
                    })
                    .collect()
                } else {
                    Vec::new()
                },
            });
        }
    }

    Evaluation { checked, measured }
}

pub(super) fn conductor_subject(
    design: &Design,
    id: ConductorId,
    role: &'static str,
    layer: &str,
) -> Subject {
    let (kind, name, set_index, feature_index) = match id {
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
            set_index: None,
            feature_index: None,
            instance_index: id.instance(),
        }),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
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
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules).unwrap();
        checks::run(
            &rules,
            &design,
            None,
            NaiveDate::from_ymd_opt(2026, 8, 25).unwrap(),
        )
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
        let xml = BOARD.replace("<Set net=\"N2\">", "<Set>");
        let ipc = Ipc2581::parse(&xml).unwrap();
        let pdk = Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();

        let error = Design::extract(&imported, ArtworkScope::Board, &rules)
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
