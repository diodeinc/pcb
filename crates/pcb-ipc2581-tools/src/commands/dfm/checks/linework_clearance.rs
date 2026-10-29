//! Minimum clearance from reference linework to copper.
//!
//! The reference is a set of segments `S = {s₁ … sₙ}` — a V-score tool
//! centerline, or a board profile's outer and cutout rings — and the target
//! is one layer's composed copper image `M` of the design's own Step, a
//! regularized closed filled region. A V-score line is a reference for every
//! Step it crosses: the one that draws it and each one placed under that,
//! which meets it as [`Design::inherited_scores`]. The measured quantity is
//! the Euclidean set distance
//!
//! ```text
//! dist(S, M) = minᵢ inf { ‖x − y‖ : x ∈ sᵢ, y ∈ M },
//! ```
//!
//! and the check requires `dist(S, M) ≥ L` per (reference item, layer).
//!
//! Because `M` is closed and filled, the distance decomposes exactly:
//! `dist(S, M) = 0` iff some segment meets `M`, which happens iff a segment
//! endpoint lies in `M` (winding number, batched per layer) or a segment
//! crosses `∂M` (a segment with both endpoints outside a filled region
//! intersects it iff it crosses its boundary). Otherwise `S` is disjoint
//! from `M` and `dist(S, M) = dist(S, ∂M)`, the minimum segment-to-segment
//! distance against the indexed boundary, searched only within `L` since
//! farther boundaries cannot violate. A profile's rings are flattened
//! curves and count toward the measurement's uncertainty; a score line is
//! exact.

use std::ops::Range;

use pcb_ir::geom::dfm::{ClearanceSite, Distance, linework_clearance_sites};
use pcb_ir::geom::region::PaintComposer;
use pcb_ir::geom::region::ring_edges;
use pcb_ir::geom::{BBox, ContourSet, Point, Polarity, PreparedRegion};

use crate::accessors::castellations;
use crate::commands::dfm::design::{BoardOutline, CopperLayer, Design, HoleClass, Score};
use crate::commands::dfm::report::{
    Evidence, LayerRef, MeasurementKind, ReportPoint, SourceLocator, Subject,
};
use crate::commands::dfm::rules::{Conditions, Linework};

use super::{Evaluation, Measured, MeasuredSite, layers, violates, without_own_lands};

/// One reference item: a V-score centerline or a board outline, as a range
/// of bare segments plus its report identity.
struct LineworkItem {
    segments: Range<usize>,
    uncertainty_mm: f64,
    layer: Option<LayerRef>,
    subject: Subject,
}

/// The reference items and their segments, flattened into one pool so the
/// endpoint containment sweep runs once per layer.
#[derive(Default)]
struct LineworkPool {
    items: Vec<LineworkItem>,
    segments: Vec<(Point, Point)>,
}

impl LineworkPool {
    fn push(
        &mut self,
        segments: Vec<(Point, Point)>,
        item: impl FnOnce(Range<usize>) -> LineworkItem,
    ) {
        let start = self.segments.len();
        self.segments.extend(segments);
        self.items.push(item(start..self.segments.len()));
    }

    fn push_score(&mut self, score: &Score) {
        self.push(vec![(score.start, score.end)], |segments| LineworkItem {
            segments,
            uncertainty_mm: 0.0,
            layer: Some(score.layer.clone()),
            subject: Subject {
                role: "reference",
                kind: "vscore_centerline",
                name: Some(score.layer.name.clone()),
                provenance: Some(score.provenance.clone()),
                ..Subject::default()
            },
        });
    }
}

/// The reference items, pooled by the placements of the design's Step they
/// exist at: all of them (`None`) for what the Step draws itself, and those
/// a line from above crosses the Step at.
fn linework_pools(linework: Linework, design: &Design) -> Vec<(Option<Vec<u32>>, LineworkPool)> {
    let mut pools = vec![(None, LineworkPool::default())];
    match linework {
        Linework::VScore => {
            for score in &design.scores {
                pools[0].1.push_score(score);
            }
            for (score, placements) in &design.inherited_scores {
                let at = Some(placements);
                let pool = match pools.iter().position(|(known, _)| known.as_ref() == at) {
                    Some(pool) => pool,
                    None => {
                        pools.push((at.cloned(), LineworkPool::default()));
                        pools.len() - 1
                    }
                };
                pools[pool].1.push_score(score);
            }
        }
        Linework::BoardEdge => {
            for outline in design
                .board_outlines
                .iter()
                .filter(|outline| outline.is_board())
            {
                pools[0].1.push(
                    outline.region.rings.iter().flat_map(ring_edges).collect(),
                    |segments| LineworkItem {
                        segments,
                        uncertainty_mm: outline.region.uncertainty_mm,
                        layer: None,
                        subject: outline_subject(outline, "reference"),
                    },
                );
            }
        }
    }
    pools.retain(|(_, pool)| !pool.items.is_empty());
    pools
}

/// Every pool's evaluation, with the placements it holds at.
pub(super) fn evaluate(
    limit_mm: f64,
    linework: Linework,
    conditions: &Conditions,
    design: &Design,
) -> anyhow::Result<Vec<(Option<Vec<u32>>, Evaluation)>> {
    let cuts = match linework {
        Linework::BoardEdge => castellation_cuts(design)?,
        Linework::VScore => Vec::new(),
    };
    linework_pools(linework, design)
        .into_iter()
        .map(|(placements, pool)| {
            Ok((
                placements,
                measure(limit_mm, &pool, conditions, design, &cuts)?,
            ))
        })
        .collect()
}

/// Each copper layer's image without the lands of castellated holes, where
/// the board has castellations: a plated half-hole's own land meets the
/// edge by design. The lands are those the hole is linked to by identity,
/// as [`Ownership`](super::Ownership) reads them; their traces, pours and
/// every other copper on the layer are measured as before. `None` leaves a
/// layer's image whole, as does a design whose holes or their lands could
/// not be extracted: castellations are then measured like other copper.
fn castellation_cuts(design: &Design) -> anyhow::Result<Vec<Option<(ContourSet, PreparedRegion)>>> {
    let edges = design
        .board_outlines
        .iter()
        .filter(|outline| outline.is_board())
        .collect::<Vec<_>>();
    let castellated = design
        .holes
        .iter()
        .enumerate()
        .filter(|(_, hole)| {
            hole.class == HoleClass::Pth
                && hole.branch.is_none()
                && edges.iter().any(|outline| {
                    outline
                        .boundary
                        .signed_distance(hole.center)
                        .is_some_and(|distance| {
                            castellations::on_edge(distance.mm, hole.diameter_mm)
                        })
                })
        })
        .collect::<Vec<_>>();
    let mut lands = vec![Vec::new(); design.copper_layers.len()];
    if castellated.len() >= castellations::MIN_CASTELLATED_HOLES {
        for (hole_index, hole) in castellated {
            // Without linked lands (their pool unbuilt), nothing is exempt.
            for link in design.hole_lands.get(hole_index).into_iter().flatten() {
                let land = &design.copper_layers[link.copper_index as usize].lands
                    [link.land_index as usize];
                if hole.padstack == Some(land.padstack)
                    && hole.net.zip(land.net).is_none_or(|(own, land)| own == land)
                    && land.image.is_some()
                {
                    lands[link.copper_index as usize].push(land);
                }
            }
        }
    }
    design
        .copper_layers
        .iter()
        .zip(lands)
        .map(|(copper, lands)| {
            if lands.is_empty() {
                return Ok(None);
            }
            let mut image = PaintComposer::new(design.resolution);
            for conductor in copper
                .conductors
                .iter()
                .filter(|conductor| conductor.id.instance().is_none())
            {
                let own = without_own_lands(conductor, lands.iter().copied(), design.resolution)?;
                image.push(
                    Polarity::Dark,
                    own.unwrap_or_else(|| conductor.image.clone()),
                );
            }
            let image = image.finish()?;
            let boundary = image.prepare_query();
            Ok(Some((image, boundary)))
        })
        .collect()
}

fn measure(
    limit_mm: f64,
    pool: &LineworkPool,
    conditions: &Conditions,
    design: &Design,
    cuts: &[Option<(ContourSet, PreparedRegion)>],
) -> anyhow::Result<Evaluation> {
    let copper_layers = &design.copper_layers;
    let (items, segments) = (&pool.items, &pool.segments);
    let endpoints = segments
        .iter()
        .flat_map(|&(start, end)| [start, end])
        .collect::<Vec<_>>();

    let mut measured = Vec::new();
    for (copper_index, copper) in copper_layers
        .iter()
        .enumerate()
        .filter(|(_, copper)| conditions.applies_to_layer(copper))
    {
        let (image, boundary) = match cuts.get(copper_index) {
            Some(Some((image, boundary))) => (image, boundary),
            _ => (&copper.image, &design.copper_boundaries[copper_index]),
        };
        let inside = image.contains_points_batch(&endpoints);
        for item in items {
            let Some(nearest) = item
                .segments
                .clone()
                .filter_map(|segment_index| {
                    let (start, end) = segments[segment_index];
                    let (start_inside, end_inside) =
                        (inside[2 * segment_index], inside[2 * segment_index + 1]);
                    // A segment touching copper measures zero at the
                    // contained endpoint; otherwise both ends are outside
                    // and the boundary distance is the set distance.
                    match (start_inside, end_inside) {
                        (true, _) => Some(Distance::with_uncertainty(
                            0.0,
                            start,
                            start,
                            image.uncertainty_mm,
                        )),
                        (_, true) => Some(Distance::with_uncertainty(
                            0.0,
                            end,
                            end,
                            image.uncertainty_mm,
                        )),
                        _ => boundary.segment_nearest_within(start, end, limit_mm),
                    }
                })
                .min_by(|left, right| left.mm.total_cmp(&right.mm))
            else {
                continue;
            };
            let distance = nearest.also_uncertain(item.uncertainty_mm);
            let site_layers = layers(item.layer.iter().chain([&copper.layer]));
            let sites = if violates(&distance, limit_mm) {
                let geometry = linework_clearance_sites(
                    &segments[item.segments.clone()],
                    image,
                    boundary,
                    limit_mm,
                    item.uncertainty_mm,
                );
                report_sites(geometry, &site_layers, limit_mm)
            } else {
                Vec::new()
            };
            measured.push(Measured {
                distance,
                bbox: BBox::spanning(distance.first, distance.second),
                layers: site_layers,
                subjects: vec![item.subject.clone(), copper_subject(copper)],
                sites,
            });
        }
    }
    Ok(Evaluation {
        checked: copper_layers
            .iter()
            .filter(|layer| conditions.applies_to_layer(layer))
            .count()
            * items.len(),
        measured,
    })
}

/// The report sites of a violating measurement's local clearance geometry.
pub(super) fn report_sites(
    geometry: Vec<ClearanceSite>,
    layers: &[LayerRef],
    limit_mm: f64,
) -> Vec<MeasuredSite> {
    geometry
        .into_iter()
        .map(|site| report_site(site, layers.to_vec(), limit_mm))
        .collect()
}

/// All clearance families share the same local path/constraint construction;
/// the rule and inherited subjects give these boundaries their physical roles.
fn report_site(geometry: ClearanceSite, layers: Vec<LayerRef>, limit_mm: f64) -> MeasuredSite {
    let mut evidence = [
        ("first_boundary", &geometry.first_paths),
        ("second_boundary", &geometry.second_paths),
    ]
    .into_iter()
    .filter(|(_, paths)| !paths.is_empty())
    .map(|(role, paths)| Evidence::path(role, joined_boundary_paths(paths)))
    .collect::<Vec<_>>();
    // The band is everything within the limit of the first boundary.
    let mut bbox = geometry.bbox;
    let band = joined_boundary_paths(&geometry.first_paths);
    if band.iter().any(|path| path.len() >= 2) {
        for point in band.iter().flatten() {
            bbox = bbox.union(BBox::from_point(Point::new(point.x, point.y)).expand(limit_mm));
        }
        evidence.push(Evidence::stroke(
            "required_clearance_band",
            band,
            2.0 * limit_mm,
        ));
    }
    let overlaps = !geometry.overlap.is_empty();
    if overlaps {
        evidence.push(Evidence::region("overlap_region", &geometry.overlap));
    }
    let mut site = MeasuredSite::new(
        geometry.distance,
        bbox,
        layers,
        evidence,
        if geometry.distance.mm == 0.0 {
            MeasurementKind::Overlap
        } else {
            MeasurementKind::Clearance
        },
    );
    if overlaps {
        site.note = Some("The filled regions overlap; their clearance is zero.".to_owned());
    } else if geometry.distance.mm == 0.0 {
        site.note = Some("The subjects touch or intersect; their clearance is zero.".to_owned());
    } else {
        site.note = Some("Highlighted boundary spans are below the limit after accounting for geometric uncertainty.".to_owned());
    }
    site
}

/// Consecutive open paths that share an exact endpoint describe the same
/// directed segments as one polyline. Retain every vertex and discontinuity;
/// only the duplicate junction coordinate and path container disappear.
fn joined_boundary_paths(paths: &[Vec<Point>]) -> Vec<Vec<ReportPoint>> {
    let mut joined: Vec<Vec<ReportPoint>> = Vec::new();
    let mut previous_end = None;
    for path in paths {
        let continuation = !path.is_empty() && previous_end == path.first().copied();
        if !continuation {
            joined.push(Vec::new());
        }
        joined.last_mut().expect("a path was started").extend(
            path.iter()
                .skip(usize::from(continuation))
                .copied()
                .map(ReportPoint::from),
        );
        previous_end = path.last().copied();
    }
    joined
}

fn copper_subject(copper: &CopperLayer) -> Subject {
    Subject {
        role: "offender",
        kind: "copper_image",
        name: Some(copper.layer.name.clone()),
        ..Subject::default()
    }
}

pub(super) fn outline_subject(outline: &BoardOutline, role: &'static str) -> Subject {
    let locator = SourceLocator {
        step: Some(outline.name.clone()),
        layer: None,
        set_index: None,
        feature_index: None,
        instance_index: None,
    };
    Subject {
        role,
        kind: "board_outline",
        name: Some(outline.name.clone()),
        source: Some(locator.clone()),
        provenance: Some(locator),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearance_display_keeps_certified_spans_and_an_exact_round_stroke() {
        let first_paths = vec![
            vec![Point::new(1.0, 2.0), Point::new(3.0, 2.0)],
            vec![Point::new(3.0, 2.0), Point::new(3.0, 4.0)],
            vec![Point::new(8.0, 8.0), Point::new(9.0, 9.0)],
        ];
        let geometry = ClearanceSite {
            distance: Distance::with_uncertainty(
                0.1,
                Point::new(3.0, 2.0),
                Point::new(3.1, 2.0),
                0.005,
            ),
            bbox: BBox::new(Point::new(1.0, 2.0), Point::new(9.0, 9.0)),
            first_paths,
            second_paths: vec![vec![Point::new(3.1, 2.0), Point::new(3.1, 4.0)]],
            overlap: pcb_ir::geom::ContourSet::empty(pcb_ir::geom::Resolution::default()),
        };
        let site = report_site(geometry, Vec::new(), 0.2);
        let band = site
            .evidence
            .iter()
            .find(|evidence| evidence.role == "required_clearance_band")
            .unwrap();
        assert_eq!(band.shape.kind, "stroke");
        assert_eq!(
            band.shape.width_mm,
            Some(0.4),
            "the band extends the full limit on each side"
        );
        let paths = &band.shape.paths;
        assert_eq!(paths.len(), 2, "disconnected spans must stay disconnected");
        assert_eq!(
            paths[0].iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            vec![(1.0, 2.0), (3.0, 2.0), (3.0, 4.0)]
        );
        assert_eq!(
            paths[1].iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            vec![(8.0, 8.0), (9.0, 9.0)]
        );
        assert_eq!(site.distance.mm, 0.1);
        assert_eq!(site.distance.uncertainty_mm, 0.005);
    }

    #[test]
    fn boundary_path_joining_preserves_directed_segments_and_discontinuities() {
        let point = |x| Point::new(x, 0.0);
        let paths = vec![
            vec![point(0.0), point(1.0)],
            vec![point(1.0), point(2.0)],
            vec![point(4.0), point(5.0)],
            vec![point(5.0), point(4.0)],
            vec![point(4.0 + 1e-12), point(6.0)],
        ];
        let joined = joined_boundary_paths(&paths)
            .into_iter()
            .map(|path| {
                path.into_iter()
                    .map(|p| Point::new(p.x, p.y))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(joined.len(), 3);
        assert_eq!(joined[0], vec![point(0.0), point(1.0), point(2.0)]);
        assert_eq!(joined[1], vec![point(4.0), point(5.0), point(4.0)]);
        let segments = |paths: &[Vec<Point>]| {
            paths
                .iter()
                .flat_map(|path| path.windows(2).map(|pair| (pair[0], pair[1])))
                .collect::<Vec<_>>()
        };
        assert_eq!(segments(&paths), segments(&joined));
    }

    /// A 10 mm board whose left edge at x = 0 cuts plated half-holes at
    /// each `y`, each on a 2 × 1 mm land of its own net, plus `extra`.
    fn castellated(ys: &[f64], extra: &str) -> String {
        let (mut lands, mut holes) = (String::new(), String::new());
        for (index, y) in ys.iter().enumerate() {
            lands.push_str(&format!(
                r#"<Set net="N{index}"><Pad padstackDefRef="castle"><Location x="0" y="{y}"/><StandardPrimitiveRef id="land"/><PinRef componentRef="J1" pin="{index}"/></Pad></Set>"#
            ));
            holes.push_str(&format!(
                r#"<Set geometry="castle" net="N{index}"><Hole name="H{index}" diameter="0.6" platingStatus="PLATED" x="0" y="{y}"/></Set>"#
            ));
        }
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
    <LayerRef name="TOP"/><LayerRef name="DRILL"/>
    <DictionaryStandard units="MILLIMETER"><EntryStandard id="land"><RectCenter width="2" height="1"/></EntryStandard></DictionaryStandard>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="TOP"/></Layer>
    <Step name="board" type="BOARD">
      <Profile><Polygon>
        <PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/><PolyStepSegment x="0" y="10"/>
        <PolyStepSegment x="0" y="0"/>
      </Polygon></Profile>
      <PadStackDef name="castle">
        <PadstackHoleDef name="castle-hole" diameter="0.6" platingStatus="PLATED" plusTol="0" minusTol="0" x="0" y="0"/>
        <PadstackPadDef layerRef="TOP" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
      </PadStackDef>
      <LayerFeature layerRef="TOP">{lands}{extra}</LayerFeature>
      <LayerFeature layerRef="DRILL">{holes}</LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#
        )
    }

    #[test]
    fn castellation_lands_meet_the_edge_but_nothing_else_near_them_does() {
        use crate::commands::dfm::fixtures;
        let pdk = fixtures::pdk(
            "[[rules.copper.board_edge_clearance]]\nid = \"edge\"\nlimit = { minimum = \"0.20 mm\" }",
        );
        let edge = |xml: &str| {
            fixtures::run_board(xml, &pdk)
                .findings
                .iter()
                .map(|finding| finding.measurement.actual_mm().unwrap())
                .collect::<Vec<_>>()
        };
        let strip = |net: &str, x: f64, y: f64| {
            format!(
                r#"<Set net="{net}"><Features><Contour><Polygon>
                  <PolyBegin x="{x}" y="{y}"/><PolyStepSegment x="2" y="{y}"/>
                  <PolyStepSegment x="2" y="{}"/><PolyStepSegment x="{x}" y="{}"/>
                  <PolyStepSegment x="{x}" y="{y}"/>
                </Polygon></Contour></Features></Set>"#,
                y + 0.2,
                y + 0.2
            )
        };
        assert_eq!(edge(&castellated(&[3.0, 6.0], "")), Vec::<f64>::new());
        // One plated hole on the edge may be a stray, as for board arrays.
        assert_eq!(edge(&castellated(&[3.0], "")), [0.0]);
        // A land's own net running on along the edge, and another net
        // between the castellations, are both ordinary edge copper.
        // Another net's copper wholly under a castellation land still
        // touches the edge: only the land's own copper is set aside.
        let under = r#"<Set net="OTHER"><Features><Contour><Polygon>
          <PolyBegin x="0" y="2.8"/><PolyStepSegment x="0.1" y="2.8"/>
          <PolyStepSegment x="0.1" y="2.9"/><PolyStepSegment x="0" y="2.9"/>
          <PolyStepSegment x="0" y="2.8"/>
        </Polygon></Contour></Features></Set>"#;
        let [touch] = edge(&castellated(&[3.0, 6.0], under)).try_into().unwrap();
        assert!(touch < 1e-9, "{touch}");
        for extra in [strip("N0", 0.05, 3.6), strip("OTHER", 0.1, 4.4)] {
            let found = edge(&castellated(&[3.0, 6.0], &extra));
            assert_eq!(found.len(), 1, "{extra}");
            assert!(found[0] > 0.0 && found[0] < 0.2, "{found:?}");
        }
    }
}
