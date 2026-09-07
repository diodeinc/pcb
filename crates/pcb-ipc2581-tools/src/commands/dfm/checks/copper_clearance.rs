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

use pcb_ir::geom::dfm::{
    region_clearance_sites_except_contacts, region_clearance_sites_with_index,
    region_clearance_within,
};
use pcb_ir::geom::{BBox, ContourSet, tol};

use crate::commands::dfm::design::{
    ConductorId, CopperConductor, CopperLayer, Design, NET_SHORT_LOCATION_TOLERANCE_MM, spans,
};
use crate::commands::dfm::report::{Evidence, SourceLocator, Subject};
use crate::commands::dfm::rules::Conditions;

use super::{Evaluation, Measured, linework_clearance, violates};

struct Piece {
    conductor_index: usize,
    region: pcb_ir::geom::ContourSet,
}

/// Bound where approximation could change this pair's contact. Guaranteed
/// material is a lower bound; the image plus all approximation bounds is an
/// upper bound. Equal intersections certify the contact even when an uncertain
/// operand is redundant inside an exact pad. Use entire owners, not pieces:
/// another island or clear operand may also affect this neighborhood.
fn contact_approximation_bounds(
    first: &CopperConductor,
    second: &CopperConductor,
    neighborhood: BBox,
) -> Result<Vec<BBox>, pcb_ir::geom::AccuracyError> {
    if !first
        .approximation_bounds
        .iter()
        .chain(&second.approximation_bounds)
        .any(|bounds| bounds.intersects(neighborhood))
    {
        return Ok(Vec::new());
    }
    let resolution = first.image.resolution.strict();
    let window = ContourSet::rectangle(neighborhood, resolution);
    let possible = |owner: &CopperConductor| {
        let mut image = owner.image.intersection(&window)?;
        for &bbox in &owner.approximation_bounds {
            if bbox.intersects(neighborhood) {
                image.union_assign(
                    &ContourSet::rectangle(bbox, resolution).intersection(&window)?,
                )?;
            }
        }
        Ok::<_, pcb_ir::geom::AccuracyError>(image)
    };
    let upper = possible(first)?.intersection(&possible(second)?)?;
    let lower = first
        .guaranteed_image
        .intersection(&window)?
        .intersection(&second.guaranteed_image)?;
    // Separate paint folds round shared vertices independently. Forgive only
    // numerical coincidence at the lower bound's boundary, never an area
    // cutoff or the much larger source-approximation budget.
    Ok(upper
        .difference(&lower.disk_dilate(tol::EPSILON_MM)?)?
        .connected_components()
        .into_iter()
        .map(|region| region.bbox)
        .collect())
}

pub(super) fn evaluate(
    limit_mm: f64,
    conditions: &Conditions,
    design: &Design,
) -> anyhow::Result<Evaluation> {
    let measure = |layer: &CopperLayer| {
        let conductors = layer
            .contact_conductors
            .as_ref()
            .unwrap_or(&layer.conductors);
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
        let mut authorized = std::collections::HashMap::<_, Vec<_>>::new();
        for short in &layer.net_shorts {
            let mut contacts = Vec::new();
            for (first, _) in pieces
                .iter()
                .enumerate()
                .filter(|(_, piece)| conductors[piece.conductor_index].id == short.nets[0])
            {
                for (second, _) in pieces
                    .iter()
                    .enumerate()
                    .filter(|(_, piece)| conductors[piece.conductor_index].id == short.nets[1])
                {
                    let contains = |index: usize| {
                        boundaries[index]
                            .signed_distance(short.location)
                            .is_some_and(|distance| distance.mm <= NET_SHORT_LOCATION_TOLERANCE_MM)
                    };
                    if !contains(first) || !contains(second) {
                        continue;
                    }
                    contacts.push((first.min(second), first.max(second)));
                }
            }
            if contacts.len() != 1 {
                anyhow::bail!(
                    "NetShort at ({}, {}) on '{}' must identify exactly one actual contact between its NetRefs (found {})",
                    short.location.x,
                    short.location.y,
                    layer.layer.name,
                    contacts.len()
                );
            }
            authorized
                .entry(contacts[0])
                .or_default()
                .push(short.location);
        }
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
                        left.conductor_index != right.conductor_index
                            && spans(
                                conductors[left.conductor_index].branch,
                                conductors[right.conductor_index].branch,
                            )
                            && left.region.bbox.distance_to(right.region.bbox) < limit_mm
                    })
                    .map(move |(offset, _)| (left_index, left_index + 1 + offset))
            })
            .collect::<Vec<_>>();

        let measure_pair = |(left_index, right_index): (usize, usize)| {
            let (left, right) = (&pieces[left_index], &pieces[right_index]);
            let right_boundary = &boundaries[right_index];
            let contact_sites = authorized
                .get(&(left_index, right_index))
                .map(|locations| {
                    let neighborhood = left
                        .region
                        .intersection(&right.region)?
                        .bbox
                        .expand(tol::EPSILON_MM);
                    let approximation_bounds = contact_approximation_bounds(
                        &conductors[left.conductor_index],
                        &conductors[right.conductor_index],
                        neighborhood,
                    )?;
                    region_clearance_sites_except_contacts(
                        &left.region,
                        &right.region,
                        locations,
                        NET_SHORT_LOCATION_TOLERANCE_MM,
                        &approximation_bounds,
                        limit_mm,
                    )
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "NetShort on '{}': {error}; copper clearance cannot be certified",
                            layer.layer.name
                        )
                    })
                })
                .transpose()?;
            let distance = if let Some(sites) = &contact_sites {
                sites
                    .iter()
                    .map(|site| site.distance)
                    .min_by(|a, b| a.mm.total_cmp(&b.mm))
            } else {
                region_clearance_within(
                    &left.region,
                    &boundaries[left_index],
                    &right.region,
                    right_boundary,
                    limit_mm,
                )
            };
            let Some(distance) = distance else {
                return Ok(None);
            };

            let left_id = conductors[left.conductor_index].id;
            let right_id = conductors[right.conductor_index].id;
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
                    let local_contacts = contact_sites.is_some();
                    let sites = match contact_sites {
                        Some(sites) => sites,
                        None => region_clearance_sites_with_index(
                            &left.region,
                            &right.region,
                            right_boundary,
                            limit_mm,
                        )?,
                    };
                    let mut reported = linework_clearance::report_sites(
                        sites,
                        std::slice::from_ref(&layer.layer),
                        limit_mm,
                        design.resolution,
                    )?;
                    if local_contacts {
                        for site in &mut reported {
                            site.note = Some("A separate contact or edge-pair gap remains outside the declared NetShort contact.".to_owned());
                        }
                    }
                    reported
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
        let rule = "[[rules.copper.clearance]]\nid = \"copper-clearance\"\nlimit = { minimum = \"0.15 mm\" }";
        fixtures::run_board(xml, &fixtures::pdk(rule))
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
                input: FileIdentity {
                    path: "antenna.xml".to_owned(),
                    sha256: dfm::sha256(xml.as_bytes()),
                    size_bytes: xml.len() as u64,
                },
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
            anyhow::bail!(
                "{}",
                rule.skip_reason.as_deref().unwrap_or("incomplete DFM rule")
            );
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
            assert!(
                results.findings.iter().any(|finding| finding
                    .subjects
                    .iter()
                    .any(|s| s.net.as_deref() == Some("GND"))
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
        ] {
            let error = antenna_check(&xml, "standard").err().unwrap().to_string();
            assert!(error.contains("NetShort"), "{error}");
        }
        for xml in [
            annotated.replace(TIE, &format!("{TIE}{TIE}")),
            annotated.replace(r#" componentRef="E1""#, ""),
            annotated.replace(TIE, "").replace(
                "</Step>",
                &format!(r#"<LayerFeature layerRef="F.Cu"><Set>{TIE}</Set></LayerFeature></Step>"#),
            ),
        ] {
            // Metadata need not live on the graphic or name a component.
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
    fn each_contact_needs_a_declaration_and_nearby_gaps_are_not_exempt() {
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT">
            <Pad padstackDefRef="PADSTACK_1"><Location x="173.8" y="-100.439392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
          </Set></LayerFeature></Step>"#);
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report.findings.iter().any(|f| f
                    .subjects
                    .iter()
                    .any(|s| s.role == "first_conductor")
                    && f.measurement.actual_mm() == Some(0.0))
            );

            let both = xml.replace(TIE, &format!("{TIE}{}", TIE.replace("168.9", "173.8")));
            let report = antenna_check(&both, pdk).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
            );

            let gap = both.replace("</Step>", r#"
              <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT">
                <Pad padstackDefRef="PADSTACK_1"><Location x="169.31" y="-112.039392"/><StandardPrimitiveRef id="RECT_1"/></Pad>
              </Set></LayerFeature></Step>"#);
            let report = antenna_check(&gap, pdk).unwrap();
            assert!(report.findings.iter().any(|f| {
                f.subjects.iter().any(|s| s.role == "first_conductor")
                    && f.measurement
                        .actual_mm()
                        .is_some_and(|mm| (mm - 0.05).abs() < 1e-6)
            }));
        }
    }

    #[test]
    fn net_short_accepts_partial_overlap_but_not_uncertain_contact() {
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
        assert!(
            antenna_check(&xml, "standard")
                .err()
                .unwrap()
                .to_string()
                .contains("uncertain contact topology")
        );
        // Curved subtractive paint can change contact topology too. The
        // declaration remains in copper; the small hole is elsewhere in
        // the same contact, so rejecting only its Location is insufficient.
        let xml = annotated_antenna().replace(
            "</Step>",
            r#"
          <LayerFeature layerRef="F.Cu"><Set polarity="NEGATIVE"><Features>
            <Location x="169.0" y="-100.439392"/>
            <UserSpecial><Circle diameter="0.05"/></UserSpecial>
          </Features></Set></LayerFeature></Step>"#,
        );
        assert!(
            antenna_check(&xml, "standard")
                .unwrap_err()
                .to_string()
                .contains("uncertain contact topology")
        );
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
    fn net_short_matches_rounded_boundary_locations() {
        for (x, accepted) in [
            ("168.650000", true),
            ("168.6499995", true),
            ("168.649997", false),
        ] {
            let xml = annotated_antenna().replace(TIE, &TIE.replace("168.9", x));
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
                    assert!(result.unwrap_err().to_string().contains("actual contact"));
                }
            }
        }
    }

    fn add_feed_rectangle(xml: &str, [x0, y0, x1, y1]: [f64; 4]) -> String {
        xml.replace("</Step>", &format!(r#"
          <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT"><Features><UserSpecial><Contour><Polygon>
            <PolyBegin x="{x0}" y="{y0}"/><PolyStepSegment x="{x1}" y="{y0}"/>
            <PolyStepSegment x="{x1}" y="{y1}"/><PolyStepSegment x="{x0}" y="{y1}"/>
            <PolyStepSegment x="{x0}" y="{y0}"/>
          </Polygon></Contour></UserSpecial></Features></Set></LayerFeature></Step>"#))
    }

    #[test]
    fn contact_preparation_does_not_change_shared_copper_images() {
        let rules = fixtures::rules(&fixtures::pdk(
            "[[rules.copper.clearance]]\nid = \"clearance\"\nlimit = { minimum = \"0.15 mm\" }",
        ));
        let xml = add_feed_rectangle(&annotated_antenna(), [166.0, -104.0, 166.0005, -103.9995]);
        let ipc = Ipc2581::parse(&xml).unwrap();
        let without = Ipc2581::parse(&xml.replace(TIE, "")).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap();
        let without =
            pcb_ir::import::ipc2581::import_design(&without, Resolution::default()).unwrap();
        let tied = Design::board(&imported, &rules, Resolution::default());
        let plain = Design::board(&without, &rules, Resolution::default());
        for (tied, plain) in tied.copper_layers.iter().zip(&plain.copper_layers) {
            assert_eq!(tied.image.rings, plain.image.rings);
            for (tied, plain) in tied.conductors.iter().zip(&plain.conductors) {
                assert_eq!(tied.image.rings, plain.image.rings);
            }
        }
        let top = tied
            .copper_layers
            .iter()
            .find(|layer| layer.layer.name == "F.Cu")
            .unwrap();
        let rings = |owners: &[crate::commands::dfm::design::CopperConductor]| {
            owners
                .iter()
                .map(|owner| owner.image.rings.len())
                .sum::<usize>()
        };
        // The tiny source island survives only in copper-clearance's
        // unfiltered preparation, not in other checks' shared images.
        assert_eq!(
            rings(top.contact_conductors.as_ref().unwrap()),
            rings(&top.conductors) + 1
        );
    }

    #[test]
    fn routed_antenna_keeps_second_short_and_gap_on_the_same_connected_feed() {
        // Retain the source antenna and the actual round-ended feed entering
        // its square pad. Extend it for a return branch approaching the
        // bottom radiator arm on the same feed net.
        let routed = annotated_antenna().replace(
            "</Step>",
            r#"
          <LayerFeature layerRef="F.Cu">
            <Set net="WIFI.RF_ANT"><Features><UserSpecial>
              <Line startX="168.850" startY="-100.439392" endX="168.498427" endY="-100.439392">
                <LineDesc lineWidth="0.20" lineEnd="ROUND"/>
              </Line>
              <Line startX="165" startY="-100.439392" endX="168.3" endY="-100.439392">
                <LineDesc lineWidth="0.25" lineEnd="ROUND"/>
              </Line>
            </UserSpecial></Features></Set>
            <Set net="GND"><Features><Location x="173.8" y="-100.439392"/>
              <UserSpecial><Circle diameter="0.5"/></UserSpecial>
            </Features></Set>
          </LayerFeature></Step>"#,
        );
        let branch = add_feed_rectangle(&routed, [165.0, -113.0, 165.25, -100.4]);
        let branch = add_feed_rectangle(&branch, [165.0, -113.0, 169.9, -112.75]);
        for pdk in ["standard", "jlcpcb-1oz"] {
            // Moving the rounded cap past the exact pad into the radiator
            // changes the contact itself. It must remain uncertifiable.
            let protruding = routed.replace(r#"startX="168.850""#, r#"startX="169.20""#);
            assert!(
                antenna_check(&protruding, pdk)
                    .unwrap_err()
                    .to_string()
                    .contains("uncertain contact topology")
            );
            // A distant same-owner island changes the paint fold's rounding
            // frame, but cannot make the exact local contact ambiguous.
            let distant = add_feed_rectangle(&routed, [999.0, -0.1, 999.2, 0.1]);
            let report = antenna_check(&distant, pdk).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor"))
            );
            let clean = antenna_check(&routed, pdk).unwrap();
            assert!(
                clean
                    .findings
                    .iter()
                    .all(|f| !f.subjects.iter().any(|s| s.role == "first_conductor")),
                "{pdk}: {:?}",
                clean.findings
            );
            assert!(clean.findings.iter().any(|f| {
                f.rule_id.contains("pth_annular_ring")
                    && f.measurement
                        .actual_mm()
                        .is_some_and(|mm| (mm - 0.1).abs() < 1e-6)
            }));

            for (end_y, expected) in [(-112.1, 0.0), (-112.339392, 0.05)] {
                let center_y = end_y - 0.1;
                let xml = branch.replace(
                    "</Step>",
                    &format!(
                        r#"
                  <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT"><Features><UserSpecial>
                    <Line startX="169.8" startY="-112.8" endX="169.8" endY="{center_y}">
                      <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
                    </Line>
                  </UserSpecial></Features></Set></LayerFeature></Step>"#
                    ),
                );
                let report = antenna_check(&xml, pdk).unwrap();
                assert!(
                    report.findings.iter().any(|f| f
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("GND"))
                        && f.subjects
                            .iter()
                            .any(|s| s.net.as_deref() == Some("WIFI.RF_ANT"))
                        && f.sites.iter().any(|site| site.uncertainty_mm > 1e-6
                            && site
                                .measurement
                                .actual_mm()
                                .is_some_and(|mm| (mm - expected).abs() <= site.uncertainty_mm))),
                    "{pdk} expected {expected}: {:?}",
                    report.findings
                );
            }
        }
    }

    #[test]
    fn antenna_feed_arc_keeps_the_gap_next_to_its_narrower_trace() {
        // The saved board's 0.26 mm arc ends in a 0.20 mm trace entering the
        // tied pad. Its exposed cap must not be merged with the trace/pad
        // interior when deciding which boundary intervals remain exposed.
        let xml = annotated_antenna().replace("</Step>", r#"
          <LayerFeature layerRef="F.Cu"><Set net="WIFI.RF_ANT"><Features><UserSpecial>
            <Line startX="168.850" startY="-100.439392" endX="168.498427" endY="-100.439392">
              <LineDesc lineWidth="0.20" lineEnd="ROUND"/>
            </Line>
            <Arc startX="167.084213" startY="-101.025179" endX="168.498427" endY="-100.439392" centerX="168.49840" centerY="-102.43940" clockwise="true">
              <LineDesc lineWidth="0.260" lineEnd="ROUND"/>
            </Arc>
          </UserSpecial></Features></Set></LayerFeature></Step>"#);
        // At the trace edge, the circular cap extends sqrt(r² - half_width²)
        // beyond its center. The radiator's facing edge is at x = 168.65.
        // Bound the cap radius by its preparation error before projecting
        // onto this chord, where horizontal error is larger than radial error.
        let gap = |radius: f64| 168.65 - 168.498427 - (radius.powi(2) - 0.10_f64.powi(2)).sqrt();
        for pdk in ["standard", "jlcpcb-1oz"] {
            let report = antenna_check(&xml, pdk).unwrap();
            assert!(
                report.findings.iter().any(|finding| {
                    finding
                        .subjects
                        .iter()
                        .any(|s| s.net.as_deref() == Some("GND"))
                        && finding
                            .subjects
                            .iter()
                            .any(|s| s.net.as_deref() == Some("WIFI.RF_ANT"))
                        && finding.sites.iter().any(|site| {
                            site.measurement.actual_mm().is_some_and(|mm| {
                                mm >= gap(0.13 + site.uncertainty_mm)
                                    && mm <= gap(0.13 - site.uncertainty_mm)
                            })
                        })
                }),
                "{pdk}: missing feed cap gap: {:?}",
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
