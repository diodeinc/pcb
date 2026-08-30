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
    DenseCopperLattice, DenseCopperLatticeSite, DenseCopperVoid,
    ROUNDED_HEXAGON_CORNER_RADIUS_RATIO, SpatialCopperBalanceLayerRequest,
    SpatialCopperBalanceRequest, StackMomentField, generate_spatial_dense_copper_balance,
    rounded_hexagonal_void,
};
use pcb_ir::geom::path::ContourBuf;
use pcb_ir::geom::region::{rings_to_contours, simplify_shapes};
use pcb_ir::geom::{ContourSet, FillRule, PathOp, tol};
use serde::Serialize;

use crate::geometry;
use crate::ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::CopperBalanceKind;

pub(crate) const COPPER_BALANCE_ATTRIBUTE_NAME: &str = "diode.copper_balance";
pub(crate) const COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME: &str = "diode.copper_balance_lattice";
pub(crate) const COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_x_mm";
pub(crate) const COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_y_mm";
pub(crate) const COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_pitch_mm";
pub(crate) const COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_void_radius_mm";
pub(crate) const COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_void_corner_radius_mm";
pub(crate) const COPPER_BALANCE_LATTICE_VALUE: &str = "staggered-hex-v1";

pub(crate) fn copper_balance_attribute_value(kind: CopperBalanceKind) -> &'static str {
    match kind {
        CopperBalanceKind::Plane => "plane",
        CopperBalanceKind::FullVoid => "full_void",
        CopperBalanceKind::EdgeVoid => "edge_void",
        CopperBalanceKind::BoundaryWeb => "boundary_web",
    }
}

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

/// One radius class of exact rounded-hex voids on the declared lattice.
#[derive(Debug, Clone)]
pub struct BalanceVoidSet {
    pub template: String,
    pub radius_mm: f64,
    pub corner_radius_mm: f64,
    pub lattice: DenseCopperLattice,
    pub sites: Vec<DenseCopperLatticeSite>,
}

/// Generated IPC features for one balanced layer, split by polarity: the
/// plane over the whole usable region, voids as shared-template lattice
/// instances, crossing edge voids as pre-clipped contours, and the boundary
/// web as the labeled copper band along the usable boundary.
#[derive(Debug, Clone, Default)]
pub struct BalanceFeatureSets {
    pub plane: Vec<SetFeature>,
    pub boundary_web: Vec<SetFeature>,
    pub templates: Vec<BalanceVoidTemplate>,
    pub void_sets: Vec<BalanceVoidSet>,
    pub edge_voids: Vec<SetFeature>,
}

impl BalanceFeatureSets {
    pub fn is_empty(&self) -> bool {
        self.plane.is_empty()
            && self.boundary_web.is_empty()
            && self.void_sets.is_empty()
            && self.edge_voids.is_empty()
    }

    /// Lower one balanced layer to its ordered generated feature sets — the
    /// positive plane, one negative set per rounded-hex radius class, and the
    /// positive boundary web — plus the dictionary templates the void sets
    /// reference.
    pub(crate) fn into_layer_features(
        self,
        layer_name: &str,
    ) -> (
        Vec<BalanceVoidTemplate>,
        Vec<crate::generated::GeneratedLayerFeature>,
    ) {
        use crate::generated::GeneratedLayerFeature;
        use ipc2581::types::ecad::Polarity;

        let Self {
            plane,
            boundary_web,
            templates,
            void_sets,
            edge_voids,
        } = self;
        let balance_features = |polarity, kind, features, void_set| GeneratedLayerFeature {
            layer_name: layer_name.to_string(),
            polarity,
            copper_balance: Some(kind),
            spec_refs: Vec::new(),
            features,
            void_set,
        };
        let has_edge_voids = !edge_voids.is_empty();
        let features = std::iter::once(balance_features(
            Polarity::Positive,
            CopperBalanceKind::Plane,
            plane,
            None,
        ))
        .chain(void_sets.into_iter().map(|void_set| {
            balance_features(
                Polarity::Negative,
                CopperBalanceKind::FullVoid,
                Vec::new(),
                Some(void_set),
            )
        }))
        .chain(has_edge_voids.then(|| {
            balance_features(
                Polarity::Negative,
                CopperBalanceKind::EdgeVoid,
                edge_voids,
                None,
            )
        }))
        .chain(std::iter::once(balance_features(
            Polarity::Positive,
            CopperBalanceKind::BoundaryWeb,
            boundary_web,
            None,
        )))
        .collect();
        (templates, features)
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
            plane: ipc_region_features(&result.usable)?,
            ..BalanceFeatureSets::default()
        }),
        DenseCopperBalanceMode::Perforated { .. } => {
            let emission = &result.edge_void_emission;
            let (templates, void_sets) = void_sets(result, &emission.instanced)?;
            Ok(BalanceFeatureSets {
                plane: ipc_region_features(&result.usable)?,
                // The plane covers the web exactly, so its decimation is free.
                boundary_web: ipc_region_features(
                    &result.boundary_web().decimate_inward(tol::FLATTEN_MM),
                )?,
                templates,
                void_sets,
                edge_voids: ipc_region_features(&emission.clipped)?,
            })
        }
    }
}

/// One dictionary template and one declared-lattice site set per exact
/// radius, covering the interior voids and the instanced edge voids.
fn void_sets(
    result: &DenseCopperBalanceResult,
    instanced_edge_voids: &[DenseCopperVoid],
) -> Result<(Vec<BalanceVoidTemplate>, Vec<BalanceVoidSet>)> {
    let mut sites_by_radius = std::collections::BTreeMap::<i64, Vec<DenseCopperLatticeSite>>::new();
    for void in result.full_voids.iter().chain(instanced_edge_voids) {
        sites_by_radius
            .entry((void.radius_mm * 1e6).round() as i64)
            .or_default()
            .push(void.site);
    }
    sites_by_radius
        .into_iter()
        .map(|(radius_nm, mut sites)| {
            sites.sort_by_key(|site| (site.column, site.row));
            let radius_mm = radius_nm as f64 / 1e6;
            let id = format!("balance_hex_{radius_nm}nm");
            let contour = rounded_hexagonal_void(radius_mm)
                .context("generated copper balance has an invalid rounded-hex void radius")?;
            let template = BalanceVoidTemplate {
                id: id.clone(),
                contour: IpcContour {
                    polygon: ipc_polygon_from_contour(&contour)?,
                    cutouts: Vec::new(),
                },
            };
            let set = BalanceVoidSet {
                template: id,
                radius_mm,
                corner_radius_mm: radius_mm * ROUNDED_HEXAGON_CORNER_RADIUS_RATIO,
                lattice: result.lattice,
                sites,
            };
            Ok((template, set))
        })
        .collect::<Result<Vec<_>>>()
        .map(|pairs| pairs.into_iter().unzip())
}

/// Extract one layer's flattened, composed copper image.
///
/// Composition goes through the artwork mask fold so clear-polarity
/// features subtract in paint order instead of being unioned as copper.
pub fn composed_copper_image(ipc: &Ipc2581, layer_name: &str) -> Result<ContourSet> {
    let document = geometry::extract_layer_for_view(ipc, layer_name, ArtworkScope::ArrayFlattened)
        .with_context(|| format!("failed to extract IPC-2581 copper layer '{layer_name}'"))?;
    Ok(composed_copper_image_from_document(document))
}

/// Compose an already extracted copper document into its final painted image.
///
/// Callers that also need source-feature identity can inspect or clone the
/// structure-preserving document before handing it to this destructive fold.
pub(crate) fn composed_copper_image_from_document(
    mut document: geometry::GeometryDocument,
) -> ContourSet {
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
    ContourSet::new(rings, FillRule::NonZero, tol::REGION_MM)
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

/// Convert one positive plane or boundary-web region to IPC contours. Voids
/// are emitted separately as negative dictionary instances.
fn ipc_region_features(region: &ContourSet) -> Result<Vec<SetFeature>> {
    simplify_shapes(region.rings.clone(), FillRule::NonZero)
        .into_iter()
        .map(|shape| {
            let mut contours = rings_to_contours(shape).into_iter();
            let outer = contours
                .next()
                .context("generated copper balance plane has no outer boundary")?;
            let cutouts = contours.collect::<Vec<_>>();
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
        assert!(!result.edge_voids.is_empty());
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
            .plane
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

        assert!(!features.boundary_web.is_empty());
        // Dictionary instances cover interior and contained edge voids;
        // crossing voids become clipped contours inside the voidable region.
        let emission = &result.edge_void_emission;
        assert_eq!(
            features
                .void_sets
                .iter()
                .map(|set| set.sites.len())
                .sum::<usize>(),
            result.full_voids.len() + emission.instanced.len()
        );
        assert_eq!(features.edge_voids.is_empty(), emission.clipped.is_empty());
        assert!(emission.clipped.difference(&result.voidable).area() < 1e-6);
        assert!(features.void_sets.iter().all(|set| {
            features
                .templates
                .iter()
                .any(|template| template.id == set.template)
        }));
        assert!(
            features.templates.len()
                < features
                    .void_sets
                    .iter()
                    .map(|set| set.sites.len())
                    .sum::<usize>()
        );
        assert!(features.templates.len() <= DenseCopperBalanceProfile::V1.void_area_levels);
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
