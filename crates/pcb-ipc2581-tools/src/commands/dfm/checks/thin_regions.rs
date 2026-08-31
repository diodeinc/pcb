//! Minimum copper feature width and soldermask web width, by morphology.
//!
//! Let `M` be one layer's composed image and `B_ρ` the closed disk of
//! radius `ρ = L/2`. The morphological opening and closing of `M` with
//! `B_ρ` are
//!
//! ```text
//! M ∘ B_ρ = (M ⊖ B_ρ) ⊕ B_ρ        (erosion, then dilation)
//! M • B_ρ = (M ⊕ B_ρ) ⊖ B_ρ        (dilation, then erosion)
//! ```
//!
//! The opening equals the union of all translates of `B_ρ` contained in
//! `M`, so the residue `M \ (M ∘ B_ρ)` is exactly the material through
//! which no disk of diameter `L` passes — the sub-minimum features.
//! Dually, the closing residue `(M • B_ρ) \ M` is exactly the complement
//! material narrower than `L`, used here for soldermask webs.
//!
//! Morphology is the conservative broad phase: it runs with an offset guard
//! large enough to retain candidates across curve/offset tessellation. A
//! candidate becomes a measurement only when two opposing source-boundary
//! branches wall it; their exact separation is the piece's width. One-sided
//! residue — the bite an isolated corner sheds — has no opposing branches
//! and is discarded. Thus the composed image supplies the authoritative
//! measurement while morphology only localizes the work.
//!
//! The opening residue flags copper that will not survive etching. A
//! soldermask image is the mask *openings*, so its closing residue flags the
//! web of mask remaining between them. Copper clearance is a separate
//! electrical-conductor boundary-distance check; morphology must not
//! reinterpret a same-net notch as spacing between conductors.

use pcb_ir::geom::dfm::{
    RegionBoundaryIndex, ThinPiece, circular_region, thin_features, thin_gaps,
};
use pcb_ir::geom::{BBox, ContourSet, Point, tol};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use super::{Evaluation, Measured, MeasuredSite};
use crate::commands::dfm::design::{CopperConductor, CopperLayer, Design, MaskLayer, MaskOwner};
use crate::commands::dfm::report::{Evidence, LayerRef, MeasurementKind, SourceLocator, Subject};

pub(super) fn copper_feature_width(limit_mm: f64, design: &Design) -> Evaluation {
    let measure = |layer: &CopperLayer| {
        thin_features(&layer.image, limit_mm)
            .into_iter()
            .map(move |piece| {
                let mut measured = measured_piece(&piece, limit_mm, &layer.layer, "copper_image");
                // Prove ownership against final composed material. A bounding box
                // is only a broad phase; overlapping owners remain ambiguous.
                let construction =
                    piece
                        .sites
                        .iter()
                        .fold(piece.candidate.clone(), |region, site| {
                            region.union(&circular_region(site.disk.center, site.disk.radius_mm))
                        });
                let owner = unique_copper_owner(&construction, &layer.conductors);
                if let Some(owner) = owner {
                    measured.subjects[0].provenance =
                        copper_subject(design, owner, &layer.layer).provenance;
                }
                for (geometry, site) in piece.sites.iter().zip(&mut measured.sites) {
                    let local_owner = owner.or_else(|| {
                        let local = piece
                            .candidate
                            .intersection(&ContourSet::rectangle(
                                geometry.bbox.expand(limit_mm / 2.0),
                                tol::REGION_MM,
                            ))
                            .union(&circular_region(
                                geometry.disk.center,
                                geometry.disk.radius_mm,
                            ));
                        unique_copper_owner(&local, &layer.conductors)
                    });
                    if let Some(owner) = local_owner {
                        site.subjects = vec![copper_subject(design, owner, &layer.layer)];
                    }
                }
                measured
            })
            .collect::<Vec<_>>()
    };
    #[cfg(not(target_family = "wasm"))]
    let measured = design
        .copper_layers
        .par_iter()
        .flat_map_iter(measure)
        .collect();
    #[cfg(target_family = "wasm")]
    let measured = design.copper_layers.iter().flat_map(measure).collect();
    Evaluation {
        checked: design.copper_layers.len(),
        measured,
    }
}

pub(super) fn soldermask_web(limit_mm: f64, design: &Design) -> Evaluation {
    let measure = |layer: &MaskLayer| {
        let boundaries = layer
            .owners
            .iter()
            .map(|owner| {
                (
                    owner.image.bbox,
                    RegionBoundaryIndex::new(&owner.image, limit_mm),
                )
            })
            .collect::<Vec<_>>();
        thin_gaps(&layer.image, limit_mm)
            .into_iter()
            .map(move |piece| {
                let mut measured =
                    measured_piece(&piece, limit_mm, &layer.layer, "soldermask_image");
                let mut aggregate_owner = None;
                let mut one_owner = true;
                for (geometry, site) in piece.sites.iter().zip(&mut measured.sites) {
                    let Some(owners) = wall_owners(&geometry.walls, &boundaries) else {
                        one_owner = false;
                        continue;
                    };
                    if owners.len() != 1 || aggregate_owner.is_some_and(|owner| owners[0] != owner)
                    {
                        one_owner = false;
                    }
                    aggregate_owner = aggregate_owner.or_else(|| owners.first().copied());
                    site.subjects = owners
                        .into_iter()
                        .map(|index| mask_subject(design, &layer.owners[index], &layer.layer))
                        .collect();
                }
                if one_owner && let Some(index) = aggregate_owner {
                    measured.subjects[0].provenance =
                        mask_subject(design, &layer.owners[index], &layer.layer).provenance;
                }
                measured
            })
            .collect::<Vec<_>>()
    };
    #[cfg(not(target_family = "wasm"))]
    let measured = design
        .mask_layers
        .par_iter()
        .flat_map_iter(measure)
        .collect();
    #[cfg(target_family = "wasm")]
    let measured = design.mask_layers.iter().flat_map(measure).collect();
    Evaluation {
        checked: design.mask_layers.len(),
        measured,
    }
}

fn measured_piece(
    piece: &ThinPiece,
    limit_mm: f64,
    layer: &LayerRef,
    offender_kind: &'static str,
) -> Measured {
    Measured {
        distance: piece.width,
        bbox: piece.bbox,
        layers: vec![layer.clone()],
        subjects: vec![Subject {
            role: "offender",
            kind: offender_kind,
            name: Some(layer.name.clone()),
            ..Subject::default()
        }],
        evidence: vec![Evidence::bounds("thin_piece", piece.bbox)],
        sites: piece_sites(piece, limit_mm, layer),
    }
}

fn unique_copper_owner<'a>(
    region: &ContourSet,
    owners: &'a [CopperConductor],
) -> Option<&'a CopperConductor> {
    if region.is_empty() {
        return None;
    }
    let mut intersections = owners.iter().filter(|owner| {
        region.bbox.intersects(owner.image.bbox) && !region.intersection(&owner.image).is_empty()
    });
    let owner = intersections.next()?;
    (intersections.next().is_none() && region.difference(&owner.image).is_empty()).then_some(owner)
}

fn copper_subject(design: &Design, owner: &CopperConductor, layer: &LayerRef) -> Subject {
    Subject {
        role: "offender",
        kind: "copper_conductor",
        name: Some(layer.name.clone()),
        net: design.resolve(owner.id.net()),
        provenance: Some(SourceLocator {
            step: design.resolve(owner.id.step()),
            layer: Some(layer.name.clone()),
            set_index: None,
            feature_index: None,
            instance_index: owner.id.instance(),
        }),
        ..Subject::default()
    }
}

fn mask_subject(design: &Design, owner: &MaskOwner, layer: &LayerRef) -> Subject {
    Subject {
        role: "opening_boundary",
        kind: "soldermask_opening",
        name: Some(layer.name.clone()),
        provenance: Some(SourceLocator {
            step: design.resolve(owner.step),
            layer: Some(layer.name.clone()),
            set_index: None,
            feature_index: None,
            instance_index: owner.instance_index,
        }),
        ..Subject::default()
    }
}

/// Ownership is a boundary-coverage proof, not proximity to a board outline
/// or just attribution of the minimum disk. Every contacted wall span must
/// be completely covered. Distinct owners may contribute different spans;
/// overlapping ownership of the same span remains explicitly unknown.
fn wall_owners(
    walls: &[(Point, Point)],
    boundaries: &[(BBox, RegionBoundaryIndex)],
) -> Option<Vec<usize>> {
    const BOUNDARY_EPSILON_MM: f64 = 1e-6;
    let mut owners = Vec::new();
    if walls.is_empty() {
        return None;
    }
    for &(start, end) in walls {
        let bbox = BBox::from_point(start)
            .union(BBox::from_point(end))
            .expand(BOUNDARY_EPSILON_MM);
        let length = start.distance_to(end);
        let mut intervals = boundaries
            .iter()
            .enumerate()
            .filter(|(_, (owner_bbox, _))| bbox.intersects(*owner_bbox))
            .flat_map(|(index, (_, boundary))| {
                boundary
                    .segment_boundary_intervals(start, end, BOUNDARY_EPSILON_MM)
                    .into_iter()
                    .map(move |(low, high)| (low, high, index))
            })
            .collect::<Vec<_>>();
        if intervals.is_empty() {
            return None;
        }
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut covered: f64 = 0.0;
        for (position, &(low, high, owner)) in intervals.iter().enumerate() {
            if low > covered + 1e-12 {
                return None;
            }
            if intervals[..position]
                .iter()
                .any(|&(previous_low, previous_high, previous_owner)| {
                    previous_owner != owner
                        && (length <= BOUNDARY_EPSILON_MM
                            || (high.min(previous_high) - low.max(previous_low)) * length
                                > 4.0 * BOUNDARY_EPSILON_MM)
                })
            {
                return None;
            }
            covered = covered.max(high);
            owners.push(owner);
        }
        if covered < 1.0 - 1e-12 {
            return None;
        }
    }
    owners.sort_unstable();
    owners.dedup();
    Some(owners)
}

/// Shared width construction for composed copper, mask webs and routed slots.
/// The conservative residue is explicitly context. Only the independently
/// verified medial-axis paths and their real inscribed disks are offenders.
pub(super) fn piece_sites(piece: &ThinPiece, limit_mm: f64, layer: &LayerRef) -> Vec<MeasuredSite> {
    piece.sites.iter().map(|geometry| {
        let disk = geometry.disk;
        let bbox = geometry.bbox.union(BBox::from_point(disk.center).expand(limit_mm / 2.0));
        let local_candidate = piece.candidate.intersection(&ContourSet::rectangle(bbox, tol::REGION_MM));
        let mut evidence = vec![
            Evidence::region("candidate_region", &local_candidate),
            Evidence::circle("inscribed_width_disk", disk.center, disk.radius_mm * 2.0),
            Evidence::circle("required_width_disk", disk.center, limit_mm),
        ];
        if !geometry.axis.is_empty() {
            evidence.push(Evidence {
                role: "verified_width_axis",
                kind: "path",
                paths: geometry.axis.iter().map(|path| path.iter().copied().map(Into::into).collect()).collect(),
                ..Evidence::default()
            });
        }
        if !geometry.walls.is_empty() {
            evidence.push(Evidence {
                role: "width_boundary",
                kind: "path",
                paths: geometry.walls.iter().map(|&(start, end)| vec![start.into(), end.into()]).collect(),
                ..Evidence::default()
            });
        }
        let mut site = MeasuredSite::new(disk.width, bbox, vec![layer.clone()], evidence, MeasurementKind::InscribedWidth);
        site.note = Some("Width is the inscribed disk diameter. Candidate contours are context; highlighted axis portions are verified below the limit, including geometric uncertainty.".to_owned());
        site
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dfm::design::ConductorId;

    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourSet {
        ContourSet::rectangle(
            BBox {
                min: Point::new(x0, y0),
                max: Point::new(x1, y1),
            },
            tol::REGION_MM,
        )
    }

    fn conductor(image: ContourSet, instance: u32) -> CopperConductor {
        CopperConductor {
            id: ConductorId::Auxiliary {
                step: None,
                instance: Some(instance),
                source_set_index: 0,
            },
            image,
        }
    }

    fn boundaries(images: &[ContourSet]) -> Vec<(BBox, RegionBoundaryIndex)> {
        images
            .iter()
            .map(|image| (image.bbox, RegionBoundaryIndex::new(image, 0.2)))
            .collect()
    }

    #[test]
    fn copper_ownership_requires_unique_complete_material_coverage() {
        let candidate = rectangle(0.0, 0.0, 0.08, 2.0);
        let decoy = rectangle(-1.0, -1.0, 1.0, 3.0).difference(&rectangle(-0.1, -0.1, 0.2, 2.1));
        let owners = [conductor(decoy, 8), conductor(candidate.clone(), 3)];
        assert_eq!(
            unique_copper_owner(&candidate, &owners)
                .unwrap()
                .id
                .instance(),
            Some(3)
        );

        let split = [
            conductor(rectangle(0.0, 0.0, 0.04, 2.0), 3),
            conductor(rectangle(0.04, 0.0, 0.08, 2.0), 4),
        ];
        assert!(
            unique_copper_owner(&candidate, &split).is_none(),
            "a connected image can span occurrences"
        );
        assert!(
            unique_copper_owner(&candidate, &split[..1]).is_none(),
            "one intersecting owner does not prove complete coverage"
        );
        let overlapping = [
            conductor(candidate.clone(), 3),
            conductor(candidate.clone(), 4),
        ];
        assert!(
            unique_copper_owner(&candidate, &overlapping).is_none(),
            "coincident material remains ambiguous"
        );
    }

    #[test]
    fn web_wall_ownership_keeps_both_physical_occurrences() {
        let left = rectangle(0.0, 0.0, 1.0, 1.0);
        let right = rectangle(1.1, 0.0, 2.1, 1.0);
        let walls = [
            (Point::new(1.0, 0.2), Point::new(1.0, 0.8)),
            (Point::new(1.1, 0.2), Point::new(1.1, 0.8)),
        ];
        assert_eq!(
            wall_owners(&walls, &boundaries(&[left.union(&right)])),
            Some(vec![0])
        );
        assert_eq!(
            wall_owners(&walls, &boundaries(&[left.clone(), right.clone()])),
            Some(vec![0, 1])
        );
        assert!(wall_owners(&walls, &boundaries(std::slice::from_ref(&left))).is_none());
        assert!(
            wall_owners(&walls, &boundaries(&[left.clone(), right, left])).is_none(),
            "duplicate ownership of one wall is ambiguous"
        );
    }

    #[test]
    fn wall_endpoints_do_not_prove_the_interior_span() {
        let incomplete = rectangle(0.0, 0.0, 1.0, 0.4).union(&rectangle(0.0, 0.6, 1.0, 1.0));
        let walls = [(Point::new(1.0, 0.1), Point::new(1.0, 0.9))];
        assert!(wall_owners(&walls, &boundaries(&[incomplete])).is_none());
        let contiguous = [rectangle(0.0, 0.0, 1.0, 0.5), rectangle(0.0, 0.5, 1.0, 1.0)];
        assert_eq!(
            wall_owners(&walls, &boundaries(&contiguous)),
            Some(vec![0, 1])
        );
    }
}
