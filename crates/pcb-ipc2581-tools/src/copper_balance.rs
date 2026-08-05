//! Copper-balance planning shared by the board-array and fab-panel panelizers.
//!
//! Each panelizer derives its own certified safe regions and per-layer density
//! targets; the shared solve distributes the selected copper spatially across
//! the stackup and lowers the result to positive IPC-2581 features.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    ecad::{FeatureUserPrimitive, SetFeature},
    primitives::{
        Contour as IpcContour, Point as IpcPoint, PolyStep, PolyStepCurve, PolyStepSegment,
        Polygon, UserPrimitive, UserShape, UserShapeType, UserSpecial,
    },
};
use pcb_ir::dialects::ipc::View;
use pcb_ir::geom::copper_balance::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, DenseCopperBalanceResult,
    SpatialCopperBalanceLayerRequest, SpatialCopperBalanceRequest,
    generate_spatial_dense_copper_balance, rounded_hexagonal_void,
};
use pcb_ir::geom::region::simplify_shapes;
use pcb_ir::geom::{ContourBuf, ContourSet, FillRule, PathOp, Point, tol};
use serde::Serialize;

use crate::geometry;
use crate::ipc2581::Ipc2581;

/// Maximum violation area tolerated when certifying a balancing region.
pub const CERTIFICATE_AREA_TOLERANCE_MM2: f64 = 1e-4;

/// One copper layer prepared for the joint spatial solve.
#[derive(Debug, Clone)]
pub struct PreparedCopperLayer {
    pub layer_name: String,
    /// Copper density measured inside the immutable placed footprints.
    pub target_density: f64,
    /// Signed physical stack weight, or zero when the stackup supplied none.
    pub stack_weight_mm2: f64,
    pub existing_copper: ContourSet,
    /// Certified, initially empty region available for generated copper.
    pub safe_region: ContourSet,
}

/// One origin-centered rounded-hex dictionary entry, shared by every void of
/// its radius.
#[derive(Debug, Clone)]
pub struct BalanceVoidTemplate {
    pub id: String,
    pub polygon: Polygon,
}

/// One translated instance of a [`BalanceVoidTemplate`].
#[derive(Debug, Clone)]
pub struct BalanceVoidInstance {
    pub template: String,
    pub x: f64,
    pub y: f64,
}

/// Generated IPC features for one balanced layer, split by polarity.
///
/// The perforated plane is a positive set covering the whole usable region
/// (partial edge voids stay cut into its contours), while every full interior
/// void is a negative dictionary-instance reference. The solver's radius grid
/// keeps the template set small, so each void costs one reference instead of
/// one inline contour.
#[derive(Debug, Clone, Default)]
pub struct BalanceFeatureSets {
    pub positive: Vec<SetFeature>,
    pub templates: Vec<BalanceVoidTemplate>,
    pub instances: Vec<BalanceVoidInstance>,
}

impl BalanceFeatureSets {
    pub fn is_empty(&self) -> bool {
        self.positive.is_empty() && self.instances.is_empty()
    }
}

/// One copper layer's balancing plan and generated IPC features.
#[derive(Debug, Clone)]
pub struct CopperBalanceLayer {
    pub layer_name: String,
    pub target_density: f64,
    pub stack_weight_mm2: f64,
    pub existing_copper: ContourSet,
    pub result: DenseCopperBalanceResult,
    pub features: BalanceFeatureSets,
}

/// Inspectable result of planning automatic copper balance for a panel.
#[derive(Debug, Clone)]
pub struct CopperBalancePlan {
    /// Immutable placed footprints whose density the generated copper extends.
    pub footprints: ContourSet,
    pub retained_area_mm2: f64,
    /// Whether the physical stackup supplied signed stack weights. When false,
    /// every layer balances independently with zero through-stack coupling.
    pub stack_weights_available: bool,
    pub layers: Vec<CopperBalanceLayer>,
}

/// Compact, serializable accounting for one layer's automatic balance.
#[derive(Debug, Clone, Serialize)]
pub struct CopperBalanceLayerReport {
    pub layer_name: String,
    pub mode: CopperBalanceMode,
    pub void_radius_mm: Option<f64>,
    pub min_void_radius_mm: Option<f64>,
    pub max_void_radius_mm: Option<f64>,
    pub stack_weight_mm2: f64,
    pub target_density: f64,
    pub initial_density: f64,
    pub achieved_density: f64,
    pub residual_error: f64,
    pub existing_copper_area_mm2: f64,
    pub desired_added_area_mm2: f64,
    pub generated_area_mm2: f64,
    pub usable_area_mm2: f64,
    pub fixed_empty_area_mm2: f64,
    pub void_count: usize,
}

/// Topology selected for one layer's automatic balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopperBalanceMode {
    None,
    Solid,
    Perforated,
}

/// Compact, serializable accounting for a completed automatic balance plan.
#[derive(Debug, Clone, Serialize)]
pub struct CopperBalanceReport {
    pub retained_area_mm2: f64,
    pub footprint_area_mm2: f64,
    pub stack_weights_available: bool,
    pub layers: Vec<CopperBalanceLayerReport>,
}

impl CopperBalanceReport {
    /// One short status line per copper layer, plus a warning when the
    /// stackup could not supply physical stack weights.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = self
            .layers
            .iter()
            .map(|layer| {
                let geometry = match (
                    layer.mode,
                    layer.min_void_radius_mm,
                    layer.max_void_radius_mm,
                ) {
                    (CopperBalanceMode::None, ..) => "no fill".to_string(),
                    (CopperBalanceMode::Solid, ..) => "solid fill".to_string(),
                    (CopperBalanceMode::Perforated, Some(min), Some(max)) => {
                        format!("{} hex voids r {min:.2}-{max:.2} mm", layer.void_count)
                    }
                    (CopperBalanceMode::Perforated, ..) => {
                        format!("{} hex voids", layer.void_count)
                    }
                };
                format!(
                    "balance {}: {geometry}, density {:.3} -> {:.3} (target {:.3})",
                    layer.layer_name,
                    layer.initial_density,
                    layer.achieved_density,
                    layer.target_density
                )
            })
            .collect::<Vec<_>>();
        if !self.layers.is_empty() && !self.stack_weights_available {
            lines.push(
                "balance: stackup thickness data unavailable; layers balanced independently"
                    .to_string(),
            );
        }
        lines
    }
}

impl CopperBalancePlan {
    /// Discard heavy geometry while retaining enough data to audit the result.
    pub fn report(&self) -> CopperBalanceReport {
        CopperBalanceReport {
            retained_area_mm2: self.retained_area_mm2,
            footprint_area_mm2: self.footprints.area(),
            stack_weights_available: self.stack_weights_available,
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
                        DenseCopperBalanceMode::None => (CopperBalanceMode::None, None),
                        DenseCopperBalanceMode::Solid => (CopperBalanceMode::Solid, None),
                        DenseCopperBalanceMode::Perforated { void_radius_mm } => {
                            (CopperBalanceMode::Perforated, Some(void_radius_mm))
                        }
                    };
                    let (min_void_radius_mm, max_void_radius_mm) = layer
                        .result
                        .full_void_radius_range_mm()
                        .map_or((None, None), |(min, max)| (Some(min), Some(max)));
                    CopperBalanceLayerReport {
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
                        usable_area_mm2,
                        fixed_empty_area_mm2,
                        void_count: layer.result.void_count(),
                    }
                })
                .collect(),
        }
    }
}

/// Jointly distribute every prepared layer's copper over the panel region,
/// then lower each solved layer to positive IPC features.
///
/// The lattice origin is the panel region's minimum corner, so identical
/// inputs always produce identical geometry.
pub fn solve_copper_balance(
    panel_region: &ContourSet,
    footprints: ContourSet,
    stack_weights_available: bool,
    prepared: Vec<PreparedCopperLayer>,
) -> Result<CopperBalancePlan> {
    let spatial_layers = prepared
        .iter()
        .map(|layer| SpatialCopperBalanceLayerRequest {
            safe_region: &layer.safe_region,
            existing_copper: &layer.existing_copper,
            target_density: layer.target_density,
            stack_weight_mm2: layer.stack_weight_mm2,
        })
        .collect::<Vec<_>>();
    let results = generate_spatial_dense_copper_balance(
        DenseCopperBalanceProfile::V1,
        SpatialCopperBalanceRequest {
            panel_region,
            lattice_origin: panel_region.bbox.min,
            layers: &spatial_layers,
        },
    )
    .context("failed to generate spatial copper balance")?;

    let layers = prepared
        .into_iter()
        .zip(results)
        .map(|(layer, result)| {
            let features = balance_features(&result)?;
            Ok(CopperBalanceLayer {
                layer_name: layer.layer_name,
                target_density: layer.target_density,
                stack_weight_mm2: layer.stack_weight_mm2,
                existing_copper: layer.existing_copper,
                result,
                features,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(CopperBalancePlan {
        footprints,
        retained_area_mm2: panel_region.area(),
        stack_weights_available,
        layers,
    })
}

/// Convert a solved layer to IPC features; empty when nothing was generated.
pub fn balance_features(result: &DenseCopperBalanceResult) -> Result<BalanceFeatureSets> {
    match result.solution.mode {
        DenseCopperBalanceMode::None => Ok(BalanceFeatureSets::default()),
        DenseCopperBalanceMode::Solid => Ok(BalanceFeatureSets {
            positive: ipc_contour_features(result)?,
            ..BalanceFeatureSets::default()
        }),
        DenseCopperBalanceMode::Perforated { .. } => {
            let (templates, instances) = hex_void_instances(result)?;
            Ok(BalanceFeatureSets {
                positive: ipc_contour_features(result)?,
                templates,
                instances,
            })
        }
    }
}

/// One shared template per distinct void radius plus one translated instance
/// reference per full interior void.
fn hex_void_instances(
    result: &DenseCopperBalanceResult,
) -> Result<(Vec<BalanceVoidTemplate>, Vec<BalanceVoidInstance>)> {
    let mut templates: Vec<BalanceVoidTemplate> = Vec::new();
    let instances = result
        .full_voids
        .iter()
        .map(|void| {
            let id = format!("balance_hex_{}nm", (void.radius_mm * 1e6).round() as i64);
            if !templates.iter().any(|template| template.id == id) {
                let contour = rounded_hexagonal_void(void.radius_mm)
                    .context("generated copper balance has an invalid rounded-hex void radius")?;
                templates.push(BalanceVoidTemplate {
                    id: id.clone(),
                    polygon: ipc_polygon_from_contour(&contour)?,
                });
            }
            Ok(BalanceVoidInstance {
                template: id,
                x: void.center.x,
                y: void.center.y,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((templates, instances))
}

/// Extract one layer's flattened, composed copper image.
pub fn composed_copper_image(ipc: &Ipc2581, layer_name: &str) -> Result<ContourSet> {
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

/// Signed first-moment weight `z * thickness` per copper layer, from the
/// physical stackup's ordered layer thicknesses. `None` when the stackup
/// cannot locate every copper layer.
pub fn physical_copper_stack_weights(ipc: &Ipc2581) -> Option<HashMap<String, f64>> {
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

/// The positive plane: usable-region components with partial edge voids cut
/// in. Full interior voids are not cut here — they subtract via the negative
/// instance set.
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
        let existing = ContourSet::empty(tol::REGION_MM);
        let layers = [SpatialCopperBalanceLayerRequest {
            safe_region: &safe_region,
            existing_copper: &existing,
            target_density: 0.75,
            stack_weight_mm2: 0.0,
        }];
        let result = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &safe_region,
                lattice_origin: Point::new(10.0, 5.0),
                layers: &layers,
            },
        )
        .unwrap()
        .pop()
        .unwrap();
        let features = balance_features(&result).unwrap();

        assert!(
            matches!(
                result.solution.mode,
                DenseCopperBalanceMode::Perforated { .. }
            ),
            "{:?}",
            result.solution
        );
        assert!(result.solution.generated_area_mm2 > 0.0);
        assert!(!result.full_voids.is_empty());
        assert!(!result.partial_voids.is_empty());
        let contour = |feature: &SetFeature| match feature {
            SetFeature::UserPrimitive(feature) => {
                let UserPrimitive::UserSpecial(user_special) = &feature.primitive;
                let UserShapeType::Contour(contour) = &user_special.shapes[0].shape else {
                    return None;
                };
                Some(contour.clone())
            }
            _ => None,
        };

        // The positive plane carries only the clipped edge voids as cutouts.
        let positive_contours = features
            .positive
            .iter()
            .map(|feature| contour(feature).expect("positive balance feature is a contour"))
            .collect::<Vec<_>>();
        assert_eq!(
            positive_contours
                .iter()
                .map(|contour| contour.cutouts.len())
                .sum::<usize>(),
            result.partial_voids.connected_components().len()
        );

        // Every full void is one translated reference to a shared template.
        assert_eq!(features.instances.len(), result.full_voids.len());
        for (instance, void) in features.instances.iter().zip(&result.full_voids) {
            assert_eq!((instance.x, instance.y), (void.center.x, void.center.y));
            assert!(
                features
                    .templates
                    .iter()
                    .any(|template| template.id == instance.template)
            );
        }
        assert!(features.templates.len() < features.instances.len());
        assert!(features.templates.iter().all(|template| {
            template
                .polygon
                .steps
                .iter()
                .any(|step| matches!(step, PolyStep::Curve(_)))
        }));
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
