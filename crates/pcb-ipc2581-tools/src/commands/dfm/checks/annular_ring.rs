//! Minimum annular ring: radial copper enclosure of a drilled hole.
//!
//! Let `M ⊂ ℝ²` be one layer's composed copper image — a regularized
//! closed filled region — and let the hole be the disk `D(p, r)`. The
//! annular enclosure is the largest uniform ring of copper the hole is
//! guaranteed on that layer:
//!
//! ```text
//! a = sup { t : D(p, r + t) ⊆ M }.
//! ```
//!
//! For `p ∈ M` the largest disk centered at `p` inside `M` has radius
//! `d(p, ∂M)`, the Euclidean distance from the center to the copper
//! boundary, so `a = d(p, ∂M) − r`; the value is signed, a negative `a`
//! being the depth by which the drill breaches the copper. The check
//! requires
//!
//! ```text
//! a ≥ A_min    on every copper layer the hole must land on,
//! ```
//!
//! equivalently `D(p, r + A_min) ⊆ M` — the indexed form of a morphological
//! erosion test. The layers a hole must land on are the spanned layers
//! holding copper at `p`, plus spanned layers carrying a matching source
//! land or at an end of the drill span even without copper there: absence
//! on those is a zero enclosure. A spanned intermediate layer with neither
//! is a plane anti-pad or a removed unused land and has no ring to measure.
//! One measurement per hole reports the layer minimizing `a`.
//!
//! Computation: `p ∈ M` by a batched winding-number sweep over all hole
//! centers; `d(p, ∂M)` by a nearest-boundary query against a uniform grid
//! over the boundary segments, searched only to `r + A_min` since farther
//! boundaries cannot violate.

use pcb_ir::geom::dfm::{BBoxIndex, Distance, circular_region};
use pcb_ir::geom::region::difference_rings;
use pcb_ir::geom::{BBox, ContourSet, FillRule, Point};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use crate::commands::dfm::design::{CopperLayer, Design, Hole, HoleClass, HoleLand, Land};
use crate::commands::dfm::report::{
    Evidence, EvidenceDisplay, MeasurementKind, SourceLocator, Subject,
};
use crate::commands::dfm::rules::Conditions;

use super::{Evaluation, Measured, MeasuredSite, hole_subject, holes_of_class, layers, violates};

/// One copper layer on which a hole has a ring to measure.
struct RingSubject<'a> {
    copper: &'a CopperLayer,
    ring_index: &'a BBoxIndex,
    land: Option<&'a Land>,
    in_copper: bool,
}

pub(super) fn evaluate(
    limit_mm: f64,
    class: HoleClass,
    conditions: &Conditions,
    design: &Design,
) -> Evaluation {
    let holes = holes_of_class(design, class);
    let copper_layers = &design.copper_layers;
    let hole_lands = &design.hole_lands;
    let boundaries = &design.copper_boundaries;
    #[cfg(not(target_family = "wasm"))]
    let layers = copper_layers.par_iter();
    #[cfg(target_family = "wasm")]
    let layers = copper_layers.iter();
    let ring_indices = layers
        .clone()
        .map(|layer| {
            BBoxIndex::new(
                layer
                    .image
                    .rings
                    .iter()
                    .map(|ring| {
                        ring.iter().fold(BBox::empty(), |mut bbox, &[x, y]| {
                            bbox.include_point(Point::new(x, y));
                            bbox
                        })
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let centers = holes
        .iter()
        .map(|(_, hole)| hole.center)
        .collect::<Vec<_>>();
    let contains = layers
        .map(|layer| layer.image.contains_points_batch(&centers))
        .collect::<Vec<_>>();

    #[cfg(not(target_family = "wasm"))]
    let holes = holes.par_iter();
    #[cfg(target_family = "wasm")]
    let holes = holes.iter();
    let per_hole = holes
        .enumerate()
        .map(|(position, &(hole_index, hole))| {
            let radius = hole.diameter_mm / 2.0;
            let enclosures = copper_layers
                .iter()
                .enumerate()
                .filter(|(copper_index, _)| hole.drill_span.contains_copper(*copper_index))
                .filter(|(_, copper)| conditions.applies_to_layer(copper))
                .filter_map(|(copper_index, copper)| {
                    let land = hole_lands[hole_index]
                        .iter()
                        .find(|link| link.copper_index as usize == copper_index)
                        .map(|link: &HoleLand| &copper.lands[link.land_index as usize]);
                    let in_copper = contains[copper_index][position];
                    let required = land.is_some() || hole.drill_span.terminates_on(copper_index);
                    (in_copper || required).then_some((
                        copper_index,
                        RingSubject {
                            copper,
                            ring_index: &ring_indices[copper_index],
                            land,
                            in_copper,
                        },
                    ))
                })
                .map(|(copper_index, subject)| {
                    let enclosure = if subject.in_copper {
                        boundaries[copper_index].circular_enclosure(hole.center, radius, limit_mm)
                    } else {
                        Some(Distance::exact(0.0, hole.center, hole.center))
                    };
                    (subject, enclosure)
                })
                .collect::<Vec<_>>();
            let mut worst = enclosures
                .iter()
                .filter_map(|(subject, enclosure)| enclosure.map(|enclosure| (subject, enclosure)))
                .min_by(|(_, left), (_, right)| left.mm.total_cmp(&right.mm))
                .map(|(subject, enclosure)| {
                    measured(design, hole, subject, enclosure, radius + limit_mm)
                });
            if let Some(worst) = &mut worst {
                worst.sites = enclosures.iter().filter_map(|(subject, enclosure)| {
                    let enclosure = (*enclosure).filter(|distance| violates(distance, limit_mm))?;
                    let mut detail = measured(design, hole, subject, enclosure, radius + limit_mm);
                    let required = circular_region(hole.center, radius + limit_mm);
                    detail.evidence.push(Evidence {
                        display: Some(EvidenceDisplay::CircleMinusLayer {
                            center: hole.center.into(),
                            diameter: 2.0 * (radius + limit_mm),
                            layer: subject.copper.layer.name.clone(),
                        }),
                        ..Evidence::region("missing_copper", &missing_copper(
                            &required, &subject.copper.image, subject.ring_index,
                        ))
                    });
                    let mut site = MeasuredSite::new(
                        enclosure, detail.bbox, detail.layers, detail.evidence,
                        if subject.in_copper { MeasurementKind::RadialEnclosure } else { MeasurementKind::MissingCopper },
                    );
                    site.subjects = detail.subjects;
                    if !subject.in_copper {
                        site.note = Some("Required copper is absent at the drill center; this layer has zero enclosure.".to_owned());
                    } else if enclosure.mm < 0.0 {
                        site.note = Some("The drilled hole breaches the copper boundary; the annular enclosure is signed.".to_owned());
                    }
                    Some(site)
                }).collect();
            }
            (enclosures.len(), worst)
        })
        .collect::<Vec<_>>();

    Evaluation {
        checked: per_hole.iter().map(|(checked, _)| checked).sum(),
        measured: per_hole
            .into_iter()
            .filter_map(|(_, measured)| measured)
            .collect(),
    }
}

/// A ring whose bounds miss the required disk cannot change material inside
/// it. Retaining complete intersecting rings (including enclosing planes and
/// their holes) preserves polarity while avoiding a full-panel boolean for
/// each individual annular finding.
fn missing_copper(required: &ContourSet, copper: &ContourSet, index: &BBoxIndex) -> ContourSet {
    let rings = index
        .query(required.bbox)
        .into_iter()
        .map(|id| copper.rings[id].clone())
        .collect();
    ContourSet::new(
        difference_rings(required.rings.clone(), rings),
        FillRule::NonZero,
        required.tolerance,
    )
}

fn measured(
    design: &Design,
    hole: &Hole,
    subject: &RingSubject,
    enclosure: Distance,
    required_radius_mm: f64,
) -> Measured {
    let copper = subject.copper;
    let land_subject = subject.land.map_or_else(
        || Subject {
            role: "land",
            kind: "composed_copper",
            name: Some(copper.layer.name.clone()),
            ..Subject::default()
        },
        |land| Subject {
            role: "land",
            kind: "padstack_land",
            name: design.resolve(land.primitive_ref),
            reference_designator: design.resolve(land.reference_designator),
            pin: design.resolve(land.pin),
            net: design.resolve(land.net),
            padstack_ref: design.resolve(Some(land.padstack)),
            source: Some(SourceLocator {
                step: design.resolve(land.step),
                layer: Some(copper.layer.name.clone()),
                set_index: Some(land.source_set_index),
                feature_index: Some(land.source_feature_index),
                instance_index: None,
            }),
            provenance: Some(land.provenance.clone()),
            ..Subject::default()
        },
    );
    let land_evidence = subject
        .land
        .map(|land| Evidence::bounds("source_padstack_land_bounds", land.bbox));
    Measured {
        distance: enclosure,
        bbox: BBox::from_point(hole.center).expand(required_radius_mm),
        layers: layers([&hole.layer, &copper.layer]),
        subjects: vec![hole_subject(design, hole, "hole"), land_subject],
        evidence: [
            Evidence::circle("drilled_hole", hole.center, hole.diameter_mm),
            Evidence::circle(
                "required_copper_envelope",
                hole.center,
                2.0 * required_radius_mm,
            ),
        ]
        .into_iter()
        .chain(land_evidence)
        .collect(),
        sites: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use pcb_ir::dialects::ipc::ArtworkScope;

    use crate::commands::dfm::pdk::Pdk;
    use crate::commands::dfm::rules::{self, Rule};
    use crate::ipc2581::Ipc2581;

    use super::*;

    fn rule() -> Rule {
        let pdk = Pdk::parse(
            r#"schema_version = 2
default_profile = "test"

[pdk]
id = "test"
name = "Test"
revision = "1"

[profiles.test]
name = "Test"

[[rules.copper.annular_ring]]
id = "pth-ring"
select = { hole = "pth" }
limit = { minimum = "0.2 mm" }
"#,
        )
        .unwrap();
        rules::lower(&pdk, None).unwrap().remove(0)
    }

    /// A 1 mm plated hole at the origin through copper layers `L0..Ln`,
    /// with a 2 mm copper square on each layer named in `copper_on`, and
    /// an optional drill span.
    fn board(layer_count: usize, copper_on: &[usize], span: Option<(usize, usize)>) -> Ipc2581 {
        Ipc2581::parse(&board_xml(layer_count, copper_on, span)).unwrap()
    }

    fn board_xml(layer_count: usize, copper_on: &[usize], span: Option<(usize, usize)>) -> String {
        let layer_names = (0..layer_count).map(|index| format!("L{index}"));
        let refs = layer_names
            .clone()
            .map(|name| format!(r#"<LayerRef name="{name}"/>"#))
            .collect::<String>();
        let layers = layer_names
            .clone()
            .map(|name| {
                format!(
                    r#"<Layer name="{name}" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>"#
                )
            })
            .collect::<String>();
        let drill_span = span
            .map(|(from, to)| format!(r#"<Span fromLayer="L{from}" toLayer="L{to}"/>"#))
            .unwrap_or_default();
        let copper = copper_on
            .iter()
            .map(|index| {
                format!(
                    r#"<LayerFeature layerRef="L{index}"><Set polarity="POSITIVE"><Features><Contour><Polygon>
                      <PolyBegin x="-1" y="-1"/><PolyStepSegment x="1" y="-1"/><PolyStepSegment x="1" y="1"/>
                      <PolyStepSegment x="-1" y="1"/><PolyStepSegment x="-1" y="-1"/>
                    </Polygon></Contour></Features></Set></LayerFeature>"#
                )
            })
            .collect::<String>();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    {refs}
    <LayerRef name="DRILL"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      {layers}
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">{drill_span}</Layer>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        {copper}
        <LayerFeature layerRef="DRILL">
          <Set polarity="POSITIVE">
            <Hole name="H1" diameter="1" platingStatus="PLATED" x="0" y="0"/>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        )
    }

    fn evaluate_pth(ipc: &Ipc2581) -> Evaluation {
        let rule = rule();
        let imported = pcb_ir::import::ipc2581::import_design(ipc).unwrap();
        let design =
            Design::extract(&imported, ArtworkScope::Board, std::slice::from_ref(&rule)).unwrap();
        evaluate(
            rule.limit.length().millimeters(),
            HoleClass::Pth,
            &rule.conditions,
            &design,
        )
    }

    #[test]
    fn missing_terminal_copper_is_a_zero_enclosure_violation() {
        let evaluation = evaluate_pth(&board(3, &[2], None));

        assert_eq!(evaluation.checked, 2);
        assert_eq!(evaluation.measured.len(), 1);
        let measured = &evaluation.measured[0];
        assert_eq!(measured.distance.mm, 0.0);
        assert_eq!(measured.layers[1].name, "L0");
    }

    #[test]
    fn intermediate_antipad_without_a_source_land_is_not_an_annular_subject() {
        let evaluation = evaluate_pth(&board(3, &[0, 2], None));

        assert_eq!(evaluation.checked, 2);
        assert!(evaluation.measured.is_empty());
    }

    #[test]
    fn fully_cleared_intermediate_source_land_still_requires_an_annular_ring() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="land"><Circle diameter="2"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="L0" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
    <Layer name="L1" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
    <Layer name="L2" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
      <Span fromLayer="L0" toLayer="L2"/>
    </Layer>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/>
      <LayerFeature layerRef="L0"><Set><Pad padstackDefRef="stack"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pad></Set></LayerFeature>
      <LayerFeature layerRef="L1">
        <Set><Pad padstackDefRef="stack"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pad></Set>
        <Set polarity="NEGATIVE"><Features><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Features></Set>
      </LayerFeature>
      <LayerFeature layerRef="L2"><Set><Pad padstackDefRef="stack"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pad></Set></LayerFeature>
      <LayerFeature layerRef="DRILL"><Set geometry="stack">
        <Hole name="H1" diameter="1" platingStatus="PLATED" x="0" y="0"/>
      </Set></LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let middle = imported.layer_id("L1").unwrap();
        assert!(
            imported
                .physical_lands(ArtworkScope::Board)
                .unwrap()
                .iter()
                .all(|land| land.layer != middle),
            "the source land has no final copper"
        );
        let evaluation = evaluate_pth(&ipc);
        assert_eq!(evaluation.checked, 3, "source identity makes L1 required");
        assert_eq!(evaluation.measured.len(), 1);
        let measured = &evaluation.measured[0];
        assert_eq!(measured.distance.mm, 0.0);
        assert_eq!(measured.layers[1].name, "L1");
        assert_eq!(measured.subjects[1].kind, "padstack_land");
        assert_eq!(measured.subjects[1].padstack_ref.as_deref(), Some("stack"));
    }

    #[test]
    fn known_blind_span_requires_copper_at_its_own_terminal_layers() {
        let evaluation = evaluate_pth(&board(4, &[2], Some((1, 2))));

        assert_eq!(evaluation.checked, 2);
        assert_eq!(evaluation.measured.len(), 1);
        let measured = &evaluation.measured[0];
        assert_eq!(measured.layers[1].name, "L1");
        assert_eq!(measured.distance.mm, 0.0);
    }

    #[test]
    fn physical_terminals_are_independent_of_copper_declaration_order() {
        let stackup = [0, 1, 2, 3].map(|i| format!(r#"<StackupLayer layerOrGroupRef="L{i}" thickness="0.035" tolPlus="0" tolMinus="0" sequence="{i}"/>"#)).join("");
        for span in [None, Some((1, 2))] {
            let terminals = if span.is_some() { [1, 2] } else { [0, 3] };
            for copper_on in [&terminals[..], &terminals[..1]] {
                let xml = board_xml(4, copper_on, span).replace(r#"<Step name="board""#, &format!(r#"
                    <Stackup name="Primary" overallThickness="0.14" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
                      <StackupGroup name="Group" thickness="0.14" tolPlus="0" tolMinus="0">{stackup}</StackupGroup>
                    </Stackup><Step name="board""#));
                let shuffled = xml
                    .replace(r#"<Layer name="L1""#, r#"<Layer name="TEMP""#)
                    .replace(r#"<Layer name="L3""#, r#"<Layer name="L1""#)
                    .replace(r#"<Layer name="TEMP""#, r#"<Layer name="L3""#);
                for xml in [&xml, &shuffled] {
                    let evaluation = evaluate_pth(&Ipc2581::parse(xml).unwrap());
                    assert_eq!(evaluation.checked, 2, "span {span:?}");
                    assert_eq!(
                        evaluation.measured.len(),
                        2 - copper_on.len(),
                        "span {span:?}"
                    );
                    if let Some(finding) = evaluation.measured.first() {
                        assert_eq!(finding.layers[1].name, format!("L{}", terminals[1]));
                        assert_eq!(finding.distance.mm, 0.0);
                    }
                }
            }
        }
    }

    #[test]
    fn all_failing_terminal_layers_have_independent_geometry_sites() {
        let evaluation = evaluate_pth(&board(4, &[], None));
        assert_eq!(evaluation.measured.len(), 1, "retain one finding per hole");
        let finding = &evaluation.measured[0];
        assert_eq!(finding.sites.len(), 2);
        assert_eq!(
            finding.layers[1].name, "L0",
            "retain the original worst-layer representative"
        );
        assert_eq!(
            finding
                .sites
                .iter()
                .map(|site| site.layers[1].name.as_str())
                .collect::<Vec<_>>(),
            vec!["L0", "L3"]
        );
        assert!(finding.sites.iter().all(|site| {
            matches!(site.measurement_kind, MeasurementKind::MissingCopper)
                && site
                    .evidence
                    .iter()
                    .any(|evidence| evidence.role == "missing_copper" && !evidence.paths.is_empty())
        }));
        for site in &finding.sites {
            let evidence = site
                .evidence
                .iter()
                .find(|evidence| evidence.role == "missing_copper")
                .unwrap();
            let Some(EvidenceDisplay::CircleMinusLayer {
                center,
                diameter,
                layer,
            }) = &evidence.display
            else {
                panic!("missing copper retains its analytic construction");
            };
            assert_eq!((center.x, center.y), (0.0, 0.0));
            assert!((*diameter - 1.4).abs() < 1e-12);
            assert_eq!(
                layer, &site.layers[1].name,
                "each site cuts its own copper layer"
            );
            let envelope = site
                .evidence
                .iter()
                .find(|evidence| evidence.role == "required_copper_envelope")
                .unwrap();
            assert_eq!(Some(*diameter), envelope.diameter);
            assert_eq!(
                site.distance.mm, 0.0,
                "display leaves the measurement unchanged"
            );
        }
    }

    #[test]
    fn local_missing_copper_keeps_enclosing_planes_holes_and_repainted_islands() {
        use pcb_ir::geom::tol;
        let rectangle = |x0, y0, x1, y1| {
            ContourSet::rectangle(
                BBox {
                    min: Point::new(x0, y0),
                    max: Point::new(x1, y1),
                },
                tol::REGION_MM,
            )
        };
        let copper = rectangle(-100.0, -100.0, 100.0, 100.0)
            .difference(&rectangle(-0.4, -0.4, 0.4, 0.4))
            .union(&rectangle(-0.1, -0.1, 0.1, 0.1))
            .union(&rectangle(200.0, 200.0, 201.0, 201.0));
        let required = circular_region(Point::ZERO, 0.6);
        let bounds = copper
            .rings
            .iter()
            .map(|ring| {
                ring.iter().fold(BBox::empty(), |mut bbox, &[x, y]| {
                    bbox.include_point(Point::new(x, y));
                    bbox
                })
            })
            .collect();
        let index = BBoxIndex::new(bounds);
        assert!(index.query(required.bbox).len() < copper.rings.len());
        let local = missing_copper(&required, &copper, &index);
        let complete = required.difference(&copper);
        assert!(local.difference(&complete).is_empty());
        assert!(complete.difference(&local).is_empty());
        assert!(
            !local.contains_point(Point::ZERO),
            "repainted island supplies copper"
        );
        assert!(
            local.contains_point(Point::new(0.3, 0.0)),
            "hole remains missing copper"
        );
        assert!(
            !local.contains_point(Point::new(0.5, 0.0)),
            "enclosing plane is retained"
        );
    }
}
