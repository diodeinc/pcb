//! Minimum clearance from a circular drilled hole to unrelated final copper.
//!
//! For a drill disk `D(c, r)` and one attributed final copper image `M`, the
//! clearance is `dist(D, M) = max(0, dist(c, M) - r)`. The drill is analytic;
//! only the composed copper boundary contributes geometric uncertainty. Via
//! and PTH checks exclude copper proven to belong to the hole's scoped net or
//! a physically associated land, and copper a local NetShort lets meet it,
//! without merging the tied nets. NPTH checks exclude only, layer by layer,
//! the geometry of the lands the hole is drilled through by design
//! ([`Design::npth_lands`]), which the annular-ring rule judges instead. The
//! rest of that land's net is measured. Measurements are made only on copper layers in
//! the declared drill span.

use std::collections::VecDeque;

use pcb_ir::geom::dfm::{Distance, circular_region, region_clearance_sites_with_index};
use pcb_ir::geom::{BBox, ContourSet, Point, PreparedRegion};

use crate::commands::dfm::design::{ConductorId, CopperLayer, Design, HoleClass, spans};
use crate::commands::dfm::report::{Evidence, MeasurementKind};
use crate::commands::dfm::rules::Conditions;

use super::copper_clearance::{TiePermissions, conductor_subject};
use super::{
    Drilled, Evaluation, Measured, MeasuredSite, Ownership, hole_subject, layers,
    linework_clearance, spanned_layers, violates, without_own_lands,
};

/// Preparation shared by adjacent NPTH tiers in one rule-engine run. Keys
/// identify the immutable design, hole and layer, not just their geometry.
/// Retain at most 1,000,000 vertices and 16 entries, never hole×conductor results
/// for a whole design. An oversized result lives only until the next query.
#[derive(Default)]
pub(super) struct Preparation {
    entries: VecDeque<LandRemainders>,
    vertices: usize,
    uncached: Vec<(ConductorId, ContourSet, PreparedRegion)>,
}

struct LandRemainders {
    key: [usize; 3],
    window: BBox,
    vertices: usize,
    conductors: Vec<(ConductorId, ContourSet, PreparedRegion)>,
}

impl Preparation {
    fn remainders(
        &mut self,
        key: [usize; 3],
        copper: &CopperLayer,
        design: &Design,
        window: BBox,
    ) -> anyhow::Result<&[(ConductorId, ContourSet, PreparedRegion)]> {
        self.uncached.clear();
        if !design.npth_lands[key[1]]
            .iter()
            .any(|link| link.copper_index as usize == key[2])
        {
            return Ok(&[]);
        }
        if let Some(slot) = self.entries.iter().position(|entry| entry.key == key) {
            let entry = self.entries.remove(slot).unwrap();
            if entry.window.contains_point(window.min) && entry.window.contains_point(window.max) {
                self.entries.push_back(entry);
                return Ok(&self.entries.back().unwrap().conductors);
            }
            self.vertices -= entry.vertices;
        }
        let conductors = npth_land_remainders(key[1], key[2], copper, design, window)?;
        // Count empty conductor records too, bounding metadata independently
        // of whether removing a land left any boundary behind.
        let vertices = conductors
            .iter()
            .map(|(_, image, _)| 1 + image.rings().map(<[_]>::len).sum::<usize>())
            .sum::<usize>();
        if vertices > 1_000_000 {
            self.uncached = conductors;
            return Ok(&self.uncached);
        }
        while self.vertices + vertices > 1_000_000 || self.entries.len() == 16 {
            self.vertices -= self.entries.pop_front().unwrap().vertices;
        }
        self.vertices += vertices;
        self.entries.push_back(LandRemainders {
            key,
            window,
            vertices,
            conductors,
        });
        Ok(&self.entries.back().unwrap().conductors)
    }
}

pub(super) fn evaluate(
    limit_mm: f64,
    class: HoleClass,
    conditions: &Conditions,
    design: &Design,
    preparation: &mut Preparation,
    design_index: usize,
) -> anyhow::Result<Evaluation> {
    let mut checked = 0;
    let mut measured = Vec::new();
    // A nonplated hole has no net for a tie to name.
    let ties = match class {
        HoleClass::Npth | HoleClass::MouseBite => Vec::new(),
        HoleClass::Via | HoleClass::Pth => design
            .copper_layers
            .iter()
            .map(|layer| TiePermissions::of(design, layer))
            .collect::<anyhow::Result<Vec<_>>>()?,
    };

    // A placed hole is measured here too, against what its own Step's design
    // does not hold: this Step's copper and that of the other placements.
    for (hole_index, hole) in design
        .holes
        .iter()
        .enumerate()
        .filter(|(_, hole)| hole.class.against_neighbours() == class)
    {
        let radius_mm = hole.diameter_mm / 2.0;
        let owner = Ownership::of(
            design,
            hole.net,
            hole.padstack,
            hole.step,
            hole.provenance.instance_index,
            &design.hole_lands[hole_index],
            Drilled::Disk {
                center: hole.center,
                radius_mm,
            },
        );
        for (copper_index, copper) in spanned_layers(design, &hole.drill_span, conditions) {
            checked += usize::from(hole.branch.is_none());
            // An NPTH's own mounting land is not an offender; the rest of
            // that land's conductor is measured without it.
            // Only rings reaching the keepout can affect its measurements or
            // overlap sites. Keep an uncertainty/tolerance guard and retain
            // whole rings, so this introduces no artificial clipping edges.
            let window = hole.bbox.expand(
                limit_mm
                    + 2.0 * design.resolution.accuracy.max_error_mm()
                    + design
                        .resolution
                        .tolerance_mm
                        .max(pcb_ir::geom::tol::EPSILON_MM),
            );
            let land_remainders = match class {
                HoleClass::Npth | HoleClass::MouseBite => preparation.remainders(
                    [design_index, hole_index, copper_index],
                    copper,
                    design,
                    window,
                )?,
                HoleClass::Via | HoleClass::Pth => &[],
            };
            // Only a conductor whose bounds reach the keepout can enter it.
            let nearest = copper
                .conductors_near
                .query(hole.bbox.expand(limit_mm))
                .into_iter()
                .map(|index| {
                    let conductor = &copper.conductors[index];
                    match land_remainders.iter().find(|(id, ..)| *id == conductor.id) {
                        Some((_, image, boundary)) => (conductor, image, boundary),
                        None => (conductor, &conductor.image, conductor.boundary()),
                    }
                })
                .filter(|(conductor, ..)| spans(hole.branch, conductor.branch))
                .filter(|(conductor, ..)| class == HoleClass::Npth || !owner.owns(conductor.id))
                .filter_map(|(conductor, image, boundary)| {
                    disk_to_copper_clearance(hole.center, radius_mm, image, boundary, limit_mm)
                        .map(|distance| (conductor, image, boundary, distance))
                })
                .map(|(conductor, image, boundary, distance)| {
                    let tied = match ties.get(copper_index) {
                        Some(ties) => owner.tied(ties, copper, conductor.id, distance.second)?,
                        None => false,
                    };
                    Ok((!tied).then_some((conductor, image, boundary, distance)))
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .min_by(|(.., left), (.., right)| left.mm.total_cmp(&right.mm));

            let Some((offender, offender_image, boundary, distance)) = nearest else {
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
            let mut sites = Vec::new();
            if violates(&distance, limit_mm) {
                let drill = circular_region(hole.center, radius_mm, design.resolution)?;
                sites = linework_clearance::report_sites(
                    region_clearance_sites_with_index(
                        &drill,
                        &drill.prepare_query(),
                        offender_image,
                        boundary,
                        limit_mm,
                    )?,
                    &finding_layers,
                    limit_mm,
                );
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
                sites,
            });
        }
    }

    Ok(Evaluation { checked, measured })
}

/// The conductors on `copper` that hold an intentional land of the
/// nonplated hole ([`Design::npth_lands`]), each without that land. Only the
/// land's geometry is exempt, never its net.
fn npth_land_remainders(
    hole_index: usize,
    copper_index: usize,
    copper: &CopperLayer,
    design: &Design,
    window: BBox,
) -> anyhow::Result<Vec<(ConductorId, ContourSet, PreparedRegion)>> {
    let lands = design.npth_lands[hole_index]
        .iter()
        .filter(|link| link.copper_index as usize == copper_index)
        .map(|link| &copper.lands[link.land_index as usize])
        .collect::<Vec<_>>();
    if lands.is_empty() {
        return Ok(Vec::new());
    }
    copper
        .conductors
        .iter()
        .filter_map(|conductor| {
            let image = without_own_lands(
                conductor,
                lands.iter().copied(),
                design.resolution,
                Some(window),
            );
            image.transpose().map(|image| {
                let image = image?;
                let boundary = image.prepare_query();
                Ok((conductor.id, image, boundary))
            })
        })
        .collect()
}

fn disk_to_copper_clearance(
    center: Point,
    radius_mm: f64,
    copper: &pcb_ir::geom::ContourSet,
    boundary: &pcb_ir::geom::PreparedRegion,
    limit_mm: f64,
) -> Option<Distance> {
    // Match ContourSet's closed, tolerance-inclusive containment, but reuse
    // the conductor's boundary hierarchy instead of scanning every edge for
    // every hole. Keep its exact bounds gate, including near an outer edge.
    // Only boundary proximity within the tolerance matters for containment;
    // do not compute an unrestricted nearest distance just to test the sign.
    let epsilon = copper.tolerance().max(pcb_ir::geom::tol::EPSILON_MM);
    if copper.bbox.contains_point(center)
        && (boundary.contains_regular_point(center)
            || boundary
                .nearest_within(center, epsilon)
                .is_some_and(|distance| distance.mm <= epsilon))
    {
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
            let mut passes = checked.scene.as_ref().unwrap().passes.iter();
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
    fn npth_exempts_only_its_own_mounting_land_geometry() {
        // A 1 mm NPTH through a 1.2 mm mounting land of net MH on L0.
        let pad = |x: f64, primitive: &str, pin: bool| {
            let pin = if pin {
                r#"<PinRef componentRef="H1" pin="1"/>"#
            } else {
                ""
            };
            format!(
                r#"<LayerFeature layerRef="L0"><Set net="MH" polarity="POSITIVE"><Pad padstackDefRef="land-stack"><Location x="{x}" y="0"/>{primitive}{pin}</Pad></Set></LayerFeature>"#
            )
        };
        let disk = r#"<StandardPrimitiveRef id="land"/>"#;
        let land = |x: f64, pin: bool| pad(x, disk, pin);
        let run = |features: &[String]| {
            let xml = board("NONPLATED", Some((0, 2)), features)
                .replace(
                    r#"<LayerRef name="DRILL"/>"#,
                    r#"<LayerRef name="DRILL"/><DictionaryStandard units="MILLIMETER"><EntryStandard id="land"><Circle diameter="1.2"/></EntryStandard></DictionaryStandard>
    <DictionaryUser units="MILLIMETER"><EntryUser id="ring"><UserSpecial><Circle diameter="1.4"><LineDesc lineWidth="0.3" lineEnd="ROUND"/><FillDesc fillProperty="HOLLOW"/></Circle></UserSpecial></EntryUser></DictionaryUser>"#,
                )
                .replace(
                    r#"<Datum x="0" y="0"/>"#,
                    r#"<Datum x="0" y="0"/><PadStackDef name="land-stack"><PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef></PadStackDef>"#,
                );
            // Each tier has its own keepout and sites, but prepares the same
            // land remainders. The 0.12/0.15 mm cases below pass the minimum
            // and fail only the preferred tier; the 0.05/0.10 mm cases fail both.
            let tiers = fixtures::run_board(
                &xml,
                &pdk("npth").replace(
                    "minimum = \"0.20 mm\"",
                    "minimum = \"0.11 mm\", preferred = \"0.20 mm\"",
                ),
            );
            assert_eq!(tiers.rules.len(), 2);
            assert!(tiers.rules.iter().all(|rule| rule.checked == 3));
            tiers
                .findings
                .iter()
                .map(|finding| {
                    let limit = match finding.rule_id.as_str() {
                        "hole-clearance" => 0.11,
                        "hole-clearance.preferred" => 0.20,
                        other => panic!("unexpected rule {other}"),
                    };
                    let actual = finding.measurement.actual_mm().unwrap();
                    assert!(actual < limit);
                    assert_eq!(actual < 0.11, finding.rule_id == "hole-clearance");
                    for site in &finding.sites {
                        let keepout = site
                            .evidence
                            .iter()
                            .find(|evidence| evidence.role == "required_copper_keepout")
                            .unwrap();
                        assert_eq!(keepout.shape.diameter, Some(1.0 + 2.0 * limit));
                    }
                    (
                        finding.layers[1].name.clone(),
                        finding.subjects[1].net.clone().unwrap_or_default(),
                        (finding.measurement.actual_mm().unwrap() * 1e6).round() / 1e6,
                    )
                })
                .collect::<Vec<_>>()
        };
        let found = |layer: &str, net: &str, mm: f64| vec![(layer.to_owned(), net.to_owned(), mm)];
        // A same-net 1 × 1 mm square from x = `x`, apart from the land or not.
        let same_net = |x: f64| copper(0, Some("MH"), x);

        assert_eq!(run(&[land(0.0, true)]), []);
        // Bare copper is no land, nor, for a free hole, an off-centre pad.
        assert_eq!(run(&[land(0.0, false)]), found("L0", "MH", 0.0));
        assert_eq!(run(&[land(0.1, true)]), found("L0", "MH", 0.0));
        // A ring whose bounds span the drill but whose copper does not cover
        // it is not a land the hole is drilled through: its flattened inner
        // edge is about 0.05 mm away.
        let ring = r#"<UserPrimitiveRef id="ring"/>"#;
        let [(layer, net, mm)] = run(&[pad(0.0, ring, true)]).try_into().unwrap();
        assert_eq!((layer.as_str(), net.as_str()), ("L0", "MH"));
        assert!((0.04..=0.05).contains(&mm), "{mm}");
        // The land's net is not exempt: separate same-net copper 0.12 mm
        // from the drill, and same-net copper joined to the land but 0.10 mm
        // from the drill beyond it, both still violate.
        assert_eq!(
            run(&[land(0.0, true), same_net(0.62)]),
            found("L0", "MH", 0.12)
        );
        // This component is outside the minimum tier's guarded window but
        // inside the preferred tier. Reusing the smaller remainder would
        // silently lose the preferred finding.
        assert_eq!(
            run(&[land(0.0, true), same_net(0.67)]),
            found("L0", "MH", 0.17)
        );
        let [(layer, net, mm)] = run(&[land(0.0, true), same_net(0.55)]).try_into().unwrap();
        assert_eq!((layer.as_str(), net.as_str()), ("L0", "MH"));
        assert!(
            (0.095..=0.1).contains(&mm),
            "the flattened land's edge: {mm}"
        );
        // Other copper near the hole is measured as before: another net on
        // the land's layer, and the land's own net where it has no land.
        assert_eq!(
            run(&[land(0.0, true), copper(0, Some("N2"), 0.65)]),
            found("L0", "N2", 0.15)
        );
        assert_eq!(
            run(&[land(0.0, true), copper(1, Some("MH"), 0.55)]),
            found("L1", "MH", 0.05)
        );
        // Both layers have a land but different remaining copper. Sharing
        // preparation must not substitute one layer's remainder for another.
        let second_land = land(0.0, true).replace("layerRef=\"L0\"", "layerRef=\"L1\"");
        assert_eq!(
            run(&[
                land(0.0, true),
                second_land,
                same_net(0.62),
                copper(1, Some("MH"), 0.65),
            ]),
            vec![
                ("L0".to_owned(), "MH".to_owned(), 0.12),
                ("L1".to_owned(), "MH".to_owned(), 0.15),
            ]
        );
    }

    #[test]
    fn npth_land_is_known_by_identity_and_geometry_not_by_its_anchor() {
        // A 1 mm NPTH drilled by padstack `hole-stack`, whose instance pin
        // names component H1. Its land is a separate H1 pad anchored
        // `anchor` mm off the hole: a 2 mm square (`outer` mm half-width)
        // centred on the hole, with a square bore of `bore` mm half-width.
        let ring = |anchor: f64, outer: f64, bore: f64, shift: f64| {
            let (lo, hi) = (shift - outer - anchor, shift + outer - anchor);
            let (blo, bhi) = (-bore - anchor, bore - anchor);
            format!(
                r#"<EntryUser id="ring"><UserSpecial><Contour><Polygon>
                <PolyBegin x="{lo}" y="{lo2}"/><PolyStepSegment x="{hi}" y="{lo2}"/>
                <PolyStepSegment x="{hi}" y="{hi2}"/><PolyStepSegment x="{lo}" y="{hi2}"/>
                <PolyStepSegment x="{lo}" y="{lo2}"/></Polygon>
                <Cutout><PolyBegin x="{blo}" y="{bore_lo}"/><PolyStepSegment x="{bhi}" y="{bore_lo}"/>
                <PolyStepSegment x="{bhi}" y="{bore}"/><PolyStepSegment x="{blo}" y="{bore}"/>
                <PolyStepSegment x="{blo}" y="{bore_lo}"/></Cutout>
                </Contour></UserSpecial></EntryUser>"#,
                lo2 = -outer,
                hi2 = outer,
                bore_lo = -bore,
            )
        };
        let xml = |anchor: f64,
                   outer: f64,
                   bore: f64,
                   shift: f64,
                   owner: &str,
                   claims: &[&str],
                   extra: &[String]| {
            let claims = claims
                .iter()
                .map(|component| {
                    format!(
                        r#"<Set polarity="POSITIVE"><Pad padstackDefRef="hole-stack"><Location x="0" y="0"/><StandardPrimitiveRef id="bore"/><PinRef componentRef="{component}" pin="NPTH0"/></Pad></Set>"#
                    )
                })
                .collect::<String>();
            let land = format!(
                r#"<LayerFeature layerRef="SM">{claims}</LayerFeature><LayerFeature layerRef="L0"><Set net="MH" polarity="POSITIVE"><Pad padstackDefRef="ring-stack"><Location x="{anchor}" y="0"/><UserPrimitiveRef id="ring"/><PinRef componentRef="{owner}" pin="1"/></Pad></Set></LayerFeature>"#
            );
            let features = std::iter::once(land)
                .chain(extra.iter().cloned())
                .collect::<Vec<_>>();
            board("NONPLATED", Some((0, 2)), &features)
                .replace(
                    r#"<Layer name="DRILL""#,
                    r#"<Layer name="SM" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
    <Layer name="DRILL""#,
                )
                .replace(
                    r#"<LayerRef name="DRILL"/>"#,
                    &format!(
                        r#"<LayerRef name="SM"/><LayerRef name="DRILL"/><DictionaryStandard units="MILLIMETER"><EntryStandard id="bore"><Circle diameter="1"/></EntryStandard></DictionaryStandard>
    <DictionaryUser units="MILLIMETER">{}</DictionaryUser>"#,
                        ring(anchor, outer, bore, shift)
                    ),
                )
                .replace(
                    r#"<Datum x="0" y="0"/>"#,
                    r#"<Datum x="0" y="0"/><PadStackDef name="hole-stack"><PadstackHoleDef name="H" diameter="1" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="0" y="0"/><PadstackPadDef layerRef="SM" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="bore"/></PadstackPadDef></PadStackDef>
      <PadStackDef name="ring-stack"><PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><UserPrimitiveRef id="ring"/></PadstackPadDef></PadStackDef>"#,
                )
                .replace(
                    r#"<LayerFeature layerRef="DRILL"><Set net="N1" polarity="POSITIVE">"#,
                    r#"<LayerFeature layerRef="DRILL"><Set geometry="hole-stack" polarity="POSITIVE">"#,
                )
        };
        let findings = |xml: &str| {
            run(xml, "npth")
                .findings
                .iter()
                .map(|finding| {
                    (
                        finding.subjects[1].net.clone().unwrap_or_default(),
                        (finding.measurement.actual_mm().unwrap() * 1e3).round() / 1e3,
                    )
                })
                .collect::<Vec<_>>()
        };
        let reported = |net: &str, mm: f64| vec![(net.to_owned(), mm)];

        assert_eq!(findings(&xml(2.4, 1.0, 0.3, 0.0, "H1", &["H1"], &[])), []);
        assert_eq!(
            findings(&xml(2.4, 1.0, 0.3, 0.0, "H2", &["H1"], &[])),
            reported("MH", 0.0)
        );
        assert_eq!(
            findings(&xml(2.4, 1.0, 0.3, 0.0, "H1", &["H1", "H2"], &[])),
            reported("MH", 0.0)
        );
        // A bore circumscribing the drill, as a polygonized circle does.
        assert_eq!(findings(&xml(2.4, 1.0, 0.5, 0.0, "H1", &["H1"], &[])), []);
        assert_eq!(
            findings(&xml(2.4, 1.0, 0.52, 0.0, "H1", &["H1"], &[])),
            reported("MH", 0.02)
        );
        assert_eq!(
            findings(&xml(2.4, 1.0, 0.6, 0.0, "H1", &["H1"], &[])),
            reported("MH", 0.1)
        );
        assert_eq!(
            findings(&xml(2.4, 1.0, 0.3, 0.6, "H1", &["H1"], &[])),
            reported("MH", 0.0)
        );
        // Only the land is exempt: copper beside it is still measured.
        assert_eq!(findings(&xml(2.4, 1.0, 0.3, -0.45, "H1", &["H1"], &[])), []);
        for net in ["MH", "N2"] {
            let beside = copper(0, Some(net), 0.62);
            let found = findings(&xml(2.4, 1.0, 0.3, -0.45, "H1", &["H1"], &[beside]));
            assert_eq!(found, reported(net, 0.12), "{net}");
        }

        // The NPTH annular-ring rule measures the recognized land.
        let ring_pdk = fixtures::pdk(
            r#"[[rules.copper.annular_ring]]
id = "npth-ring"
select = { hole = "npth" }
limit = { minimum = "0.45 mm" }"#,
        );
        let rings = |xml: &str| {
            let results = fixtures::run_board(xml, &ring_pdk);
            let rings = results
                .findings
                .iter()
                .map(|finding| (finding.measurement.actual_mm().unwrap() * 1e3).round() / 1e3)
                .collect::<Vec<_>>();
            (results.rules[0].checked, rings)
        };
        assert_eq!(
            rings(&xml(2.4, 1.0, 0.3, 0.0, "H1", &["H1"], &[])),
            (1, vec![])
        );
        assert_eq!(
            rings(&xml(2.4, 1.0, 0.3, -0.45, "H1", &["H1"], &[])),
            (1, vec![0.05])
        );
        assert_eq!(rings(&xml(2.4, 1.0, 0.3, 0.0, "H2", &["H1"], &[])).0, 0);
        // Copper a clear feature removes from the land is no ring.
        let clear = r#"<LayerFeature layerRef="L0"><Set polarity="NEGATIVE"><Features><Contour><Polygon>
              <PolyBegin x="0.55" y="-2"/><PolyStepSegment x="3" y="-2"/>
              <PolyStepSegment x="3" y="2"/><PolyStepSegment x="0.55" y="2"/>
              <PolyStepSegment x="0.55" y="-2"/>
            </Polygon></Contour></Features></Set></LayerFeature>"#;
        assert_eq!(
            rings(&xml(2.4, 1.0, 0.3, 0.0, "H1", &["H1"], &[clear.to_owned()])),
            (1, vec![0.05])
        );
    }

    #[test]
    fn a_via_inside_a_tied_pad_joins_the_tie_by_its_drill_not_by_proximity() {
        // NT1's 3 × 3 mm pad of net `pad_net`, centred at `pad_x`, is tied
        // to NT1's N2 graphic that overlaps it from x = 0.6. The 1 mm via of
        // net `via_net` at the origin is 0.1 mm from the graphic.
        let xml = |via_net: &str, pad_net: &str, pad_x: f64, tie: bool| {
            let tie = if tie {
                r#"<NetShort><NetRef name="N1"/><NetRef name="N2"/><Location x="1.2" y="0"/><LayerRef name="L0"/></NetShort>"#
            } else {
                ""
            };
            let features = [format!(
                r#"<LayerFeature layerRef="L0">
              <Set net="{pad_net}" polarity="POSITIVE"><Pad padstackDefRef="tie-stack"><Location x="{pad_x}" y="0"/><StandardPrimitiveRef id="tie"/><PinRef componentRef="NT1" pin="1"/></Pad></Set>
              <Set net="N2" geometryUsage="GRAPHIC" componentRef="NT1" polarity="POSITIVE">{tie}<Features><Contour><Polygon>
                <PolyBegin x="0.6" y="-0.5"/><PolyStepSegment x="3" y="-0.5"/>
                <PolyStepSegment x="3" y="0.5"/><PolyStepSegment x="0.6" y="0.5"/>
                <PolyStepSegment x="0.6" y="-0.5"/>
              </Polygon></Contour></Features></Set></LayerFeature>"#
            )];
            board("VIA", Some((0, 2)), &features)
                .replace(
                    r#"<LayerRef name="DRILL"/>"#,
                    r#"<LayerRef name="DRILL"/><DictionaryStandard units="MILLIMETER"><EntryStandard id="tie"><RectCenter width="3" height="3"/></EntryStandard></DictionaryStandard>"#,
                )
                .replace(
                    r#"<Datum x="0" y="0"/>"#,
                    r#"<Datum x="0" y="0"/><PadStackDef name="tie-stack"><PadstackPadDef layerRef="L0" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="tie"/></PadstackPadDef></PadStackDef>"#,
                )
                .replace(r#"<Set net="N1" polarity="POSITIVE">"#, &format!(r#"<Set net="{via_net}" polarity="POSITIVE">"#))
        };
        let offenders = |xml: &str| {
            let mut offenders = run(xml, "via")
                .findings
                .iter()
                .map(|finding| {
                    (
                        finding.subjects[1].net.clone().unwrap_or_default(),
                        (finding.measurement.actual_mm().unwrap() * 1e3).round() / 1e3,
                    )
                })
                .collect::<Vec<_>>();
            offenders.sort_by(|left, right| left.0.cmp(&right.0));
            offenders
        };
        let graphic = [("N2".to_owned(), 0.1)];

        assert_eq!(offenders(&xml("N1", "N1", 0.0, false)), graphic.clone());
        assert_eq!(offenders(&xml("N1", "N1", 0.0, true)), []);
        // Moved so the pad no longer holds the drill.
        assert_eq!(offenders(&xml("N1", "N1", 1.2, true)), graphic.clone());
        // The tie permits the graphic, not the N2 net.
        let untied = xml("N1", "N1", 0.0, true).replace(
            "</Step>",
            &format!("{}</Step>", copper(0, Some("N2"), -1.65)),
        );
        assert_eq!(offenders(&untied), [("N2".to_owned(), 0.15)]);
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
