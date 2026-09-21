//! The checkable design: entity pools extracted from one IPC-2581 file for
//! one layout target, in plain millimeters.
//!
//! A layout is a few Step definitions placed many times, so it is checked as
//! one [`Design`] per Step: the Step with everything it places, in the Step's
//! own frame. A measurement belongs to the lowest Step holding all of its
//! subjects. Each Step therefore measures its own content once, however often
//! the layout repeats it, and measures what it places only against its own
//! content and across placements. A V-score line is the exception that one
//! subject makes of many placements: every Step under the one drawing it
//! meets the line in its own frame. A lone board is the layout of one Step.
//!
//! Exactly the pools the configured rules read are extracted; the rest stay
//! empty. Pools are flat vectors; copper follows physical stackup order
//! when available, otherwise declaration order. Derived facts that
//! relate pools (a hole's lands, a copper layer's boundary index) are side
//! tables indexed like their primary pool. Extraction fails closed without
//! failing whole: a drilled feature whose plating, diameter, or outline the
//! file does not state is never a quietly dropped subject — it blocks the
//! rules that would have measured it, and every other rule still runs.

use pcb_ir::geom::Resolution;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    ArtworkScope, ArtworkTarget, Feature, FeatureDomain, FeatureKind, FeatureSpan, LayoutPurpose,
    LayoutStepKind, PlatingKind, ProfileSet, SimpleShape, lower_layer_to_artwork_with,
    profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side, artwork};
use pcb_ir::geom::dfm::{BBoxIndex, Distance, WidthDisk, min_width_disk};
use pcb_ir::geom::path::ContourBuf;
use pcb_ir::geom::{Affine2, BBox, ContourSet, Point, Polarity, PreparedRegion, Span};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use crate::geometry::GeometryDocument;
use crate::layers;
#[cfg(test)]
use pcb_ir::import::ipc2581::import_design;
use pcb_ir::import::ipc2581::{
    FeatureOccurrenceId, ImportedDesign, LayerId, LayoutOccurrenceId, feature_occurrence_id,
};
use pcb_ir::import::physical::{Association, LandId, PhysicalHole};

use super::report::{
    DrillSpan, Frame, LayerRef, LayoutContext, LayoutOccurrence, Placement, SourceLocator,
};
use super::rules::{self, Pools, Rule};

pub(super) struct Design<'a> {
    pub imported: &'a ImportedDesign,
    pub scope: ArtworkScope,
    /// The layout Step this design is the frame of.
    pub step: u32,
    /// Every occurrence of that Step in the scope. The first stands for all
    /// of them: the pools hold what it places, named as the scope names it.
    pub placements: Vec<LayoutOccurrenceId>,
    /// The resolution every pool was prepared at; checks derive their own
    /// constructions from it.
    pub resolution: Resolution,
    pub stackup: Option<PhysicalStackup>,
    pub holes: Vec<Hole>,
    pub slots: Vec<Slot>,
    pub copper_layers: Vec<CopperLayer>,
    /// One boundary index per copper layer, for clearance and enclosure
    /// queries against the Step's composed copper.
    pub copper_boundaries: Vec<PreparedRegion>,
    /// One boundary index per attributed conductor on each copper layer.
    pub conductor_boundaries: Vec<Vec<PreparedRegion>>,
    /// Each copper layer's conductors by their bounds, so a drilled feature
    /// meets the few conductors near it rather than every one on the layer.
    pub conductors_near: Vec<BBoxIndex>,
    /// Each hole's lands, one per copper layer it owns a land on, indexed
    /// like `holes`.
    pub hole_lands: Vec<Vec<HoleLand>>,
    pub slot_lands: Vec<Vec<HoleLand>>,
    pub mask_layers: Vec<MaskLayer>,
    /// The V-score lines the Step draws.
    pub scores: Vec<Score>,
    /// The lines Steps above it draw, where they reach the Step's copper, in
    /// the Step's frame, each with the placements it does so at, by index.
    /// A line crosses every board along it, and each measures it once.
    pub inherited_scores: Vec<(Score, Vec<u32>)>,
    pub board_outlines: Vec<BoardOutline>,
    pub board_arrays: Vec<BoardArray>,
    /// What extraction could not build. Every pool above is usable for a
    /// rule that no blocker names.
    pub blockers: Vec<Blocker>,
}

/// Why a pool could not be built, and the pools it leaves unusable. A rule
/// reading any of them is reported as not evaluated; every other rule runs.
#[derive(Debug)]
pub(super) struct Blocker {
    pub pools: Pools,
    pub reason: String,
}

/// Build a pool only when a rule reads it. A pool that cannot be built stays
/// empty and blocks exactly the rules that read it, never the whole run. A
/// pool derived from blocked `inputs` is not attempted: every rule reading it
/// reads those inputs too, so its failure would only restate theirs.
fn pool<T: Default>(
    wanted: Pools,
    pools: Pools,
    inputs: Pools,
    blockers: &mut Vec<Blocker>,
    build: impl FnOnce() -> Result<T>,
) -> T {
    let inputs_blocked = blockers
        .iter()
        .any(|blocker| blocker.pools.intersects(inputs));
    if !wanted.intersects(pools) || inputs_blocked {
        return T::default();
    }
    build().unwrap_or_else(|error| {
        blockers.push(Blocker {
            pools,
            reason: format!("{error:#}"),
        });
        T::default()
    })
}

/// Where every pool of one design comes from: the first placement of its
/// Step, which the scope names like any other occurrence.
#[derive(Clone, Copy)]
struct Source<'a> {
    imported: &'a ImportedDesign,
    scope: ArtworkScope,
    root: LayoutOccurrenceId,
    resolution: Resolution,
}

impl Source<'_> {
    /// One layer of the Step and everything it places, in the Step's frame.
    fn layer(&self, layer_index: usize) -> Result<GeometryDocument> {
        self.imported.materialize_occurrence_layer(
            LayerId(layer_index as u32),
            self.scope,
            self.root,
        )
    }

    /// The occurrence holding a feature, as the frame names it: `None` for
    /// the Step's own content, the scope's instance for what it places.
    fn placed(&self, feature: &Feature) -> Option<u32> {
        let own = match self.root {
            LayoutOccurrenceId::Root => None,
            LayoutOccurrenceId::Instance(instance) => Some(instance),
        };
        feature
            .source_instance
            .filter(|_| feature.source_instance != own)
    }

    /// The placement directly under the Step that holds `placed`.
    fn branch(&self, placed: Option<u32>) -> Option<u32> {
        let instances = &self.imported.geometry.layout.instances;
        std::iter::successors(placed, |&at| instances[at as usize].parent_instance)
            .take_while(|&at| LayoutOccurrenceId::Instance(at) != self.root)
            .last()
    }
}

/// Whether a measurement between two subjects is this design's to make. One
/// inside a single placement is made once, in that placement's own frame.
pub(super) fn spans(first_branch: Option<u32>, second_branch: Option<u32>) -> bool {
    first_branch.is_none() || first_branch != second_branch
}

impl<'a> Design<'a> {
    /// One design per Step the scope places, the layout root first.
    pub fn frames(
        imported: &'a ImportedDesign,
        scope: ArtworkScope,
        rules: &[Rule],
        resolution: Resolution,
    ) -> Result<Vec<Self>> {
        let wanted = rules::pools(rules, !imported.stackups.is_empty());
        let mut steps = Vec::<(u32, Vec<LayoutOccurrenceId>)>::new();
        for (step, occurrence) in imported.layout_occurrences(scope)? {
            match steps.iter_mut().find(|(placed, _)| *placed == step) {
                Some((_, placements)) => placements.push(occurrence),
                None => steps.push((step, vec![occurrence])),
            }
        }
        let mut designs = steps
            .into_iter()
            .map(|(step, placements)| {
                let source = Source {
                    imported,
                    scope,
                    root: placements[0],
                    resolution,
                };
                Self::extract(source, step, placements, wanted)
            })
            .collect::<Vec<_>>();
        // Only copper within a rule's limit of a line is ever measured to it.
        let reach_mm = rules
            .iter()
            .filter(|rule| {
                rule.kind == rules::RuleKind::LineworkToCopperClearance(rules::Linework::VScore)
            })
            .map(|rule| rule.limit.length().millimeters())
            .fold(0.0, f64::max);
        let inherited = designs
            .iter()
            .map(|design| design.scores_from_above(&designs, reach_mm))
            .collect::<Vec<_>>();
        for (design, inherited) in designs.iter_mut().zip(inherited) {
            design.inherited_scores = inherited;
        }
        Ok(designs)
    }

    /// The scope's placement of an occurrence, and the occurrence placing it.
    pub fn placed(&self, occurrence: LayoutOccurrenceId) -> (Affine2, Option<LayoutOccurrenceId>) {
        match occurrence {
            LayoutOccurrenceId::Root => (Affine2::IDENTITY, None),
            LayoutOccurrenceId::Instance(instance) => {
                let instance = &self.imported.geometry.layout.instances[instance as usize];
                let above = instance
                    .parent_instance
                    .map_or(LayoutOccurrenceId::Root, LayoutOccurrenceId::Instance);
                (instance.transform, Some(above))
            }
        }
    }

    /// The V-score lines that the Steps placing this one draw within
    /// `reach_mm` of its copper. Lines of different placements that coincide
    /// within the resolution's tolerance are one line at all of them.
    fn scores_from_above(&self, designs: &[Self], reach_mm: f64) -> Vec<(Score, Vec<u32>)> {
        let copper = self
            .copper_layers
            .iter()
            .map(|layer| layer.image.bbox)
            .fold(BBox::empty(), BBox::union);
        if copper.is_empty() {
            return Vec::new();
        }
        let window = ContourSet::rectangle(copper.expand(reach_mm), self.resolution);
        let layout = &self.imported.geometry.layout;
        let mut inherited = Vec::<(Score, Vec<u32>)>::new();
        for (index, &placement) in self.placements.iter().enumerate() {
            let (scope_from_frame, above) = self.placed(placement);
            let Some(frame_from_scope) = scope_from_frame.inverse() else {
                continue;
            };
            for occurrence in std::iter::successors(above, |&above| self.placed(above).1) {
                let step = match occurrence {
                    LayoutOccurrenceId::Root => layout.root_step,
                    LayoutOccurrenceId::Instance(instance) => {
                        Some(layout.instances[instance as usize].child_step)
                    }
                };
                let frame_from_step = frame_from_scope.concat(self.placed(occurrence).0);
                for score in designs
                    .iter()
                    .filter(|design| Some(design.step) == step)
                    .flat_map(|design| &design.scores)
                {
                    for (start, end) in window.segment_spans(
                        frame_from_step.transform_point(score.start),
                        frame_from_step.transform_point(score.end),
                    ) {
                        let same = |known: &Score| {
                            let meets = |first: Point, second: Point| {
                                first.distance_to(second) <= self.resolution.tolerance_mm
                            };
                            known.layer.name == score.layer.name
                                && ((meets(known.start, start) && meets(known.end, end))
                                    || (meets(known.start, end) && meets(known.end, start)))
                        };
                        match inherited.iter_mut().find(|(known, _)| same(known)) {
                            Some((_, placements)) if placements.last() == Some(&(index as u32)) => {
                            }
                            Some((_, placements)) => placements.push(index as u32),
                            None => inherited.push((
                                Score {
                                    start,
                                    end,
                                    ..score.clone()
                                },
                                vec![index as u32],
                            )),
                        }
                    }
                }
            }
        }
        inherited
    }

    fn extract(
        source: Source<'a>,
        step: u32,
        placements: Vec<LayoutOccurrenceId>,
        wanted: Pools,
    ) -> Self {
        let Source {
            imported,
            scope,
            resolution,
            ..
        } = source;
        let mut blockers = Vec::new();
        let stackup = pool(wanted, Pools::STACKUP, Pools::NONE, &mut blockers, || {
            collect_physical_stackup(imported).map(Some)
        });
        let (holes, slots, unusable) = pool(
            wanted,
            Pools::HOLES | Pools::SLOTS,
            Pools::NONE,
            &mut blockers,
            || collect_drilled(source, stackup.as_ref()),
        );
        blockers.extend(unusable);
        let copper_layers = pool(wanted, Pools::COPPER, Pools::NONE, &mut blockers, || {
            collect_copper_layers(source, stackup.as_ref())
        });
        if wanted.intersects(Pools::CONDUCTOR_OWNERSHIP) {
            blockers.extend(unattributed_copper(imported, &copper_layers));
        }
        // The physical view of the Step's own drilled features, which join
        // it by the occurrence identity the scope gives them.
        let lands = Pools::HOLE_LANDS | Pools::SLOT_LANDS;
        let physical = wanted.intersects(lands).then(|| {
            Ok::<_, anyhow::Error>(
                imported
                    .physical_holes_of(scope, source.root, resolution)?
                    .into_iter()
                    .map(|hole| (hole.id.0, hole))
                    .collect::<HashMap<_, _>>(),
            )
        });
        let land_indices = pool(wanted, lands, Pools::COPPER, &mut blockers, || {
            if let Some(Err(error)) = &physical {
                bail!("{error:#}");
            }
            Ok(copper_layers
                .iter()
                .enumerate()
                .flat_map(|(copper_index, layer)| {
                    layer
                        .lands
                        .iter()
                        .enumerate()
                        .map(move |(land_index, land)| {
                            (
                                land.id,
                                HoleLand {
                                    copper_index: copper_index as u32,
                                    land_index: land_index as u32,
                                },
                            )
                        })
                })
                .collect::<HashMap<_, _>>())
        });
        // Without the physical view the land pools are already blocked.
        let link = |drilled: Vec<Option<FeatureOccurrenceId>>| match &physical {
            Some(Ok(physical_holes)) => link_lands(drilled, &land_indices, physical_holes),
            _ => Ok(Vec::new()),
        };
        let (conductor_boundaries, conductors_near) = pool(
            wanted,
            Pools::CONDUCTOR_BOUNDARIES,
            Pools::COPPER,
            &mut blockers,
            || {
                #[cfg(not(target_family = "wasm"))]
                let layers = copper_layers.par_iter();
                #[cfg(target_family = "wasm")]
                let layers = copper_layers.iter();
                Ok(layers
                    .map(|layer| {
                        let conductors = layer.conductors.iter();
                        (
                            conductors
                                .clone()
                                .map(|conductor| conductor.image.prepare_query())
                                .collect::<Vec<_>>(),
                            BBoxIndex::new(
                                conductors.map(|conductor| conductor.image.bbox).collect(),
                            ),
                        )
                    })
                    .unzip())
            },
        );
        Self {
            imported,
            scope,
            step,
            placements,
            resolution,
            conductor_boundaries,
            conductors_near,
            copper_boundaries: pool(
                wanted,
                Pools::COPPER_BOUNDARIES,
                Pools::COPPER,
                &mut blockers,
                || {
                    #[cfg(not(target_family = "wasm"))]
                    let layers = copper_layers.par_iter();
                    #[cfg(target_family = "wasm")]
                    let layers = copper_layers.iter();
                    Ok(layers.map(|layer| layer.image.prepare_query()).collect())
                },
            ),
            hole_lands: pool(
                wanted,
                Pools::HOLE_LANDS,
                Pools::COPPER | Pools::HOLES,
                &mut blockers,
                || {
                    link(
                        holes
                            .iter()
                            .map(|hole| hole.branch.is_none().then_some(hole.id))
                            .collect(),
                    )
                },
            ),
            slot_lands: pool(
                wanted,
                Pools::SLOT_LANDS,
                Pools::COPPER | Pools::SLOTS,
                &mut blockers,
                || {
                    link(
                        slots
                            .iter()
                            .map(|slot| slot.branch.is_none().then_some(slot.id))
                            .collect(),
                    )
                },
            ),
            mask_layers: pool(wanted, Pools::MASKS, Pools::NONE, &mut blockers, || {
                collect_mask_layers(source)
            }),
            scores: pool(wanted, Pools::SCORES, Pools::NONE, &mut blockers, || {
                collect_scores(source)
            }),
            inherited_scores: Vec::new(),
            board_outlines: pool(
                wanted,
                Pools::BOARD_OUTLINES,
                Pools::NONE,
                &mut blockers,
                || collect_board_outlines(source, step),
            ),
            board_arrays: pool(
                wanted,
                Pools::BOARD_ARRAYS,
                Pools::NONE,
                &mut blockers,
                || collect_board_arrays(source),
            ),
            stackup,
            holes,
            slots,
            copper_layers,
            blockers,
        }
    }

    /// The design of a fixture's board.
    #[cfg(test)]
    pub fn board(imported: &'a ImportedDesign, rules: &[Rule], resolution: Resolution) -> Self {
        Self::frames(imported, ArtworkScope::Board, rules, resolution)
            .unwrap()
            .remove(0)
    }

    pub fn resolve(&self, symbol: Option<Symbol>) -> Option<String> {
        symbol.map(|symbol| self.imported.resolve(symbol).to_owned())
    }

    /// The Step at some of its placements, by index, as a report frame.
    pub fn report_frame(&self, placements: &[u32]) -> Frame {
        let layout = &self.imported.geometry.layout;
        Frame {
            step: self
                .imported
                .resolve(layout.steps[self.step as usize].source_step_ref)
                .to_owned(),
            placements: placements
                .iter()
                .map(|&index| match self.placements[index as usize] {
                    LayoutOccurrenceId::Root => Placement {
                        instance: None,
                        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    },
                    LayoutOccurrenceId::Instance(instance) => {
                        let t = layout.instances[instance as usize].transform;
                        Placement {
                            instance: Some(instance),
                            transform: [t.m00, t.m10, t.m01, t.m11, t.m02, t.m12],
                        }
                    }
                })
                .collect(),
        }
    }

    pub fn report_layout(&self) -> LayoutContext {
        let layout = &self.imported.geometry;
        let graph = &layout.layout;
        let board = self.scope == ArtworkScope::Board;
        let selected = if board {
            graph
                .steps
                .iter()
                .find(|step| step.kind == LayoutStepKind::Board)
        } else {
            graph
                .root_step
                .and_then(|index| graph.steps.get(index as usize))
        };
        let bounds = profile_occurrences_for(
            layout,
            if board {
                ProfileSet::BoardOutlines
            } else {
                ProfileSet::RootOnly
            },
        )
        .into_iter()
        .map(|occurrence| occurrence.profile.bbox.transformed(occurrence.transform))
        .fold(BBox::empty(), BBox::union);
        LayoutContext {
            kind: selected.map_or("unknown", |step| match (step.kind, step.purpose) {
                (_, LayoutPurpose::FabricationPanel) => "fab_panel",
                (LayoutStepKind::Board, _) => "board",
                (LayoutStepKind::Panel, _) => "board_array",
                (kind, _) => step_kind(kind),
            }),
            selected_step: selected
                .map(|step| self.imported.resolve(step.source_step_ref).to_owned()),
            coordinate_frame: if board {
                "selected_board"
            } else {
                "root_layout"
            },
            bounding_box: (!bounds.is_empty()).then(|| bounds.into()),
            instances: if board {
                Vec::new()
            } else {
                graph
                    .instances
                    .iter()
                    .enumerate()
                    .map(|(index, instance)| {
                        let step = &graph.steps[instance.child_step as usize];
                        let t = instance.transform;
                        LayoutOccurrence {
                            index: index as u32,
                            parent_index: instance.parent_instance,
                            step: self.imported.resolve(instance.source_step_ref).to_owned(),
                            kind: step_kind(step.kind),
                            purpose: match step.purpose {
                                LayoutPurpose::Product => "product",
                                LayoutPurpose::FabricationPanel => "fabrication_panel",
                            },
                            transform: [t.m00, t.m10, t.m01, t.m11, t.m02, t.m12],
                            bounding_box: (!instance.bbox.is_empty()).then(|| instance.bbox.into()),
                            repeat_index_x: instance.repeat_index_x,
                            repeat_index_y: instance.repeat_index_y,
                        }
                    })
                    .collect()
            },
        }
    }
}

fn step_kind(kind: LayoutStepKind) -> &'static str {
    match kind {
        LayoutStepKind::Board => "board",
        LayoutStepKind::Panel => "panel",
        LayoutStepKind::Coupon => "coupon",
        LayoutStepKind::Tooling => "tooling",
        LayoutStepKind::Ic => "ic",
        LayoutStepKind::Unknown => "unknown",
    }
}

#[derive(Debug)]
pub(super) struct PhysicalStackup {
    pub name: String,
    pub copper_layers: Vec<LayerRef>,
    overall_thickness_mm: Option<f64>,
    layers: Vec<PhysicalStackupLayer>,
}

#[derive(Debug)]
struct PhysicalStackupLayer {
    layer_ref: Symbol,
    name: String,
    thickness_mm: Option<f64>,
    copper_index: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThicknessSource {
    IpcOverallThickness,
    IpcStackupLayerThicknesses,
    ProfileDefaultBoardThickness,
}

impl ThicknessSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::IpcOverallThickness => "ipc_2581_overall_thickness",
            Self::IpcStackupLayerThicknesses => "ipc_2581_stackup_layer_thicknesses",
            Self::ProfileDefaultBoardThickness => "profile_default_board_thickness",
        }
    }
}

#[derive(Debug)]
pub(super) struct SpanThickness {
    pub millimeters: f64,
    pub source: ThicknessSource,
}

impl PhysicalStackup {
    pub fn span_thickness(&self, span: &DrillSpan) -> std::result::Result<SpanThickness, String> {
        match span.interpretation {
            "declared_through_board" => self.total_thickness(),
            "declared_layer_span" => {
                let first = self
                    .layers
                    .iter()
                    .position(|layer| layer.copper_index == Some(span.first_copper_index))
                    .ok_or_else(|| {
                        format!(
                            "physical stackup has no copper layer at drill-span index {}",
                            span.first_copper_index
                        )
                    })?;
                let last = self
                    .layers
                    .iter()
                    .position(|layer| layer.copper_index == Some(span.last_copper_index))
                    .ok_or_else(|| {
                        format!(
                            "physical stackup has no copper layer at drill-span index {}",
                            span.last_copper_index
                        )
                    })?;
                let (first, last) = (first.min(last), first.max(last));
                // Depth is what the drill removes. A blind hole enters at its
                // outer layer and terminates on its target land, so IPC-T-50M
                // measures it from the capture land foil to the target land:
                // the target copper is not drilled. A buried hole is drilled
                // through its whole sub-stack, both terminal layers included.
                let bottom = self.copper_layers.len().saturating_sub(1);
                let (from_top, from_bottom) = (
                    span.first_copper_index == 0,
                    usize::from(span.last_copper_index) == bottom,
                );
                match (from_top, from_bottom) {
                    (true, false) if first < last => self.layer_thicknesses(first, last - 1),
                    (false, true) if first < last => self.layer_thicknesses(first + 1, last),
                    _ => self.layer_thicknesses(first, last),
                }
            }
            _ => Err(
                "drill span is not resolved in the physical stackup; board-thickness fallback is permitted only for a through hole"
                    .to_owned(),
            ),
        }
    }

    fn total_thickness(&self) -> std::result::Result<SpanThickness, String> {
        if let Some(thickness) = self.overall_thickness_mm
            && thickness.is_finite()
            && thickness > 0.0
        {
            return Ok(SpanThickness {
                millimeters: thickness,
                source: ThicknessSource::IpcOverallThickness,
            });
        }
        self.layer_thicknesses(0, self.layers.len().saturating_sub(1))
    }

    fn layer_thicknesses(
        &self,
        first: usize,
        last: usize,
    ) -> std::result::Result<SpanThickness, String> {
        let mut total = 0.0;
        for layer in &self.layers[first..=last] {
            let thickness = layer.thickness_mm.ok_or_else(|| {
                format!("physical stackup layer '{}' has no thickness", layer.name)
            })?;
            if !thickness.is_finite() || thickness < 0.0 {
                return Err(format!(
                    "physical stackup layer '{}' has a negative or non-finite thickness",
                    layer.name
                ));
            }
            total += thickness;
        }
        if !(total.is_finite() && total > 0.0) {
            return Err("physical drilled span has no positive finite thickness".to_owned());
        }
        Ok(SpanThickness {
            millimeters: total,
            source: ThicknessSource::IpcStackupLayerThicknesses,
        })
    }
}

fn collect_physical_stackup(imported: &ImportedDesign) -> Result<PhysicalStackup> {
    let stackup = match imported.stackups.as_slice() {
        [stackup] => stackup,
        [] => bail!("IPC-2581 file carries no physical stackup"),
        stackups => bail!(
            "IPC-2581 file carries {} physical stackups; physical-stackup DFM requires exactly one",
            stackups.len()
        ),
    };

    let mut copper_by_name = HashMap::new();
    for layer in imported
        .layer_definitions
        .iter()
        .filter(|layer| layers::is_copper(layer.layer_function))
    {
        if copper_by_name.insert(layer.name, layer).is_some() {
            bail!(
                "IPC-2581 file declares copper layer '{}' more than once",
                imported.resolve(layer.name)
            );
        }
    }
    if copper_by_name.is_empty() {
        bail!("IPC-2581 file declares no copper layers");
    }

    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    let mut physical_layers = Vec::new();
    let mut copper_ordinal = 0u16;
    let mut stackup_layers = stackup.layers.iter().collect::<Vec<_>>();
    if stackup_layers
        .iter()
        .all(|layer| layer.layer_number.is_some())
    {
        stackup_layers.sort_by_key(|layer| layer.layer_number);
        if stackup_layers
            .windows(2)
            .any(|pair| pair[0].layer_number == pair[1].layer_number)
        {
            bail!(
                "physical stackup '{}' has duplicate layer sequence numbers",
                imported.resolve(stackup.name)
            );
        }
    }
    for stackup_layer in stackup_layers {
        let copper_index =
            if let Some(layer) = copper_by_name.get(&stackup_layer.layer_ref).copied() {
                if !seen.insert(layer.name) {
                    bail!(
                        "physical stackup '{}' contains copper layer '{}' more than once",
                        imported.resolve(stackup.name),
                        imported.resolve(layer.name)
                    );
                }
                ordered.push(layer);
                let index = copper_ordinal;
                copper_ordinal = copper_ordinal
                    .checked_add(1)
                    .context("physical stackup has too many copper layers")?;
                Some(index)
            } else {
                None
            };
        physical_layers.push(PhysicalStackupLayer {
            layer_ref: stackup_layer.layer_ref,
            name: imported.resolve(stackup_layer.layer_ref).to_owned(),
            thickness_mm: stackup_layer.thickness,
            copper_index,
        });
    }
    if physical_layers.is_empty() {
        bail!(
            "physical stackup '{}' contains no layers",
            imported.resolve(stackup.name)
        );
    }

    let mut missing = copper_by_name
        .keys()
        .filter(|name| !seen.contains(name))
        .map(|name| imported.resolve(*name))
        .collect::<Vec<_>>();
    missing.sort_unstable();
    if !missing.is_empty() {
        bail!(
            "physical stackup '{}' omits declared copper layer(s): {}",
            imported.resolve(stackup.name),
            missing.join(", ")
        );
    }

    let total = ordered.len();
    let copper_layers = ordered
        .into_iter()
        .enumerate()
        .map(|(ordinal, layer)| {
            let side = side_label(layers::ir_side(layer.side))
                .unwrap_or_else(|| stack_side(ordinal, total));
            layer_ref(
                imported.resolve(layer.name),
                layer.layer_function,
                Some(side),
            )
        })
        .collect();
    Ok(PhysicalStackup {
        name: imported.resolve(stackup.name).to_owned(),
        copper_layers,
        overall_thickness_mm: stackup.overall_thickness,
        layers: physical_layers,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoleClass {
    Via,
    Pth,
    Npth,
}

impl HoleClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Via => "via",
            Self::Pth => "PTH",
            Self::Npth => "NPTH",
        }
    }

    pub fn subject_kind(self) -> &'static str {
        match self {
            Self::Via => "via_hole",
            Self::Pth => "plated_hole",
            Self::Npth => "nonplated_hole",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Hole {
    pub id: FeatureOccurrenceId,
    pub class: HoleClass,
    pub center: Point,
    pub diameter_mm: f64,
    pub bbox: BBox,
    pub layer: LayerRef,
    /// Inclusive indices in the same order as `Design::copper_layers`. A drill
    /// layer that declares no span is through-board, as the importer reads it.
    pub drill_span: DrillSpan,
    pub provenance: SourceLocator,
    /// The placement under the design's Step that holds the hole; `None` for
    /// the Step's own.
    pub branch: Option<u32>,
    pub step: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

/// One hole's land on one copper layer, by pool indices.
#[derive(Debug, Clone, Copy)]
pub(super) struct HoleLand {
    pub copper_index: u32,
    pub land_index: u32,
}

/// A routed slot on a drill layer.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    pub id: FeatureOccurrenceId,
    pub drill_span: DrillSpan,
    pub plating: PlatingKind,
    /// Settled for the Step's own slots; a placed slot's width is measured in
    /// the design of the Step that owns it.
    pub width: Option<SlotWidth>,
    pub outline: ContourSet,
    /// Source contours in world coordinates, retained for display only. The
    /// physical cavity is their independently filled union, like `outline`.
    pub native_outline: Vec<ContourBuf>,
    pub provenance: SourceLocator,
    /// As for [`Hole::branch`].
    pub branch: Option<u32>,
    pub bbox: BBox,
    pub layer: LayerRef,
    pub step: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

/// A slot's width, settled at extraction: the stated primitive width when the
/// source gives one (exact, verified against the materialized outline),
/// otherwise the outline's narrowest local width.
#[derive(Debug, Clone)]
pub(super) struct SlotWidth {
    pub width: Distance,
    pub disk: WidthDisk,
    pub nominal_mm: Option<f64>,
}

#[derive(Debug, Clone)]
pub(super) struct Land {
    pub id: LandId,
    pub bbox: BBox,
    pub step: Option<Symbol>,
    pub padstack: Symbol,
    pub primitive_ref: Option<Symbol>,
    pub net: Option<Symbol>,
    pub reference_designator: Option<Symbol>,
    pub pin: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
    pub provenance: SourceLocator,
}

#[derive(Debug)]
pub(super) struct CopperLayer {
    pub layer: LayerRef,
    pub position: super::pdk::LayerPosition,
    pub copper_weight_oz: Option<f64>,
    /// The final composed copper of the Step itself. What is measured on one
    /// image — its width, the ring it leaves a hole, its distance to a line —
    /// is measured in the design of the Step that paints it.
    pub image: ContourSet,
    /// The final copper of every conductor, of the Step and what it places.
    pub conductors: Vec<CopperConductor>,
    /// The Step's own source lands, including those fully removed from the
    /// final copper image. Hole links still require these for annular-ring
    /// subjects and provenance.
    pub lands: Vec<Land>,
}

/// Electrical ownership of one final copper image. Net identity is scoped by
/// its materialized Step occurrence so repeated boards do not accidentally
/// share every same-named net. `instance` is `None` for the design's own Step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ConductorId {
    Net {
        step: Option<Symbol>,
        instance: Option<u32>,
        net: Symbol,
    },
    Isolated {
        step: Option<Symbol>,
        instance: Option<u32>,
        occurrence: FeatureOccurrenceId,
    },
    Auxiliary {
        step: Option<Symbol>,
        instance: Option<u32>,
        source_set_index: u32,
    },
    Unattributed {
        step: Option<Symbol>,
        instance: Option<u32>,
        source_set_index: u32,
        source_feature_index: u32,
    },
}

impl ConductorId {
    pub fn step(self) -> Option<Symbol> {
        match self {
            Self::Net { step, .. }
            | Self::Isolated { step, .. }
            | Self::Auxiliary { step, .. }
            | Self::Unattributed { step, .. } => step,
        }
    }

    pub fn instance(self) -> Option<u32> {
        match self {
            Self::Net { instance, .. }
            | Self::Isolated { instance, .. }
            | Self::Auxiliary { instance, .. }
            | Self::Unattributed { instance, .. } => instance,
        }
    }

    pub fn net(self) -> Option<Symbol> {
        match self {
            Self::Net { net, .. } => Some(net),
            Self::Isolated { .. } | Self::Auxiliary { .. } | Self::Unattributed { .. } => None,
        }
    }

    fn is_unattributed(self) -> bool {
        matches!(self, Self::Unattributed { .. })
    }
}

#[derive(Debug)]
pub(super) struct CopperConductor {
    pub id: ConductorId,
    /// As for [`Hole::branch`].
    pub branch: Option<u32>,
    pub image: ContourSet,
}

#[derive(Debug)]
pub(super) struct MaskLayer {
    pub layer: LayerRef,
    /// The composed image of the mask openings.
    pub image: ContourSet,
    /// Final openings grouped by their physical source occurrence. A web is
    /// the complement of these images, so its two walls can have two owners.
    pub owners: Vec<MaskOwner>,
}

#[derive(Debug)]
pub(super) struct MaskOwner {
    pub step: Option<Symbol>,
    pub instance_index: Option<u32>,
    /// As for [`Hole::branch`].
    pub branch: Option<u32>,
    pub image: ContourSet,
}

#[derive(Debug, Clone)]
pub(super) struct Score {
    pub start: Point,
    pub end: Point,
    pub layer: LayerRef,
    pub provenance: SourceLocator,
}

/// A physical profile of the design's own Step. A drilled feature is measured
/// to the profile of the Step that owns it: a board's holes to the board
/// edge, a rail's tooling holes to the edge of the array carrying the rail.
#[derive(Debug, Clone)]
pub(super) struct BoardOutline {
    pub name: String,
    pub kind: LayoutStepKind,
    /// Finished board material: the filled outer profile minus every cutout.
    pub region: ContourSet,
    pub boundary: PreparedRegion,
    /// Native outer and cutout contours in the checked frame.
    pub native_outline: Vec<ContourBuf>,
    pub bbox: BBox,
}

impl BoardOutline {
    /// A product board's own edge, rather than a panel or array carrying it.
    pub fn is_board(&self) -> bool {
        self.kind == LayoutStepKind::Board
    }
}

#[derive(Debug, Clone)]
pub(super) struct BoardArray {
    pub name: String,
    pub instance_index: u32,
    pub region: ContourSet,
}

fn collect_drilled(
    source: Source<'_>,
    stackup: Option<&PhysicalStackup>,
) -> Result<(Vec<Hole>, Vec<Slot>, Vec<Blocker>)> {
    let Source {
        imported,
        resolution,
        ..
    } = source;
    let copper_count = imported
        .layer_definitions
        .iter()
        .filter(|layer| layers::is_copper(layer.layer_function))
        .count();
    let whole_stack = (0, copper_count.max(1) as u16 - 1);
    let mut holes = Vec::new();
    let mut slots = Vec::new();
    // A feature that cannot be classed or measured could belong to any rule
    // of its family, so it blocks the family rather than the run.
    let mut unusable = Vec::new();
    for (layer_index, source_layer) in
        imported
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| {
                matches!(
                    layer.layer_function,
                    LayerFunction::Drill | LayerFunction::Rout
                )
            })
    {
        let layer_name = imported.resolve(source_layer.name);
        let mut document = source
            .layer(layer_index)
            .with_context(|| format!("failed to extract drill layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
        // A slot's width runs the whole width pipeline on its outline, so the
        // layer's slots are measured together, in order, and only the Step's
        // own: a placed slot's width is its own Step's to measure.
        let slot_features = document
            .features
            .iter()
            .filter(|feature| feature.is_drill_like() && feature.kind == FeatureKind::Slot)
            .collect::<Vec<_>>();
        #[cfg(not(target_family = "wasm"))]
        let slot_features = slot_features.into_par_iter();
        #[cfg(target_family = "wasm")]
        let slot_features = slot_features.into_iter();
        let mut slot_shapes = slot_features
            .map(|feature| {
                let contours = document.placed_feature_contours(feature);
                let outline = ContourSet::from_filled_contours(&contours, resolution)?;
                let width_disk = match source.placed(feature) {
                    None => min_width_disk(&outline)?,
                    Some(_) => None,
                };
                Ok((contours, outline, width_disk))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter();
        for feature in document
            .features
            .iter()
            .filter(|feature| feature.is_drill_like())
        {
            let placed = source.placed(feature);
            // What a placed feature lacks already blocks its own Step's design.
            let mut block = |pools, reason: String| {
                if placed.is_none() {
                    unusable.push(Blocker { pools, reason });
                }
            };
            match feature.kind {
                FeatureKind::Hole => {
                    let at = format!(
                        "drilled hole on layer '{layer_name}' at ({:.6}, {:.6})",
                        feature.center.x, feature.center.y
                    );
                    let size = feature.shape.and_then(SimpleShape::hole_size);
                    let Some(diameter_mm) = size.filter(|size| *size > 0.0 && size.is_finite())
                    else {
                        block(
                            Pools::HOLES,
                            format!("{at} has no positive finite diameter"),
                        );
                        continue;
                    };
                    let Some(class) = hole_class(feature.intent.plating) else {
                        block(Pools::HOLES, format!("{at} has unknown plating"));
                        continue;
                    };
                    // Every hole rule measures a disk of the stated diameter,
                    // which a square hole's corners extend beyond.
                    if matches!(feature.shape, Some(SimpleShape::Square { .. })) {
                        block(
                            Pools::HOLES,
                            format!("{at} is square, not a circular drill"),
                        );
                        continue;
                    }
                    holes.push(Hole {
                        id: feature_occurrence_id(feature)
                            .context("materialized hole has no occurrence identity")?,
                        class,
                        center: feature.center,
                        diameter_mm,
                        bbox: BBox::from_point(feature.center).expand(diameter_mm / 2.0),
                        layer: layer_ref(layer_name, source_layer.layer_function, None),
                        drill_span: drill_span(
                            feature.intent.span,
                            &imported.layer_definitions,
                            whole_stack,
                            stackup,
                        ),
                        provenance: feature_provenance(source, layer_name, feature),
                        branch: source.branch(placed),
                        step: feature.source_step_ref,
                        padstack: feature.padstack_ref,
                        net: source_net(&document, feature),
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
                }
                FeatureKind::Slot => {
                    let (contours, outline, width_disk) = slot_shapes
                        .next()
                        .expect("every slot feature of the layer was measured");
                    let at = format!(
                        "routed slot on layer '{layer_name}' at ({:.6}, {:.6})",
                        feature.bbox.center().x,
                        feature.bbox.center().y
                    );
                    if !matches!(
                        feature.intent.plating,
                        PlatingKind::Plated | PlatingKind::NonPlated
                    ) {
                        block(Pools::SLOTS, format!("{at} has unknown plating"));
                        continue;
                    }
                    let width = match (placed, width_disk) {
                        (Some(_), _) => None,
                        (None, None) => {
                            block(Pools::SLOTS, format!("{at} has no measurable outline"));
                            continue;
                        }
                        (None, Some(disk)) => {
                            let nominal_mm = feature
                                .shape
                                .and_then(SimpleShape::slot_width)
                                .filter(|width| *width > 0.0 && width.is_finite());
                            match slot_width(nominal_mm, disk.width) {
                                Ok(width) => Some(SlotWidth {
                                    width,
                                    disk,
                                    nominal_mm,
                                }),
                                Err(error) => {
                                    block(Pools::SLOTS, format!("{at} {error}"));
                                    continue;
                                }
                            }
                        }
                    };
                    slots.push(Slot {
                        id: feature_occurrence_id(feature)
                            .context("materialized slot has no occurrence identity")?,
                        drill_span: drill_span(
                            feature.intent.span,
                            &imported.layer_definitions,
                            whole_stack,
                            stackup,
                        ),
                        plating: feature.intent.plating,
                        width,
                        outline,
                        native_outline: contours,
                        provenance: feature_provenance(source, layer_name, feature),
                        branch: source.branch(placed),
                        bbox: feature.bbox,
                        layer: layer_ref(layer_name, source_layer.layer_function, None),
                        step: feature.source_step_ref,
                        padstack: feature.padstack_ref,
                        net: source_net(&document, feature),
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
                }
                _ => {}
            }
        }
    }
    holes.sort_by(|left, right| {
        left.bbox
            .min
            .x
            .total_cmp(&right.bbox.min.x)
            .then_with(|| left.center.x.total_cmp(&right.center.x))
            .then_with(|| left.center.y.total_cmp(&right.center.y))
            .then_with(|| left.diameter_mm.total_cmp(&right.diameter_mm))
    });
    slots.sort_by(|left, right| {
        left.bbox
            .min
            .x
            .total_cmp(&right.bbox.min.x)
            .then_with(|| left.bbox.min.y.total_cmp(&right.bbox.min.y))
            .then_with(|| left.bbox.max.x.total_cmp(&right.bbox.max.x))
            .then_with(|| left.bbox.max.y.total_cmp(&right.bbox.max.y))
    });
    Ok((holes, slots, unusable))
}

/// A slot's width: the stated primitive width when the source gives one,
/// otherwise the outline's measured minimum width. A stated width is exact,
/// and the outline must agree with it within the measurement's uncertainty;
/// a file that states one width and draws another is inconsistent.
fn slot_width(stated_mm: Option<f64>, measured: Distance) -> Result<Distance> {
    let Some(stated_mm) = stated_mm else {
        return Ok(measured);
    };
    if (measured.mm - stated_mm).abs() > measured.uncertainty_mm {
        bail!(
            "states width {stated_mm:.6} mm but its outline measures {:.6} mm",
            measured.mm
        );
    }
    Ok(Distance::exact(stated_mm, measured.first, measured.second))
}

/// Resolve a drill span to an inclusive ordinal range over the copper
/// stackup. Unknown, through-board, and unresolvable spans widen to `None`;
/// so does a span that reaches no copper layer at all.
fn copper_span(span: FeatureSpan, layers: &[ipc2581::types::Layer]) -> Option<(u16, u16)> {
    let position = |name: Symbol| layers.iter().position(|layer| layer.name == name);
    let (low, high) = match span {
        FeatureSpan::Unknown | FeatureSpan::ThroughBoard => return None,
        FeatureSpan::Layer(layer) => {
            let index = position(layer)?;
            (index, index)
        }
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => {
            let from = position(from)?;
            let to = position(to)?;
            (from.min(to), from.max(to))
        }
        FeatureSpan::FromTo { .. } => return None,
    };
    let mut first = None;
    let mut last = None;
    let mut ordinal: u16 = 0;
    for (index, layer) in layers.iter().enumerate() {
        if !layers::is_copper(layer.layer_function) {
            continue;
        }
        if (low..=high).contains(&index) {
            first.get_or_insert(ordinal);
            last = Some(ordinal);
        }
        ordinal += 1;
    }
    Some((first?, last?))
}

fn drill_span(
    span: FeatureSpan,
    layers: &[ipc2581::types::Layer],
    whole_stack: (u16, u16),
    stackup: Option<&PhysicalStackup>,
) -> DrillSpan {
    let resolved = match stackup {
        Some(stackup) => physical_copper_span(span, stackup),
        None => copper_span(span, layers),
    };
    let (first_copper_index, last_copper_index) = resolved.unwrap_or(whole_stack);
    let declared_through = matches!(span, FeatureSpan::ThroughBoard)
        || resolved.is_some_and(|resolved| resolved == whole_stack);
    DrillSpan {
        first_copper_index,
        last_copper_index,
        interpretation: match (declared_through, resolved) {
            (true, _) => "declared_through_board",
            (false, Some(_)) => "declared_layer_span",
            (false, None) => "assumed_whole_stack",
        },
    }
}

fn physical_copper_span(span: FeatureSpan, stackup: &PhysicalStackup) -> Option<(u16, u16)> {
    let copper_index = |name: Symbol| {
        stackup
            .layers
            .iter()
            .find(|layer| layer.layer_ref == name)
            .and_then(|layer| layer.copper_index)
    };
    match span {
        FeatureSpan::Layer(layer) => copper_index(layer).map(|index| (index, index)),
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => {
            let from = copper_index(from)?;
            let to = copper_index(to)?;
            Some((from.min(to), from.max(to)))
        }
        FeatureSpan::Unknown | FeatureSpan::ThroughBoard | FeatureSpan::FromTo { .. } => None,
    }
}

fn source_net(document: &GeometryDocument, feature: &Feature) -> Option<Symbol> {
    feature
        .net
        .or_else(|| document.feature_set(feature).and_then(|set| set.net))
}

fn feature_provenance(source: Source<'_>, layer: &str, feature: &Feature) -> SourceLocator {
    let imported = source.imported;
    let occurrence = feature_occurrence_id(feature)
        .expect("materialized DFM feature must retain its occurrence identity");
    let definition = imported
        .feature_definition(occurrence.feature)
        .expect("materialized DFM feature must reference its imported definition")
        .source;
    SourceLocator {
        step: feature
            .source_step_ref
            .map(|step| imported.resolve(step).to_owned()),
        layer: Some(layer.to_owned()),
        set_index: Some(definition.set_index),
        feature_index: Some(definition.feature_index),
        instance_index: source.placed(feature),
    }
}

fn hole_class(plating: PlatingKind) -> Option<HoleClass> {
    match plating {
        PlatingKind::Via | PlatingKind::ViaCapped => Some(HoleClass::Via),
        PlatingKind::Plated => Some(HoleClass::Pth),
        PlatingKind::NonPlated => Some(HoleClass::Npth),
        PlatingKind::Unknown | PlatingKind::None => None,
    }
}

/// The conductor a copper feature belongs to.
fn copper_conductor(
    source: Source<'_>,
    document: &GeometryDocument,
    feature: &Feature,
) -> ConductorId {
    let step = feature.source_step_ref;
    let instance = source.placed(feature);
    if let Some(net) = feature.net {
        return ConductorId::Net {
            step,
            instance,
            net,
        };
    }
    if feature.kind == FeatureKind::Padstack {
        return ConductorId::Isolated {
            step,
            instance,
            occurrence: feature_occurrence_id(feature)
                .expect("materialized copper pad must retain its occurrence identity"),
        };
    }
    if feature.is_fiducial()
        || document
            .feature_set(feature)
            .is_some_and(|set| set.copper_balance)
    {
        return ConductorId::Auxiliary {
            step,
            instance,
            source_set_index: feature.source.set_index,
        };
    }
    ConductorId::Unattributed {
        step,
        instance,
        source_set_index: feature.source.set_index,
        source_feature_index: feature.source.feature_index,
    }
}

/// The composed image of the Step's own copper, and every conductor's final
/// copper, of the Step and of everything it places.
fn compose_attributed_copper(
    document: &mut GeometryDocument,
    source: Source<'_>,
) -> Result<(ContourSet, Vec<CopperConductor>)> {
    let owners = compose_attributed_owners(
        document,
        LayerRole::Copper,
        &|document, feature| copper_conductor(source, document, feature),
        source.resolution,
    )?;
    let mut composer = pcb_ir::geom::region::PaintComposer::new(source.resolution);
    for (_, image) in owners.iter().filter(|(id, _)| id.instance().is_none()) {
        composer.push(pcb_ir::geom::Polarity::Dark, image.clone());
    }
    let image = composer.finish()?;
    let conductors = owners
        .into_iter()
        .map(|(id, rings)| CopperConductor {
            id,
            branch: source.branch(id.instance()),
            image: rings,
        })
        .collect();
    Ok((image, conductors))
}

/// Both copper and soldermask use the canonical ordered paint fold. Source
/// ownership survives clear features and cutouts, rather than being inferred
/// afterward from a feature's bounds or an enclosing board profile.
fn compose_attributed_owners<Owner: Clone + Eq + std::hash::Hash>(
    document: &mut GeometryDocument,
    role: LayerRole,
    owner: &dyn Fn(&GeometryDocument, &Feature) -> Owner,
    resolution: Resolution,
) -> Result<artwork::OwnerImages<Owner>> {
    pcb_ir::dialects::ipc::process::normalize_for_artwork(document, resolution)?;
    pcb_ir::dialects::ipc::validate_artwork_ready(document)
        .map_err(|error| anyhow::anyhow!("layer is not artwork-ready: {error}"))?;
    let layer = document
        .layers
        .first()
        .context("extracted artwork document has no layer")?;
    let header = artwork::Layer {
        name: layer.name.clone(),
        role,
        side: Side::None,
        objects: Span::EMPTY,
        bbox: layer.bbox,
        meta: layer.layer_function,
    };
    let attributed_artwork = lower_layer_to_artwork_with(
        document,
        0,
        header,
        &ArtworkTarget::default(),
        &|document, feature| Some(owner(document, feature)),
    );
    let (mut layers, _) = artwork::compose_owner_regions(
        &attributed_artwork,
        |owner| Some(owner.clone()),
        resolution,
    )?;
    let owners = layers
        .pop()
        .context("attributed artwork composition produced no layer")?;
    owners
        .into_iter()
        .map(|(id, rings)| {
            Ok((
                id.context(
                    "structural artwork instance survived source ownership materialization",
                )?,
                rings,
            ))
        })
        .collect()
}

fn conductor_order(
    imported: &ImportedDesign,
    id: ConductorId,
) -> (
    u8,
    &str,
    Option<u32>,
    &str,
    u32,
    u32,
    Option<FeatureOccurrenceId>,
) {
    match id {
        ConductorId::Net {
            step,
            instance,
            net,
        } => (
            0,
            step.map(|step| imported.resolve(step)).unwrap_or(""),
            instance,
            imported.resolve(net),
            0,
            0,
            None,
        ),
        ConductorId::Isolated {
            step,
            instance,
            occurrence,
        } => {
            let source = imported
                .feature_definition(occurrence.feature)
                .expect("isolated pad must reference its imported definition")
                .source;
            (
                1,
                step.map(|step| imported.resolve(step)).unwrap_or(""),
                instance,
                "",
                source.set_index,
                source.feature_index,
                Some(occurrence),
            )
        }
        ConductorId::Auxiliary {
            step,
            instance,
            source_set_index,
        } => (
            2,
            step.map(|step| imported.resolve(step)).unwrap_or(""),
            instance,
            "",
            source_set_index,
            0,
            None,
        ),
        ConductorId::Unattributed {
            step,
            instance,
            source_set_index,
            source_feature_index,
        } => (
            3,
            step.map(|step| imported.resolve(step)).unwrap_or(""),
            instance,
            "",
            source_set_index,
            source_feature_index,
            None,
        ),
    }
}

fn collect_copper_layers(
    source: Source<'_>,
    stackup: Option<&PhysicalStackup>,
) -> Result<Vec<CopperLayer>> {
    let imported = source.imported;
    let mut copper_layers = imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter(|(_, layer)| layers::is_copper(layer.layer_function))
        .collect::<Vec<_>>();
    if let Some(stackup) = stackup {
        copper_layers.sort_by_key(|(_, layer)| {
            stackup
                .copper_layers
                .iter()
                .position(|physical| physical.name == imported.resolve(layer.name))
                .expect("validated stackup includes every copper layer")
        });
    }
    let total = copper_layers.len();
    #[cfg(not(target_family = "wasm"))]
    let copper_layers = copper_layers.into_par_iter();
    #[cfg(target_family = "wasm")]
    let copper_layers = copper_layers.into_iter();
    copper_layers
        .enumerate()
        .map(|(ordinal, (layer_index, layer))| {
            let name = imported.resolve(layer.name);
            let mut document = source
                .layer(layer_index)
                .with_context(|| format!("failed to extract IPC-2581 copper layer '{name}'"))?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
            let mut lands = Vec::new();
            for feature in document.features.iter().filter(|feature| {
                feature.kind == FeatureKind::Padstack
                    && feature.polarity == Polarity::Dark
                    && feature.intent.domain == FeatureDomain::Copper
                    && source.placed(feature).is_none()
            }) {
                let Some(padstack) = feature.padstack_ref else {
                    continue;
                };
                let pin_ref = feature.pin_refs.slice(&document.pin_refs).first();
                lands.push(Land {
                    id: LandId(
                        feature_occurrence_id(feature)
                            .context("materialized land has no occurrence identity")?,
                    ),
                    bbox: feature.bbox,
                    step: feature.source_step_ref,
                    padstack,
                    primitive_ref: feature.primitive_ref.map(|primitive| primitive.id()),
                    net: feature.net,
                    reference_designator: pin_ref.and_then(|pin| pin.component_ref),
                    pin: pin_ref.map(|pin| pin.pin),
                    source_set_index: feature.source.set_index,
                    source_feature_index: feature.source.feature_index,
                    provenance: feature_provenance(source, name, feature),
                });
            }
            let (image, mut conductors) = compose_attributed_copper(&mut document, source)?;
            conductors.sort_by_key(|conductor| conductor_order(imported, conductor.id));
            // The file's side attribute is authoritative; the stackup
            // position is the fallback for files that omit it.
            let side =
                side_label(layers::ir_side(layer.side)).unwrap_or(stack_side(ordinal, total));
            Ok(CopperLayer {
                layer: layer_ref(name, layer.layer_function, Some(side)),
                position: if side == "inner" {
                    super::pdk::LayerPosition::Inner
                } else {
                    super::pdk::LayerPosition::Outer
                },
                copper_weight_oz: copper_weight_oz(imported, layer.name),
                image,
                conductors,
                lands,
            })
        })
        .collect()
}

/// Copper clearance is between electrical owners, so functional copper the
/// file attributes to no net leaves that rule uncertifiable. Other copper
/// rules measure the composed image and are unaffected. What a placed Step
/// leaves unattributed already blocks its own design.
fn unattributed_copper(imported: &ImportedDesign, layers: &[CopperLayer]) -> Vec<Blocker> {
    layers
        .iter()
        .filter_map(|layer| {
            let id = layer
                .conductors
                .iter()
                .map(|conductor| conductor.id)
                .find(|id| id.is_unattributed() && id.instance().is_none())?;
            Some(Blocker {
                pools: Pools::CONDUCTOR_OWNERSHIP,
                reason: format!(
                    "copper layer '{}' has final functional copper without net attribution in Step '{}'",
                    layer.layer.name,
                    id.step()
                        .map(|step| imported.resolve(step))
                        .unwrap_or("<root>"),
                ),
            })
        })
        .collect()
}

fn copper_weight_oz(imported: &ImportedDesign, layer: Symbol) -> Option<f64> {
    let stackup_layer = imported
        .stackups
        .iter()
        .flat_map(|stackup| &stackup.layers)
        .find(|candidate| candidate.layer_ref == layer)?;
    stackup_layer
        .spec_ref
        .and_then(|spec| imported.specs.get(&spec))
        .and_then(|spec| spec.copper_weight_oz)
        .or_else(|| {
            stackup_layer
                .thickness
                .map(|millimeters| millimeters / 0.0348)
        })
}

fn side_label(side: Side) -> Option<&'static str> {
    match side {
        Side::Top => Some("top"),
        Side::Bottom => Some("bottom"),
        Side::Inner => Some("inner"),
        Side::None => None,
    }
}

fn stack_side(ordinal: usize, total: usize) -> &'static str {
    if ordinal == 0 {
        "top"
    } else if ordinal + 1 == total {
        "bottom"
    } else {
        "inner"
    }
}

fn collect_mask_layers(source: Source<'_>) -> Result<Vec<MaskLayer>> {
    let Source {
        imported,
        resolution,
        ..
    } = source;
    let layers = imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.layer_function == LayerFunction::Soldermask)
        .collect::<Vec<_>>();
    #[cfg(not(target_family = "wasm"))]
    let layers = layers.into_par_iter();
    #[cfg(target_family = "wasm")]
    let layers = layers.into_iter();
    layers
        .map(|(layer_index, layer)| {
            let name = imported.resolve(layer.name);
            let mut document = source
                .layer(layer_index)
                .with_context(|| format!("failed to extract soldermask layer '{name}'"))?;
            let image = document.clone().into_layer_image(
                0,
                LayerRole::Soldermask,
                pcb_ir::dialects::Side::None,
                resolution,
            )?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
            let owners = compose_attributed_owners(
                &mut document,
                LayerRole::Soldermask,
                &|_, feature| (feature.source_step_ref, source.placed(feature)),
                resolution,
            )?;
            Ok(MaskLayer {
                layer: layer_ref(
                    name,
                    layer.layer_function,
                    side_label(layers::ir_side(layer.side)),
                ),
                image,
                owners: owners
                    .into_iter()
                    .map(|((step, instance_index), rings)| MaskOwner {
                        step,
                        instance_index,
                        branch: source.branch(instance_index),
                        image: rings,
                    })
                    .collect(),
            })
        })
        .collect()
}

/// Join DFM pool indices through the canonical physical relationships. An
/// ambiguous or conflicting relationship fails closed; DFM never chooses the
/// nearest candidate. Only the Step's own drilled features are linked, to its
/// own lands: what it places is linked in that Step's own design.
fn link_lands(
    drilled: Vec<Option<FeatureOccurrenceId>>,
    land_indices: &HashMap<LandId, HoleLand>,
    physical_holes: &HashMap<FeatureOccurrenceId, PhysicalHole>,
) -> Result<Vec<Vec<HoleLand>>> {
    drilled
        .into_iter()
        .map(|own| {
            let Some(id) = own else {
                return Ok(Vec::new());
            };
            let physical_hole = physical_holes
                .get(&id)
                .context("DFM hole is missing from the canonical physical view")?;
            let mut links = Vec::new();
            for relationship in &physical_hole.lands {
                match &relationship.land {
                    Association::Resolved(land) if land.0.layout == id.layout => links.push(
                        *land_indices.get(land).context(
                            "resolved physical land is missing from the DFM copper pool",
                        )?,
                    ),
                    Association::Resolved(_) | Association::Unresolved => {}
                    Association::Ambiguous(candidates) => bail!(
                        "drilled feature has an ambiguous physical-land association ({} candidates)",
                        candidates.len()
                    ),
                    Association::Conflicting(candidates) => bail!(
                        "drilled feature has conflicting physical-land evidence ({} candidates)",
                        candidates.len()
                    ),
                }
            }
            Ok(links)
        })
        .collect()
}

/// The V-score lines the Step draws itself.
fn collect_scores(source: Source<'_>) -> Result<Vec<Score>> {
    let imported = source.imported;
    let mut scores = Vec::new();
    for (layer_index, layer) in
        imported
            .layer_definitions
            .iter()
            .enumerate()
            .filter(|(_, layer)| {
                matches!(
                    layer.layer_function,
                    LayerFunction::VCut | LayerFunction::Score
                )
            })
    {
        let document = source.layer(layer_index)?;
        scores.extend(
            pcb_ir::dialects::ipc::relief::vscore_feature_lines_for(&document)
                .into_iter()
                .filter(|(feature_index, _)| {
                    source.placed(&document.features[*feature_index]).is_none()
                })
                .map(|(feature_index, line)| Score {
                    start: line.start,
                    end: line.end,
                    layer: layer_ref(imported.resolve(layer.name), layer.layer_function, None),
                    provenance: feature_provenance(
                        source,
                        imported.resolve(layer.name),
                        &document.features[feature_index],
                    ),
                }),
        );
    }
    Ok(scores)
}

/// The Step's own profiles. What it places keeps its profiles to itself: a
/// drilled feature and an edge are each measured in their own Step's design.
fn collect_board_outlines(source: Source<'_>, step: u32) -> anyhow::Result<Vec<BoardOutline>> {
    let Source {
        imported,
        resolution,
        ..
    } = source;
    let layout = &imported.geometry;
    let definition = &layout.layout.steps[step as usize];
    Ok(definition
        .profiles
        .slice(&layout.profiles)
        .iter()
        .map(|profile| {
            let mut native_outline =
                layout.transformed_path_contours(profile.outer_path, Affine2::IDENTITY);
            let outer_count = native_outline.len();
            for cutout in profile.cutouts.slice(&layout.profile_cutouts) {
                native_outline
                    .extend(layout.transformed_path_contours(cutout.path, Affine2::IDENTITY));
            }
            let outer =
                ContourSet::from_filled_contours(&native_outline[..outer_count], resolution)?;
            let cutouts =
                ContourSet::from_filled_contours(&native_outline[outer_count..], resolution)?;
            let region = outer.difference(&cutouts)?;
            if region.is_empty() {
                return Ok(None);
            }
            let bbox = region.bbox;
            let boundary = region.prepare_query();
            Ok::<_, anyhow::Error>(Some(BoardOutline {
                name: imported.resolve(definition.source_step_ref).to_owned(),
                kind: definition.kind,
                region,
                boundary,
                native_outline,
                bbox,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// The board arrays the Step places directly.
fn collect_board_arrays(source: Source<'_>) -> anyhow::Result<Vec<BoardArray>> {
    let Source {
        imported,
        resolution,
        ..
    } = source;
    let layout = &imported.geometry;
    let own = match source.root {
        LayoutOccurrenceId::Root => None,
        LayoutOccurrenceId::Instance(instance) => Some(instance),
    };
    let frame_from_scope = own
        .map(|instance| layout.layout.instances[instance as usize].transform)
        .unwrap_or(Affine2::IDENTITY)
        .inverse()
        .context("layout occurrence has a singular placement")?;
    // A panel-kind child that wraps exactly one board is per-board packaging
    // (a cell in a larger grid), not a sibling array to keep spacing from; a
    // one-board array still nests its own panel-kind cell.
    let wraps_single_board = |instance_index: usize| {
        let mut children = layout
            .layout
            .instances
            .iter()
            .filter(|child| child.parent_instance == Some(instance_index as u32));
        match (children.next(), children.next()) {
            (Some(only), None) => {
                layout.layout.steps[only.child_step as usize].kind == LayoutStepKind::Board
            }
            _ => false,
        }
    };
    Ok(layout
        .layout
        .instances
        .iter()
        .enumerate()
        .filter(|(instance_index, instance)| {
            instance.parent_instance == own
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
                && !wraps_single_board(*instance_index)
        })
        .map(|(instance_index, instance)| {
            let step = &layout.layout.steps[instance.child_step as usize];
            let placement = frame_from_scope.concat(instance.transform);
            let contours = step
                .profiles
                .slice(&layout.profiles)
                .iter()
                .flat_map(|profile| layout.transformed_path_contours(profile.outer_path, placement))
                .collect::<Vec<_>>();
            let region = ContourSet::from_filled_contours(&contours, resolution)?;
            Ok::<_, anyhow::Error>((!region.is_empty()).then(|| BoardArray {
                name: imported.resolve(step.source_step_ref).to_owned(),
                instance_index: instance_index as u32,
                region,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

fn layer_ref(name: &str, function: LayerFunction, side: Option<&'static str>) -> LayerRef {
    LayerRef {
        name: name.to_owned(),
        function: function.as_str().to_owned(),
        side,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc2581::Ipc2581;

    fn root(imported: &ImportedDesign, scope: ArtworkScope) -> Source<'_> {
        Source {
            imported,
            scope,
            root: LayoutOccurrenceId::Root,
            resolution: Resolution::default(),
        }
    }

    #[test]
    fn mask_owners_preserve_composed_openings_and_repeat_identity() {
        let resolution = Resolution::default();

        let rectangle = |polarity, min, max| {
            format!(
                r#"<Set polarity="{polarity}"><Features><UserSpecial><Contour><Polygon>
                <PolyBegin x="{min}" y="{min}"/>
                <PolyStepSegment x="{max}" y="{min}"/>
                <PolyStepSegment x="{max}" y="{max}"/>
                <PolyStepSegment x="{min}" y="{max}"/>
            </Polygon></Contour></UserSpecial></Features></Set>"#
            )
        };
        let source = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/><LayerRef name="F.Mask"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
    <Step name="board" type="BOARD">
      <LayerFeature layerRef="F.Mask">{}{}{}</LayerFeature>
    </Step>
    <Step name="panel" type="PALLET">
      <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="10" dy="0"/>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#,
            rectangle("POSITIVE", 0.0, 4.0),
            rectangle("NEGATIVE", 1.0, 3.0),
            rectangle("POSITIVE", 1.8, 2.2),
        );
        let ipc = Ipc2581::parse(&source).unwrap();
        let imported = import_design(&ipc, resolution).unwrap();
        let document = imported
            .materialize_layer(
                imported.layer_id("F.Mask").unwrap(),
                ArtworkScope::ArrayFlattened,
            )
            .unwrap();
        let previous = document
            .into_layer_image(
                0,
                LayerRole::Soldermask,
                pcb_ir::dialects::Side::None,
                resolution,
            )
            .unwrap();
        let layer = collect_mask_layers(root(&imported, ArtworkScope::ArrayFlattened))
            .unwrap()
            .remove(0);
        assert_eq!(
            layer.image.rings, previous.rings,
            "source attribution must not change the measured image"
        );
        assert_eq!(layer.owners.len(), 2);
        let mut instances = HashSet::new();
        for (owner, x) in layer.owners.iter().zip([10.0, 20.0]) {
            assert_eq!(owner.step.map(|step| imported.resolve(step)), Some("board"));
            assert!(instances.insert(owner.instance_index.unwrap()));
            assert!(owner.image.contains_point(Point::new(x + 0.5, 20.5)));
            assert!(
                !owner.image.contains_point(Point::new(x + 1.5, 21.5)),
                "clear set removes the opening"
            );
            assert!(
                owner.image.contains_point(Point::new(x + 2.0, 22.0)),
                "later positive set repaints the opening"
            );
            assert!(
                owner.image.bbox.max.x < x + 5.0,
                "owners do not absorb neighboring repeats"
            );
        }
    }

    fn slot_fixture(shape: &str) -> Ipc2581 {
        Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="ROUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="ROUT">
          <Set>
            <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
              {shape}
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        ))
        .unwrap()
    }

    #[test]
    fn slot_width_is_stated_when_given_and_measured_otherwise() {
        let resolution = Resolution::default();

        let oval = slot_fixture(
            r#"<Location x="10" y="20"/>
              <Oval width="1.8" height="0.6"/>"#,
        );
        let oval = import_design(&oval, resolution).unwrap();
        let (_, slots, _) = collect_drilled(root(&oval, ArtworkScope::Board), None).unwrap();
        assert_eq!(slots.len(), 1);
        let stated = slots[0].width.as_ref().unwrap().width;
        assert!((stated.mm - 0.6).abs() < 1e-9);
        assert_eq!(stated.uncertainty_mm, 0.0, "a stated width is exact");
        let native = &slots[0].native_outline;
        assert!(
            native
                .iter()
                .flat_map(|contour| &contour.cmds)
                .any(|command| { command.op == pcb_ir::geom::path::PathOp::ArcTo }),
            "native slot outlines retain source curves"
        );
        assert_eq!(
            native
                .iter()
                .map(|contour| contour.bbox)
                .fold(BBox::empty(), BBox::union),
            slots[0].bbox
        );
        let reconstructed = ContourSet::from_filled_contours(native, resolution).unwrap();
        assert!(
            reconstructed
                .difference(&slots[0].outline)
                .unwrap()
                .is_empty()
        );
        assert!(
            slots[0]
                .outline
                .difference(&reconstructed)
                .unwrap()
                .is_empty()
        );

        let outline = slot_fixture(
            r#"<Outline>
                <Polygon>
                  <PolyBegin x="10" y="20"/>
                  <PolyStepSegment x="10.6" y="20"/>
                  <PolyStepSegment x="10.6" y="21.8"/>
                  <PolyStepSegment x="10" y="21.8"/>
                  <PolyStepSegment x="10" y="20"/>
                </Polygon>
                <LineDesc lineWidth="0" lineEnd="ROUND"/>
              </Outline>"#,
        );
        let outline = import_design(&outline, resolution).unwrap();
        let (_, slots, _) = collect_drilled(root(&outline, ArtworkScope::Board), None).unwrap();
        assert_eq!(slots.len(), 1);
        let width = slots[0].width.as_ref().unwrap().width;
        assert!(
            (width.mm - 0.6).abs() < 1e-8,
            "measured width was {}",
            width.mm
        );
        assert!(
            width.uncertainty_mm > 0.0,
            "a measured outline carries uncertainty"
        );
        assert!(
            slots[0]
                .native_outline
                .iter()
                .flat_map(|contour| &contour.cmds)
                .all(|command| command.op != pcb_ir::geom::path::PathOp::ArcTo),
            "actual source polygons must not be smoothed into curves"
        );
    }

    #[test]
    fn stated_width_must_match_the_outline() {
        let resolution = Resolution::default();

        let ipc = slot_fixture(
            r#"<Location x="10" y="20"/>
              <Oval width="1.8" height="0.6"/>"#,
        );
        let imported = import_design(&ipc, resolution).unwrap();
        let oval = collect_drilled(root(&imported, ArtworkScope::Board), None)
            .unwrap()
            .1
            .remove(0);
        assert!(slot_width(Some(0.9), oval.width.unwrap().width).is_err());
    }
}
