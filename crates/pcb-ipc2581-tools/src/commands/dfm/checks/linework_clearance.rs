//! Minimum clearance from reference linework to copper.
//!
//! The reference is a set of segments `S = {s₁ … sₙ}` — a V-score tool
//! centerline, or a board profile's outer and cutout rings — and the target
//! is one layer's composed copper image `M`, a regularized closed filled
//! region. The measured quantity is the Euclidean set distance
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

use pcb_ir::geom::GeometryAccuracy;
use std::ops::Range;

use pcb_ir::geom::dfm::{ClearanceSite, Distance, linework_clearance_sites, linework_envelope};
use pcb_ir::geom::region::ring_edges;
use pcb_ir::geom::{BBox, Point};

use crate::commands::dfm::design::{BoardOutline, CopperLayer, Design};
use crate::commands::dfm::report::{
    Evidence, EvidenceDisplay, LayerRef, MeasurementKind, ReportPoint, SourceLocator, Subject,
};
use crate::commands::dfm::rules::{Conditions, Linework};

use super::{Evaluation, Measured, MeasuredSite, layers, violates};

/// One reference item: a V-score centerline or a board outline, as a range
/// of bare segments plus its report identity.
struct LineworkItem {
    segments: Range<usize>,
    uncertainty_mm: f64,
    layer: Option<LayerRef>,
    subject: Subject,
    evidence: Evidence,
}

/// The reference items and their segments, flattened into one pool so the
/// endpoint containment sweep runs once per layer.
struct LineworkPool {
    items: Vec<LineworkItem>,
    segments: Vec<(Point, Point)>,
}

fn linework_items(linework: Linework, design: &Design) -> LineworkPool {
    let mut segments = Vec::new();
    let mut push = |item_segments: Vec<(Point, Point)>| {
        let start = segments.len();
        segments.extend(item_segments);
        start..segments.len()
    };
    let items = match linework {
        Linework::VScore => design
            .scores
            .iter()
            .map(|score| LineworkItem {
                segments: push(vec![(score.start, score.end)]),
                uncertainty_mm: 0.0,
                layer: Some(score.layer.clone()),
                subject: Subject {
                    role: "reference",
                    kind: "vscore_centerline",
                    name: Some(score.layer.name.clone()),
                    provenance: Some(score.provenance.clone()),
                    ..Subject::default()
                },
                evidence: Evidence::segment("vscore_centerline", score.start, score.end),
            })
            .collect(),
        Linework::BoardEdge => design
            .board_outlines
            .iter()
            .map(|outline| LineworkItem {
                segments: push(outline.contours.iter().flat_map(ring_edges).collect()),
                uncertainty_mm: outline.region.uncertainty_mm,
                layer: None,
                subject: outline_subject(outline, "reference"),
                evidence: Evidence::bounds("board_outline", outline.bbox),
            })
            .collect(),
    };
    LineworkPool { items, segments }
}

pub(super) fn evaluate(
    limit_mm: f64,
    linework: Linework,
    conditions: &Conditions,
    design: &Design,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Evaluation> {
    let copper_layers = &design.copper_layers;
    let boundaries = &design.copper_boundaries;
    let pool = linework_items(linework, design);
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
        let inside = copper.image.contains_points_batch(&endpoints);
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
                            copper.image.uncertainty_mm,
                        )),
                        (_, true) => Some(Distance::with_uncertainty(
                            0.0,
                            end,
                            end,
                            copper.image.uncertainty_mm,
                        )),
                        _ => boundaries[copper_index].segment_nearest_within(start, end, limit_mm),
                    }
                })
                .min_by(|left, right| left.mm.total_cmp(&right.mm))
            else {
                continue;
            };
            let distance = nearest.also_uncertain(item.uncertainty_mm);
            let site_layers = layers(item.layer.iter().chain([&copper.layer]));
            let sites = if violates(&distance, limit_mm) {
                linework_clearance_sites(
                    &segments[item.segments.clone()],
                    &copper.image,
                    &boundaries[copper_index],
                    limit_mm,
                    item.uncertainty_mm,
                )
                .into_iter()
                .map(|site| report_site(site, site_layers.clone(), limit_mm, accuracy))
                .collect::<anyhow::Result<Vec<_>>>()?
                .into_iter()
                .collect()
            } else {
                Vec::new()
            };
            measured.push(Measured {
                distance,
                bbox: BBox::from_point(distance.first).union(BBox::from_point(distance.second)),
                layers: site_layers,
                subjects: vec![item.subject.clone(), copper_subject(copper)],
                evidence: vec![item.evidence.clone()],
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

/// All clearance families share the same local path/constraint construction;
/// the rule and inherited subjects give these boundaries their physical roles.
pub(super) fn report_site(
    geometry: ClearanceSite,
    layers: Vec<LayerRef>,
    limit_mm: f64,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<MeasuredSite> {
    let mut evidence = [
        ("first_boundary", &geometry.first_paths),
        ("second_boundary", &geometry.second_paths),
    ]
    .into_iter()
    .filter(|(_, paths)| !paths.is_empty())
    .map(|(role, paths)| Evidence {
        role,
        kind: "path",
        paths: joined_boundary_paths(paths),
        ..Evidence::default()
    })
    .collect::<Vec<_>>();
    let band = linework_envelope(&geometry.first_paths, limit_mm, accuracy)?;
    if !band.is_empty() {
        evidence.push(Evidence {
            display: Some(EvidenceDisplay::RoundStroke {
                paths: joined_boundary_paths(&geometry.first_paths),
                width_mm: 2.0 * limit_mm,
            }),
            ..Evidence::region("required_clearance_band", &band)
        });
    }
    let overlaps = !geometry.overlap.is_empty();
    if overlaps {
        evidence.push(Evidence::region("overlap_region", &geometry.overlap));
    }
    let bbox = if band.is_empty() {
        geometry.bbox
    } else {
        geometry.bbox.union(band.bbox)
    };
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
    Ok(site)
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
    Subject {
        role,
        kind: "board_outline",
        name: Some(outline.name.clone()),
        source: Some(SourceLocator {
            step: Some(outline.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: outline.instance_index,
        }),
        provenance: Some(SourceLocator {
            step: Some(outline.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: outline.instance_index,
        }),
        ..Subject::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearance_display_keeps_certified_spans_and_an_exact_round_stroke() {
        let accuracy = GeometryAccuracy::default();

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
            overlap: pcb_ir::geom::ContourSet::empty(pcb_ir::geom::tol::REGION_MM),
        };
        let site = report_site(geometry, Vec::new(), 0.2, accuracy).unwrap();
        let band = site
            .evidence
            .iter()
            .find(|evidence| evidence.role == "required_clearance_band")
            .unwrap();
        let Some(EvidenceDisplay::RoundStroke { paths, width_mm }) = &band.display else {
            panic!("clearance band must retain its exact stroke construction");
        };
        assert_eq!(
            *width_mm, 0.4,
            "the band extends the full limit on each side"
        );
        assert_eq!(paths.len(), 2, "disconnected spans must stay disconnected");
        assert_eq!(
            paths[0].iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            vec![(1.0, 2.0), (3.0, 2.0), (3.0, 4.0)]
        );
        assert_eq!(
            paths[1].iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            vec![(8.0, 8.0), (9.0, 9.0)]
        );
        assert!(
            !band.paths.is_empty(),
            "measured polygon evidence remains available"
        );
        assert_eq!(site.distance.mm, 0.1);
        assert_eq!(site.distance.uncertainty_mm, pcb_ir::geom::tol::FLATTEN_MM);
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
}
