//! Automatic board-array copper balancing.
//!
//! The high-level planner derives a certified safe region and the composed
//! copper image of every copper layer from a completed board array. The
//! low-level adapter remains available for callers that already have explicit
//! geometry and area inputs.

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    LayerFunction,
    ecad::{FeatureUserPrimitive, SetFeature},
    primitives::{
        Contour as IpcContour, Point as IpcPoint, PolyStep, PolyStepCurve, PolyStepSegment,
        Polygon, UserPrimitive, UserShape, UserShapeType, UserSpecial,
    },
};
use pcb_ir::dialects::ipc::{
    BalancingRegionOptions, BoardArraySupportDocument, BoardArraySupportLayerPolicy, View,
    board_array_balancing_region, collect_board_array_balancing_input,
};
use pcb_ir::geom::copper_balance::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, DenseCopperBalanceRequest,
    DenseCopperBalanceResult, generate_dense_copper_balance, rounded_hexagonal_void,
};
use pcb_ir::geom::path::transform_cmds;
use pcb_ir::geom::region::simplify_shapes;
use pcb_ir::geom::{Affine2, ContourBuf, ContourSet, FillRule, PathOp, Point, tol};

use crate::geometry;
use crate::ipc2581::Ipc2581;

const CERTIFICATE_AREA_TOLERANCE_MM2: f64 = 1e-4;

/// One copper layer's automatic balancing plan and generated IPC features.
#[derive(Debug, Clone)]
pub struct AutomaticBoardArrayLayerBalance {
    pub layer_name: String,
    /// Copper density measured inside the repeated board footprints.
    pub board_target_density: f64,
    pub existing_copper: ContourSet,
    pub safe_region: ContourSet,
    pub result: DenseCopperBalanceResult,
    pub features: Vec<SetFeature>,
}

/// Inspectable result of planning automatic copper balance for a board array.
#[derive(Debug, Clone)]
pub struct AutomaticBoardArrayCopperBalance {
    pub panel_outer: ContourSet,
    pub board_footprints: ContourSet,
    pub common_safe_region: ContourSet,
    pub retained_area_mm2: f64,
    pub layers: Vec<AutomaticBoardArrayLayerBalance>,
}

/// Plan best-effort copper balancing for every copper layer in a board array.
///
/// Each layer targets the copper density measured inside the repeated board
/// footprints, extending the board's own density into controllable panel
/// material instead of imposing one universal density across the stackup.
///
/// `ipc` must describe the completed, not-yet-balanced array so that generated
/// rails, V-scores, tooling holes, and fiducials participate in safe-region
/// discovery while balance copper itself does not.
pub fn generate_automatic_board_array_copper_balance(
    ipc: &Ipc2581,
) -> Result<AutomaticBoardArrayCopperBalance> {
    let layout = geometry::extract_layout(ipc)
        .context("failed to extract board-array layout for copper balancing")?;
    let score_lines = geometry::board_array_vscore_lines(ipc)
        .context("failed to extract board-array V-scores for copper balancing")?;
    let fabrication_profile = geometry::board_array_fabrication_profile(ipc, &layout, &score_lines)
        .context("failed to derive board-array fabrication profile for copper balancing")?;
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let support_documents = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| {
            let layer_name = ipc.resolve(layer.name);
            let document =
                geometry::extract_layer_for_view(ipc, layer_name, View::ArraySupport).with_context(
                    || {
                        format!(
                            "failed to extract IPC-2581 array-support layer '{layer_name}' for copper balancing"
                        )
                    },
                )?;
            let policy = if layer.layer_function == LayerFunction::VCut {
                BoardArraySupportLayerPolicy::VCutOperationsOnly
            } else {
                BoardArraySupportLayerPolicy::AllPaintedFeatures
            };
            Ok((document, policy))
        })
        .collect::<Result<Vec<_>>>()?;
    let collection = collect_board_array_balancing_input(
        &layout,
        &fabrication_profile,
        support_documents
            .iter()
            .map(|(document, policy)| BoardArraySupportDocument::new(document, *policy)),
    )
    .context("failed to collect board-array balancing obstacles")?;
    let balancing_region =
        board_array_balancing_region(&collection.input, BalancingRegionOptions::default())
            .context("failed to compute board-array balancing region")?;
    if !balancing_region
        .certificate
        .passes(CERTIFICATE_AREA_TOLERANCE_MM2)
    {
        bail!("computed board-array balancing region failed clearance certification");
    }

    let panel_outer = collection.input.panel_outer;
    let board_footprints = collection.input.board_footprints;
    let common_safe_region = balancing_region.safe_region;
    let retained_area_mm2 = panel_outer.area();
    let board_area_mm2 = board_footprints.area();
    let lattice_origin = panel_outer.bbox.min;
    let mut layers = Vec::new();

    for layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
    {
        let layer_name = ipc.resolve(layer.name).to_string();
        let existing_copper = composed_copper_image(ipc, &layer_name)?.intersection(&panel_outer);
        let board_target_density = (existing_copper.intersection(&board_footprints).area()
            / board_area_mm2)
            .clamp(0.0, 1.0);
        // This is normally redundant because panel-root copper features are
        // already support obstacles, but makes the solver's disjoint-input
        // contract explicit and robust to sub-tolerance extraction overlap.
        let safe_region = common_safe_region.difference(&existing_copper);
        let (result, features) = generate_board_array_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                retained_area_mm2,
                existing_copper_area_mm2: existing_copper.area(),
                target_density: board_target_density,
                lattice_origin,
            },
        )
        .with_context(|| {
            format!("failed to generate copper balance geometry for layer '{layer_name}'")
        })?;
        layers.push(AutomaticBoardArrayLayerBalance {
            layer_name,
            board_target_density,
            existing_copper,
            safe_region,
            result,
            features,
        });
    }

    Ok(AutomaticBoardArrayCopperBalance {
        panel_outer,
        board_footprints,
        common_safe_region,
        retained_area_mm2,
        layers,
    })
}

/// Solve and convert one layer's explicit safe region to positive IPC contours.
pub fn generate_board_array_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<(DenseCopperBalanceResult, Vec<SetFeature>)> {
    let result = generate_dense_copper_balance(profile, request)?;
    let features = match result.solution.mode {
        DenseCopperBalanceMode::None => Vec::new(),
        DenseCopperBalanceMode::Solid => ipc_contour_features(&result.usable, None)?,
        DenseCopperBalanceMode::Perforated { void_radius_mm } => ipc_contour_features(
            &result.usable,
            Some((&result.lattice_centers, void_radius_mm)),
        )?,
    };
    Ok((result, features))
}

fn composed_copper_image(ipc: &Ipc2581, layer_name: &str) -> Result<ContourSet> {
    let mut document = geometry::extract_layer_for_view(ipc, layer_name, View::ArrayFlattened)
        .with_context(|| {
            format!("failed to extract flattened IPC-2581 copper layer '{layer_name}'")
        })?;
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut document);
    Ok(ContourSet::from_painted_paths(
        &document.arena,
        document
            .features
            .iter()
            .flat_map(|feature| feature.paths.slice(&document.arena.paths)),
        tol::REGION_MM,
    ))
}

fn ipc_contour_features(
    region: &ContourSet,
    perforation: Option<(&[Point], f64)>,
) -> Result<Vec<SetFeature>> {
    let void_template = perforation
        .map(|(_, radius)| {
            rounded_hexagonal_void(radius)
                .context("generated copper balance has an invalid rounded-hex void radius")
        })
        .transpose()?;

    simplify_shapes(region.rings.clone(), FillRule::NonZero)
        .into_iter()
        .map(|shape| {
            let component = ContourSet::new(shape.clone(), FillRule::NonZero, region.tolerance);
            let mut contours = shape
                .iter()
                .map(|ring| pcb_ir::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM));
            let polygon = ipc_polygon_from_contour(
                &contours
                    .next()
                    .context("generated copper contour has no outer boundary")?,
            )?;
            let cutouts = contours
                .map(|contour| ipc_polygon_from_contour(&contour))
                .chain(
                    perforation
                        .into_iter()
                        .flat_map(|(centers, _)| centers)
                        .filter(|center| component.contains_point(**center))
                        .map(|center| {
                            let contour = transform_cmds(
                                void_template
                                    .as_ref()
                                    .expect("perforation has a void template")
                                    .cmds
                                    .iter()
                                    .copied(),
                                Affine2::translation(*center),
                            );
                            ipc_polygon_from_contour(&contour)
                        }),
                )
                .collect::<Result<Vec<_>>>()?;
            Ok(SetFeature::UserPrimitive(FeatureUserPrimitive {
                primitive: UserPrimitive::UserSpecial(UserSpecial {
                    shapes: vec![UserShape {
                        shape: UserShapeType::Contour(IpcContour { polygon, cutouts }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
                    }],
                }),
                x: 0.0,
                y: 0.0,
            }))
        })
        .collect()
}

fn ipc_polygon_from_contour(contour: &ContourBuf) -> Result<Polygon> {
    let mut begin = None;
    let mut steps = Vec::new();

    for command in &contour.cmds {
        match command.op {
            PathOp::MoveTo if begin.is_none() => {
                begin = Some(IpcPoint {
                    x: command.p0.x,
                    y: command.p0.y,
                });
            }
            PathOp::LineTo if begin.is_some() => {
                steps.push(PolyStep::Segment(PolyStepSegment {
                    point: IpcPoint {
                        x: command.p0.x,
                        y: command.p0.y,
                    },
                }));
            }
            PathOp::ArcTo if begin.is_some() => {
                steps.push(PolyStep::Curve(PolyStepCurve {
                    point: IpcPoint {
                        x: command.p0.x,
                        y: command.p0.y,
                    },
                    center: IpcPoint {
                        x: command.p1.x,
                        y: command.p1.y,
                    },
                    clockwise: command.clockwise,
                }));
            }
            PathOp::Close if begin.is_some() => {}
            PathOp::CubicTo => {
                bail!("generated copper balance polygon contains an unsupported cubic segment")
            }
            _ => bail!("generated copper balance polygon contains multiple or missing contours"),
        }
    }

    let begin = begin.context("generated copper balance polygon has no starting point")?;
    if steps.len() < 2 {
        bail!("generated copper balance polygon has fewer than three vertices");
    }
    Ok(Polygon { begin, steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::geom::{BBox, ContourSet, Point, tol};

    #[test]
    fn converts_perforated_region_to_positive_ipc_contours() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let (result, features) = generate_board_array_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                retained_area_mm2: 200.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.75,
                lattice_origin: Point::new(10.0, 5.0),
            },
        )
        .unwrap();

        assert!(
            matches!(
                result.solution.mode,
                pcb_ir::geom::copper_balance::DenseCopperBalanceMode::Perforated { .. }
            ),
            "{:?}",
            result.solution
        );
        assert!(!result.copper.is_empty());
        assert!(
            features
                .iter()
                .all(|feature| matches!(feature, SetFeature::UserPrimitive(_)))
        );
        assert!(
            features
                .iter()
                .filter_map(|feature| match feature {
                    SetFeature::UserPrimitive(feature) => {
                        let UserPrimitive::UserSpecial(user_special) = &feature.primitive;
                        let UserShapeType::Contour(contour) = &user_special.shapes[0].shape else {
                            return None;
                        };
                        Some(contour)
                    }
                    _ => None,
                })
                .flat_map(|contour| {
                    std::iter::once(&contour.polygon).chain(contour.cutouts.iter())
                })
                .flat_map(|polygon| &polygon.steps)
                .any(|step| matches!(step, PolyStep::Curve(_)))
        );
    }
}
