//! The imported design: step-local definitions joined to the layout graph,
//! and their materialization per artwork scope.

use super::*;

/// One self-contained, source-faithful IPC design.
///
/// Geometry is stored once in step-local coordinates. Layout and feature
/// occurrences are derived by joining those definitions to the layout graph;
/// final layer images are lowerings, not independent design state. The layer
/// features that make up nearly all of a source file are therefore kept only
/// as that lowered geometry, never as a second copy of the parsed Steps.
#[derive(Debug, Clone)]
pub struct ImportedDesign {
    pub(super) strings: Interner,
    pub revision: String,
    pub content: ipc2581::types::Content,
    pub specs: HashMap<Symbol, ipc2581::types::Spec>,
    pub logistic_header: Option<ipc2581::types::LogisticHeader>,
    pub history_record: Option<ipc2581::types::HistoryRecord>,
    pub boms: Vec<ipc2581::types::Bom>,
    pub avl: Option<ipc2581::types::Avl>,
    pub geometry: GeometryDocument,
    pub layer_definitions: Vec<Layer>,
    pub stackups: Vec<ipc2581::types::Stackup>,
    pub steps: Vec<StepDefinition>,
    pub step_layers: Vec<StepLayer>,
    pub packages: Vec<PackageDefinition>,
    pub components: Vec<ComponentDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u32);

/// What a source Step declares about itself, in source order. Its bulk —
/// features, padstacks, packages, components and nets — is lowered into
/// `geometry`, `packages` and `components` instead of being kept here.
#[derive(Debug, Clone)]
pub struct StepDefinition {
    pub name: Symbol,
    pub step_type: Option<StepType>,
    pub datum: Option<Datum>,
    pub step_repeats: Vec<StepRepeat>,
}

impl StepDefinition {
    pub fn is_panel(&self) -> bool {
        matches!(self.step_type, Some(StepType::Pallet))
            || (self.step_type.is_none() && !self.step_repeats.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepLayer {
    pub step: u32,
    pub layer: LayerId,
    pub document_layer: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureDefinitionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureOccurrenceId {
    pub feature: FeatureDefinitionId,
    pub layout: LayoutOccurrenceId,
    pub placement: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct FeatureOccurrence {
    pub id: FeatureOccurrenceId,
    pub root_from_local: Affine2,
    pub board: Option<LayoutOccurrenceId>,
    pub root_from_board: Affine2,
    pub board_from_local: Option<Affine2>,
}

pub fn feature_occurrence_id(feature: &GeometryFeature) -> Option<FeatureOccurrenceId> {
    Some(FeatureOccurrenceId {
        feature: FeatureDefinitionId(feature.source.definition?),
        layout: feature
            .source_instance
            .map(LayoutOccurrenceId::Instance)
            .unwrap_or(LayoutOccurrenceId::Root),
        placement: feature.source_placement,
    })
}

#[derive(Debug, Clone)]
pub struct ComponentDefinition {
    pub step: u32,
    pub source_index: u32,
    pub source: ipc2581::types::Component,
    pub package: Option<PackageDefinitionId>,
    pub bom_references: Vec<BomReferenceId>,
    pub local_from_component: Affine2,
    pub population: PopulationState,
}

#[derive(Debug, Clone)]
pub struct PackageDefinition {
    pub step: u32,
    pub source_index: u32,
    pub source: ipc2581::types::Package,
}

#[derive(Debug, Clone, Copy)]
pub struct ComponentOccurrence {
    pub id: ComponentOccurrenceId,
    pub root_from_component: Affine2,
    pub board: Option<LayoutOccurrenceId>,
    pub root_from_board: Affine2,
    pub board_from_component: Option<Affine2>,
    pub population: PopulationState,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::import) struct StepOccurrence {
    pub(super) step: u32,
    pub(in crate::import) layout: LayoutOccurrenceId,
    pub(super) root_from_step: Affine2,
    pub(in crate::import) board: Option<LayoutOccurrenceId>,
    pub(super) root_from_board: Affine2,
}

impl StepOccurrence {
    /// A step alone at the root of its own frame.
    fn root(step: u32, kind: LayoutStepKind) -> Self {
        Self {
            step,
            layout: LayoutOccurrenceId::Root,
            root_from_step: Affine2::IDENTITY,
            board: (kind == LayoutStepKind::Board).then_some(LayoutOccurrenceId::Root),
            root_from_board: Affine2::IDENTITY,
        }
    }

    /// The board frame's view of something placed in the root frame, when
    /// this occurrence is on a board.
    fn board_from(&self, root_from_local: Affine2) -> Option<Affine2> {
        self.board
            .and_then(|_| self.root_from_board.inverse())
            .map(|board_from_root| board_from_root.concat(root_from_local))
    }
}

/// Import the complete source design once, retaining step-local geometry.
pub fn import_design(ipc: &Ipc2581, resolution: Resolution) -> Result<ImportedDesign> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut geometry = extract_layout(ipc)?;

    // The layout traversal only needs reachable steps, while the canonical
    // document retains every source step definition.
    let step_ids = ecad
        .cad_data
        .steps
        .iter()
        .map(|step| ensure_layout_step_for_step(&mut geometry, step))
        .collect::<Vec<_>>();

    let mut context = ExtractContext::for_layers(ipc, resolution, &mut geometry);
    let mut step_layers = Vec::new();
    for (step, &step_id) in ecad.cad_data.steps.iter().zip(&step_ids) {
        context.enter_step(step);
        for (layer_index, source_layer) in ecad.cad_data.layers.iter().enumerate() {
            let feature_start = geometry.features.len() as u32;
            let Some(document_layer) = append_step_layer(
                &context,
                &mut geometry,
                step,
                &ecad.cad_data.layers,
                source_layer,
                ipc.resolve(source_layer.name),
            )?
            else {
                continue;
            };
            for definition in feature_start..geometry.features.len() as u32 {
                let feature = &mut geometry.features[definition as usize];
                feature.source.definition = Some(definition);
                if let Some(group) = feature.placement_group {
                    feature.source_placement = Some(
                        geometry.feature_placement_groups[group as usize]
                            .placements
                            .start,
                    );
                }
            }
            step_layers.push(StepLayer {
                step: step_id,
                layer: LayerId(layer_index as u32),
                document_layer,
            });
        }
    }
    crate::dialects::ipc::process::normalize_bounds(&mut geometry);

    let mut packages = Vec::new();
    let mut package_ids = HashMap::<Symbol, Vec<PackageDefinitionId>>::new();
    for (step, &step_id) in ecad.cad_data.steps.iter().zip(&step_ids) {
        for (source_index, package) in step.packages.iter().enumerate() {
            let id = PackageDefinitionId(packages.len() as u32);
            package_ids.entry(package.name).or_default().push(id);
            packages.push(PackageDefinition {
                step: step_id,
                source_index: source_index as u32,
                source: package.clone(),
            });
        }
    }

    let mut components = Vec::new();
    for (step, &step_id) in ecad.cad_data.steps.iter().zip(&step_ids) {
        for (source_index, component) in step.components.iter().enumerate() {
            let placement = ipc_placement(
                Point::new(component.location.x, component.location.y),
                component.xform,
            );
            // A packageRef names its own Step's Package; only a name the Step
            // lacks resolves across Steps.
            let package = component.package_ref.and_then(|reference| {
                let candidates = package_ids.get(&reference)?;
                candidates
                    .iter()
                    .find(|id| packages[id.0 as usize].step == step_id)
                    .or(candidates.first())
                    .copied()
            });
            let bom_references = component
                .ref_des
                .map(|reference| component_bom_references(ipc, step.name, reference))
                .unwrap_or_default();
            let population = population_state(ipc.boms(), &bom_references);
            components.push(ComponentDefinition {
                step: step_id,
                source_index: source_index as u32,
                source: component.clone(),
                package,
                bom_references,
                local_from_component: placement.transform,
                population,
            });
        }
    }

    Ok(ImportedDesign {
        strings: ipc.interner().clone(),
        revision: ipc.revision().to_owned(),
        content: ipc.content().clone(),
        specs: ecad.cad_header.specs.clone(),
        logistic_header: ipc.logistic_header().cloned(),
        history_record: ipc.history_record().cloned(),
        boms: ipc.boms().to_vec(),
        avl: ipc.avl().cloned(),
        geometry,
        layer_definitions: ecad.cad_data.layers.clone(),
        stackups: ecad.cad_data.stackups.clone(),
        steps: ecad
            .cad_data
            .steps
            .iter()
            .map(|step| StepDefinition {
                name: step.name,
                step_type: step.step_type,
                datum: step.datum,
                step_repeats: step.step_repeats.clone(),
            })
            .collect(),
        step_layers,
        packages,
        components,
    })
}

pub(super) fn component_bom_references(
    ipc: &Ipc2581,
    source_step: Symbol,
    component: Symbol,
) -> Vec<BomReferenceId> {
    let mut matches = Vec::new();
    for (bom_index, bom) in ipc.boms().iter().enumerate() {
        if bom.header.as_ref().is_some_and(|header| {
            !header.step_refs.is_empty() && !header.step_refs.contains(&source_step)
        }) {
            continue;
        }
        for (item_index, item) in bom.items.iter().enumerate() {
            for (designator_index, designator) in item.designators.iter().enumerate() {
                let ipc2581::types::BomDesignator::Reference(reference) = designator else {
                    continue;
                };
                if reference.name == component {
                    matches.push(BomReferenceId {
                        bom: bom_index as u32,
                        item: item_index as u32,
                        designator: designator_index as u32,
                    });
                }
            }
        }
    }
    matches
}

pub(super) fn population_state(
    boms: &[ipc2581::types::Bom],
    references: &[BomReferenceId],
) -> PopulationState {
    references
        .iter()
        .filter_map(|id| bom_reference(boms, *id)?.populate)
        .map(|populate| {
            if populate {
                PopulationState::Populate
            } else {
                PopulationState::DoNotPopulate
            }
        })
        .fold(
            PopulationState::Unspecified,
            |state, incoming| match state {
                PopulationState::Unspecified => incoming,
                PopulationState::Conflicting => PopulationState::Conflicting,
                state if state == incoming => state,
                PopulationState::Populate | PopulationState::DoNotPopulate => {
                    PopulationState::Conflicting
                }
            },
        )
}

pub(super) fn bom_reference(
    boms: &[ipc2581::types::Bom],
    reference: BomReferenceId,
) -> Option<&ipc2581::types::BomRefDes> {
    let designator = boms
        .get(reference.bom as usize)?
        .items
        .get(reference.item as usize)?
        .designators
        .get(reference.designator as usize)?;
    match designator {
        ipc2581::types::BomDesignator::Reference(reference) => Some(reference),
        _ => None,
    }
}

impl ImportedDesign {
    pub fn resolve(&self, symbol: Symbol) -> &str {
        self.strings.resolve(symbol)
    }

    pub fn bom(&self) -> Option<&ipc2581::types::Bom> {
        self.content
            .bom_refs
            .iter()
            .find_map(|reference| self.boms.iter().find(|bom| bom.name == *reference))
            .or_else(|| self.boms.first())
    }

    pub fn resolve_enterprise(&self, enterprise_ref: Symbol) -> Option<&str> {
        let enterprise = self
            .logistic_header
            .as_ref()?
            .enterprises
            .iter()
            .find(|enterprise| enterprise.id == enterprise_ref)?;
        match enterprise.name.map(|name| self.resolve(name))? {
            "Manufacturer" | "NONE" | "N/A" | "" => None,
            name => Some(name),
        }
    }

    pub fn layer_definition(&self, layer: LayerId) -> Option<&Layer> {
        self.layer_definitions.get(layer.0 as usize)
    }

    pub fn feature_definition(&self, feature: FeatureDefinitionId) -> Option<&GeometryFeature> {
        self.geometry.features.get(feature.0 as usize)
    }

    pub fn component_definition(
        &self,
        component: ComponentDefinitionId,
    ) -> Option<&ComponentDefinition> {
        self.components.get(component.0 as usize)
    }

    pub fn package_definition(&self, package: PackageDefinitionId) -> Option<&PackageDefinition> {
        self.packages.get(package.0 as usize)
    }

    pub fn bom_item(&self, reference: BomReferenceId) -> Option<&ipc2581::types::BomItem> {
        self.boms
            .get(reference.bom as usize)?
            .items
            .get(reference.item as usize)
    }

    pub fn layer_id(&self, name: &str) -> Option<LayerId> {
        self.layer_definitions
            .iter()
            .position(|layer| self.resolve(layer.name) == name)
            .map(|index| LayerId(index as u32))
    }

    fn step_layer(&self, step: u32, layer: LayerId) -> Option<&StepLayer> {
        self.step_layers
            .iter()
            .find(|step_layer| step_layer.layer == layer && step_layer.step == step)
    }

    pub fn step_id(&self, source_step_ref: Symbol) -> Option<u32> {
        self.geometry
            .layout
            .steps
            .iter()
            .position(|step| step.source_step_ref == source_step_ref)
            .map(|index| index as u32)
    }

    /// Materialize one canonical step-local layer definition without layout
    /// occurrences. This is the source-faithful input for hierarchical
    /// manufacturing lowerings.
    pub fn materialize_step_layer(&self, step: u32, layer: LayerId) -> Result<GeometryDocument> {
        let definition = self
            .geometry
            .layout
            .steps
            .get(step as usize)
            .context("step id is outside the imported design")?;
        let alone = StepOccurrence::root(step, definition.kind);
        self.materialize(layer, &[alone], Some(step), &|_| true)
    }

    pub fn feature_occurrences(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<Vec<FeatureOccurrence>> {
        let step_occurrences = self.step_occurrences(scope)?;
        let mut occurrences = Vec::new();
        for step_occurrence in &step_occurrences {
            let Some(step_layer) = self.step_layer(step_occurrence.step, layer) else {
                continue;
            };
            let source_layer = &self.geometry.layers[step_layer.document_layer as usize];
            for feature_index in source_layer.features.indices() {
                let feature = &self.geometry.features[feature_index as usize];
                match feature.placement_group {
                    Some(group) => {
                        let group = self.geometry.feature_placement_groups[group as usize];
                        for placement in group.placements.indices() {
                            let root_from_local = step_occurrence
                                .root_from_step
                                .concat(self.geometry.feature_placements[placement as usize]);
                            occurrences.push(self.feature_occurrence(
                                feature_index,
                                Some(placement),
                                *step_occurrence,
                                root_from_local,
                            ));
                        }
                    }
                    None => occurrences.push(self.feature_occurrence(
                        feature_index,
                        None,
                        *step_occurrence,
                        step_occurrence.root_from_step,
                    )),
                }
            }
        }
        Ok(occurrences)
    }

    fn feature_occurrence(
        &self,
        feature: u32,
        placement: Option<u32>,
        step: StepOccurrence,
        root_from_local: Affine2,
    ) -> FeatureOccurrence {
        FeatureOccurrence {
            id: FeatureOccurrenceId {
                feature: FeatureDefinitionId(feature),
                layout: step.layout,
                placement,
            },
            root_from_local,
            board: step.board,
            root_from_board: step.root_from_board,
            board_from_local: step.board_from(root_from_local),
        }
    }

    /// Prepare this occurrence from the retained source curves. Any earlier
    /// approximation in those curves remains part of the total budget.
    pub fn feature_region(
        &self,
        occurrence: FeatureOccurrence,
        resolution: Resolution,
    ) -> Result<ContourSet> {
        let feature = self
            .geometry
            .features
            .get(occurrence.id.feature.0 as usize)
            .context("feature occurrence is outside the imported design")?;
        Ok(ContourSet::from_placed_painted_paths(
            &self.geometry.arena,
            feature
                .paths
                .slice(&self.geometry.arena.paths)
                .iter()
                .map(|path| (path, occurrence.root_from_local)),
            resolution,
        )?)
    }

    pub fn component_occurrences(&self, scope: ArtworkScope) -> Result<Vec<ComponentOccurrence>> {
        let steps = self.step_occurrences(scope)?;
        let mut occurrences = Vec::new();
        for (component_index, component) in self.components.iter().enumerate() {
            for step in steps
                .iter()
                .filter(|occurrence| occurrence.step == component.step)
            {
                let root_from_component =
                    step.root_from_step.concat(component.local_from_component);
                occurrences.push(ComponentOccurrence {
                    id: ComponentOccurrenceId {
                        component: ComponentDefinitionId(component_index as u32),
                        layout: step.layout,
                    },
                    root_from_component,
                    board: step.board,
                    root_from_board: step.root_from_board,
                    board_from_component: step.board_from(root_from_component),
                    population: component.population,
                });
            }
        }
        Ok(occurrences)
    }

    pub fn materialize_layer(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<GeometryDocument> {
        let occurrences = self.step_occurrences(scope)?;
        // A single-step view carries only that step's layout.
        let alone = matches!(scope, ArtworkScope::Board | ArtworkScope::ArrayLocal)
            .then(|| occurrences[0].step);
        self.materialize(layer, &occurrences, alone, &|_| true)
    }

    /// The Step occurrences `scope` materializes, each with the layout Step it
    /// places. An occurrence precedes everything it places.
    pub fn layout_occurrences(
        &self,
        scope: ArtworkScope,
    ) -> Result<Vec<(u32, LayoutOccurrenceId)>> {
        Ok(self
            .step_occurrences(scope)?
            .into_iter()
            .map(|occurrence| (occurrence.step, occurrence.layout))
            .collect())
    }

    /// One layer of the part of `scope` that its Step occurrence `root`
    /// places, itself included, in that Step's own frame. Features keep the
    /// occurrence identity they have in `scope`, so they still join its
    /// physical view; the layout is the root Step's alone. An occurrence
    /// that `held` declines leaves its features out and every other feature
    /// as it would be among them.
    pub fn materialize_occurrence_layer(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
        root: LayoutOccurrenceId,
        held: &dyn Fn(LayoutOccurrenceId) -> bool,
    ) -> Result<GeometryDocument> {
        let placed = self.placed_occurrences(scope, root)?;
        self.materialize(layer, &placed, Some(placed[0].step), held)
    }

    /// Bounds of one layer in every Step occurrence that `root` places,
    /// itself included, in the frame of `root`: they enclose what
    /// [`Self::materialize_occurrence_layer`] holds of each.
    pub fn occurrence_layer_bounds(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
        root: LayoutOccurrenceId,
    ) -> Result<Vec<(LayoutOccurrenceId, BBox)>> {
        Ok(self
            .placed_occurrences(scope, root)?
            .into_iter()
            .filter_map(|occurrence| {
                let step_layer = self.step_layer(occurrence.step, layer)?;
                let bounds = self.geometry.layers[step_layer.document_layer as usize].bbox;
                Some((
                    occurrence.layout,
                    bounds.transformed(occurrence.root_from_step),
                ))
            })
            .collect())
    }

    /// The occurrences of `scope` that `root` places, itself first, placed
    /// in its own frame.
    fn placed_occurrences(
        &self,
        scope: ArtworkScope,
        root: LayoutOccurrenceId,
    ) -> Result<Vec<StepOccurrence>> {
        let occurrences = self.step_occurrences(scope)?;
        let start = occurrences
            .iter()
            .position(|occurrence| occurrence.layout == root)
            .context("layout occurrence is outside the materialized scope")?;
        let instances = &self.geometry.layout.instances;
        let placed_by_root = |occurrence: &&StepOccurrence| {
            std::iter::successors(Some(occurrence.layout), |layout| {
                let instance = instances.get(layout.source_instance()? as usize)?;
                Some(
                    instance
                        .parent_instance
                        .map_or(LayoutOccurrenceId::Root, LayoutOccurrenceId::Instance),
                )
            })
            .any(|layout| layout == root)
        };
        let frame_from_root = occurrences[start]
            .root_from_step
            .inverse()
            .context("layout occurrence has a singular placement")?;
        // Traversal order keeps an occurrence's placements right behind it.
        Ok(occurrences[start..]
            .iter()
            .take_while(placed_by_root)
            .map(|occurrence| StepOccurrence {
                // The root is where its own frame says it is, exactly.
                root_from_step: if occurrence.layout == root {
                    Affine2::IDENTITY
                } else {
                    frame_from_root.concat(occurrence.root_from_step)
                },
                root_from_board: frame_from_root.concat(occurrence.root_from_board),
                ..*occurrence
            })
            .collect())
    }

    /// One layer's definitions copied into every given step occurrence that
    /// `held` names, with the layout of `alone` when the view is that single
    /// step and the whole layout graph otherwise.
    fn materialize(
        &self,
        layer: LayerId,
        occurrences: &[StepOccurrence],
        alone: Option<u32>,
        held: &dyn Fn(LayoutOccurrenceId) -> bool,
    ) -> Result<GeometryDocument> {
        let definition = self
            .layer_definition(layer)
            .context("layer id is outside the imported design")?;
        let mut target = GeometryDocument::new();
        match alone {
            Some(step) => self.copy_step_layout_sidecar(&mut target, step),
            None => self.copy_layout_sidecar(&mut target),
        }
        target.diagnostics = self.geometry.diagnostics.clone();
        target.specs = self.geometry.specs.clone();
        target.spec_items = self.geometry.spec_items.clone();

        let mut bbox = BBox::empty();
        let mut source_set_offset = 0;
        for occurrence in occurrences {
            let Some(step_layer) = self.step_layer(occurrence.step, layer) else {
                continue;
            };
            let source_layer = step_layer.document_layer as usize;
            if held(occurrence.layout) {
                bbox = bbox.union(append_transformed_layer(
                    &mut target,
                    &self.geometry,
                    source_layer,
                    occurrence.root_from_step,
                    source_set_offset,
                    occurrence.layout.source_instance(),
                )?);
            }
            // Sets are numbered across every occurrence, held or not.
            source_set_offset = source_set_offset
                .checked_add(source_layer_set_span(&self.geometry, source_layer)?)
                .context("layout contains too many source feature sets")?;
        }
        let spec_refs = push_spec_refs(&mut target, &definition.spec_refs);
        target.layers.push(GeometryLayer {
            name: self.resolve(definition.name).to_owned(),
            source_layer_ref: definition.name,
            layer_function: definition.layer_function,
            spec_refs,
            sets: Span::new(0, target.feature_sets.len() as u32),
            features: Span::new(0, target.features.len() as u32),
            bbox,
        });
        crate::dialects::ipc::process::normalize_bounds(&mut target);
        Ok(target)
    }

    fn copy_layout_sidecar(&self, target: &mut GeometryDocument) {
        let arena = &self.geometry.arena;
        target.profiles = self.geometry.profiles.clone();
        target.profile_cutouts = self.geometry.profile_cutouts.clone();
        for profile in &mut target.profiles {
            profile.outer_path =
                target
                    .arena
                    .append_path_from(arena, profile.outer_path, Affine2::IDENTITY);
        }
        for cutout in &mut target.profile_cutouts {
            cutout.path = target
                .arena
                .append_path_from(arena, cutout.path, Affine2::IDENTITY);
        }
        target.layout = self.geometry.layout.clone();
    }

    fn copy_step_layout_sidecar(&self, target: &mut GeometryDocument, step: u32) {
        let source_step = &self.geometry.layout.steps[step as usize];
        for profile_index in source_step.profiles.indices() {
            let source_profile = &self.geometry.profiles[profile_index as usize];
            let outer_path = target.arena.paths.len() as u32;
            target.arena.append_path_from(
                &self.geometry.arena,
                source_profile.outer_path,
                Affine2::IDENTITY,
            );
            let cutout_start = target.profile_cutouts.len() as u32;
            for source_cutout in source_profile.cutouts.slice(&self.geometry.profile_cutouts) {
                let path = target.arena.paths.len() as u32;
                target.arena.append_path_from(
                    &self.geometry.arena,
                    source_cutout.path,
                    Affine2::IDENTITY,
                );
                target.profile_cutouts.push(StepProfileCutout {
                    path,
                    bbox: source_cutout.bbox,
                });
            }
            target.profiles.push(StepProfile {
                outer_path,
                cutouts: Span::new(
                    cutout_start,
                    target.profile_cutouts.len() as u32 - cutout_start,
                ),
                bbox: source_profile.bbox,
            });
        }
        let mut step = source_step.clone();
        step.profiles = Span::new(0, target.profiles.len() as u32);
        target.layout.steps.push(step);
        target.layout.root_step = Some(0);
    }

    /// The final painted image of one layer, prepared at `resolution`.
    pub fn composed_layer_image(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
        resolution: Resolution,
    ) -> Result<ContourSet> {
        let definition = self
            .layer_definition(layer)
            .context("layer id is outside the imported design")?;
        self.materialize_layer(layer, scope)?.into_layer_image(
            0,
            layer_role(definition.layer_function),
            side_for_layer(definition.side),
            resolution,
        )
    }

    pub(in crate::import) fn step_occurrences(
        &self,
        scope: ArtworkScope,
    ) -> Result<Vec<StepOccurrence>> {
        let root_step = self
            .geometry
            .layout
            .root_step
            .context("IPC-2581 primary step has no canonical layout root")?;
        let root_definition = self
            .geometry
            .layout
            .steps
            .get(root_step as usize)
            .context("canonical layout root references a missing step")?;
        let root = StepOccurrence::root(root_step, root_definition.kind);
        if scope == ArtworkScope::ArrayLocal {
            return Ok(vec![root]);
        }

        let mut occurrences = vec![root];
        self.append_step_occurrences(root, scope == ArtworkScope::ArraySupport, &mut occurrences)?;
        if scope != ArtworkScope::Board {
            return Ok(occurrences);
        }

        let board = occurrences
            .into_iter()
            .find(|occurrence| {
                self.geometry.layout.steps[occurrence.step as usize].kind == LayoutStepKind::Board
            })
            .with_context(|| {
                format!(
                    "IPC-2581 primary step '{}' does not reference a board step",
                    self.resolve(root_definition.source_step_ref)
                )
            })?;
        Ok(vec![StepOccurrence::root(
            board.step,
            LayoutStepKind::Board,
        )])
    }

    fn append_step_occurrences(
        &self,
        parent: StepOccurrence,
        support_only: bool,
        occurrences: &mut Vec<StepOccurrence>,
    ) -> Result<()> {
        for (repeat_index, repeat) in
            self.geometry
                .layout
                .repeats
                .iter()
                .enumerate()
                .filter(|(_, repeat)| {
                    repeat.parent_step == parent.step
                        && repeat.parent_instance == parent.layout.source_instance()
                })
        {
            for instance_index in repeat.instances.indices() {
                let instance = self
                    .geometry
                    .layout
                    .instances
                    .get(instance_index as usize)
                    .context("layout repeat references a missing instance")?;
                if instance.repeat != repeat_index as u32
                    || instance.parent_instance != parent.layout.source_instance()
                    || instance.child_step != repeat.child_step
                {
                    bail!("layout repeat and instance relationships are inconsistent");
                }
                let step = self
                    .geometry
                    .layout
                    .steps
                    .get(instance.child_step as usize)
                    .context("layout instance references a missing step")?;
                if support_only && step.kind == LayoutStepKind::Board {
                    continue;
                }
                let layout = LayoutOccurrenceId::Instance(instance_index);
                let (board, root_from_board) = if step.kind == LayoutStepKind::Board {
                    (Some(layout), instance.transform)
                } else {
                    (parent.board, parent.root_from_board)
                };
                let occurrence = StepOccurrence {
                    step: instance.child_step,
                    layout,
                    root_from_step: instance.transform,
                    board,
                    root_from_board,
                };
                occurrences.push(occurrence);
                self.append_step_occurrences(occurrence, support_only, occurrences)?;
            }
        }
        Ok(())
    }
}

pub fn extract_layer(
    ipc: &Ipc2581,
    layer_name: &str,
    resolution: Resolution,
) -> Result<GeometryDocument> {
    extract_layer_for_view(ipc, layer_name, ArtworkScope::ArrayFlattened, resolution)
}

pub fn extract_layer_for_view(
    ipc: &Ipc2581,
    layer_name: &str,
    view: ArtworkScope,
    resolution: Resolution,
) -> Result<GeometryDocument> {
    let design = import_design(ipc, resolution)?;
    let layer = design
        .layer_id(layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    design.materialize_layer(layer, view)
}
