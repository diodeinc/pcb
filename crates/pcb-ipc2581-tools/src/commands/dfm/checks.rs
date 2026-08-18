use std::cmp::Ordering;

use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::dfm::{
    CircularEnclosureCheck, CircularEnclosureMeasurement, ClearanceMeasurement,
    RegionBoundaryIndex, region_clearance, segment_region_clearance,
};
use pcb_ir::geom::region::Ring;
use pcb_ir::geom::{BBox, Point, tol};

use super::design::{BoardArray, BoardOutline, CopperLayer, Design, Hole, HoleClass, Land, Score};
use super::pdk::{Length, Pdk};
use super::report::{
    Evidence, Finding, LayerRef, Location, Measurement, RuleResult, Severity, SourceLocator,
    Subject, Witness,
};

const COMPARISON_EPSILON_MM: f64 = 1e-6;

#[derive(Default)]
pub(super) struct Results {
    pub(super) rules: Vec<RuleResult>,
    pub(super) findings: Vec<Finding>,
}

pub(super) fn run(design: &Design, pdk: &Pdk) -> Results {
    let mut output = Results::default();

    let drilling = &pdk.capabilities.drilling;
    if let Some(limit) = &drilling.minimum_via_hole_diameter {
        check_minimum_hole_diameter(&design.holes, HoleClass::Via, limit, &mut output);
    }
    if let Some(limit) = &drilling.minimum_pth_hole_diameter {
        check_minimum_hole_diameter(&design.holes, HoleClass::Pth, limit, &mut output);
    }
    if let Some(limit) = &drilling.minimum_npth_hole_diameter {
        check_minimum_hole_diameter(&design.holes, HoleClass::Npth, limit, &mut output);
    }
    if let Some(limit) = &drilling.minimum_hole_to_hole_clearance {
        check_hole_to_hole_clearance(&design.holes, limit, &mut output);
    }

    let copper = &pdk.capabilities.copper;
    if let Some(limit) = &copper.minimum_via_annular_ring {
        check_annular_ring(
            &design.holes,
            &design.copper_layers,
            HoleClass::Via,
            limit,
            &mut output,
        );
    }
    if let Some(limit) = &copper.minimum_pth_annular_ring {
        check_annular_ring(
            &design.holes,
            &design.copper_layers,
            HoleClass::Pth,
            limit,
            &mut output,
        );
    }
    if let Some(limit) = &copper.minimum_vscore_to_copper_clearance {
        check_vscore_to_copper(&design.scores, &design.copper_layers, limit, &mut output);
    }
    if let Some(limit) = &copper.minimum_board_edge_clearance {
        check_board_edge_to_copper(
            &design.board_outlines,
            &design.copper_layers,
            limit,
            &mut output,
        );
    }

    if let Some(limit) = &pdk.capabilities.panelization.minimum_board_array_spacing {
        check_board_array_spacing(design.scope, &design.board_arrays, limit, &mut output);
    }

    output.findings.sort_by(|left, right| {
        left.rule_id
            .cmp(right.rule_id)
            .then_with(|| compare_locations(&left.location, &right.location))
    });
    for (index, finding) in output.findings.iter_mut().enumerate() {
        finding.id = format!("DFM-{:06}", index + 1);
    }
    output
}

fn check_minimum_hole_diameter(
    holes: &[Hole],
    class: HoleClass,
    limit: &Length,
    output: &mut Results,
) {
    let rule_id = match class {
        HoleClass::Via => "drill.via.minimum_hole_diameter",
        HoleClass::Pth => "drill.pth.minimum_hole_diameter",
        HoleClass::Npth => "drill.npth.minimum_hole_diameter",
    };
    let title = match class {
        HoleClass::Via => "Minimum via hole diameter",
        HoleClass::Pth => "Minimum plated through-hole diameter",
        HoleClass::Npth => "Minimum non-plated hole diameter",
    };
    let mut result = RuleResult::new(rule_id, title, limit);
    let start = output.findings.len();
    for hole in holes.iter().filter(|hole| hole.class == class) {
        result.checked_entities += 1;
        if violates_minimum(hole.diameter_mm, limit.millimeters()) {
            output.findings.push(Finding {
                id: String::new(),
                rule_id,
                severity: Severity::Error,
                title: format!("{} hole is below minimum diameter", class.label()),
                message: format!(
                    "{} hole diameter is {:.6} mm; the PDK requires at least {:.6} mm",
                    class.label(),
                    hole.diameter_mm,
                    limit.millimeters()
                ),
                measurements: vec![Measurement::minimum(
                    "hole_diameter",
                    hole.diameter_mm,
                    limit.millimeters(),
                    "ipc_hole_diameter",
                )],
                location: Location {
                    point: Some(hole.center.into()),
                    bounding_box: Some(hole.bbox.into()),
                    witnesses: Vec::new(),
                },
                layers: vec![hole.layer.clone()],
                subjects: vec![hole_subject(hole, "offender")],
                evidence: vec![Evidence::circle(
                    "drilled_hole",
                    hole.center,
                    hole.diameter_mm,
                )],
            });
        }
    }
    result.finish(output.findings.len() - start);
    output.rules.push(result);
}

fn check_hole_to_hole_clearance(holes: &[Hole], limit: &Length, output: &mut Results) {
    const RULE_ID: &str = "drill.hole_to_hole.minimum_clearance";
    let mut result = RuleResult::new(RULE_ID, "Minimum hole edge-to-edge clearance", limit);
    result.checked_entities = holes.len();
    let start = output.findings.len();

    for first_index in 0..holes.len() {
        let first = &holes[first_index];
        for second in &holes[first_index + 1..] {
            if second.bbox.min.x - first.bbox.max.x >= limit.millimeters() - COMPARISON_EPSILON_MM {
                break;
            }
            let y_gap = (second.bbox.min.y - first.bbox.max.y)
                .max(first.bbox.min.y - second.bbox.max.y)
                .max(0.0);
            if y_gap >= limit.millimeters() - COMPARISON_EPSILON_MM {
                continue;
            }
            let center_distance = first.center.distance_to(second.center);
            let actual =
                (center_distance - first.diameter_mm / 2.0 - second.diameter_mm / 2.0).max(0.0);
            if !violates_minimum(actual, limit.millimeters()) {
                continue;
            }
            let (first_witness, second_witness) = circle_witnesses(first, second);
            output.findings.push(Finding {
                id: String::new(),
                rule_id: RULE_ID,
                severity: Severity::Error,
                title: "Hole-to-hole clearance is below minimum".to_owned(),
                message: format!(
                    "hole edges are {:.6} mm apart; the PDK requires at least {:.6} mm",
                    actual,
                    limit.millimeters()
                ),
                measurements: vec![Measurement::minimum(
                    "hole_edge_to_hole_edge_clearance",
                    actual,
                    limit.millimeters(),
                    "circle_edge_distance",
                )],
                location: Location {
                    point: Some(first_witness.midpoint(second_witness).into()),
                    bounding_box: Some(first.bbox.union(second.bbox).into()),
                    witnesses: vec![
                        Witness::new("first_hole_boundary", first_witness),
                        Witness::new("second_hole_boundary", second_witness),
                    ],
                },
                layers: unique_layers([&first.layer, &second.layer]),
                subjects: vec![hole_subject(first, "first"), hole_subject(second, "second")],
                evidence: vec![
                    Evidence::circle("first_hole", first.center, first.diameter_mm),
                    Evidence::circle("second_hole", second.center, second.diameter_mm),
                ],
            });
        }
    }
    result.finish(output.findings.len() - start);
    output.rules.push(result);
}

fn circle_witnesses(first: &Hole, second: &Hole) -> (Point, Point) {
    let delta = second.center - first.center;
    let length = delta.length();
    if length <= f64::EPSILON {
        return (first.center, second.center);
    }
    let direction = delta / length;
    (
        first.center + direction * (first.diameter_mm / 2.0),
        second.center - direction * (second.diameter_mm / 2.0),
    )
}

fn check_annular_ring(
    holes: &[Hole],
    copper_images: &[CopperLayer],
    class: HoleClass,
    limit: &Length,
    output: &mut Results,
) {
    let (rule_id, title) = match class {
        HoleClass::Via => (
            "copper.via.minimum_annular_ring",
            "Minimum via annular ring",
        ),
        HoleClass::Pth => (
            "copper.pth.minimum_annular_ring",
            "Minimum plated through-hole annular ring",
        ),
        HoleClass::Npth => unreachable!("NPTH holes have no annular-ring rule"),
    };
    let mut result = RuleResult::new(rule_id, title, limit);
    let start = output.findings.len();
    let checked_holes = holes
        .iter()
        .filter(|hole| hole.class == class)
        .collect::<Vec<_>>();
    result.checked_entities = checked_holes.len();
    let centers = checked_holes
        .iter()
        .map(|hole| hole.center)
        .collect::<Vec<_>>();
    let maximum_search_mm = checked_holes
        .iter()
        .map(|hole| hole.diameter_mm / 2.0 + limit.millimeters())
        .max_by(f64::total_cmp)
        .unwrap_or(limit.millimeters());
    let mut indexed_layers = copper_images
        .iter()
        .map(|copper| {
            (
                copper,
                copper.image.contains_points_batch(&centers),
                RegionBoundaryIndex::new(&copper.image, maximum_search_mm),
            )
        })
        .collect::<Vec<_>>();

    for (hole_index, hole) in checked_holes.into_iter().enumerate() {
        let worst = indexed_layers
            .iter_mut()
            .filter(|(copper, _, _)| hole_applies_to_copper_layer(hole, &copper.layer.name))
            .filter_map(|(copper, contains, boundary)| {
                let check = boundary.as_mut()?.check_circular_enclosure(
                    hole.center,
                    hole.diameter_mm / 2.0,
                    limit.millimeters(),
                    tol::FLATTEN_MM + COMPARISON_EPSILON_MM,
                    contains[hole_index],
                )?;
                match check {
                    CircularEnclosureCheck::Satisfied => None,
                    CircularEnclosureCheck::Violated(measurement) => Some((*copper, measurement)),
                }
            })
            .min_by(|(_, left), (_, right)| left.enclosure_mm.total_cmp(&right.enclosure_mm));
        let Some((worst_layer, enclosure)) = worst else {
            continue;
        };
        let actual = enclosure.enclosure_mm;
        let required_radius = hole.diameter_mm / 2.0 + limit.millimeters();
        let required_bbox = BBox::new(
            Point::new(
                hole.center.x - required_radius,
                hole.center.y - required_radius,
            ),
            Point::new(
                hole.center.x + required_radius,
                hole.center.y + required_radius,
            ),
        );
        let witnesses = enclosure_witnesses(enclosure);
        let location_point = enclosure.material_boundary.map_or(hole.center, |boundary| {
            enclosure.cutout_boundary.midpoint(boundary)
        });
        let detail = if !enclosure.center_in_material {
            "the hole center is outside copper, so no continuous annular ring exists".to_owned()
        } else if actual < 0.0 {
            format!(
                "the drilled hole breaches the copper image by {:.6} mm",
                -actual
            )
        } else {
            format!(
                "only {:.6} mm of copper remains outside the drilled hole",
                actual
            )
        };
        let land = matching_land(hole, worst_layer);
        output.findings.push(Finding {
            id: String::new(),
            rule_id,
            severity: Severity::Error,
            title: format!("{} annular ring is below minimum", class.label()),
            message: format!(
                "{} minimum radial copper enclosure is {:.6} mm on {}; the PDK requires {:.6} mm ({detail})",
                class.label(),
                actual,
                worst_layer.layer.name,
                limit.millimeters()
            ),
            measurements: vec![Measurement::minimum(
                "annular_ring",
                actual,
                limit.millimeters(),
                "maximal_centered_disk_minus_hole_radius",
            )],
            location: Location {
                point: Some(location_point.into()),
                bounding_box: Some(required_bbox.into()),
                witnesses,
            },
            layers: unique_layers([&hole.layer, &worst_layer.layer]),
            subjects: annular_ring_subjects(hole, worst_layer, land),
            evidence: annular_ring_evidence(hole, land, 2.0 * required_radius),
        });
    }

    if copper_images.is_empty() {
        result.skip("no copper layers were found in the selected layout target");
    } else {
        result.finish(output.findings.len() - start);
    }
    output.rules.push(result);
}

fn hole_applies_to_copper_layer(hole: &Hole, layer_name: &str) -> bool {
    hole.copper_layers
        .as_ref()
        .is_none_or(|layers| layers.iter().any(|layer| layer == layer_name))
}

fn matching_land<'a>(hole: &Hole, copper_layer: &'a CopperLayer) -> Option<&'a Land> {
    let hole_padstack = hole.padstack_ref.as_deref()?;
    copper_layer
        .lands_by_padstack
        .iter()
        .filter(|(land_padstack, _)| padstack_refs_match(hole_padstack, land_padstack))
        .flat_map(|(_, lands)| lands)
        .filter(|land| land.step == hole.step)
        .filter(|land| {
            land.net.is_none() || hole.net.is_none() || land.net.as_deref() == hole.net.as_deref()
        })
        .filter(|land| land.bbox.intersects(hole.bbox))
        .min_by(|left, right| {
            left.center
                .distance_to(hole.center)
                .total_cmp(&right.center.distance_to(hole.center))
        })
}

fn padstack_refs_match(hole_ref: &str, land_ref: &str) -> bool {
    hole_ref == land_ref
        || qualified_name_ends_with(land_ref, hole_ref)
        || qualified_name_ends_with(hole_ref, land_ref)
}

fn qualified_name_ends_with(qualified: &str, local: &str) -> bool {
    qualified
        .strip_suffix(local)
        .is_some_and(|prefix| prefix.ends_with('_'))
}

fn annular_ring_subjects(
    hole: &Hole,
    copper_layer: &CopperLayer,
    land: Option<&Land>,
) -> Vec<Subject> {
    let mut subjects = vec![hole_subject(hole, "hole")];
    subjects.push(match land {
        Some(land) => Subject {
            role: "land",
            kind: "padstack_land",
            name: land.primitive_ref.clone(),
            reference_designator: land.reference_designator.clone(),
            pin: land.pin.clone(),
            net: land.net.clone(),
            padstack_ref: Some(land.padstack_ref.clone()),
            source: Some(SourceLocator {
                step: land.step.clone(),
                layer: Some(copper_layer.layer.name.clone()),
                set_index: Some(land.source_set_index),
                feature_index: Some(land.source_feature_index),
                instance_index: None,
            }),
            ..Subject::default()
        },
        None => Subject {
            role: "land",
            kind: "composed_copper",
            name: Some(copper_layer.layer.name.clone()),
            ..Subject::default()
        },
    });
    subjects
}

fn annular_ring_evidence(
    hole: &Hole,
    land: Option<&Land>,
    required_diameter_mm: f64,
) -> Vec<Evidence> {
    let mut evidence = vec![
        Evidence::circle("drilled_hole", hole.center, hole.diameter_mm),
        Evidence::circle(
            "required_copper_envelope",
            hole.center,
            required_diameter_mm,
        ),
    ];
    if let Some(land) = land {
        evidence.push(Evidence::bounds("source_padstack_land_bounds", land.bbox));
    }
    evidence
}

fn enclosure_witnesses(enclosure: CircularEnclosureMeasurement) -> Vec<Witness> {
    let mut witnesses = vec![Witness::new("hole_boundary", enclosure.cutout_boundary)];
    if let Some(material_boundary) = enclosure.material_boundary {
        witnesses.push(Witness::new("copper_boundary", material_boundary));
    }
    witnesses
}

fn check_vscore_to_copper(
    scores: &[Score],
    copper_images: &[CopperLayer],
    limit: &Length,
    output: &mut Results,
) {
    const RULE_ID: &str = "copper.vscore.minimum_clearance";
    let mut result = RuleResult::new(
        RULE_ID,
        "Minimum V-score centerline-to-copper clearance",
        limit,
    );
    let start = output.findings.len();

    for score in scores {
        for copper_layer in copper_images {
            result.checked_entities += 1;
            let Some(clearance) =
                segment_region_clearance(score.start, score.end, &copper_layer.image)
            else {
                continue;
            };
            if !violates_minimum(clearance.distance_mm, limit.millimeters()) {
                continue;
            }
            output.findings.push(
                ClearanceViolation {
                    rule_id: RULE_ID,
                    title: "V-score centerline is too close to copper",
                    message: format!(
                    "V-score centerline is {:.6} mm from copper on {}; the PDK requires at least {:.6} mm",
                    clearance.distance_mm,
                    copper_layer.layer.name,
                    limit.millimeters()
                    ),
                    quantity: "vscore_centerline_to_copper_clearance",
                    method: "segment_to_filled_region",
                    witness_roles: ["vscore_centerline", "copper_boundary"],
                    clearance,
                    limit,
                    layers: unique_layers([&score.layer, &copper_layer.layer]),
                    subjects: vec![
                        Subject {
                            role: "reference",
                            kind: "vscore_centerline",
                            name: Some(score.layer.name.clone()),
                            ..Subject::default()
                        },
                        Subject {
                            role: "offender",
                            kind: "copper_image",
                            name: Some(copper_layer.layer.name.clone()),
                            ..Subject::default()
                        },
                    ],
                    evidence: vec![Evidence::segment(
                        "vscore_centerline",
                        score.start,
                        score.end,
                    )],
                }
                .into_finding(),
            );
        }
    }
    if scores.is_empty() {
        result.skip("no V-score centerlines were found in the selected layout target");
    } else if copper_images.is_empty() {
        result.skip("no copper layers were found in the selected layout target");
    } else {
        result.finish(output.findings.len() - start);
    }
    output.rules.push(result);
}

fn check_board_edge_to_copper(
    outlines: &[BoardOutline],
    copper_images: &[CopperLayer],
    limit: &Length,
    output: &mut Results,
) {
    const RULE_ID: &str = "copper.board_edge.minimum_clearance";
    let mut result = RuleResult::new(RULE_ID, "Minimum board-edge-to-copper clearance", limit);
    let start = output.findings.len();

    for outline in outlines {
        for copper_layer in copper_images {
            result.checked_entities += 1;
            let clearance = outline
                .contours
                .iter()
                .flat_map(ring_segments)
                .filter_map(|(edge_start, edge_end)| {
                    segment_region_clearance(edge_start, edge_end, &copper_layer.image)
                })
                .min_by(|left, right| left.distance_mm.total_cmp(&right.distance_mm));
            let Some(clearance) = clearance else {
                continue;
            };
            if !violates_minimum(clearance.distance_mm, limit.millimeters()) {
                continue;
            }
            output.findings.push(
                ClearanceViolation {
                    rule_id: RULE_ID,
                    title: "Board edge is too close to copper",
                    message: format!(
                    "board edge is {:.6} mm from copper on {}; the PDK requires at least {:.6} mm",
                    clearance.distance_mm,
                    copper_layer.layer.name,
                    limit.millimeters()
                    ),
                    quantity: "board_edge_to_copper_clearance",
                    method: "profile_boundary_to_filled_region",
                    witness_roles: ["board_outline", "copper_boundary"],
                    clearance,
                    limit,
                    layers: vec![copper_layer.layer.clone()],
                    subjects: vec![
                        outline_subject(outline, "reference"),
                        Subject {
                            role: "offender",
                            kind: "copper_image",
                            name: Some(copper_layer.layer.name.clone()),
                            ..Subject::default()
                        },
                    ],
                    evidence: vec![Evidence::bounds("board_outline", outline.bbox)],
                }
                .into_finding(),
            );
        }
    }
    if outlines.is_empty() {
        result.skip("no board profile outlines were found in the selected layout target");
    } else if copper_images.is_empty() {
        result.skip("no copper layers were found in the selected layout target");
    } else {
        result.finish(output.findings.len() - start);
    }
    output.rules.push(result);
}

fn ring_segments(ring: &Ring) -> impl Iterator<Item = (Point, Point)> + '_ {
    ring.iter()
        .copied()
        .zip(ring.iter().copied().cycle().skip(1))
        .take(ring.len())
        .map(|([x0, y0], [x1, y1])| (Point::new(x0, y0), Point::new(x1, y1)))
}

fn check_board_array_spacing(
    scope: ArtworkScope,
    arrays: &[BoardArray],
    limit: &Length,
    output: &mut Results,
) {
    const RULE_ID: &str = "panel.board_array.minimum_spacing";
    let mut result = RuleResult::new(
        RULE_ID,
        "Minimum spacing between board-array outlines",
        limit,
    );
    if scope != ArtworkScope::ArrayFlattened {
        result.skip("board-array spacing requires --layout-target board-array");
        output.rules.push(result);
        return;
    }
    if arrays.len() < 2 {
        result.skip(
            "fewer than two direct board-array instances were found in the fabrication panel",
        );
        output.rules.push(result);
        return;
    }

    let start = output.findings.len();
    for first_index in 0..arrays.len() {
        for second in &arrays[first_index + 1..] {
            result.checked_entities += 1;
            let Some(clearance) = region_clearance(&arrays[first_index].region, &second.region)
            else {
                continue;
            };
            if !violates_minimum(clearance.distance_mm, limit.millimeters()) {
                continue;
            }
            let first = &arrays[first_index];
            output.findings.push(
                ClearanceViolation {
                    rule_id: RULE_ID,
                    title: "Board arrays are too close together",
                    message: format!(
                    "board-array outlines are {:.6} mm apart; the PDK requires at least {:.6} mm",
                    clearance.distance_mm,
                    limit.millimeters()
                    ),
                    quantity: "board_array_outline_spacing",
                    method: "filled_profile_boundary_distance",
                    witness_roles: ["first_board_array", "second_board_array"],
                    clearance,
                    limit,
                    layers: Vec::new(),
                    subjects: vec![
                        board_array_subject(first, "first"),
                        board_array_subject(second, "second"),
                    ],
                    evidence: vec![
                        Evidence::bounds("first_board_array", first.region.bbox),
                        Evidence::bounds("second_board_array", second.region.bbox),
                    ],
                }
                .into_finding(),
            );
        }
    }
    result.finish(output.findings.len() - start);
    output.rules.push(result);
}

struct ClearanceViolation<'a> {
    rule_id: &'static str,
    title: &'static str,
    message: String,
    quantity: &'static str,
    method: &'static str,
    witness_roles: [&'static str; 2],
    clearance: ClearanceMeasurement,
    limit: &'a Length,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
    evidence: Vec<Evidence>,
}

impl ClearanceViolation<'_> {
    fn into_finding(self) -> Finding {
        let bbox = BBox::new(
            Point::new(
                self.clearance.first.x.min(self.clearance.second.x),
                self.clearance.first.y.min(self.clearance.second.y),
            ),
            Point::new(
                self.clearance.first.x.max(self.clearance.second.x),
                self.clearance.first.y.max(self.clearance.second.y),
            ),
        );
        Finding {
            id: String::new(),
            rule_id: self.rule_id,
            severity: Severity::Error,
            title: self.title.to_owned(),
            message: self.message,
            measurements: vec![Measurement::minimum(
                self.quantity,
                self.clearance.distance_mm,
                self.limit.millimeters(),
                self.method,
            )],
            location: Location {
                point: Some(self.clearance.first.midpoint(self.clearance.second).into()),
                bounding_box: Some(bbox.into()),
                witnesses: vec![
                    Witness::new(self.witness_roles[0], self.clearance.first),
                    Witness::new(self.witness_roles[1], self.clearance.second),
                ],
            },
            layers: self.layers,
            subjects: self.subjects,
            evidence: self.evidence,
        }
    }
}

fn hole_subject(hole: &Hole, role: &'static str) -> Subject {
    Subject {
        role,
        kind: hole.class.subject_kind(),
        net: hole.net.clone(),
        padstack_ref: hole.padstack_ref.clone(),
        source: Some(SourceLocator {
            step: hole.step.clone(),
            layer: Some(hole.layer.name.clone()),
            set_index: Some(hole.source_set_index),
            feature_index: Some(hole.source_feature_index),
            instance_index: None,
        }),
        ..Subject::default()
    }
}

fn outline_subject(outline: &BoardOutline, role: &'static str) -> Subject {
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
        ..Subject::default()
    }
}

fn board_array_subject(array: &BoardArray, role: &'static str) -> Subject {
    Subject {
        role,
        kind: "board_array_outline",
        name: Some(array.name.clone()),
        source: Some(SourceLocator {
            step: Some(array.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: Some(array.instance_index),
        }),
        ..Subject::default()
    }
}

fn unique_layers<const N: usize>(layers: [&LayerRef; N]) -> Vec<LayerRef> {
    let mut result = Vec::new();
    for layer in layers {
        if !result
            .iter()
            .any(|existing: &LayerRef| existing.name == layer.name)
        {
            result.push(layer.clone());
        }
    }
    result
}

fn violates_minimum(actual: f64, required: f64) -> bool {
    actual + COMPARISON_EPSILON_MM < required
}

fn compare_locations(left: &Location, right: &Location) -> Ordering {
    match (left.point, right.point) {
        (Some(left), Some(right)) => left
            .x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
