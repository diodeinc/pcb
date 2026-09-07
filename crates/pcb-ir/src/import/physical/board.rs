//! Board-local mechanical evidence over the existing headless import APIs.

use anyhow::{Result, ensure};
use ipc2581::{Symbol, types::LayerFunction};

use super::{Association, PhysicalHole, physical_stackup_layers};
use crate::dialects::assembly::{
    self, ComponentOccurrenceId, PackageGeometryStatus, PackageViewKind,
};
use crate::dialects::ipc::{ArtworkScope, LayoutStepKind};
use crate::geom::{Affine2, ContourSet, Diagnostic, Mirror, Point, Resolution};
use crate::import::ipc2581::{FeatureOccurrenceId, ImportedDesign, LayerId, is_copper};

#[cfg(test)]
mod tests;

/// A canonical board definition, not a panel instance. All planar coordinates
/// and thicknesses are millimeters; no datum shift or placement is applied.
/// Regions use the caller's explicit resolution and propagate accuracy errors.
#[derive(Debug, Clone)]
pub struct BoardPhysicalView {
    pub step: u32,
    /// Nominal substrate after profile cutouts, before drilling/routing.
    /// Blind/buried or unknown-span holes must not be subtracted in 2D.
    pub substrate: ContourSet,
    pub profile_cutouts: ContourSet,
    /// Indices into `ImportedDesign::geometry.profiles` backing the substrate.
    pub profiles: Vec<u32>,
    /// Final, polarity-composed copper, kept separate by source layer.
    pub copper: Vec<BoardCopper>,
    /// Final drill/rout images, including non-hole artwork. These are not
    /// automatically through-board voids; source features retain their spans.
    pub removal_layers: Vec<BoardRemovalLayer>,
    /// Source-identified hole/slot apertures, retaining plating and Z-span.
    /// Unlike removal_layers these are source images, before polarity clears.
    pub holes: Vec<PhysicalHole>,
    pub components: Vec<ComponentEnvelopes>,
    pub metadata: BoardPhysicalMetadata,
    pub diagnostics: Vec<BoardPhysicalDiagnostic>,
    pub source_diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct BoardRemovalLayer {
    pub layer: LayerId,
    pub image: ContourSet,
    pub sources: Vec<FeatureOccurrenceId>,
}

#[derive(Debug, Clone)]
pub struct BoardCopper {
    pub layer: LayerId,
    pub image: ContourSet,
}

/// These are source semantics, not interchangeable collision envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeKind {
    /// IPC Package/Outline does not specify whether the exporter used a body
    /// outline or a courtyard. It must not be promoted to physical material.
    PackageOutlineUnspecified,
    /// Outline explicitly carried by the package AssemblyDrawing. This is
    /// assembly evidence, not a measured 3D body or a clearance requirement.
    AssemblyOutline,
}

#[derive(Debug, Clone)]
pub struct ComponentEnvelope {
    pub kind: EnvelopeKind,
    pub view: PackageViewKind,
    pub status: PackageGeometryStatus,
    /// Interior of the source closed outline (not the ink stroke).
    pub image: ContourSet,
}

#[derive(Debug, Clone)]
pub struct ComponentEnvelopes {
    pub component: ComponentOccurrenceId,
    pub designator: Option<String>,
    pub population: assembly::Population,
    pub side: assembly::Side,
    pub envelopes: Vec<ComponentEnvelope>,
}

/// Source evidence only. No implicit FR-4, copper weight conversion, elastic
/// constants, or thickness fallback. Raw stackups/specs remain on ImportedDesign.
#[derive(Debug, Clone)]
pub struct BoardPhysicalMetadata {
    pub stackup: Association<Symbol>,
    pub overall_thickness_mm: Option<f64>,
    pub layers: Vec<BoardMaterialLayer>,
    pub diagnostics: Vec<BoardPhysicalDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct BoardMaterialLayer {
    pub layer_ref: Symbol,
    pub thickness_mm: Option<f64>,
    pub material: Association<Symbol>,
    pub spec_ref: Option<Symbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoardPhysicalDiagnostic {
    MissingProfile,
    MissingStackup,
    AmbiguousStackup,
    InvalidStackupOrder(String),
    MissingThickness { layer: Option<Symbol> },
    InvalidThickness { layer: Option<Symbol>, value: f64 },
    MissingMaterial { layer: Symbol },
    AmbiguousMaterial { layer: Symbol },
    ConflictingMaterial { layer: Symbol },
    UnresolvedSpec { layer: Symbol, spec: Symbol },
    MissingComponentEnvelope(ComponentOccurrenceId),
    UnspecifiedPackageOutline(ComponentOccurrenceId),
    IncompleteComponentEnvelope(ComponentOccurrenceId),
}

impl ImportedDesign {
    /// Consume a single canonical board without reconstructing IPC geometry.
    /// Multiple board definitions are rejected rather than selecting an
    /// arbitrary board from a heterogeneous panel. Nested panel placement
    /// does not alter the board-local result. Missing metadata is diagnostic;
    /// invalid geometry/materialization remains an error, never an empty view.
    pub fn physical_board(&self, resolution: Resolution) -> Result<BoardPhysicalView> {
        let boards = self
            .geometry
            .layout
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.kind == LayoutStepKind::Board)
            .collect::<Vec<_>>();
        ensure!(
            boards.len() == 1,
            "physical board view requires exactly one board definition; found {}",
            boards.len()
        );
        let (step_index, step) = boards[0];
        let mut substrate = ContourSet::empty(resolution);
        let mut profile_cutouts = ContourSet::empty(resolution);
        let profiles = step.profiles.indices().collect::<Vec<_>>();
        for &index in &profiles {
            let profile = &self.geometry.profiles[index as usize];
            let outer = profile_region(self, profile.outer_path, resolution)?;
            let mut cutouts = ContourSet::empty(resolution);
            for cutout in profile.cutouts.slice(&self.geometry.profile_cutouts) {
                cutouts.union_assign(&profile_region(self, cutout.path, resolution)?)?;
            }
            substrate.union_assign(&outer.difference(&cutouts)?)?;
            profile_cutouts.union_assign(&cutouts)?;
        }
        let copper = self
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| is_copper(layer.layer_function))
            .map(|(index, _)| {
                let layer = LayerId(index as u32);
                Ok(BoardCopper {
                    layer,
                    image: self.composed_layer_image(layer, ArtworkScope::Board, resolution)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let removal_layers = self
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| {
                matches!(
                    layer.layer_function,
                    LayerFunction::Drill | LayerFunction::Rout
                )
            })
            .map(|(index, _)| {
                let layer = LayerId(index as u32);
                Ok(BoardRemovalLayer {
                    layer,
                    image: self.composed_layer_image(layer, ArtworkScope::Board, resolution)?,
                    sources: self
                        .feature_occurrences(layer, ArtworkScope::Board)?
                        .into_iter()
                        .map(|occurrence| occurrence.id)
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let assembly = self.assembly_document(assembly::Scope::Board)?;
        ensure!(
            assembly.root_step == Some(step_index as u32),
            "canonical board is not reachable from the layout root"
        );
        let components = component_envelopes(&assembly, resolution)?;
        let mut diagnostics = Vec::new();
        if substrate.is_empty() {
            diagnostics.push(BoardPhysicalDiagnostic::MissingProfile);
        }
        for component in &components {
            if component.envelopes.is_empty() {
                diagnostics.push(BoardPhysicalDiagnostic::MissingComponentEnvelope(
                    component.component,
                ));
            }
            if component
                .envelopes
                .iter()
                .any(|envelope| envelope.kind == EnvelopeKind::PackageOutlineUnspecified)
            {
                diagnostics.push(BoardPhysicalDiagnostic::UnspecifiedPackageOutline(
                    component.component,
                ));
            }
            if component.envelopes.iter().any(|envelope| {
                envelope.status != PackageGeometryStatus::Complete || envelope.image.is_empty()
            }) {
                diagnostics.push(BoardPhysicalDiagnostic::IncompleteComponentEnvelope(
                    component.component,
                ));
            }
        }
        Ok(BoardPhysicalView {
            step: step_index as u32,
            substrate,
            profile_cutouts,
            profiles,
            copper,
            removal_layers,
            holes: self.physical_holes(ArtworkScope::Board, resolution)?,
            components,
            metadata: self.physical_board_metadata(),
            diagnostics,
            source_diagnostics: self.geometry.diagnostics.clone(),
        })
    }

    /// Inspect material/thickness evidence even when ambiguity prevents
    /// geometry composition. Layer order is validated by the existing physical
    /// span contract; no declaration-order fallback for an invalid stackup.
    pub fn physical_board_metadata(&self) -> BoardPhysicalMetadata {
        let mut result = BoardPhysicalMetadata {
            stackup: Association::Unresolved,
            overall_thickness_mm: None,
            layers: Vec::new(),
            diagnostics: Vec::new(),
        };
        let stackup = match self.stackups.as_slice() {
            [] => {
                result
                    .diagnostics
                    .push(BoardPhysicalDiagnostic::MissingStackup);
                return result;
            }
            [stackup] => stackup,
            stackups => {
                result.stackup =
                    Association::Ambiguous(stackups.iter().map(|stackup| stackup.name).collect());
                result
                    .diagnostics
                    .push(BoardPhysicalDiagnostic::AmbiguousStackup);
                return result;
            }
        };
        result.stackup = Association::Resolved(stackup.name);
        result.overall_thickness_mm =
            thickness(stackup.overall_thickness, None, &mut result.diagnostics);
        let order = match physical_stackup_layers(&self.stackups, &self.layer_definitions) {
            Ok(Some(order)) => order,
            Err(error) => {
                result
                    .diagnostics
                    .push(BoardPhysicalDiagnostic::InvalidStackupOrder(
                        error.to_string(),
                    ));
                return result;
            }
            Ok(None) => unreachable!("one stackup was selected"),
        };
        for layer_ref in order {
            let layer = stackup
                .layers
                .iter()
                .find(|layer| layer.layer_ref == layer_ref)
                .unwrap();
            let spec = layer
                .spec_ref
                .and_then(|reference| self.specs.get(&reference));
            if let Some(reference) = layer.spec_ref
                && spec.is_none()
            {
                result
                    .diagnostics
                    .push(BoardPhysicalDiagnostic::UnresolvedSpec {
                        layer: layer_ref,
                        spec: reference,
                    });
            }
            let mut materials = spec
                .into_iter()
                .flat_map(|spec| spec.properties.iter().copied().chain(spec.material))
                .filter(|material| !self.resolve(*material).trim().is_empty())
                .collect::<Vec<_>>();
            materials.sort_by_key(|material| self.resolve(*material));
            materials.dedup();
            let declared = layer
                .material
                .filter(|material| !self.resolve(*material).trim().is_empty());
            let material = match (declared, materials.as_slice()) {
                (Some(a), values) if !values.is_empty() && !values.contains(&a) => {
                    result
                        .diagnostics
                        .push(BoardPhysicalDiagnostic::ConflictingMaterial { layer: layer_ref });
                    Association::Conflicting(
                        std::iter::once(a).chain(values.iter().copied()).collect(),
                    )
                }
                (Some(material), []) => Association::Resolved(material),
                (_, [material]) => Association::Resolved(*material),
                (None, []) => {
                    result
                        .diagnostics
                        .push(BoardPhysicalDiagnostic::MissingMaterial { layer: layer_ref });
                    Association::Unresolved
                }
                (_, _) => {
                    result
                        .diagnostics
                        .push(BoardPhysicalDiagnostic::AmbiguousMaterial { layer: layer_ref });
                    Association::Ambiguous(materials)
                }
            };
            result.layers.push(BoardMaterialLayer {
                layer_ref,
                thickness_mm: thickness(layer.thickness, Some(layer_ref), &mut result.diagnostics),
                material,
                spec_ref: layer.spec_ref,
            });
        }
        result
    }
}

fn thickness(
    value: Option<f64>,
    layer: Option<Symbol>,
    diagnostics: &mut Vec<BoardPhysicalDiagnostic>,
) -> Option<f64> {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => Some(value),
        Some(value) => {
            diagnostics.push(BoardPhysicalDiagnostic::InvalidThickness { layer, value });
            None
        }
        None => {
            diagnostics.push(BoardPhysicalDiagnostic::MissingThickness { layer });
            None
        }
    }
}

fn profile_region(
    design: &ImportedDesign,
    path: u32,
    resolution: Resolution,
) -> Result<ContourSet> {
    Ok(ContourSet::from_filled_contours(
        &design
            .geometry
            .transformed_path_contours(path, Affine2::IDENTITY),
        resolution,
    )?)
}

/// Derive separate placed envelope evidence directly from canonical assembly
/// IR. This pure operation requires no IPC parser, GUI, or material model.
/// Output coordinates follow the document scope (board-local for Scope::Board).
pub fn component_envelopes(
    document: &assembly::Document,
    resolution: Resolution,
) -> Result<Vec<ComponentEnvelopes>> {
    document
        .occurrences
        .iter()
        .map(|occurrence| {
            let component = &document.components[occurrence.id.component.0 as usize];
            let envelopes = component
                .package
                .into_iter()
                .flat_map(|package| &document.packages[package.0 as usize].views)
                .flat_map(|view| {
                    [
                        (
                            EnvelopeKind::PackageOutlineUnspecified,
                            view.outline.as_ref(),
                        ),
                        (
                            EnvelopeKind::AssemblyOutline,
                            view.assembly_drawing
                                .as_ref()
                                .and_then(|drawing| drawing.outline.as_ref()),
                        ),
                    ]
                    .into_iter()
                    .filter_map(move |(kind, outline)| {
                        outline.map(|outline| (view.kind, kind, outline))
                    })
                })
                .map(|(view, kind, outline)| {
                    let local = outline
                        .transform
                        .map_or(Affine2::IDENTITY, outline_transform);
                    let transform = occurrence.root_from_component.concat(local);
                    let contours = outline
                        .shape
                        .paths
                        .iter()
                        .flat_map(|path| &path.contours)
                        .map(|contour| contour.clone().transformed(transform))
                        .collect::<Vec<_>>();
                    Ok(ComponentEnvelope {
                        kind,
                        view,
                        status: outline.shape.status,
                        image: ContourSet::from_filled_contours(&contours, resolution)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ComponentEnvelopes {
                component: occurrence.id,
                designator: component.designator.clone(),
                population: occurrence.population,
                side: component.side,
                envelopes,
            })
        })
        .collect()
}

fn outline_transform(transform: assembly::Transform) -> Affine2 {
    let linear = Affine2::placement(
        Point::default(),
        transform.rotation_degrees,
        Mirror::across_y(transform.mirror),
        transform.scale,
    );
    // IPC offsets are in the transformed local frame, as in ipc_placement.
    Affine2::translation(
        linear.transform_vector(Point::new(transform.x_offset, transform.y_offset)),
    )
    .concat(linear)
}
