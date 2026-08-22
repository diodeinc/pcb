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
//! a ≥ A_min    on every spanned copper layer where p ∈ M,
//! ```
//!
//! equivalently `D(p, r + A_min) ⊆ M` — the indexed form of a morphological
//! erosion test. For `p ∉ M`, an intermediate layer with no matching land is
//! an anti-pad or removed unused land and has no ring to measure. A terminal
//! layer, or any layer carrying a matching source land, is required to retain
//! copper at `p`; absence there is an authoritative zero-enclosure failure.
//! One finding per hole reports the layer minimizing `a`.
//!
//! Computation: `p ∈ M` by a batched winding-number sweep over all hole
//! centers; `d(p, ∂M)` by a nearest-boundary query against a uniform grid
//! over the boundary segments, searched only to `r + A_min` since farther
//! boundaries cannot violate. The polygon flattening tolerance is
//! subtracted from the requirement so arc discretization cannot
//! manufacture violations.

use pcb_ir::geom::{BBox, Point, tol};
use rayon::prelude::*;

use crate::commands::dfm::design::{CopperLayer, Hole, HoleClass, Land};
use crate::commands::dfm::report::{
    Evidence, Finding, Location, Measurement, SourceLocator, Subject, Witness,
};
use crate::commands::dfm::rules::Rule;

use super::{
    COMPARISON_EPSILON_MM, Context, blank_finding, hole_subject, holes_of_class, unique_layers,
};

/// One (hole, copper layer) enclosure violation candidate.
struct AnnularViolation<'a> {
    copper_index: usize,
    land: Option<&'a Land>,
    enclosure_mm: f64,
    geometry: AnnularViolationGeometry,
}

enum AnnularViolationGeometry {
    Boundary {
        cutout_boundary: Point,
        material_boundary: Point,
    },
    MissingCopper,
}

pub(super) fn evaluate(rule: &Rule, class: HoleClass, ctx: &Context) -> (usize, Vec<Finding>) {
    let design = ctx.design;
    let holes = holes_of_class(design, class);
    let limit = rule.limit.millimeters();
    let tolerance = tol::FLATTEN_MM + COMPARISON_EPSILON_MM;
    let centers = holes.iter().map(|hole| hole.center).collect::<Vec<_>>();
    let contains = design
        .copper_layers
        .par_iter()
        .map(|layer| layer.image.contains_points_batch(&centers))
        .collect::<Vec<_>>();
    let boundaries = ctx.copper_boundaries();

    let per_hole = holes
        .par_iter()
        .enumerate()
        .map(|(hole_index, hole)| {
            let radius = hole.diameter_mm / 2.0;
            let mut checked = 0;
            let mut worst: Option<AnnularViolation> = None;
            for (copper_index, copper) in design.copper_layers.iter().enumerate() {
                if !hole.spans_copper(copper_index as u16) {
                    continue;
                }
                let land = hole
                    .land_on(copper_index)
                    .map(|link| &copper.lands[link.land_index as usize]);
                if !contains[copper_index][hole_index] {
                    // Intermediate layers without a source land are plane
                    // anti-pads or removed unused lands. A terminal layer or
                    // intended land with no composed copper is not allowed to
                    // disappear from an annular-ring check.
                    if land.is_none()
                        && !hole.terminates_on(copper_index, design.copper_layers.len())
                    {
                        continue;
                    }
                    checked += 1;
                    let violation = AnnularViolation {
                        copper_index,
                        land,
                        enclosure_mm: 0.0,
                        geometry: AnnularViolationGeometry::MissingCopper,
                    };
                    if worst
                        .as_ref()
                        .is_none_or(|current| violation.enclosure_mm < current.enclosure_mm)
                    {
                        worst = Some(violation);
                    }
                    continue;
                }
                checked += 1;
                let violation = boundaries[copper_index]
                    .circular_enclosure(hole.center, radius, limit - tolerance)
                    .filter(|measurement| measurement.enclosure_mm + tolerance < limit)
                    .map(|measurement| AnnularViolation {
                        copper_index,
                        land,
                        enclosure_mm: measurement.enclosure_mm,
                        geometry: AnnularViolationGeometry::Boundary {
                            cutout_boundary: measurement.cutout_boundary,
                            material_boundary: measurement.material_boundary,
                        },
                    });
                if let Some(violation) = violation
                    && worst
                        .as_ref()
                        .is_none_or(|current| violation.enclosure_mm < current.enclosure_mm)
                {
                    worst = Some(violation);
                }
            }
            (
                checked,
                worst.map(|worst| annular_finding(rule, class, hole, ctx, worst)),
            )
        })
        .collect::<Vec<_>>();

    (
        per_hole.iter().map(|(checked, _)| checked).sum(),
        per_hole
            .into_iter()
            .filter_map(|(_, finding)| finding)
            .collect(),
    )
}

fn annular_finding(
    rule: &Rule,
    class: HoleClass,
    hole: &Hole,
    ctx: &Context,
    violation: AnnularViolation,
) -> Finding {
    let limit = rule.limit.millimeters();
    let copper = &ctx.design.copper_layers[violation.copper_index];
    let required_radius = hole.diameter_mm / 2.0 + limit;
    let (location_point, witnesses, detail) = match violation.geometry {
        AnnularViolationGeometry::Boundary {
            cutout_boundary,
            material_boundary,
        } => {
            let detail = if violation.enclosure_mm < 0.0 {
                format!(
                    "the drilled hole breaches the copper image by {:.6} mm",
                    -violation.enclosure_mm
                )
            } else {
                format!(
                    "only {:.6} mm of copper remains outside the drilled hole",
                    violation.enclosure_mm
                )
            };
            (
                cutout_boundary.midpoint(material_boundary),
                vec![
                    Witness::new("hole_boundary", cutout_boundary),
                    Witness::new("copper_boundary", material_boundary),
                ],
                detail,
            )
        }
        AnnularViolationGeometry::MissingCopper => (
            hole.center,
            Vec::new(),
            "no composed copper remains at the required hole center".to_owned(),
        ),
    };
    Finding {
        title: format!("{} annular ring is below minimum", class.label()),
        message: format!(
            "{} minimum radial copper enclosure is {:.6} mm on {}; the PDK requires {:.6} mm ({detail})",
            class.label(),
            violation.enclosure_mm,
            copper.layer.name,
            limit
        ),
        measurement: Measurement::minimum(violation.enclosure_mm, limit),
        location: Location {
            point: Some(location_point.into()),
            bounding_box: Some(BBox::from_point(hole.center).expand(required_radius).into()),
            witnesses,
        },
        layers: unique_layers(&hole.layer, &copper.layer),
        subjects: annular_ring_subjects(ctx, hole, copper, violation.land),
        evidence: annular_ring_evidence(hole, violation.land, 2.0 * required_radius),
        ..blank_finding(rule)
    }
}

fn annular_ring_subjects(
    ctx: &Context,
    hole: &Hole,
    copper: &CopperLayer,
    land: Option<&Land>,
) -> Vec<Subject> {
    let mut subjects = vec![hole_subject(ctx, hole, "hole")];
    subjects.push(match land {
        Some(land) => Subject {
            role: "land",
            kind: "padstack_land",
            name: ctx.resolve(land.primitive_ref),
            reference_designator: ctx.resolve(land.reference_designator),
            pin: ctx.resolve(land.pin),
            net: ctx.resolve(land.net),
            padstack_ref: ctx.resolve(Some(land.padstack)),
            source: Some(SourceLocator {
                step: ctx.resolve(land.step),
                layer: Some(copper.layer.name.clone()),
                set_index: Some(land.source_set_index),
                feature_index: Some(land.source_feature_index),
                instance_index: None,
            }),
        },
        None => Subject {
            role: "land",
            kind: "composed_copper",
            name: Some(copper.layer.name.clone()),
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

#[cfg(test)]
mod tests {
    use pcb_ir::dialects::ipc::ArtworkScope;
    use pcb_ir::geom::{ContourSet, FillRule, Point, shapes, tol};

    use crate::commands::dfm::design::{Design, Hole};
    use crate::commands::dfm::pdk::Pdk;
    use crate::commands::dfm::report::LayerRef;
    use crate::commands::dfm::rules;
    use crate::ipc2581::Ipc2581;

    use super::*;

    fn rule() -> Rule {
        let pdk = Pdk::parse(
            r#"schema_version = 1

[pdk]
id = "test"
name = "Test"
revision = "1"

[capabilities.copper]
minimum_pth_annular_ring = "0.2 mm"
"#,
        )
        .unwrap();
        rules::lower(&pdk).unwrap().remove(0)
    }

    fn ipc() -> Ipc2581 {
        Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap()
    }

    fn copper_layer(name: &str, diameter_mm: Option<f64>) -> CopperLayer {
        let image = diameter_mm.map_or_else(
            || ContourSet::empty(tol::REGION_MM),
            |diameter| {
                ContourSet::from_contours(
                    &[shapes::circle(diameter).unwrap()],
                    FillRule::NonZero,
                    tol::REGION_MM,
                )
            },
        );
        CopperLayer {
            layer: LayerRef {
                name: name.to_owned(),
                function: "CONDUCTOR".to_owned(),
                side: None,
            },
            image,
            lands: Vec::new(),
        }
    }

    fn through_hole_design(layer_diameters: &[Option<f64>]) -> Design {
        let center = Point::ZERO;
        Design {
            scope: ArtworkScope::Board,
            holes: vec![Hole {
                class: HoleClass::Pth,
                center,
                diameter_mm: 1.0,
                bbox: BBox::from_point(center).expand(0.5),
                layer: LayerRef {
                    name: "DRILL".to_owned(),
                    function: "DRILL".to_owned(),
                    side: None,
                },
                copper_span: None,
                step: None,
                padstack: None,
                net: None,
                lands: Vec::new(),
                source_set_index: 0,
                source_feature_index: 0,
            }],
            slots: Vec::new(),
            copper_layers: layer_diameters
                .iter()
                .enumerate()
                .map(|(index, diameter)| copper_layer(&format!("L{index}"), *diameter))
                .collect(),
            mask_layers: Vec::new(),
            scores: Vec::new(),
            board_outlines: Vec::new(),
            board_arrays: Vec::new(),
        }
    }

    #[test]
    fn missing_terminal_copper_is_a_zero_enclosure_violation() {
        let rule = rule();
        let ipc = ipc();
        let design = through_hole_design(&[None, None, Some(2.0)]);
        let ctx = Context::new(&design, &ipc, std::slice::from_ref(&rule));

        let (checked, findings) = evaluate(&rule, HoleClass::Pth, &ctx);

        assert_eq!(checked, 2);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].measurement.actual_mm, 0.0);
        assert!(findings[0].message.contains("no composed copper remains"));
    }

    #[test]
    fn intermediate_antipad_without_a_source_land_is_not_an_annular_subject() {
        let rule = rule();
        let ipc = ipc();
        let design = through_hole_design(&[Some(2.0), None, Some(2.0)]);
        let ctx = Context::new(&design, &ipc, std::slice::from_ref(&rule));

        let (checked, findings) = evaluate(&rule, HoleClass::Pth, &ctx);

        assert_eq!(checked, 2);
        assert!(findings.is_empty());
    }

    #[test]
    fn known_blind_span_requires_copper_at_its_own_terminal_layers() {
        let rule = rule();
        let ipc = ipc();
        let mut design = through_hole_design(&[None, None, Some(2.0), None]);
        design.holes[0].copper_span = Some((1, 2));
        let ctx = Context::new(&design, &ipc, std::slice::from_ref(&rule));

        let (checked, findings) = evaluate(&rule, HoleClass::Pth, &ctx);

        assert_eq!(checked, 2);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].layers[1].name, "L1");
        assert_eq!(findings[0].measurement.actual_mm, 0.0);
    }
}
