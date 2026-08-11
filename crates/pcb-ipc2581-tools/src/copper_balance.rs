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
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::copper_balance::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, DenseCopperBalanceResult,
    SpatialCopperBalanceLayerRequest, SpatialCopperBalanceRequest, StackMomentField,
    generate_spatial_dense_copper_balance, rounded_hexagonal_void,
};
use pcb_ir::geom::region::simplify_shapes;
use pcb_ir::geom::{ContourBuf, ContourSet, FillRule, PathOp, Point, tol};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::geometry;
use crate::ipc2581::Ipc2581;

pub(crate) const COPPER_BALANCE_ATTRIBUTE_NAME: &str = "diode.copper_balance";

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
    /// Region the target density is measured and applied over: the immutable
    /// footprints, this layer's safe region, and any copper outside both.
    /// Permanently bare area is excluded, so the solve never budgets copper it
    /// has nowhere to put. See
    /// [`SpatialCopperBalanceLayerRequest::density_domain`].
    pub density_domain: ContourSet,
}

/// One origin-centered rounded-hex dictionary entry, shared by every void of
/// its radius.
#[derive(Debug, Clone)]
pub struct BalanceVoidTemplate {
    pub id: String,
    pub contour: IpcContour,
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
/// The perforated plane is a positive set covering the whole usable region,
/// while full and clipped voids are negative dictionary-instance references.
/// Repeated shapes share templates, so each occurrence costs only a location.
#[derive(Debug, Clone, Default)]
pub struct BalanceFeatureSets {
    pub positive: Vec<SetFeature>,
    pub templates: Vec<BalanceVoidTemplate>,
    pub full_instances: Vec<BalanceVoidInstance>,
    pub clipped_instances: Vec<BalanceVoidInstance>,
}

impl BalanceFeatureSets {
    pub fn is_empty(&self) -> bool {
        self.positive.is_empty()
            && self.full_instances.is_empty()
            && self.clipped_instances.is_empty()
    }
}

/// One copper layer's balancing plan and generated IPC features.
#[derive(Debug, Clone)]
pub struct CopperBalanceLayer {
    pub layer_name: String,
    pub target_density: f64,
    pub stack_weight_mm2: f64,
    pub existing_copper: ContourSet,
    /// Area of this layer's density domain, the denominator behind every
    /// density in [`CopperBalanceLayer::result`].
    pub density_domain_area_mm2: f64,
    pub result: DenseCopperBalanceResult,
    pub features: BalanceFeatureSets,
}

/// Inspectable result of planning automatic copper balance for a panel.
#[derive(Debug, Clone)]
pub struct CopperBalancePlan {
    /// Immutable placed footprints whose density the generated copper extends.
    pub footprints: ContourSet,
    /// Area of the whole panel region the solve ran over. Context only — each
    /// layer's densities are relative to its own density domain.
    pub panel_area_mm2: f64,
    /// Whether the physical stackup supplied signed stack weights. When false,
    /// every layer balances independently with zero through-stack coupling.
    pub stack_weights_available: bool,
    /// What the solve did to the panel's copper-moment field, when the stackup
    /// supplied the weights to measure it.
    pub moment_field: Option<StackMomentField>,
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
    /// Denominator behind every density on this layer.
    pub density_domain_area_mm2: f64,
    /// Domain area that holds no copper and can take none: the footprints'
    /// own empty space. Permanently bare area outside the domain — process
    /// margins, clearance rings, material removal — is not counted here
    /// because it never enters the density calculation.
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

/// RMS of the panel's copper-moment field before and after the spatial solve.
///
/// Named separately from [`StackMomentField`] only because the report is
/// serializable and `pcb-ir` carries no serde dependency.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct StackMomentFieldReport {
    pub initial_mean: f64,
    pub initial_rms: f64,
    pub achieved_mean: f64,
    pub achieved_rms: f64,
}

impl From<StackMomentField> for StackMomentFieldReport {
    fn from(field: StackMomentField) -> Self {
        Self {
            initial_mean: field.initial_mean,
            initial_rms: field.initial_rms,
            achieved_mean: field.achieved_mean,
            achieved_rms: field.achieved_rms,
        }
    }
}

/// Compact, serializable accounting for a completed automatic balance plan.
#[derive(Debug, Clone, Serialize)]
pub struct CopperBalanceReport {
    pub panel_area_mm2: f64,
    pub footprint_area_mm2: f64,
    pub stack_weights_available: bool,
    /// RMS of the copper-moment field before and after the spatial solve.
    /// Carries bow and twist together, where
    /// [`CopperBalanceReport::stack_moments`] carries bow alone.
    pub moment_field: Option<StackMomentFieldReport>,
    pub layers: Vec<CopperBalanceLayerReport>,
}

impl CopperBalanceReport {
    /// One short status line per copper layer, what the trade did to the
    /// panel's copper moment, plus a warning when the stackup could not supply
    /// physical stack weights.
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
        if let Some(field) = self.moment_field {
            lines.push(format!(
                "balance: stack moment {:.4} -> {:.4} (field rms {:.4} -> {:.4})",
                field.initial_mean, field.achieved_mean, field.initial_rms, field.achieved_rms
            ));
        }
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
            panel_area_mm2: self.panel_area_mm2,
            footprint_area_mm2: self.footprints.area(),
            stack_weights_available: self.stack_weights_available,
            moment_field: self.moment_field.map(StackMomentFieldReport::from),
            layers: self
                .layers
                .iter()
                .map(|layer| {
                    let solution = layer.result.solution;
                    let existing_copper_area_mm2 = layer.existing_copper.area();
                    let usable_area_mm2 = layer.result.usable.area();
                    let fixed_empty_area_mm2 = (layer.density_domain_area_mm2
                        - existing_copper_area_mm2
                        - usable_area_mm2)
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
                        density_domain_area_mm2: layer.density_domain_area_mm2,
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
            density_domain: &layer.density_domain,
            target_density: layer.target_density,
            stack_weight_mm2: layer.stack_weight_mm2,
        })
        .collect::<Vec<_>>();
    let balance = generate_spatial_dense_copper_balance(
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
        .zip(balance.layers)
        .map(|(layer, result)| {
            let features = balance_features(&result)?;
            Ok(CopperBalanceLayer {
                layer_name: layer.layer_name,
                target_density: layer.target_density,
                stack_weight_mm2: layer.stack_weight_mm2,
                existing_copper: layer.existing_copper,
                density_domain_area_mm2: layer.density_domain.area(),
                result,
                features,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(CopperBalancePlan {
        footprints,
        panel_area_mm2: panel_region.area(),
        stack_weights_available,
        moment_field: balance.moment_field,
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
            let (templates, full_instances, clipped_instances) = void_instances(result)?;
            Ok(BalanceFeatureSets {
                positive: ipc_contour_features(result)?,
                templates,
                full_instances,
                clipped_instances,
            })
        }
    }
}

/// One shared template per distinct full or clipped void shape plus one
/// translated instance reference per occurrence.
fn void_instances(
    result: &DenseCopperBalanceResult,
) -> Result<(
    Vec<BalanceVoidTemplate>,
    Vec<BalanceVoidInstance>,
    Vec<BalanceVoidInstance>,
)> {
    let mut templates: Vec<BalanceVoidTemplate> = Vec::new();
    let partial_shapes = simplify_shapes(result.partial_voids.rings.clone(), FillRule::NonZero);
    let mut full_instances = Vec::with_capacity(result.full_voids.len());
    let mut clipped_instances = Vec::with_capacity(partial_shapes.len());

    for void in &result.full_voids {
        let id = format!("balance_hex_{}nm", (void.radius_mm * 1e6).round() as i64);
        if !templates.iter().any(|template| template.id == id) {
            let contour = rounded_hexagonal_void(void.radius_mm)
                .context("generated copper balance has an invalid rounded-hex void radius")?;
            templates.push(BalanceVoidTemplate {
                id: id.clone(),
                contour: IpcContour {
                    polygon: ipc_polygon_from_contour(&contour)?,
                    cutouts: Vec::new(),
                },
            });
        }
        full_instances.push(BalanceVoidInstance {
            template: id,
            x: void.center.x,
            y: void.center.y,
        });
    }

    let mut partial_templates = HashMap::<String, String>::new();
    for shape in partial_shapes {
        let (contour, origin) = local_partial_void_template(&shape)?;
        let key = serialized_contour_key(&contour);
        let id = if let Some(id) = partial_templates.get(&key) {
            id.clone()
        } else {
            let digest = Sha256::digest(key.as_bytes());
            let id = format!("balance_partial_{}", hex::encode(&digest[..8]));
            partial_templates.insert(key, id.clone());
            templates.push(BalanceVoidTemplate {
                id: id.clone(),
                contour,
            });
            id
        };
        clipped_instances.push(BalanceVoidInstance {
            template: id,
            x: origin.x,
            y: origin.y,
        });
    }
    Ok((templates, full_instances, clipped_instances))
}

fn local_partial_void_template(shape: &pcb_ir::geom::region::Shape) -> Result<(IpcContour, Point)> {
    let outer = shape
        .first()
        .context("generated partial copper void has no outer boundary")?;
    let outer = pcb_ir::geom::arcfit::ring_to_contour_with_arcs(outer, tol::FLATTEN_MM);
    let origin = outer.bbox.min;
    let polygon = ipc_polygon_from_contour(&localized_template_contour(&outer, origin))?;
    let cutouts = shape
        .iter()
        .skip(1)
        .map(|ring| {
            let contour = pcb_ir::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM);
            ipc_polygon_from_contour(&localized_template_contour(&contour, origin))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((IpcContour { polygon, cutouts }, origin))
}

fn localized_template_contour(contour: &ContourBuf, origin: Point) -> ContourBuf {
    pcb_ir::geom::path::transform_cmds(
        contour.cmds.iter().copied(),
        pcb_ir::geom::Affine2::translation(Point::new(-origin.x, -origin.y)),
    )
}

fn serialized_contour_key(contour: &IpcContour) -> String {
    let mut writer = ipc2581::XmlWriter::new();
    ipc2581::write::contour(&mut writer, ipc2581::types::Units::Millimeter, contour);
    writer.into_string()
}

/// Extract one layer's flattened, composed copper image.
///
/// Composition goes through the artwork mask fold so clear-polarity
/// features subtract in paint order instead of being unioned as copper.
pub fn composed_copper_image(ipc: &Ipc2581, layer_name: &str) -> Result<ContourSet> {
    let mut document =
        geometry::extract_layer_for_view(ipc, layer_name, ArtworkScope::ArrayFlattened)
            .with_context(|| {
                format!("failed to extract flattened IPC-2581 copper layer '{layer_name}'")
            })?;
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut document);
    let artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
        &document,
        0,
        pcb_ir::dialects::LayerRole::Copper,
        pcb_ir::dialects::Side::None,
    );
    let mask = pcb_ir::dialects::artwork::compose_to_mask(&artwork);
    let mut rings = Vec::new();
    for layer in &mask.layers {
        for shape in mask.shapes(layer) {
            rings.extend(pcb_ir::geom::region::rings_from_contours(
                &mask.arena.path_contours(shape),
            ));
        }
    }
    Ok(ContourSet::new(rings, FillRule::NonZero, tol::REGION_MM))
}

/// Signed first-moment weight `t * z` per copper layer, arms measured from
/// the stackup's stiffness-weighted neutral axis, positive out of the top
/// face. `None` when the stackup cannot locate every copper layer.
///
/// Derived from the same [`pcb_ir::geom::warp::ThermalStack`] the warp
/// estimate reads, so the moment balancing flattens is the moment warp
/// measures.
pub fn physical_copper_stack_weights(ipc: &Ipc2581) -> Option<HashMap<String, f64>> {
    let (stack, copper_names) = crate::warp::physical_stack(ipc).ok()?;
    let expected = ipc
        .ecad()?
        .cad_data
        .layers
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
        .count();
    (copper_names.len() == expected).then(|| {
        copper_names
            .into_iter()
            .zip(stack.conductor_weights())
            .map(|(name, weight)| (name, weight.moment_arm_mm2))
            .collect()
    })
}

/// The positive plane covers the usable region. Full and clipped voids are
/// emitted separately as negative dictionary instances so neither IPC-2581
/// nor Gerber has to repeat their contours inline.
fn ipc_contour_features(result: &DenseCopperBalanceResult) -> Result<Vec<SetFeature>> {
    simplify_shapes(result.usable.rings.clone(), FillRule::NonZero)
        .into_iter()
        .map(|shape| {
            let outer = pcb_ir::geom::arcfit::ring_to_contour_with_arcs(&shape[0], tol::FLATTEN_MM);
            let cutouts = shape
                .iter()
                .skip(1)
                .map(|ring| pcb_ir::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM))
                .collect::<Vec<_>>();
            ipc_contour_feature(&outer, &cutouts)
        })
        .collect()
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
            density_domain: &safe_region,
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
        .layers
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

        // The positive plane stays the usable region; every generated void is
        // a separate negative dictionary instance.
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
            0
        );

        assert_eq!(
            features.full_instances.len() + features.clipped_instances.len(),
            result.void_count()
        );
        for (instance, void) in features.full_instances.iter().zip(&result.full_voids) {
            assert_eq!((instance.x, instance.y), (void.center.x, void.center.y));
            assert!(
                features
                    .templates
                    .iter()
                    .any(|template| template.id == instance.template)
            );
        }
        assert!(
            features.templates.len()
                < features.full_instances.len() + features.clipped_instances.len()
        );
        assert!(
            features
                .templates
                .iter()
                .filter(|template| template.id.starts_with("balance_hex_"))
                .all(|template| {
                    template
                        .contour
                        .polygon
                        .steps
                        .iter()
                        .any(|step| matches!(step, PolyStep::Curve(_)))
                })
        );
    }

    #[test]
    fn partial_template_ids_do_not_alias_across_layers() {
        let panel = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let inset = ContourSet::rectangle(
            BBox::new(Point::new(1.0, 0.5), Point::new(19.0, 9.5)),
            tol::REGION_MM,
        );
        let existing = ContourSet::empty(tol::REGION_MM);
        let layers = [
            SpatialCopperBalanceLayerRequest {
                safe_region: &panel,
                existing_copper: &existing,
                density_domain: &panel,
                target_density: 0.75,
                stack_weight_mm2: 0.0,
            },
            SpatialCopperBalanceLayerRequest {
                safe_region: &inset,
                existing_copper: &existing,
                density_domain: &inset,
                target_density: 0.75,
                stack_weight_mm2: 0.0,
            },
        ];
        let results = generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::new(10.0, 5.0),
                layers: &layers,
            },
        )
        .unwrap()
        .layers;

        let mut contours_by_id = HashMap::new();
        let mut partial_template_count = 0;
        for result in results {
            let features = balance_features(&result).unwrap();
            let partial_templates = features
                .templates
                .iter()
                .filter(|template| template.id.starts_with("balance_partial_"));
            for template in partial_templates {
                partial_template_count += 1;
                let key = serialized_contour_key(&template.contour);
                if let Some(existing) = contours_by_id.insert(template.id.clone(), key.clone()) {
                    assert_eq!(existing, key, "one template id must name one contour");
                }
            }
        }

        assert!(partial_template_count > 1);
        assert!(contours_by_id.len() > 1);
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
