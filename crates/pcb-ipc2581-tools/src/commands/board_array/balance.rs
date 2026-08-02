//! Automatic board-array copper balancing.
//!
//! The high-level planner derives a certified safe region and the composed
//! copper image of every copper layer from a completed board array. The
//! low-level adapter remains available for callers that already have explicit
//! geometry and area inputs.

use std::collections::HashMap;

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
    DenseCopperBalanceResult, SpatialCopperBalanceLayerRequest, SpatialCopperBalanceRequest,
    generate_dense_copper_balance, generate_spatial_dense_copper_balance, rounded_hexagonal_void,
};
use pcb_ir::geom::path::transform_cmds;
use pcb_ir::geom::region::simplify_shapes;
use pcb_ir::geom::{Affine2, ContourBuf, ContourSet, FillRule, PathOp, Point, tol};
use serde::Serialize;

use crate::geometry;
use crate::ipc2581::Ipc2581;

const CERTIFICATE_AREA_TOLERANCE_MM2: f64 = 1e-4;

/// One copper layer's automatic balancing plan and generated IPC features.
#[derive(Debug, Clone)]
pub struct AutomaticBoardArrayLayerBalance {
    pub layer_name: String,
    /// Copper density measured inside the repeated board footprints.
    pub board_target_density: f64,
    pub stack_weight_mm2: Option<f64>,
    pub existing_copper: ContourSet,
    pub result: DenseCopperBalanceResult,
    pub features: Vec<SetFeature>,
}

/// Inspectable result of planning automatic copper balance for a board array.
#[derive(Debug, Clone)]
pub struct AutomaticBoardArrayCopperBalance {
    pub panel_outer: ContourSet,
    pub board_footprints: ContourSet,
    pub retained_area_mm2: f64,
    pub layers: Vec<AutomaticBoardArrayLayerBalance>,
}

/// Compact, serializable accounting for one layer's automatic balance.
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticBoardArrayLayerBalanceReport {
    pub layer_name: String,
    pub mode: AutomaticBoardArrayCopperBalanceMode,
    pub void_radius_mm: Option<f64>,
    pub min_void_radius_mm: Option<f64>,
    pub max_void_radius_mm: Option<f64>,
    pub stack_weight_mm2: Option<f64>,
    pub target_density: f64,
    pub initial_density: f64,
    pub achieved_density: f64,
    pub residual_error: f64,
    pub existing_copper_area_mm2: f64,
    pub desired_added_area_mm2: f64,
    pub generated_area_mm2: f64,
    pub safe_area_mm2: f64,
    pub usable_area_mm2: f64,
    pub fixed_empty_area_mm2: f64,
    pub void_count: usize,
}

/// Topology selected for one layer's automatic balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticBoardArrayCopperBalanceMode {
    None,
    Solid,
    Perforated,
}

/// Compact, serializable accounting for a completed automatic balance plan.
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticBoardArrayCopperBalanceReport {
    pub retained_area_mm2: f64,
    pub board_footprint_area_mm2: f64,
    pub layers: Vec<AutomaticBoardArrayLayerBalanceReport>,
}

impl AutomaticBoardArrayCopperBalance {
    /// Discard heavy geometry while retaining enough data to audit the result.
    pub fn report(&self) -> AutomaticBoardArrayCopperBalanceReport {
        AutomaticBoardArrayCopperBalanceReport {
            retained_area_mm2: self.retained_area_mm2,
            board_footprint_area_mm2: self.board_footprints.area(),
            layers: self
                .layers
                .iter()
                .map(|layer| {
                    let solution = layer.result.solution;
                    let existing_copper_area_mm2 = layer.existing_copper.area();
                    let usable_area_mm2 = layer.result.usable.area();
                    let fixed_empty_area_mm2 =
                        (self.retained_area_mm2 - existing_copper_area_mm2 - usable_area_mm2)
                            .max(0.0);
                    let (mode, void_radius_mm) = match solution.mode {
                        DenseCopperBalanceMode::None => {
                            (AutomaticBoardArrayCopperBalanceMode::None, None)
                        }
                        DenseCopperBalanceMode::Solid => {
                            (AutomaticBoardArrayCopperBalanceMode::Solid, None)
                        }
                        DenseCopperBalanceMode::Perforated { void_radius_mm } => (
                            AutomaticBoardArrayCopperBalanceMode::Perforated,
                            Some(void_radius_mm),
                        ),
                    };
                    let (min_void_radius_mm, max_void_radius_mm) = layer
                        .result
                        .full_void_radius_range_mm()
                        .map_or((None, None), |(min, max)| (Some(min), Some(max)));
                    AutomaticBoardArrayLayerBalanceReport {
                        layer_name: layer.layer_name.clone(),
                        mode,
                        void_radius_mm,
                        min_void_radius_mm,
                        max_void_radius_mm,
                        stack_weight_mm2: layer.stack_weight_mm2,
                        target_density: solution.target_density,
                        initial_density: solution.initial_density,
                        achieved_density: solution.achieved_density,
                        residual_error: solution.residual_error,
                        existing_copper_area_mm2,
                        desired_added_area_mm2: solution.desired_added_area_mm2,
                        generated_area_mm2: solution.generated_area_mm2,
                        safe_area_mm2: layer.result.usable.area(),
                        usable_area_mm2,
                        fixed_empty_area_mm2,
                        void_count: layer.result.void_count(),
                    }
                })
                .collect(),
        }
    }
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
    let copper_layers = crate::layers::copper_layers(ecad);
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
        &copper_layers,
        support_documents
            .iter()
            .map(|(document, policy)| BoardArraySupportDocument::new(document, *policy)),
    )
    .context("failed to collect board-array balancing obstacles")?;
    let panel_outer = collection.panel_outer.clone();
    let board_footprints = collection.board_footprints.clone();
    let retained_area_mm2 = panel_outer.area();
    let board_area_mm2 = board_footprints.area();
    let lattice_origin = panel_outer.bbox.min;
    let stack_weights = physical_copper_stack_weights(ipc);
    let prepared = ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
        .map(|layer| {
            let layer_name = ipc.resolve(layer.name).to_string();
            let existing_copper =
                composed_copper_image(ipc, &layer_name)?.intersection(&panel_outer);
            let board_target_density = (existing_copper.intersection(&board_footprints).area()
                / board_area_mm2)
                .clamp(0.0, 1.0);
            let stack_weight_mm2 = stack_weights
                .as_ref()
                .and_then(|weights| weights.get(&layer_name).copied());
            let balancing_input = collection.input_for_layer(layer.name);
            let balancing_region =
                board_array_balancing_region(&balancing_input, BalancingRegionOptions::default())
                    .with_context(|| {
                        format!(
                            "failed to compute board-array balancing region for layer '{layer_name}'"
                        )
                    })?;
            if !balancing_region
                .certificate
                .passes(CERTIFICATE_AREA_TOLERANCE_MM2)
            {
                bail!(
                    "computed board-array balancing region for layer '{layer_name}' failed clearance certification"
                );
            }
            let safe_region = balancing_region.safe_region;
            Ok(PreparedLayerBalance {
                layer_name,
                board_target_density,
                stack_weight_mm2,
                existing_copper,
                safe_region,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let spatial_layers = prepared
        .iter()
        .map(|layer| SpatialCopperBalanceLayerRequest {
            safe_region: &layer.safe_region,
            existing_copper: &layer.existing_copper,
            existing_copper_area_mm2: layer.existing_copper.area(),
            target_density: layer.board_target_density,
            stack_weight_mm2: layer.stack_weight_mm2.unwrap_or(0.0),
        })
        .collect::<Vec<_>>();
    let results = generate_spatial_dense_copper_balance(
        DenseCopperBalanceProfile::V1,
        SpatialCopperBalanceRequest {
            panel_region: &panel_outer,
            retained_area_mm2,
            lattice_origin,
            layers: &spatial_layers,
        },
    )
    .context("failed to generate spatial board-array copper balance")?;
    let layers = prepared
        .into_iter()
        .zip(results)
        .map(|(layer, result)| {
            let features = match result.solution.mode {
                DenseCopperBalanceMode::None => Vec::new(),
                DenseCopperBalanceMode::Solid | DenseCopperBalanceMode::Perforated { .. } => {
                    ipc_contour_features(&result)?
                }
            };
            Ok(AutomaticBoardArrayLayerBalance {
                layer_name: layer.layer_name,
                board_target_density: layer.board_target_density,
                stack_weight_mm2: layer.stack_weight_mm2,
                existing_copper: layer.existing_copper,
                result,
                features,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(AutomaticBoardArrayCopperBalance {
        panel_outer,
        board_footprints,
        retained_area_mm2,
        layers,
    })
}

struct PreparedLayerBalance {
    layer_name: String,
    board_target_density: f64,
    stack_weight_mm2: Option<f64>,
    existing_copper: ContourSet,
    safe_region: ContourSet,
}

/// Solve and convert one layer's explicit safe region to positive IPC contours.
pub fn generate_board_array_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<(DenseCopperBalanceResult, Vec<SetFeature>)> {
    let result = generate_dense_copper_balance(profile, request)?;
    let features = match result.solution.mode {
        DenseCopperBalanceMode::None => Vec::new(),
        DenseCopperBalanceMode::Solid | DenseCopperBalanceMode::Perforated { .. } => {
            ipc_contour_features(&result)?
        }
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

fn physical_copper_stack_weights(ipc: &Ipc2581) -> Option<HashMap<String, f64>> {
    let ecad = ipc.ecad()?;
    let stackup = ecad.cad_data.stackups.first()?;
    let mut layers = stackup.layers.iter().collect::<Vec<_>>();
    if layers.iter().all(|layer| layer.layer_number.is_some()) {
        layers.sort_by_key(|layer| layer.layer_number);
    }

    let mut cursor_mm = 0.0;
    let mut copper = Vec::new();
    for stack_layer in layers {
        let thickness_mm = stack_layer.thickness?;
        if !thickness_mm.is_finite() || thickness_mm < 0.0 {
            return None;
        }
        let center_mm = cursor_mm + thickness_mm / 2.0;
        let name = ipc.resolve(stack_layer.layer_ref);
        if ecad.cad_data.layers.iter().any(|layer| {
            ipc.resolve(layer.name) == name && crate::layers::is_copper(layer.layer_function)
        }) {
            if thickness_mm <= 0.0 {
                return None;
            }
            copper.push((name.to_string(), center_mm, thickness_mm));
        }
        cursor_mm += thickness_mm;
    }

    let expected = ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
        .count();
    if copper.len() != expected || cursor_mm <= 0.0 {
        return None;
    }
    let midplane_mm = cursor_mm / 2.0;
    Some(
        copper
            .into_iter()
            .map(|(name, center_mm, thickness_mm)| (name, (midplane_mm - center_mm) * thickness_mm))
            .collect(),
    )
}

struct IpcCopperComponent {
    region: ContourSet,
    outer: ContourBuf,
    cutouts: Vec<ContourBuf>,
}

fn ipc_contour_features(result: &DenseCopperBalanceResult) -> Result<Vec<SetFeature>> {
    let mut components = simplify_shapes(result.usable.rings.clone(), FillRule::NonZero)
        .into_iter()
        .map(|shape| IpcCopperComponent {
            region: ContourSet::new(shape.clone(), FillRule::NonZero, result.usable.tolerance),
            outer: pcb_ir::geom::arcfit::ring_to_contour_with_arcs(&shape[0], tol::FLATTEN_MM),
            cutouts: shape
                .iter()
                .skip(1)
                .map(|ring| pcb_ir::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM))
                .collect(),
        })
        .collect::<Vec<_>>();

    if matches!(
        result.solution.mode,
        DenseCopperBalanceMode::Perforated { .. }
    ) {
        for void in &result.full_voids {
            let template = rounded_hexagonal_void(void.radius_mm)
                .context("generated copper balance has an invalid rounded-hex void radius")?;
            let component = containing_component(&mut components, void.center)
                .context("generated full copper void lies outside the usable region")?;
            component.cutouts.push(transform_cmds(
                template.cmds.iter().copied(),
                Affine2::translation(void.center),
            ));
        }
    }

    let mut islands = Vec::new();
    for shape in simplify_shapes(result.partial_voids.rings.clone(), FillRule::NonZero) {
        let &[x, y] = shape
            .first()
            .and_then(|ring| ring.first())
            .context("generated partial copper void has no outer boundary")?;
        let component = containing_component(&mut components, Point::new(x, y))
            .context("generated partial copper void lies outside the usable region")?;
        component
            .cutouts
            .push(pcb_ir::geom::arcfit::ring_to_contour_with_arcs(
                &shape[0],
                tol::FLATTEN_MM,
            ));
        islands.extend(
            shape
                .iter()
                .skip(1)
                .map(|ring| pcb_ir::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM)),
        );
    }

    components
        .into_iter()
        .map(|component| ipc_contour_feature(&component.outer, &component.cutouts))
        .chain(
            islands
                .iter()
                .map(|island| ipc_contour_feature(island, &[])),
        )
        .collect()
}

fn containing_component(
    components: &mut [IpcCopperComponent],
    point: Point,
) -> Option<&mut IpcCopperComponent> {
    components
        .iter_mut()
        .find(|component| component.region.contains_point(point))
}

fn ipc_contour_feature(outer: &ContourBuf, cutout_contours: &[ContourBuf]) -> Result<SetFeature> {
    let polygon = ipc_polygon_from_contour(outer)?;
    let cutouts = cutout_contours
        .iter()
        .map(ipc_polygon_from_contour)
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
        assert!(result.solution.generated_area_mm2 > 0.0);
        assert!(!result.full_voids.is_empty());
        assert!(!result.partial_voids.is_empty());
        assert!(
            features
                .iter()
                .all(|feature| matches!(feature, SetFeature::UserPrimitive(_)))
        );
        let contours = features
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
            .collect::<Vec<_>>();
        assert_eq!(
            contours
                .iter()
                .map(|contour| contour.cutouts.len())
                .sum::<usize>(),
            result.void_count()
        );
        assert!(
            contours
                .iter()
                .flat_map(|contour| {
                    std::iter::once(&contour.polygon).chain(contour.cutouts.iter())
                })
                .flat_map(|polygon| &polygon.steps)
                .any(|step| matches!(step, PolyStep::Curve(_)))
        );
    }

    #[test]
    fn derives_signed_stack_weights_from_physical_layer_centers() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Cu"/>
    <LayerRef name="B.Cu"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="1.6">
        <StackupGroup name="Primary_Group">
          <StackupLayer layerOrGroupRef="COATING_TOP" thickness="0" sequence="0"/>
          <StackupLayer layerOrGroupRef="F.Cu" thickness="0.035" sequence="1"/>
          <StackupLayer layerOrGroupRef="CORE" thickness="1.53" sequence="2"/>
          <StackupLayer layerOrGroupRef="B.Cu" thickness="0.035" sequence="3"/>
          <StackupLayer layerOrGroupRef="COATING_BOTTOM" thickness="0" sequence="4"/>
        </StackupGroup>
      </Stackup>
      <Step name="board" type="BOARD"><Datum x="0" y="0"/></Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let weights = physical_copper_stack_weights(&ipc).unwrap();

        assert!((weights["F.Cu"] + weights["B.Cu"]).abs() <= 1e-12);
        assert!(weights["F.Cu"] > 0.0);
        assert!(weights["B.Cu"] < 0.0);
    }
}
