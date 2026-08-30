//! Derived physical occurrences and source-backed relationships.
//!
//! This is a view over [`ImportedDesign`], not another design representation:
//! geometry remains owned once by the canonical IPC document and final images
//! are composed on demand for the requested layout scope.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use ipc2581::types::LayerFunction;
use ipc2581::{Symbol, types::Side as IpcSide};

use crate::dialects::Side;
use crate::dialects::artwork;
use crate::dialects::ipc::{
    ArtworkLowering, ArtworkObjectKind, ArtworkScope, Feature, FeatureDomain, FeatureKind,
    PlatingKind, lower_layer_to_artwork_with,
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

#[derive(Debug, Clone)]
pub struct PasteIsland {
    pub id: PasteIslandId,
    pub layer: LayerId,
    pub side: Side,
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub component: Association<ComponentOccurrenceId>,
    pub population: PopulationState,
    pub land: Association<LandId>,
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
    pub image: ContourSet,
    pub board: Option<LayoutOccurrenceId>,
    pub plating: PlatingKind,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
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

impl ImportedDesign {
    /// Derive physical copper lands without materializing unrelated physical
    /// layers.
    pub fn physical_lands(&self, scope: ArtworkScope) -> Result<Vec<PhysicalLand>> {
        let components = self.component_occurrences(scope)?;
        self.derive_physical_lands(scope, &components)
    }

    /// Derive drilled openings and their copper-land relationships without
    /// materializing paste or mask layers.
    pub fn physical_holes(&self, scope: ArtworkScope) -> Result<Vec<PhysicalHole>> {
        let lands = self.physical_lands(scope)?;
        self.derive_physical_holes(scope, &lands)
    }

    /// Derive physical lands, final paste islands, mask openings, and drilled
    /// openings without reinterpreting source XML or duplicating geometry.
    pub fn physical_view(&self, scope: ArtworkScope) -> Result<PhysicalView> {
        let components = self.component_occurrences(scope)?;
        let lands = self.derive_physical_lands(scope, &components)?;
        let paste_islands = self.paste_islands(scope, &components, &lands)?;
        let mask_openings = self.mask_openings(scope, &lands)?;
        let holes = self.derive_physical_holes(scope, &lands)?;
        Ok(PhysicalView {
            lands,
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
            for (occurrence, image) in self.attributed_feature_images(layer_id, scope)? {
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

    fn paste_islands(
        &self,
        scope: ArtworkScope,
        components: &[ComponentOccurrence],
        lands: &[PhysicalLand],
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
                let evidence = self.feature_evidence(source);
                let board = occurrence.board;
                let component = self.component_association(source, &evidence, components);
                let population = component
                    .resolved()
                    .and_then(|component| self.component_definition(component.component))
                    .map(|component| component.population)
                    .unwrap_or_default();
                for (island, image) in image.connected_components().into_iter().enumerate() {
                    let land = associate_land(&image, board, side, &evidence, &component, lands);
                    islands.push(PasteIsland {
                        id: PasteIslandId {
                            source,
                            island: island as u32,
                        },
                        layer: layer_id,
                        side,
                        image,
                        board,
                        component: component.clone(),
                        population,
                        land,
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
                        lands: associate_land(
                            &image,
                            board,
                            side,
                            &evidence,
                            &Association::Unresolved,
                            lands,
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
                for copper_layer in lands.iter().map(|land| land.layer).collect::<BTreeSet<_>>() {
                    if !feature_spans_layer(
                        feature.intent.span,
                        copper_layer,
                        &self.layer_definitions,
                    ) {
                        continue;
                    }
                    let candidates = lands
                        .iter()
                        .filter(|land| land.layer == copper_layer)
                        .collect::<Vec<_>>();
                    let land = associate_land_candidates(
                        &image,
                        occurrence.board,
                        Side::None,
                        &evidence,
                        &Association::Unresolved,
                        &candidates,
                    );
                    layer_lands.push(LayerLandAssociation {
                        layer: copper_layer,
                        land,
                    });
                }
                holes.push(PhysicalHole {
                    id: HoleId(occurrence.id),
                    layer: layer_id,
                    image,
                    board: occurrence.board,
                    plating: feature.intent.plating,
                    padstack: feature.padstack_ref,
                    net: feature.net,
                    lands: layer_lands,
                });
            }
        }
        holes.sort_by_key(|hole| (hole.layer, hole.id));
        Ok(holes)
    }

    fn attributed_feature_images(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
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
        let (mut images, _) = artwork::compose_attributed(&artwork, |owner| *owner);
        let image = images
            .pop()
            .context("attributed physical composition produced no layer")?;
        image
            .owners
            .into_iter()
            .filter_map(|(owner, rings)| owner.map(|owner| (owner, rings)))
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

fn associate_land(
    image: &ContourSet,
    board: Option<LayoutOccurrenceId>,
    side: Side,
    evidence: &FeatureEvidence,
    component: &Association<ComponentOccurrenceId>,
    lands: &[PhysicalLand],
) -> Association<LandId> {
    let candidates = lands.iter().collect::<Vec<_>>();
    associate_land_candidates(image, board, side, evidence, component, &candidates)
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

fn feature_spans_layer(
    span: crate::dialects::ipc::FeatureSpan<Symbol>,
    target: LayerId,
    layers: &[ipc2581::types::Layer],
) -> bool {
    let target_index = target.0 as usize;
    match span {
        crate::dialects::ipc::FeatureSpan::Unknown
        | crate::dialects::ipc::FeatureSpan::ThroughBoard => true,
        crate::dialects::ipc::FeatureSpan::Layer(layer) => layers
            .get(target_index)
            .is_some_and(|target| target.name == layer),
        crate::dialects::ipc::FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => {
            let Some(from) = layers.iter().position(|layer| layer.name == from) else {
                return true;
            };
            let Some(to) = layers.iter().position(|layer| layer.name == to) else {
                return true;
            };
            (from.min(to)..=from.max(to)).contains(&target_index)
        }
        crate::dialects::ipc::FeatureSpan::FromTo { .. } => true,
    }
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
            4
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
    fn derives_component_owned_paste_and_explicit_land_relationships() {
        let ipc = Ipc2581::parse(physical_fixture()).unwrap();
        let imported = import_design(&ipc).unwrap();
        let physical = imported.physical_view(ArtworkScope::Board).unwrap();

        assert_eq!(physical.lands.len(), 4);
        let u1_pin = imported
            .components
            .iter()
            .find_map(|component| {
                (component
                    .source
                    .ref_des
                    .is_some_and(|reference| imported.resolve(reference) == "U1"))
                .then_some(component)
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

        assert_eq!(physical.paste_islands.len(), 5);
        let resolved = physical
            .paste_islands
            .iter()
            .filter(|island| matches!(island.land, Association::Resolved(_)))
            .collect::<Vec<_>>();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].land, resolved[1].land);
        assert!(
            resolved
                .iter()
                .all(|island| island.population == PopulationState::DoNotPopulate)
        );
        assert!(physical.paste_islands.iter().any(
            |island| matches!(island.land, Association::Ambiguous(ref lands) if lands.len() == 2)
        ));
        assert!(physical.paste_islands.iter().any(
            |island| matches!(island.land, Association::Conflicting(ref lands) if lands.len() == 3)
        ));
        assert!(
            physical
                .paste_islands
                .iter()
                .any(|island| island.land == Association::Unresolved)
        );

        assert_eq!(physical.mask_openings.len(), 1);
        assert!(matches!(
            physical.mask_openings[0].lands,
            Association::Resolved(_)
        ));
        assert_eq!(physical.holes.len(), 1);
        assert_eq!(physical.holes[0].lands.len(), 1);
        assert!(matches!(
            physical.holes[0].lands[0].land,
            Association::Resolved(_)
        ));
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
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="land"><Circle diameter="1"/></EntryStandard>
      <EntryStandard id="paste"><Circle diameter="0.4"/></EntryStandard>
      <EntryStandard id="wide-paste"><Circle diameter="3"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Bom name="bom">
    <BomHeader assembly="board" revision="1"/>
    <BomItem OEMDesignNumberRef="part-u1" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="U1" packageRef="pkg" populate="false" layerRef="TOP"/>
    </BomItem>
    <BomItem OEMDesignNumberRef="part-u2" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="U2" packageRef="pkg" populate="true" layerRef="TOP"/>
    </BomItem>
  </Bom>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="PASTE" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Layer name="MASK" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Component refDes="U1" packageRef="pkg" part="part-u1" layerRef="TOP" mountType="SMT">
          <Location x="5" y="5"/>
        </Component>
        <Component refDes="U2" packageRef="pkg" part="part-u2" layerRef="TOP" mountType="SMT">
          <Location x="25" y="5"/>
        </Component>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR"><StandardPrimitiveRef id="land"/></PadstackPadDef>
          <PadstackPadDef layerRef="MASK" padUse="REGULAR"><StandardPrimitiveRef id="land"/></PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="5" y="5"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="14" y="5"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set net="N1"><Pad padstackDefRef="padstack"><Location x="16" y="5"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
          <Set net="N2"><Pad padstackDefRef="padstack"><Location x="25" y="5"/><PinRef componentRef="U2" pin="1"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="PASTE">
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="4.7" y="5"/><Location x="5.3" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="15" y="5"/><StandardPrimitiveRef id="wide-paste"/></Features></Set>
          <Set componentRef="U1" geometryUsage="GRAPHIC"><Features><Location x="25" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
          <Set geometryUsage="GRAPHIC"><Features><Location x="35" y="5"/><StandardPrimitiveRef id="paste"/></Features></Set>
        </LayerFeature>
        <LayerFeature layerRef="MASK">
          <Set><Pad padstackDefRef="padstack"><Location x="5" y="5"/><PinRef componentRef="U1" pin="1"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="DRILL">
          <Set geometry="padstack"><Hole name="H1" diameter="0.3" platingStatus="PLATED" x="5" y="5"/></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}
