//! Derived physical occurrences and source-backed relationships.
//!
//! This is a view over [`ImportedDesign`], not another design representation:
//! geometry remains owned once by the canonical IPC document and final images
//! are composed on demand for the requested layout scope.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};
use ipc2581::types::{
    LayerFunction, PackagePinElectricalType, PackagePinMountType, PackagePinType,
};
use ipc2581::{Symbol, types::Side as IpcSide};

use crate::dialects::Side;
use crate::dialects::artwork;
use crate::dialects::ipc::{
    ArtworkLowering, ArtworkObjectKind, ArtworkScope, Feature, FeatureDomain, FeatureKind,
    FeatureSpan, HoleShape, PlatingKind, lower_layer_to_artwork_with,
};
use crate::geom::{ContourSet, FillRule, Point, Polarity, Span, tol};
use crate::import::ipc2581::{
    ComponentOccurrence, ComponentOccurrenceId, FeatureOccurrenceId, ImportedDesign, LayerId,
    LayoutOccurrenceId, PopulationState, feature_occurrence_id, is_copper, layer_role,
};

/// A relationship whose uncertainty is part of the result rather than hidden
/// behind a guessed nearest object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Association<T> {
    Resolved(T),
    Unresolved,
    Ambiguous(Vec<T>),
    Conflicting(Vec<T>),
}

impl<T> Association<T> {
    pub fn resolved(&self) -> Option<&T> {
        match self {
            Self::Resolved(value) => Some(value),
            Self::Unresolved | Self::Ambiguous(_) | Self::Conflicting(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LandId(pub FeatureOccurrenceId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PhysicalTerminationId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PasteIslandId {
    pub source: FeatureOccurrenceId,
    pub island: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaskOpeningId {
    pub source: FeatureOccurrenceId,
    pub island: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HoleId(pub FeatureOccurrenceId);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationBasis {
    SourceIdentity,
    ExactGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalHoleKind {
    Round,
    Square,
    Slot,
}

#[derive(Debug, Clone)]
pub struct HoleProtectionEvidence {
    pub source: FeatureOccurrenceId,
    pub layer: LayerId,
    pub function: LayerFunction,
    pub side: Side,
    pub span: FeatureSpan<Symbol>,
    pub spec_refs: Vec<Symbol>,
}

#[derive(Debug, Clone)]
pub struct PhysicalLand {
    pub id: LandId,
    pub layer: LayerId,
    pub side: Side,
    pub at: Point,
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub component: Association<ComponentOccurrenceId>,
    pub component_refs: Vec<Symbol>,
    pub pin: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub primitive: Option<Symbol>,
    pub net: Option<Symbol>,
}

/// One explicitly identified electrical package contact on the board.
///
/// Copper-layer replicas share a termination only when their IPC component,
/// pin, padstack, and location identities are exactly equal.
#[derive(Debug, Clone)]
pub struct PhysicalTermination {
    pub id: PhysicalTerminationId,
    pub component: ComponentOccurrenceId,
    pub pin: Symbol,
    pub pin_type: PackagePinType,
    pub mount_type: Option<PackagePinMountType>,
    pub padstack: Symbol,
    pub at: Point,
    pub side: Side,
    pub population: PopulationState,
    pub lands: Vec<LandId>,
}

#[derive(Debug, Clone)]
pub struct PasteIsland {
    pub id: PasteIslandId,
    pub layer: LayerId,
    pub side: Side,
    pub at: Point,
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub component: Association<ComponentOccurrenceId>,
    pub pin: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub population: PopulationState,
    /// Present only when IPC gives the same component, pin, padstack, and
    /// location identity as an electrical termination.
    pub termination: Option<PhysicalTerminationId>,
}

#[derive(Debug, Clone)]
pub struct MaskOpening {
    pub id: MaskOpeningId,
    pub layer: LayerId,
    pub side: Side,
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub lands: Association<LandId>,
}

#[derive(Debug, Clone)]
pub struct PhysicalHole {
    pub id: HoleId,
    pub layer: LayerId,
    pub source_name: Option<Symbol>,
    pub kind: PhysicalHoleKind,
    pub at: Point,
    pub finished_diameter: Option<f64>,
    pub assembly_side: Side,
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub plating: PlatingKind,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    pub span: FeatureSpan<Symbol>,
    pub spec_refs: Vec<Symbol>,
    pub termination: Association<PhysicalTerminationId>,
    pub termination_basis: Option<AssociationBasis>,
    pub protection: Vec<HoleProtectionEvidence>,
    /// One explicit result per copper layer the opening geometrically reaches.
    pub lands: Vec<LayerLandAssociation>,
}

#[derive(Debug, Clone)]
pub struct LayerLandAssociation {
    pub layer: LayerId,
    pub land: Association<LandId>,
}

/// The physical facts used by manufacturing and DFM consumers in one common
/// coordinate frame for a selected layout scope.
#[derive(Debug, Clone, Default)]
pub struct PhysicalView {
    pub lands: Vec<PhysicalLand>,
    pub terminations: Vec<PhysicalTermination>,
    pub paste_islands: Vec<PasteIsland>,
    pub mask_openings: Vec<MaskOpening>,
    pub holes: Vec<PhysicalHole>,
}

#[derive(Debug, Clone, Default)]
struct FeatureEvidence {
    component_refs: Vec<Symbol>,
    pin: Option<Symbol>,
    geometry_ref: Option<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PhysicalTerminationKey {
    component: ComponentOccurrenceId,
    pin: Symbol,
    padstack: Symbol,
    x: u64,
    y: u64,
}

impl PhysicalTerminationKey {
    fn new(component: ComponentOccurrenceId, pin: Symbol, padstack: Symbol, at: Point) -> Self {
        Self {
            component,
            pin,
            padstack,
            x: exact_coordinate(at.x),
            y: exact_coordinate(at.y),
        }
    }
}

fn exact_coordinate(value: f64) -> u64 {
    if value == 0.0 { 0 } else { value.to_bits() }
}

impl ImportedDesign {
    /// Derive physical copper lands without materializing unrelated physical
    /// layers.
    pub fn physical_lands(&self, scope: ArtworkScope) -> Result<Vec<PhysicalLand>> {
        let components = self.component_occurrences(scope)?;
        self.derive_physical_lands(scope, &components)
    }

    /// Derive electrical package contacts using only exact IPC identities.
    pub fn physical_terminations(&self, scope: ArtworkScope) -> Result<Vec<PhysicalTermination>> {
        let lands = self.physical_lands(scope)?;
        Ok(self.derive_physical_terminations(&lands))
    }

    /// Derive drilled openings and their copper-land relationships without
    /// materializing assembly, paste, or mask layers.
    pub fn physical_holes(&self, scope: ArtworkScope) -> Result<Vec<PhysicalHole>> {
        let lands = self.physical_lands(scope)?;
        self.derive_physical_holes(scope, &lands)
    }

    /// Derive physical lands, exact electrical terminations, independent paste
    /// islands, mask openings, and drilled openings without reinterpreting
    /// source XML or duplicating geometry.
    pub fn physical_view(&self, scope: ArtworkScope) -> Result<PhysicalView> {
        let components = self.component_occurrences(scope)?;
        let lands = self.derive_physical_lands(scope, &components)?;
        let terminations = self.derive_physical_terminations(&lands);
        let paste_islands = self.paste_islands(scope, &components, &terminations)?;
        let mask_openings = self.mask_openings(scope, &lands)?;
        let mut holes = self.derive_physical_holes(scope, &lands)?;
        self.attach_hole_assembly_evidence(scope, &lands, &terminations, &mut holes)?;
        Ok(PhysicalView {
            lands,
            terminations,
            paste_islands,
            mask_openings,
            holes,
        })
    }

    fn derive_physical_lands(
        &self,
        scope: ArtworkScope,
        components: &[ComponentOccurrence],
    ) -> Result<Vec<PhysicalLand>> {
        let mut lands = Vec::new();
        for (layer_index, layer) in self.layer_definitions.iter().enumerate() {
            if !is_copper(layer.layer_function) {
                continue;
            }
            let layer_id = LayerId(layer_index as u32);
            let side = physical_side(layer.side);
            for (occurrence, image) in self.attributed_land_images(layer_id, scope)? {
                let source = occurrence.id;
                let feature = self
                    .feature_definition(source.feature)
                    .context("physical land references a missing feature definition")?;
                if feature.kind != FeatureKind::Padstack
                    || feature.polarity != Polarity::Dark
                    || feature.intent.domain != FeatureDomain::Copper
                {
                    continue;
                }
                let evidence = self.feature_evidence(source);
                let at = occurrence.root_from_local.transform_point(feature.center);
                lands.push(PhysicalLand {
                    id: LandId(source),
                    layer: layer_id,
                    side,
                    at,
                    image,
                    board: occurrence.board,
                    component: self.component_association(source, &evidence, components),
                    component_refs: evidence.component_refs.clone(),
                    pin: evidence.pin,
                    padstack: feature.padstack_ref,
                    primitive: feature.primitive_ref.map(|primitive| primitive.id()),
                    net: feature.net,
                });
            }
        }
        lands.sort_by_key(|land| (land.layer, land.id));
        Ok(lands)
    }

    fn derive_physical_terminations(&self, lands: &[PhysicalLand]) -> Vec<PhysicalTermination> {
        let mut terminations = Vec::<PhysicalTermination>::new();
        let mut by_identity = HashMap::<PhysicalTerminationKey, usize>::new();
        for land in lands {
            let Some(component) = land.component.resolved().copied() else {
                continue;
            };
            let (Some(pin), Some(padstack)) = (land.pin, land.padstack) else {
                continue;
            };
            let Some(package_pin) = self.package_pin(component, pin) else {
                continue;
            };
            if package_pin.electrical_type != Some(PackagePinElectricalType::Electrical) {
                continue;
            }

            let key = PhysicalTerminationKey::new(component, pin, padstack, land.at);
            if let Some(index) = by_identity.get(&key).copied() {
                terminations[index].lands.push(land.id);
                continue;
            }

            let index = terminations.len();
            by_identity.insert(key, index);
            let definition = self
                .component_definition(component.component)
                .expect("physical land component references its imported definition");
            let side = self
                .layer_definitions
                .iter()
                .find(|layer| layer.name == definition.source.layer_ref)
                .and_then(|layer| layer.side)
                .map(|side| physical_side(Some(side)))
                .unwrap_or(Side::None);
            terminations.push(PhysicalTermination {
                id: PhysicalTerminationId(0),
                component,
                pin,
                pin_type: package_pin.pin_type,
                mount_type: package_pin.mount_type,
                padstack,
                at: land.at,
                side,
                population: definition.population,
                lands: vec![land.id],
            });
        }

        terminations.sort_by(|left, right| {
            left.component
                .cmp(&right.component)
                .then_with(|| self.resolve(left.pin).cmp(self.resolve(right.pin)))
                .then_with(|| {
                    self.resolve(left.padstack)
                        .cmp(self.resolve(right.padstack))
                })
                .then_with(|| left.at.x.total_cmp(&right.at.x))
                .then_with(|| left.at.y.total_cmp(&right.at.y))
        });
        for (index, termination) in terminations.iter_mut().enumerate() {
            termination.id = PhysicalTerminationId(index as u32);
        }
        terminations
    }

    fn package_pin(
        &self,
        component: ComponentOccurrenceId,
        pin: Symbol,
    ) -> Option<&ipc2581::types::PackagePin> {
        let package = self
            .component_definition(component.component)?
            .package
            .and_then(|package| self.package_definition(package))?;
        package
            .source
            .pins
            .iter()
            .find(|candidate| candidate.number == pin)
            .or_else(|| {
                package
                    .source
                    .topside
                    .as_ref()?
                    .pins
                    .iter()
                    .find(|candidate| candidate.number == pin)
            })
    }

    fn paste_islands(
        &self,
        scope: ArtworkScope,
        components: &[ComponentOccurrence],
        terminations: &[PhysicalTermination],
    ) -> Result<Vec<PasteIsland>> {
        let mut islands = Vec::new();
        for (layer_index, layer) in self.layer_definitions.iter().enumerate() {
            if !matches!(
                layer.layer_function,
                LayerFunction::Solderpaste | LayerFunction::Pastemask
            ) {
                continue;
            }
            let layer_id = LayerId(layer_index as u32);
            let side = physical_side(layer.side);
            for (occurrence, image) in self.attributed_feature_images(layer_id, scope)? {
                let source = occurrence.id;
                let feature = self
                    .feature_definition(source.feature)
                    .context("paste island references a missing feature definition")?;
                let evidence = self.feature_evidence(source);
                let board = occurrence.board;
                let at = occurrence.root_from_local.transform_point(feature.center);
                let component = self.component_association(source, &evidence, components);
                let population = component
                    .resolved()
                    .and_then(|component| self.component_definition(component.component))
                    .map(|component| component.population)
                    .unwrap_or_default();
                let termination = exact_termination(
                    at,
                    evidence.pin,
                    feature.padstack_ref,
                    &component,
                    terminations,
                );
                for (island, image) in image.connected_components().into_iter().enumerate() {
                    islands.push(PasteIsland {
                        id: PasteIslandId {
                            source,
                            island: island as u32,
                        },
                        layer: layer_id,
                        side,
                        at,
                        image,
                        board,
                        component: component.clone(),
                        pin: evidence.pin,
                        padstack: feature.padstack_ref,
                        population,
                        termination,
                    });
                }
            }
        }
        islands.sort_by_key(|island| (island.layer, island.id));
        Ok(islands)
    }

    fn mask_openings(
        &self,
        scope: ArtworkScope,
        lands: &[PhysicalLand],
    ) -> Result<Vec<MaskOpening>> {
        let mut lands_by_context = HashMap::<_, Vec<_>>::new();
        for land in lands {
            lands_by_context
                .entry((land.board, land.side))
                .or_default()
                .push(land);
            if land.side != Side::None {
                lands_by_context
                    .entry((land.board, Side::None))
                    .or_default()
                    .push(land);
            }
        }
        let mut openings = Vec::new();
        for (layer_index, layer) in self.layer_definitions.iter().enumerate() {
            if layer.layer_function != LayerFunction::Soldermask {
                continue;
            }
            let layer_id = LayerId(layer_index as u32);
            let side = physical_side(layer.side);
            for (occurrence, image) in self.attributed_feature_images(layer_id, scope)? {
                let source = occurrence.id;
                let evidence = self.feature_evidence(source);
                let board = occurrence.board;
                let candidates = lands_by_context
                    .get(&(board, side))
                    .map_or(&[][..], Vec::as_slice);
                for (island, image) in image.connected_components().into_iter().enumerate() {
                    openings.push(MaskOpening {
                        id: MaskOpeningId {
                            source,
                            island: island as u32,
                        },
                        layer: layer_id,
                        side,
                        image: image.clone(),
                        board,
                        lands: associate_land_candidates(
                            &image,
                            board,
                            side,
                            &evidence,
                            &Association::Unresolved,
                            candidates,
                        ),
                    });
                }
            }
        }
        openings.sort_by_key(|opening| (opening.layer, opening.id));
        Ok(openings)
    }

    fn derive_physical_holes(
        &self,
        scope: ArtworkScope,
        lands: &[PhysicalLand],
    ) -> Result<Vec<PhysicalHole>> {
        let mut lands_by_layer = BTreeMap::<_, Vec<_>>::new();
        for land in lands {
            lands_by_layer.entry(land.layer).or_default().push(land);
        }
        let mut holes = Vec::new();
        for (layer_index, layer) in self.layer_definitions.iter().enumerate() {
            if !matches!(
                layer.layer_function,
                LayerFunction::Drill | LayerFunction::Rout
            ) {
                continue;
            }
            let layer_id = LayerId(layer_index as u32);
            for occurrence in self.feature_occurrences(layer_id, scope)? {
                let feature = self
                    .feature_definition(occurrence.id.feature)
                    .context("physical opening references a missing feature definition")?;
                if !matches!(feature.kind, FeatureKind::Hole | FeatureKind::Slot) {
                    continue;
                }
                let image = self.feature_region(occurrence);
                let evidence = self.feature_evidence(occurrence.id);
                let mut layer_lands = Vec::new();
                for (&copper_layer, candidates) in &lands_by_layer {
                    if !feature_spans_layer(
                        feature.intent.span,
                        copper_layer,
                        &self.layer_definitions,
                    ) {
                        continue;
                    }
                    let land = associate_land_candidates(
                        &image,
                        occurrence.board,
                        Side::None,
                        &evidence,
                        &Association::Unresolved,
                        candidates,
                    );
                    layer_lands.push(LayerLandAssociation {
                        layer: copper_layer,
                        land,
                    });
                }
                holes.push(PhysicalHole {
                    id: HoleId(occurrence.id),
                    layer: layer_id,
                    source_name: feature.source_name,
                    kind: match feature.kind {
                        FeatureKind::Hole => match feature.hole_shape {
                            HoleShape::Round => PhysicalHoleKind::Round,
                            HoleShape::Square => PhysicalHoleKind::Square,
                        },
                        FeatureKind::Slot => PhysicalHoleKind::Slot,
                        _ => unreachable!("physical opening is a hole or slot"),
                    },
                    at: occurrence.root_from_local.transform_point(feature.center),
                    finished_diameter: (feature.kind == FeatureKind::Hole)
                        .then_some(feature.outer_diameter),
                    assembly_side: feature.intent.side,
                    image,
                    board: occurrence.board,
                    plating: feature.intent.plating,
                    padstack: feature.padstack_ref,
                    net: feature.net,
                    span: feature.intent.span,
                    spec_refs: self.feature_spec_refs(layer, feature),
                    termination: Association::Unresolved,
                    termination_basis: None,
                    protection: Vec::new(),
                    lands: layer_lands,
                });
            }
        }
        holes.sort_by_key(|hole| (hole.layer, hole.id));
        Ok(holes)
    }

    fn attach_hole_assembly_evidence(
        &self,
        scope: ArtworkScope,
        lands: &[PhysicalLand],
        terminations: &[PhysicalTermination],
        holes: &mut [PhysicalHole],
    ) -> Result<()> {
        let stackup = physical_stackup_layers(&self.stackups);
        let land_by_id = lands
            .iter()
            .map(|land| (land.id, land))
            .collect::<HashMap<_, _>>();
        for hole in holes.iter_mut() {
            let evidence = self.feature_evidence(hole.id.0);
            let (termination, basis) = self.hole_termination_association(
                hole,
                &evidence,
                terminations,
                &land_by_id,
                stackup.as_deref(),
            );
            if let Some(side) = association_side(&termination, terminations) {
                hole.assembly_side = side;
            }
            hole.termination = termination;
            hole.termination_basis = basis;
        }
        self.attach_hole_protection(scope, holes, stackup.as_deref())
    }

    fn hole_termination_association(
        &self,
        hole: &PhysicalHole,
        evidence: &FeatureEvidence,
        terminations: &[PhysicalTermination],
        land_by_id: &HashMap<LandId, &PhysicalLand>,
        stackup: Option<&[Symbol]>,
    ) -> (Association<PhysicalTerminationId>, Option<AssociationBasis>) {
        if !evidence.component_refs.is_empty() || evidence.pin.is_some() {
            let candidates = terminations
                .iter()
                .filter(|termination| termination.component.layout == hole.id.0.layout)
                .filter(|termination| {
                    evidence.pin.is_none() || evidence.pin == Some(termination.pin)
                })
                .filter(|termination| {
                    evidence.component_refs.is_empty()
                        || self
                            .component_definition(termination.component.component)
                            .and_then(|component| component.source.ref_des)
                            .is_some_and(|reference| evidence.component_refs.contains(&reference))
                })
                .map(|termination| termination.id)
                .collect::<BTreeSet<_>>();
            return (
                association_from_candidates(candidates, true),
                Some(AssociationBasis::SourceIdentity),
            );
        }

        let candidate_lands = terminations
            .iter()
            .flat_map(|termination| {
                termination.lands.iter().filter_map(|land| {
                    let land = land_by_id[land];
                    (termination.side != Side::None
                        && land.board == hole.board
                        && land.side == termination.side
                        && feature_definitely_spans_layer(
                            hole.span,
                            land.layer,
                            &self.layer_definitions,
                            stackup,
                        )
                        && land.image.bbox().intersects(hole.image.bbox())
                        && land.image.intersection(&hole.image).area() > tol::REGION_MM.powi(2))
                    .then_some((land.id, termination.id))
                })
            })
            .collect::<Vec<_>>();
        match candidate_lands.as_slice() {
            [] => (Association::Unresolved, None),
            [(_, termination)] => (
                Association::Resolved(*termination),
                Some(AssociationBasis::ExactGeometry),
            ),
            _ => (
                Association::Ambiguous(
                    candidate_lands
                        .into_iter()
                        .map(|(_, termination)| termination)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                ),
                Some(AssociationBasis::ExactGeometry),
            ),
        }
    }

    fn attach_hole_protection(
        &self,
        scope: ArtworkScope,
        holes: &mut [PhysicalHole],
        stackup: Option<&[Symbol]>,
    ) -> Result<()> {
        for (layer_index, layer) in self.layer_definitions.iter().enumerate() {
            if !matches!(
                layer.layer_function,
                LayerFunction::HoleFill
                    | LayerFunction::CoatingCond
                    | LayerFunction::CoatingNonCond
            ) {
                continue;
            }
            let layer_id = LayerId(layer_index as u32);
            for (occurrence, image) in self.attributed_feature_images(layer_id, scope)? {
                let feature = self
                    .feature_definition(occurrence.id.feature)
                    .context("hole protection references a missing feature definition")?;
                let evidence = HoleProtectionEvidence {
                    source: occurrence.id,
                    layer: layer_id,
                    function: layer.layer_function,
                    side: feature.intent.side,
                    span: feature.intent.span,
                    spec_refs: self.feature_spec_refs(layer, feature),
                };
                for hole in holes
                    .iter_mut()
                    .filter(|hole| hole.board == occurrence.board)
                {
                    if protection_side_compatible(hole.assembly_side, evidence.side)
                        && feature_spans_overlap(hole.span, evidence.span, stackup)
                        && hole.image.bbox().intersects(image.bbox())
                        && hole.image.intersection(&image).area() > tol::REGION_MM.powi(2)
                    {
                        hole.protection.push(evidence.clone());
                    }
                }
            }
        }
        for hole in holes {
            hole.protection
                .sort_by_key(|evidence| (evidence.layer, evidence.source));
        }
        Ok(())
    }

    fn feature_spec_refs(
        &self,
        layer: &ipc2581::types::Layer,
        feature: &Feature<Symbol>,
    ) -> Vec<Symbol> {
        let mut spec_refs = layer.spec_refs.clone();
        spec_refs.extend(
            feature
                .spec_refs
                .slice(&self.geometry.spec_refs)
                .iter()
                .map(|reference| reference.spec),
        );
        if let Some(set) = feature
            .set
            .and_then(|set| self.geometry.feature_sets.get(set as usize))
        {
            spec_refs.extend(
                set.spec_refs
                    .slice(&self.geometry.spec_refs)
                    .iter()
                    .map(|reference| reference.spec),
            );
        }
        spec_refs.sort_by_key(|spec| self.resolve(*spec));
        spec_refs.dedup();
        spec_refs
    }

    fn attributed_feature_images(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<Vec<(crate::import::ipc2581::FeatureOccurrence, ContourSet)>> {
        self.attributed_feature_images_where(layer, scope, |_| true)
    }

    fn attributed_land_images(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<Vec<(crate::import::ipc2581::FeatureOccurrence, ContourSet)>> {
        self.attributed_feature_images_where(layer, scope, |feature| {
            feature.kind == FeatureKind::Padstack
                && feature.polarity == Polarity::Dark
                && feature.intent.domain == FeatureDomain::Copper
        })
    }

    fn attributed_feature_images_where(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
        include: impl Fn(&Feature<Symbol>) -> bool,
    ) -> Result<Vec<(crate::import::ipc2581::FeatureOccurrence, ContourSet)>> {
        let definition = self
            .layer_definition(layer)
            .context("layer id is outside the imported design")?;
        let occurrences = self
            .feature_occurrences(layer, scope)?
            .into_iter()
            .map(|occurrence| (occurrence.id, occurrence))
            .collect::<std::collections::HashMap<_, _>>();
        let mut document = self.materialize_layer(layer, scope)?;
        crate::dialects::ipc::process::expand_feature_placement_groups(&mut document);
        crate::dialects::ipc::process::normalize_for_artwork(&mut document);
        crate::dialects::ipc::validate_artwork_ready(&document).map_err(anyhow::Error::msg)?;
        let header = artwork::Layer {
            name: document.layers[0].name.clone(),
            role: layer_role(definition.layer_function),
            side: physical_side(definition.side),
            objects: Span::EMPTY,
            bbox: document.layers[0].bbox,
            meta: definition.layer_function,
        };
        let artwork = lower_layer_to_artwork_with(&document, 0, header, &mut OccurrenceAttribution);
        let (mut images, _) = artwork::compose_selected_attributed(&artwork, |owner| {
            let owner = (*owner)?;
            self.feature_definition(owner.feature)
                .filter(|feature| include(feature))
                .map(|_| owner)
        });
        let image = images
            .pop()
            .context("attributed physical composition produced no layer")?;
        image
            .owners
            .into_iter()
            .map(|(owner, rings)| {
                Ok((
                    *occurrences
                        .get(&owner)
                        .context("composed feature has no canonical occurrence")?,
                    ContourSet::new(rings, FillRule::NonZero, tol::REGION_MM),
                ))
            })
            .collect()
    }

    fn feature_evidence(&self, occurrence: FeatureOccurrenceId) -> FeatureEvidence {
        let Some(feature) = self.feature_definition(occurrence.feature) else {
            return FeatureEvidence::default();
        };
        let set = feature
            .set
            .and_then(|set| self.geometry.feature_sets.get(set as usize));
        let mut component_refs = feature
            .pin_refs
            .slice(&self.geometry.pin_refs)
            .iter()
            .filter_map(|pin| pin.component_ref)
            .collect::<Vec<_>>();
        if let Some(component) = set.and_then(|set| set.component_ref) {
            component_refs.push(component);
        }
        component_refs.sort_by_key(|symbol| self.resolve(*symbol));
        component_refs.dedup();
        let mut pins = feature
            .pin_refs
            .slice(&self.geometry.pin_refs)
            .iter()
            .map(|pin| pin.pin)
            .collect::<Vec<_>>();
        pins.sort_by_key(|symbol| self.resolve(*symbol));
        pins.dedup();
        FeatureEvidence {
            component_refs,
            pin: (pins.len() == 1).then(|| pins[0]),
            geometry_ref: set
                .and_then(|set| set.source_geometry_ref)
                .or(feature.padstack_ref),
        }
    }

    fn component_association(
        &self,
        occurrence: FeatureOccurrenceId,
        evidence: &FeatureEvidence,
        components: &[ComponentOccurrence],
    ) -> Association<ComponentOccurrenceId> {
        if evidence.component_refs.is_empty() {
            return Association::Unresolved;
        }
        let candidates = components
            .iter()
            .filter(|component| component.id.layout == occurrence.layout)
            .filter(|component| {
                self.component_definition(component.id.component)
                    .and_then(|definition| definition.source.ref_des)
                    .is_some_and(|reference| evidence.component_refs.contains(&reference))
            })
            .map(|component| component.id)
            .collect::<Vec<_>>();
        if evidence.component_refs.len() > 1 {
            return Association::Conflicting(candidates);
        }
        match candidates.as_slice() {
            [] => Association::Unresolved,
            [component] => Association::Resolved(*component),
            _ => Association::Ambiguous(candidates),
        }
    }
}

struct OccurrenceAttribution;

impl ArtworkLowering<Symbol, Option<FeatureOccurrenceId>> for OccurrenceAttribution {
    fn object_meta(
        &mut self,
        feature: &Feature<Symbol>,
        _kind: ArtworkObjectKind,
    ) -> Option<FeatureOccurrenceId> {
        Some(
            feature_occurrence_id(feature).expect("canonical imported feature has a definition id"),
        )
    }
}

fn association_from_candidates<T: Copy + Ord>(
    candidates: BTreeSet<T>,
    source_claimed: bool,
) -> Association<T> {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    match candidates.as_slice() {
        [candidate] => Association::Resolved(*candidate),
        [_, _, ..] => Association::Ambiguous(candidates),
        [] if source_claimed => Association::Conflicting(Vec::new()),
        [] => Association::Unresolved,
    }
}

fn association_side(
    association: &Association<PhysicalTerminationId>,
    terminations: &[PhysicalTermination],
) -> Option<Side> {
    let ids = match association {
        Association::Resolved(id) => std::slice::from_ref(id),
        Association::Ambiguous(ids) | Association::Conflicting(ids) => ids,
        Association::Unresolved => return None,
    };
    let first = ids.first()?;
    let side = terminations
        .iter()
        .find(|termination| termination.id == *first)?
        .side;
    (side != Side::None
        && ids.iter().all(|id| {
            terminations
                .iter()
                .find(|termination| termination.id == *id)
                .is_some_and(|termination| termination.side == side)
        }))
    .then_some(side)
}

fn exact_termination(
    at: Point,
    pin: Option<Symbol>,
    padstack: Option<Symbol>,
    component: &Association<ComponentOccurrenceId>,
    terminations: &[PhysicalTermination],
) -> Option<PhysicalTerminationId> {
    let component = component.resolved()?;
    let (pin, padstack) = (pin?, padstack?);
    terminations
        .iter()
        .find(|termination| {
            termination.component == *component
                && termination.pin == pin
                && termination.padstack == padstack
                && termination.at == at
        })
        .map(|termination| termination.id)
}

fn associate_land_candidates(
    image: &ContourSet,
    board: Option<LayoutOccurrenceId>,
    side: Side,
    evidence: &FeatureEvidence,
    component: &Association<ComponentOccurrenceId>,
    lands: &[&PhysicalLand],
) -> Association<LandId> {
    let same_context = lands
        .iter()
        .copied()
        .filter(|land| land.board == board)
        .filter(|land| side == Side::None || land.side == side)
        .collect::<Vec<_>>();
    let has_explicit_evidence = !evidence.component_refs.is_empty()
        || evidence.pin.is_some()
        || evidence.geometry_ref.is_some();
    let explicit = same_context
        .iter()
        .copied()
        .filter(|land| {
            evidence.component_refs.is_empty()
                || land_component_refs(land)
                    .iter()
                    .any(|reference| evidence.component_refs.contains(reference))
        })
        .filter(|land| evidence.pin.is_none() || land.pin == evidence.pin)
        .filter(|land| evidence.geometry_ref.is_none() || land.padstack == evidence.geometry_ref)
        .collect::<Vec<_>>();
    let pool = if has_explicit_evidence {
        explicit.as_slice()
    } else {
        same_context.as_slice()
    };
    let overlapping = pool
        .iter()
        .copied()
        .filter(|land| land.image.bbox().intersects(image.bbox()))
        .filter(|land| land.image.intersection(image).area() > tol::REGION_MM.powi(2))
        .map(|land| land.id)
        .collect::<Vec<_>>();

    if matches!(component, Association::Conflicting(_)) {
        return Association::Conflicting(overlapping);
    }
    match overlapping.as_slice() {
        [land] => Association::Resolved(*land),
        [_, _, ..] => Association::Ambiguous(overlapping),
        [] if has_explicit_evidence && !explicit.is_empty() => {
            Association::Conflicting(explicit.iter().map(|land| land.id).collect())
        }
        [] => Association::Unresolved,
    }
}

fn land_component_refs(land: &PhysicalLand) -> Vec<Symbol> {
    land.component_refs.clone()
}

fn physical_side(side: Option<IpcSide>) -> Side {
    match side {
        Some(IpcSide::Top) => Side::Top,
        Some(IpcSide::Bottom) => Side::Bottom,
        Some(IpcSide::Internal) => Side::Inner,
        Some(IpcSide::Both | IpcSide::All | IpcSide::None) | None => Side::None,
    }
}

fn protection_side_compatible(assembly_side: Side, protection_side: Side) -> bool {
    protection_side == Side::None
        || (assembly_side != Side::None && assembly_side == protection_side)
}

fn feature_spans_layer(
    span: FeatureSpan<Symbol>,
    target: LayerId,
    layers: &[ipc2581::types::Layer],
) -> bool {
    let target_index = target.0 as usize;
    if let FeatureSpan::Layer(layer) = span {
        return layers
            .get(target_index)
            .is_some_and(|target| target.name == layer);
    }
    resolved_feature_span(span, layers).is_none_or(|(from, to)| (from..=to).contains(&target_index))
}

fn feature_definitely_spans_layer(
    span: FeatureSpan<Symbol>,
    target: LayerId,
    layers: &[ipc2581::types::Layer],
    stackup: Option<&[Symbol]>,
) -> bool {
    let Some(target) = layers.get(target.0 as usize).map(|layer| layer.name) else {
        return false;
    };
    let (from, to) = match span {
        FeatureSpan::Unknown | FeatureSpan::FromTo { from: None, .. } => return false,
        FeatureSpan::ThroughBoard => return true,
        FeatureSpan::Layer(layer) => return layer == target,
        FeatureSpan::FromTo { to: None, .. } => return false,
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => (from, to),
    };
    if target == from || target == to {
        return true;
    }
    let Some(layers) = stackup else {
        return false;
    };
    let Some((from, to)) = span_range_in_stackup([from, to], layers) else {
        return false;
    };
    layers
        .iter()
        .position(|layer| *layer == target)
        .is_some_and(|target| (from..=to).contains(&target))
}

fn feature_spans_overlap(
    left: FeatureSpan<Symbol>,
    right: FeatureSpan<Symbol>,
    stackup: Option<&[Symbol]>,
) -> bool {
    if matches!(left, FeatureSpan::ThroughBoard) {
        return matches!(right, FeatureSpan::ThroughBoard) || span_endpoints(right).is_some();
    }
    if matches!(right, FeatureSpan::ThroughBoard) {
        return span_endpoints(left).is_some();
    }
    let Some(left) = span_endpoints(left) else {
        return false;
    };
    let Some(right) = span_endpoints(right) else {
        return false;
    };
    if left.iter().any(|layer| right.contains(layer)) {
        return true;
    }
    let Some(layers) = stackup else {
        return false;
    };
    let Some(left) = span_range_in_stackup(left, layers) else {
        return false;
    };
    let Some(right) = span_range_in_stackup(right, layers) else {
        return false;
    };
    left.0 <= right.1 && right.0 <= left.1
}

fn span_endpoints(span: FeatureSpan<Symbol>) -> Option<[Symbol; 2]> {
    match span {
        FeatureSpan::Layer(layer) => Some([layer, layer]),
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => Some([from, to]),
        FeatureSpan::Unknown | FeatureSpan::ThroughBoard | FeatureSpan::FromTo { .. } => None,
    }
}

fn span_range_in_stackup(span: [Symbol; 2], stackup: &[Symbol]) -> Option<(usize, usize)> {
    let from = stackup.iter().position(|layer| *layer == span[0])?;
    let to = stackup.iter().position(|layer| *layer == span[1])?;
    Some((from.min(to), from.max(to)))
}

fn physical_stackup_layers(stackups: &[ipc2581::types::Stackup]) -> Option<Vec<Symbol>> {
    let [stackup] = stackups else {
        return None;
    };
    let mut layers = stackup.layers.iter().collect::<Vec<_>>();
    if layers.iter().all(|layer| layer.layer_number.is_some()) {
        layers.sort_by_key(|layer| layer.layer_number);
    }
    Some(layers.into_iter().map(|layer| layer.layer_ref).collect())
}

fn resolved_feature_span(
    span: FeatureSpan<Symbol>,
    layers: &[ipc2581::types::Layer],
) -> Option<(usize, usize)> {
    let (from, to) = match span {
        FeatureSpan::Unknown => return None,
        FeatureSpan::ThroughBoard => (0, layers.len().checked_sub(1)?),
        FeatureSpan::Layer(layer) => {
            let layer = layers
                .iter()
                .position(|candidate| candidate.name == layer)?;
            (layer, layer)
        }
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => (
            layers.iter().position(|layer| layer.name == from)?,
            layers.iter().position(|layer| layer.name == to)?,
        ),
        FeatureSpan::FromTo { .. } => return None,
    };
    Some((from.min(to), from.max(to)))
}

#[cfg(test)]
mod tests {
    use ipc2581::Ipc2581;

    use super::*;
    use crate::geom::path::{ContourBuf, PathCmd};
    use crate::geom::{LineCap, Paint, StrokeStyle};
    use crate::import::ipc2581::import_design;

    #[test]
    fn domain_queries_do_not_materialize_unrelated_layers() {
        let ipc = Ipc2581::parse(physical_fixture()).unwrap();
        let mut imported = import_design(&ipc).unwrap();
        make_paste_artwork_invalid(&mut imported);

        assert_eq!(
            imported.physical_lands(ArtworkScope::Board).unwrap().len(),
            7
        );
        assert_eq!(
            imported.physical_holes(ArtworkScope::Board).unwrap().len(),
            1
        );
        assert!(
            imported
                .physical_view(ArtworkScope::Board)
                .unwrap_err()
                .to_string()
                .contains("mixes Fill and Stroke paths")
        );
    }

    #[test]
    fn physical_holes_do_not_materialize_assembly_protection_layers() {
        let xml = physical_fixture().replace(
            "<Layer name=\"PASTE\" layerFunction=\"SOLDERPASTE\" side=\"TOP\" polarity=\"POSITIVE\"/>",
            "<Layer name=\"PASTE\" layerFunction=\"HOLEFILL\" side=\"TOP\" polarity=\"POSITIVE\"/>",
        );
        let ipc = Ipc2581::parse(&xml).unwrap();
        let mut imported = import_design(&ipc).unwrap();
        make_paste_artwork_invalid(&mut imported);

        let holes = imported.physical_holes(ArtworkScope::Board).unwrap();
        assert_eq!(holes.len(), 1);
        assert_eq!(holes[0].termination, Association::Unresolved);
        assert!(holes[0].protection.is_empty());
        assert!(
            imported
                .physical_view(ArtworkScope::Board)
                .unwrap_err()
                .to_string()
                .contains("mixes Fill and Stroke paths")
        );
    }

    #[test]
    fn unresolved_hole_spans_keep_land_links_without_geometry_association() {
        let xml = physical_fixture().replace(
            "<Layer name=\"DRILL\" layerFunction=\"DRILL\" side=\"ALL\" polarity=\"POSITIVE\"/>",
            "<Layer name=\"DRILL\" layerFunction=\"DRILL\" side=\"ALL\" polarity=\"POSITIVE\"><Span fromLayer=\"TOP\"/></Layer>",
        );
        Ipc2581::validate(&xml).expect("one-ended drill span conforms to IPC-2581C");
        let ipc = Ipc2581::parse(&xml).unwrap();
        let imported = import_design(&ipc).unwrap();
        let holes = imported.physical_holes(ArtworkScope::Board).unwrap();

        assert_eq!(holes.len(), 1);
        assert_eq!(holes[0].lands.len(), 2);
        assert!(
            holes[0]
                .lands
                .iter()
                .any(|association| matches!(association.land, Association::Resolved(_)))
        );
        assert_eq!(holes[0].termination, Association::Unresolved);
        assert_eq!(
            imported.physical_view(ArtworkScope::Board).unwrap().holes[0].termination,
            Association::Unresolved
        );
    }

    #[test]
    fn derives_exact_physical_terminations_separately_from_paste() {
        Ipc2581::validate(physical_fixture()).expect("fixture conforms to IPC-2581C");
        let ipc = Ipc2581::parse(physical_fixture()).unwrap();
        let imported = import_design(&ipc).unwrap();
        let physical = imported.physical_view(ArtworkScope::Board).unwrap();

        assert_eq!(physical.lands.len(), 7);
        let u1_pin = imported
            .components
            .iter()
            .find(|component| {
                component
                    .source
                    .ref_des
                    .is_some_and(|reference| imported.resolve(reference) == "U1")
            })
            .unwrap();
        let u1 = u1_pin.source.ref_des.unwrap();
        let shared_pin_lands = physical
            .lands
            .iter()
            .filter(|land| {
                land.component_refs.contains(&u1)
                    && land.pin.is_some_and(|pin| imported.resolve(pin) == "1")
            })
            .collect::<Vec<_>>();
        assert_eq!(shared_pin_lands.len(), 3);
        assert_eq!(
            shared_pin_lands
                .iter()
                .map(|land| land.id)
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "physical lands remain distinct even when they share one logical pin"
        );

        assert_eq!(physical.terminations.len(), 5);
        assert_eq!(
            physical
                .terminations
                .iter()
                .filter(|termination| {
                    imported
                        .component_definition(termination.component.component)
                        .is_some_and(|component| component.source.ref_des == Some(u1))
                })
                .count(),
            3,
            "separate lands with one logical pin remain separate terminations"
        );
        assert!(
            physical
                .terminations
                .iter()
                .all(|termination| imported.resolve(termination.pin) != "PAD0")
        );
        assert_eq!(
            physical
                .terminations
                .iter()
                .filter(|termination| termination.pin_type == PackagePinType::Surface)
                .count(),
            4
        );
        let through = physical
            .terminations
            .iter()
            .find(|termination| termination.pin_type == PackagePinType::Through)
            .unwrap();
        assert_eq!(through.lands.len(), 2);
        assert_eq!(
            through.mount_type,
            Some(PackagePinMountType::ThroughHolePin)
        );

        assert_eq!(physical.paste_islands.len(), 9);
        let linked = physical
            .paste_islands
            .iter()
            .filter(|island| island.termination.is_some())
            .collect::<Vec<_>>();
        assert_eq!(linked.len(), 2);
        let linked_surface = linked
            .iter()
            .find(|island| island.at == Point::new(5.0, 5.0))
            .unwrap();
        assert_eq!(linked_surface.population, PopulationState::DoNotPopulate);
        let termination = &physical.terminations[linked_surface.termination.unwrap().0 as usize];
        assert_eq!(termination.at, Point::new(5.0, 5.0));
        assert_eq!(imported.resolve(termination.pin), "1");
        let linked_bottom = linked
            .iter()
            .find(|island| island.side == Side::Bottom)
            .unwrap();
        assert_eq!(linked_bottom.termination, Some(through.id));
        assert!(physical.paste_islands.iter().any(|island| {
            island
                .pin
                .is_some_and(|pin| imported.resolve(pin) == "PAD0")
                && island.termination.is_none()
        }));
        assert!(physical.paste_islands.iter().any(|island| {
            island.at == Point::new(5.1, 5.0)
                && island.pin.is_some_and(|pin| imported.resolve(pin) == "1")
                && island.termination.is_none()
        }));
        assert_eq!(
            physical
                .paste_islands
                .iter()
                .filter(|island| island.termination.is_none())
                .count(),
            7
        );

        assert_eq!(physical.mask_openings.len(), 1);
        assert!(matches!(
            physical.mask_openings[0].lands,
            Association::Resolved(_)
        ));
        assert_eq!(physical.holes.len(), 1);
        assert_eq!(physical.holes[0].lands.len(), 2);
        assert!(
            physical.holes[0]
                .lands
                .iter()
                .any(|association| matches!(association.land, Association::Resolved(_)))
        );
    }

    fn make_paste_artwork_invalid(imported: &mut ImportedDesign) {
        let paste = imported.layer_id("PASTE").unwrap();
        let source_layer = imported
            .step_layers
            .iter()
            .find(|layer| layer.layer == paste)
            .unwrap()
            .document_layer as usize;
        let feature = imported.geometry.layers[source_layer].features.start as usize;
        let start = imported.geometry.arena.paths.len() as u32;
        imported.geometry.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ])],
        );
        imported.geometry.push_path(
            Paint::Stroke(StrokeStyle::new(0.1, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
            ])],
        );
        imported.geometry.features[feature].paths = Span::new(start, 2);
    }

    fn physical_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="board"/>
    <BomRef name="bom"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="line"><LineDesc lineWidth="0" lineEnd="ROUND"/></EntryLineDesc>
    </DictionaryLineDesc>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="land"><Circle diameter="1"/></EntryStandard>
      <EntryStandard id="paste"><Circle diameter="0.4"/></EntryStandard>
      <EntryStandard id="wide-paste"><Circle diameter="3"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <LogisticHeader>
    <Role id="owner" roleFunction="OWNER"/>
    <Enterprise id="owner-enterprise" code="owner" name="Owner"/>
    <Person name="Owner" enterpriseRef="owner-enterprise" roleRef="owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="test" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="test">
      <SoftwarePackage name="test" vendor="test" revision="1"><Certification certificationStatus="SELFTEST"/></SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom">
    <BomHeader assembly="board" revision="1"/>
    <BomItem OEMDesignNumberRef="part-u1" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="U1" packageRef="pkg" populate="false" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
    <BomItem OEMDesignNumberRef="part-u2" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="U2" packageRef="pkg" populate="true" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
    <BomItem OEMDesignNumberRef="part-j1" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="J1" packageRef="pkg-tht" populate="true" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Ecad name="design">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="PASTE" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM_PASTE" layerFunction="SOLDERPASTE" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="MASK" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
          <PadstackPadDef layerRef="PASTE" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="paste"/></PadstackPadDef>
          <PadstackPadDef layerRef="MASK" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="tht-padstack">
          <PadstackHoleDef name="J1-H1" diameter="0.3" platingStatus="PLATED" plusTol="0" minusTol="0" x="0" y="0"/>
          <PadstackPadDef layerRef="TOP" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
          <PadstackPadDef layerRef="BOTTOM" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></PadstackPadDef>
          <PadstackPadDef layerRef="BOTTOM_PASTE" padUse="REGULAR"><Location x="0" y="0"/><StandardPrimitiveRef id="paste"/></PadstackPadDef>
        </PadStackDef>
        <Datum x="0" y="0"/>
        <Package name="pkg" type="OTHER" pinOne="1" pinOneOrientation="OTHER">
          <Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline>
          <Pin number="1" type="SURFACE" electricalType="ELECTRICAL" mountType="SURFACE_MOUNT_PAD"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pin>
          <Pin number="PAD0" type="SURFACE" electricalType="UNDEFINED" mountType="UNDEFINED"><Location x="3" y="0"/><StandardPrimitiveRef id="land"/></Pin>
        </Package>
        <Package name="pkg-tht" type="OTHER" pinOne="1" pinOneOrientation="OTHER">
          <Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline>
          <Pin number="1" type="THRU" electricalType="ELECTRICAL" mountType="THROUGH_HOLE_PIN"><Location x="0" y="0"/><StandardPrimitiveRef id="land"/></Pin>
        </Package>
        <Component refDes="U1" packageRef="pkg" part="part-u1" layerRef="TOP" mountType="SMT">
          <Location x="5" y="5"/>
        </Component>
        <Component refDes="U2" packageRef="pkg" part="part-u2" layerRef="TOP" mountType="SMT">
          <Location x="25" y="5"/>
        </Component>
        <Component refDes="J1" packageRef="pkg-tht" part="part-j1" layerRef="TOP" mountType="THMT">
          <Location x="40" y="5"/>
        </Component>
        <LayerFeature layerRef="TOP">
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="5" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="14" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="16" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set><Pad padstackDefRef="padstack"><Location x="8" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="PAD0"/></Pad></Set>
          <Set net="N2"><Pad padstackDefRef="padstack"><Location x="25" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U2" pin="1"/></Pad></Set>
          <Set net="N3"><Pad padstackDefRef="tht-padstack"><Location x="40" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="J1" pin="1"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="BOTTOM"><Set net="N3"><Pad padstackDefRef="tht-padstack"><Location x="40" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="J1" pin="1"/></Pad></Set></LayerFeature>
        <LayerFeature layerRef="PASTE">
          <Set><Pad padstackDefRef="padstack"><Location x="5" y="5"/><StandardPrimitiveRef id="paste"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set><Pad padstackDefRef="padstack"><Location x="5.1" y="5"/><StandardPrimitiveRef id="paste"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set><Pad padstackDefRef="padstack"><Location x="8" y="5"/><StandardPrimitiveRef id="paste"/><PinRef componentRef="U1" pin="PAD0"/></Pad></Set>
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="4.7" y="5"/><Location x="5.3" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="15" y="5"/><StandardPrimitiveRef id="wide-paste"/></Features></Set>
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="25" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
          <Set geometryUsage="GRAPHIC"><Features><Location x="35" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
        </LayerFeature>
        <LayerFeature layerRef="BOTTOM_PASTE"><Set><Pad padstackDefRef="tht-padstack"><Location x="40" y="5"/><StandardPrimitiveRef id="paste"/><PinRef componentRef="J1" pin="1"/></Pad></Set></LayerFeature>
        <LayerFeature layerRef="MASK">
          <Set><Pad padstackDefRef="padstack"><Location x="5" y="5"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="DRILL">
          <Set geometry="padstack"><Hole name="H1" diameter="0.3" platingStatus="PLATED" plusTol="0" minusTol="0" x="5" y="5"/></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}
