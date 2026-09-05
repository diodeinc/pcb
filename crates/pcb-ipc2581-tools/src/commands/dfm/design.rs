//! The checkable design: entity pools extracted from one IPC-2581 file for
//! one layout target, in plain millimeters.
//!
//! Exactly the pools the configured rules read are extracted; the rest stay
//! empty. Pools are flat vectors; copper follows physical stackup order
//! when available, otherwise declaration order. Derived facts that
//! relate pools (a hole's lands, a copper layer's boundary index) are side
//! tables indexed like their primary pool. Extraction fails closed: a
//! drilled feature whose plating, diameter, or outline the file does not
//! state is an error, never a quietly dropped subject.

use pcb_ir::geom::GeometryAccuracy;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    ArtworkLowering, ArtworkObjectKind, ArtworkScope, Feature, FeatureDomain, FeatureKind,
    FeatureSpan, LayoutPurpose, LayoutStepKind, PlatingKind, ProfileOccurrenceRole, ProfileSet,
    lower_layer_to_artwork_with, profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side, artwork};
use pcb_ir::geom::dfm::{Distance, WidthDisk, min_width_disk};
use pcb_ir::geom::path::ContourBuf;
use pcb_ir::geom::region::Ring;
use pcb_ir::geom::{BBox, ContourSet, Point, Polarity, PreparedRegion, Span, tol};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use crate::geometry::GeometryDocument;
use crate::layers;
#[cfg(test)]
use pcb_ir::import::ipc2581::import_design;
use pcb_ir::import::ipc2581::{
    FeatureOccurrenceId, ImportedDesign, LayerId, feature_occurrence_id,
};
use pcb_ir::import::physical::{Association, LandId, PhysicalHole};

use super::report::{DrillSpan, LayerRef, LayoutContext, LayoutOccurrence, SourceLocator};
use super::rules::{self, Rule};

pub(super) struct Design<'a> {
    pub imported: &'a ImportedDesign,
    pub scope: ArtworkScope,
    pub stackup: Option<PhysicalStackup>,
    pub holes: Vec<Hole>,
    pub slots: Vec<Slot>,
    pub copper_layers: Vec<CopperLayer>,
    /// One boundary index per copper layer, for clearance and enclosure
    /// queries against the composed copper.
    pub copper_boundaries: Vec<PreparedRegion>,
    /// One boundary index per attributed conductor on each copper layer.
    pub conductor_boundaries: Vec<Vec<PreparedRegion>>,
    /// Each hole's lands, one per copper layer it owns a land on, indexed
    /// like `holes`.
    pub hole_lands: Vec<Vec<HoleLand>>,
    pub slot_lands: Vec<Vec<HoleLand>>,
    pub mask_layers: Vec<MaskLayer>,
    pub scores: Vec<Score>,
    pub board_outlines: Vec<BoardOutline>,
    pub board_arrays: Vec<BoardArray>,
}

/// Build a pool only when a rule reads it.
fn when<T: Default>(wanted: bool, build: impl FnOnce() -> Result<T>) -> Result<T> {
    if wanted { build() } else { Ok(T::default()) }
}

impl<'a> Design<'a> {
    pub fn extract(
        imported: &'a ImportedDesign,
        scope: ArtworkScope,
        rules: &[Rule],
        accuracy: GeometryAccuracy,
    ) -> Result<Self> {
        let pools = rules::pools(rules);
        // Circular drill checks must use the declared physical order even
        // when no thickness or layer-count rule requests the stackup pool.
        // Keep the legacy declaration-order fallback for files without one.
        let span_checks = rules.iter().any(|rule| {
            matches!(
                rule.kind,
                rules::RuleKind::HoleToCopperClearance(_)
                    | rules::RuleKind::AnnularRing(_)
                    | rules::RuleKind::HolePairClearance(_, _)
            )
        });
        let stackup = when(
            pools.stackup || (span_checks && !imported.stackups.is_empty()),
            || collect_physical_stackup(imported).map(Some),
        )?;
        let (holes, slots) = when(pools.drilled, || {
            collect_drilled(imported, scope, stackup.as_ref(), accuracy)
        })?;
        let copper_layers = when(pools.copper, || {
            collect_copper_layers(
                imported,
                scope,
                pools.conductor_ownership,
                stackup.as_ref(),
                accuracy,
            )
        })?;
        let (physical_holes, land_indices) = when(pools.hole_lands || pools.slot_lands, || {
            let physical_holes = imported
                .physical_holes(scope, accuracy)?
                .into_iter()
                .map(|hole| (hole.id.0, hole))
                .collect();
            let land_indices = copper_layers
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
                .collect();
            Ok((physical_holes, land_indices))
        })?;
        let layout = when(pools.board_outlines || pools.board_arrays, || {
            Ok(Some(&imported.geometry))
        })?;
        let design = Self {
            imported,
            scope,
            stackup,
            copper_boundaries: when(pools.copper_boundaries, || {
                #[cfg(not(target_family = "wasm"))]
                let layers = copper_layers.par_iter();
                #[cfg(target_family = "wasm")]
                let layers = copper_layers.iter();
                Ok(layers.map(|layer| layer.image.prepare_query()).collect())
            })?,
            conductor_boundaries: when(pools.conductor_boundaries, || {
                #[cfg(not(target_family = "wasm"))]
                let layers = copper_layers.par_iter();
                #[cfg(target_family = "wasm")]
                let layers = copper_layers.iter();
                Ok(layers
                    .map(|layer| {
                        layer
                            .conductors
                            .iter()
                            .map(|conductor| conductor.image.prepare_query())
                            .collect()
                    })
                    .collect())
            })?,
            hole_lands: when(pools.hole_lands, || {
                link_lands(
                    holes.iter().map(|hole| hole.id),
                    &land_indices,
                    &physical_holes,
                )
            })?,
            slot_lands: when(pools.slot_lands, || {
                link_lands(
                    slots.iter().map(|slot| slot.id),
                    &land_indices,
                    &physical_holes,
                )
            })?,
            mask_layers: when(pools.masks, || {
                collect_mask_layers(imported, scope, accuracy)
            })?,
            scores: when(pools.scores, || collect_scores(imported, scope, accuracy))?,
            board_outlines: layout
                .as_ref()
                .filter(|_| pools.board_outlines)
                .map(|layout| collect_board_outlines(imported, layout, scope, accuracy))
                .transpose()?
                .unwrap_or_default(),
            board_arrays: layout
                .as_ref()
                .filter(|_| pools.board_arrays)
                .map(|layout| collect_board_arrays(imported, layout, accuracy))
                .transpose()?
                .unwrap_or_default(),
            holes,
            slots,
            copper_layers,
        };
        if pools.resolved_drill_spans {
            validate_drill_spans(&design, rules)?;
        }
        Ok(design)
    }

    pub fn resolve(&self, symbol: Option<Symbol>) -> Option<String> {
        symbol.map(|symbol| self.imported.resolve(symbol).to_owned())
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

fn validate_drill_spans(design: &Design, rules: &[Rule]) -> Result<()> {
    for slot in &design.slots {
        let selected = rules.iter().any(|rule| {
            (matches!(rule.kind, rules::RuleKind::SlotToCopperClearance(plating)
                if super::checks::slot_matches(slot.plating, plating))
                || (rule.kind == rules::RuleKind::PlatedSlotEnclosure
                    && slot.plating == PlatingKind::Plated))
                && rule.conditions.applies_to_design(design)
                && design
                    .copper_layers
                    .iter()
                    .any(|layer| rule.conditions.applies_to_layer(layer))
        });
        if selected
            && (!slot.span_declared || slot.drill_span.interpretation == "assumed_whole_stack")
        {
            bail!(
                "routed slot on layer '{}' has no resolvable drill span; slot copper checks cannot be certified",
                slot.layer.name
            );
        }
    }
    for hole in &design.holes {
        let selected = rules.iter().any(|rule| {
            matches!(
                rule.kind,
                rules::RuleKind::HoleToCopperClearance(class) if class == hole.class
            ) && rule.conditions.applies_to_design(design)
                && design
                    .copper_layers
                    .iter()
                    .enumerate()
                    .any(|(index, layer)| {
                        hole.drill_span.contains_copper(index)
                            && rule.conditions.applies_to_layer(layer)
                    })
        });
        if selected
            && (!hole.span_declared || hole.drill_span.interpretation == "assumed_whole_stack")
        {
            bail!(
                "{} hole on layer '{}' at ({:.6}, {:.6}) has no resolvable drill span; hole-to-copper clearance cannot be certified",
                hole.class.label(),
                hole.layer.name,
                hole.center.x,
                hole.center.y
            );
        }
    }
    Ok(())
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
                self.layer_thicknesses(first.min(last), first.max(last))
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
    pub span_declared: bool,
    /// Inclusive indices in the same order as `Design::copper_layers`.
    /// Through-board or unstated spans cover every copper layer.
    pub drill_span: DrillSpan,
    pub provenance: SourceLocator,
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

/// A routed slot on a drill layer. Its width is settled at extraction: the
/// stated primitive width when the source gives one (exact, verified
/// against the materialized outline), otherwise the outline's narrowest
/// local width.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    pub id: FeatureOccurrenceId,
    pub span_declared: bool,
    pub drill_span: DrillSpan,
    pub plating: PlatingKind,
    pub width: Distance,
    pub width_disk: WidthDisk,
    pub nominal_width_mm: Option<f64>,
    pub outline: ContourSet,
    /// Source contours in world coordinates, retained for display only. The
    /// physical cavity is their independently filled union, like `outline`.
    pub native_outline: Vec<ContourBuf>,
    pub provenance: SourceLocator,
    pub bbox: BBox,
    pub layer: LayerRef,
    pub step: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
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
    pub image: ContourSet,
    pub conductors: Vec<CopperConductor>,
    /// Source lands, including those fully removed from the final copper image.
    /// Hole links still require these for annular-ring subjects and provenance.
    pub lands: Vec<Land>,
}

/// Electrical ownership of one final copper image. Net identity is scoped by
/// its materialized Step occurrence so repeated boards do not accidentally
/// share every same-named net.
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
    pub image: ContourSet,
}

#[derive(Debug, Clone)]
pub(super) struct Score {
    pub start: Point,
    pub end: Point,
    pub layer: LayerRef,
    pub provenance: SourceLocator,
}

#[derive(Debug, Clone)]
pub(super) struct BoardOutline {
    pub name: String,
    pub instance_index: Option<u32>,
    /// Outer profile plus cutout rings.
    pub contours: Vec<Ring>,
    /// Finished board material: the filled outer profile minus every cutout.
    pub region: ContourSet,
    pub boundary: PreparedRegion,
    /// Native outer and cutout contours in the checked frame.
    pub native_outline: Vec<ContourBuf>,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub(super) struct BoardArray {
    pub name: String,
    pub instance_index: u32,
    pub region: ContourSet,
}

fn collect_drilled(
    imported: &ImportedDesign,
    scope: ArtworkScope,
    stackup: Option<&PhysicalStackup>,
    accuracy: GeometryAccuracy,
) -> Result<(Vec<Hole>, Vec<Slot>)> {
    let copper_count = imported
        .layer_definitions
        .iter()
        .filter(|layer| layers::is_copper(layer.layer_function))
        .count();
    let whole_stack = (0, copper_count.max(1) as u16 - 1);
    let mut holes = Vec::new();
    let mut slots = Vec::new();
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
        let mut document = imported
            .materialize_layer(LayerId(layer_index as u32), scope, accuracy)
            .with_context(|| format!("failed to extract drill layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document, accuracy)?;
        for feature in document
            .features
            .iter()
            .filter(|feature| feature.is_drill_like())
        {
            match feature.kind {
                FeatureKind::Hole => {
                    let at = format!(
                        "drilled hole on layer '{layer_name}' at ({:.6}, {:.6})",
                        feature.center.x, feature.center.y
                    );
                    if !(feature.outer_diameter > 0.0 && feature.outer_diameter.is_finite()) {
                        bail!("{at} has no positive finite diameter");
                    }
                    let Some(class) = hole_class(feature.intent.plating) else {
                        bail!("{at} has unknown plating; DFM rules cannot certify it");
                    };
                    holes.push(Hole {
                        id: feature_occurrence_id(feature)
                            .context("materialized hole has no occurrence identity")?,
                        class,
                        center: feature.center,
                        diameter_mm: feature.outer_diameter,
                        bbox: BBox::from_point(feature.center).expand(feature.outer_diameter / 2.0),
                        layer: layer_ref(layer_name, source_layer.layer_function, None),
                        span_declared: source_layer.span.is_some(),
                        drill_span: drill_span(
                            feature.intent.span,
                            &imported.layer_definitions,
                            whole_stack,
                            stackup,
                        ),
                        provenance: feature_provenance(imported, layer_name, feature),
                        step: feature.source_step_ref,
                        padstack: feature.padstack_ref,
                        net: source_net(&document, feature),
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
                }
                FeatureKind::Slot => {
                    let at = format!(
                        "routed slot on layer '{layer_name}' at ({:.6}, {:.6})",
                        feature.bbox.center().x,
                        feature.bbox.center().y
                    );
                    if !matches!(
                        feature.intent.plating,
                        PlatingKind::Plated | PlatingKind::NonPlated
                    ) {
                        bail!("{at} has unknown plating; DFM rules cannot certify it");
                    }
                    let contours = document.placed_feature_contours(feature, accuracy)?;
                    let outline =
                        ContourSet::from_filled_contours(&contours, tol::REGION_MM, accuracy)?;
                    let Some(width_disk) = min_width_disk(&outline, accuracy)? else {
                        bail!("{at} has no measurable outline");
                    };
                    slots.push(Slot {
                        id: feature_occurrence_id(feature)
                            .context("materialized slot has no occurrence identity")?,
                        span_declared: source_layer.span.is_some(),
                        drill_span: drill_span(
                            feature.intent.span,
                            &imported.layer_definitions,
                            whole_stack,
                            stackup,
                        ),
                        plating: feature.intent.plating,
                        width: slot_width(feature.outer_diameter, width_disk.width)
                            .with_context(|| at)?,
                        width_disk,
                        nominal_width_mm: (feature.outer_diameter > 0.0
                            && feature.outer_diameter.is_finite())
                        .then_some(feature.outer_diameter),
                        outline,
                        native_outline: contours,
                        provenance: feature_provenance(imported, layer_name, feature),
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
    Ok((holes, slots))
}

/// A slot's width: the stated primitive width when the source gives one,
/// otherwise the outline's measured minimum width. A stated width is exact,
/// and the outline must agree with it within the measurement's uncertainty;
/// a file that states one width and draws another is inconsistent.
fn slot_width(stated_mm: f64, measured: Distance) -> Result<Distance> {
    if !(stated_mm > 0.0 && stated_mm.is_finite()) {
        return Ok(measured);
    }
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
fn copper_span(span: FeatureSpan<Symbol>, layers: &[ipc2581::types::Layer]) -> Option<(u16, u16)> {
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
    span: FeatureSpan<Symbol>,
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

fn physical_copper_span(
    span: FeatureSpan<Symbol>,
    stackup: &PhysicalStackup,
) -> Option<(u16, u16)> {
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

fn source_net(document: &GeometryDocument, feature: &Feature<Symbol>) -> Option<Symbol> {
    feature.net.or_else(|| {
        feature
            .set
            .and_then(|set| document.feature_sets.get(set as usize))
            .and_then(|set| set.net)
    })
}

fn feature_provenance(
    imported: &ImportedDesign,
    layer: &str,
    feature: &Feature<Symbol>,
) -> SourceLocator {
    let occurrence = feature_occurrence_id(feature)
        .expect("materialized DFM feature must retain its occurrence identity");
    let source = imported
        .feature_definition(occurrence.feature)
        .expect("materialized DFM feature must reference its imported definition")
        .source;
    SourceLocator {
        step: feature
            .source_step_ref
            .map(|step| imported.resolve(step).to_owned()),
        layer: Some(layer.to_owned()),
        set_index: Some(source.set_index),
        feature_index: Some(source.feature_index),
        instance_index: feature.source_instance,
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

struct CopperAttributionLowering;

impl ArtworkLowering<Symbol, Option<ConductorId>> for CopperAttributionLowering {
    fn object_meta(
        &mut self,
        feature: &Feature<Symbol>,
        _kind: ArtworkObjectKind,
    ) -> Option<ConductorId> {
        if let Some(net) = feature.net {
            return Some(ConductorId::Net {
                step: feature.source_step_ref,
                instance: feature.source_instance,
                net,
            });
        }
        if feature.kind == FeatureKind::Padstack {
            return Some(ConductorId::Isolated {
                step: feature.source_step_ref,
                instance: feature.source_instance,
                occurrence: feature_occurrence_id(feature)
                    .expect("materialized copper pad must retain its occurrence identity"),
            });
        }
        if feature.is_fiducial() || feature.flags.copper_balance.is_some() {
            return Some(ConductorId::Auxiliary {
                step: feature.source_step_ref,
                instance: feature.source_instance,
                source_set_index: feature.source.set_index,
            });
        }
        Some(ConductorId::Unattributed {
            step: feature.source_step_ref,
            instance: feature.source_instance,
            source_set_index: feature.source.set_index,
            source_feature_index: feature.source.feature_index,
        })
    }
}

fn compose_attributed_copper(
    document: &mut GeometryDocument,
    accuracy: GeometryAccuracy,
) -> Result<(ContourSet, Vec<CopperConductor>)> {
    let owners = compose_attributed_owners(
        document,
        LayerRole::Copper,
        &mut CopperAttributionLowering,
        accuracy,
    )?;
    let mut composer = pcb_ir::geom::region::PaintComposer::default();
    for (_, image) in &owners {
        composer.push(pcb_ir::geom::Polarity::Dark, image.clone());
    }
    let image = composer.finish(tol::REGION_MM);
    accuracy.check(image.uncertainty_mm)?;
    let conductors = owners
        .into_iter()
        .map(|(id, rings)| CopperConductor { id, image: rings })
        .collect();
    Ok((image, conductors))
}

/// Both copper and soldermask use the canonical ordered paint fold. Source
/// ownership survives clear features and cutouts, rather than being inferred
/// afterward from a feature's bounds or an enclosing board profile.
fn compose_attributed_owners<Owner: Clone + Eq + std::hash::Hash>(
    document: &mut GeometryDocument,
    role: LayerRole,
    lowering: &mut impl ArtworkLowering<Symbol, Option<Owner>>,
    accuracy: GeometryAccuracy,
) -> Result<artwork::OwnerImages<Owner>> {
    pcb_ir::dialects::ipc::process::normalize_for_artwork(document, accuracy)?;
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
    let attributed_artwork = lower_layer_to_artwork_with(document, 0, header, lowering, accuracy)?;
    let (mut layers, _) = artwork::compose_owner_regions(
        &attributed_artwork,
        |owner| Some(owner.clone()),
        tol::REGION_MM,
        accuracy,
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
    imported: &ImportedDesign,
    scope: ArtworkScope,
    require_conductor_ownership: bool,
    stackup: Option<&PhysicalStackup>,
    accuracy: GeometryAccuracy,
) -> Result<Vec<CopperLayer>> {
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
            let mut document = imported
                .materialize_layer(LayerId(layer_index as u32), scope, accuracy).with_context(|| format!("failed to extract IPC-2581 copper layer '{name}'"))?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document, accuracy)?;
            let mut lands = Vec::new();
            for feature in document.features.iter().filter(|feature| {
                feature.kind == FeatureKind::Padstack
                    && feature.polarity == Polarity::Dark
                    && feature.intent.domain == FeatureDomain::Copper
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
                    provenance: feature_provenance(imported, name, feature),
                });
            }
            let (image, mut conductors) = compose_attributed_copper(&mut document, accuracy)?;
            conductors.sort_by_key(|conductor| conductor_order(imported, conductor.id));
            if require_conductor_ownership
                && let Some(conductor) = conductors
                    .iter()
                    .find(|conductor| conductor.id.is_unattributed())
            {
                let id = conductor.id;
                bail!(
                    "IPC-2581 copper layer '{name}' has final functional copper without net attribution in Step '{}'{}; copper clearance cannot be certified",
                    id.step()
                        .map(|step| imported.resolve(step))
                        .unwrap_or("<root>"),
                    id.instance()
                        .map(|instance| format!(", layout instance {instance}"))
                        .unwrap_or_default()
                );
            }
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

struct MaskAttributionLowering;

impl ArtworkLowering<Symbol, Option<(Option<Symbol>, Option<u32>)>> for MaskAttributionLowering {
    fn object_meta(
        &mut self,
        feature: &Feature<Symbol>,
        _kind: ArtworkObjectKind,
    ) -> Option<(Option<Symbol>, Option<u32>)> {
        Some((feature.source_step_ref, feature.source_instance))
    }
}

fn collect_mask_layers(
    imported: &ImportedDesign,
    scope: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<Vec<MaskLayer>> {
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
            let mut document = imported
                .materialize_layer(LayerId(layer_index as u32), scope, accuracy)
                .with_context(|| format!("failed to extract soldermask layer '{name}'"))?;
            let image = document.clone().into_layer_image(
                0,
                LayerRole::Soldermask,
                pcb_ir::dialects::Side::None,
                accuracy,
            )?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(
                &mut document,
                accuracy,
            )?;
            let owners = compose_attributed_owners(
                &mut document,
                LayerRole::Soldermask,
                &mut MaskAttributionLowering,
                accuracy,
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
                        image: rings,
                    })
                    .collect(),
            })
        })
        .collect()
}

/// Join DFM pool indices through the canonical physical relationships. An
/// ambiguous or conflicting relationship fails closed; DFM never chooses the
/// nearest candidate.
fn link_lands(
    holes: impl ExactSizeIterator<Item = FeatureOccurrenceId>,
    land_indices: &HashMap<LandId, HoleLand>,
    physical_holes: &HashMap<FeatureOccurrenceId, PhysicalHole>,
) -> Result<Vec<Vec<HoleLand>>> {
    let mut hole_lands = vec![Vec::new(); holes.len()];
    for (hole_index, hole) in holes.enumerate() {
        let physical_hole = physical_holes
            .get(&hole)
            .context("DFM hole is missing from the canonical physical view")?;
        for relationship in &physical_hole.lands {
            match &relationship.land {
                Association::Resolved(land) => {
                    let link = land_indices
                        .get(land)
                        .context("resolved physical land is missing from the DFM copper pool")?;
                    hole_lands[hole_index].push(*link);
                }
                Association::Unresolved => {}
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
    }
    Ok(hole_lands)
}

fn collect_scores(
    imported: &ImportedDesign,
    scope: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<Vec<Score>> {
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
        let document = imported.materialize_layer(LayerId(layer_index as u32), scope, accuracy)?;
        scores.extend(
            pcb_ir::dialects::ipc::relief::vscore_feature_lines_for(&document)
                .into_iter()
                .map(|(feature_index, line)| Score {
                    start: line.start,
                    end: line.end,
                    layer: layer_ref(imported.resolve(layer.name), layer.layer_function, None),
                    provenance: feature_provenance(
                        imported,
                        imported.resolve(layer.name),
                        &document.features[feature_index],
                    ),
                }),
        );
    }
    Ok(scores)
}

fn collect_board_outlines(
    imported: &ImportedDesign,
    layout: &GeometryDocument,
    scope: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Vec<BoardOutline>> {
    Ok(profile_occurrences_for(layout, scope.profile_set())
        .into_iter()
        .filter(|occurrence| {
            matches!(
                occurrence.role,
                ProfileOccurrenceRole::RootBoard
                    | ProfileOccurrenceRole::BoardDefinition
                    | ProfileOccurrenceRole::BoardInstance
            )
        })
        .map(|occurrence| {
            let mut native_outline = layout.transformed_path_contours(
                occurrence.profile.outer_path,
                occurrence.transform,
                accuracy,
            )?;
            let outer_count = native_outline.len();
            for cutout in occurrence.profile.cutouts.slice(&layout.profile_cutouts) {
                native_outline.extend(layout.transformed_path_contours(
                    cutout.path,
                    occurrence.transform,
                    accuracy,
                )?);
            }
            let outer = ContourSet::from_filled_contours(
                &native_outline[..outer_count],
                tol::REGION_MM,
                accuracy,
            )?;
            let cutouts = ContourSet::from_filled_contours(
                &native_outline[outer_count..],
                tol::REGION_MM,
                accuracy,
            )?;
            let region = outer.difference(&cutouts);
            if region.is_empty() {
                return Ok(None);
            }
            let bbox = region.bbox;
            let name = occurrence
                .step
                .and_then(|step| layout.layout.steps.get(step as usize))
                .map(|step| imported.resolve(step.source_step_ref).to_owned())
                .unwrap_or_else(|| "board".to_owned());
            let boundary = region.prepare_query();
            Ok::<_, anyhow::Error>(Some(BoardOutline {
                name,
                instance_index: occurrence.instance,
                contours: region.rings.clone(),
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

fn collect_board_arrays(
    imported: &ImportedDesign,
    layout: &GeometryDocument,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Vec<BoardArray>> {
    let Some(root_step) = layout.layout.root_step else {
        return Ok(Vec::new());
    };
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
            instance.parent_instance.is_none()
                && layout.layout.repeats[instance.repeat as usize].parent_step == root_step
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
                && !wraps_single_board(*instance_index)
        })
        .map(|(instance_index, instance)| {
            let step = &layout.layout.steps[instance.child_step as usize];
            let contours = step
                .profiles
                .slice(&layout.profiles)
                .iter()
                .map(|profile| {
                    Ok::<_, anyhow::Error>(layout.transformed_path_contours(
                        profile.outer_path,
                        instance.transform,
                        accuracy,
                    )?)
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let region = ContourSet::from_filled_contours(&contours, tol::REGION_MM, accuracy)?;
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

    #[test]
    fn mask_owners_preserve_composed_openings_and_repeat_identity() {
        let accuracy = GeometryAccuracy::default();

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
        let imported = import_design(&ipc, accuracy).unwrap();
        let document = imported
            .materialize_layer(
                imported.layer_id("F.Mask").unwrap(),
                ArtworkScope::ArrayFlattened,
                accuracy,
            )
            .unwrap();
        let previous = document
            .into_layer_image(
                0,
                LayerRole::Soldermask,
                pcb_ir::dialects::Side::None,
                accuracy,
            )
            .unwrap();
        let layer = collect_mask_layers(&imported, ArtworkScope::ArrayFlattened, accuracy)
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
        let accuracy = GeometryAccuracy::default();

        let oval = slot_fixture(
            r#"<Location x="10" y="20"/>
              <Oval width="1.8" height="0.6"/>"#,
        );
        let oval = import_design(&oval, accuracy).unwrap();
        let (_, slots) = collect_drilled(&oval, ArtworkScope::Board, None, accuracy).unwrap();
        assert_eq!(slots.len(), 1);
        assert!((slots[0].width.mm - 0.6).abs() < 1e-9);
        assert_eq!(
            slots[0].width.uncertainty_mm, 0.0,
            "a stated width is exact"
        );
        let native = &slots[0].native_outline;
        assert!(
            native
                .iter()
                .flat_map(|contour| &contour.cmds)
                .any(|command| {
                    matches!(
                        command.op,
                        pcb_ir::geom::path::PathOp::ArcTo | pcb_ir::geom::path::PathOp::CubicTo
                    )
                }),
            "native slot outlines retain source curves"
        );
        assert_eq!(
            native
                .iter()
                .map(|contour| contour.bbox)
                .fold(BBox::empty(), BBox::union),
            slots[0].bbox
        );
        let reconstructed =
            ContourSet::from_filled_contours(native, tol::REGION_MM, accuracy).unwrap();
        assert!(reconstructed.difference(&slots[0].outline).is_empty());
        assert!(slots[0].outline.difference(&reconstructed).is_empty());

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
        let outline = import_design(&outline, accuracy).unwrap();
        let (_, slots) = collect_drilled(&outline, ArtworkScope::Board, None, accuracy).unwrap();
        assert_eq!(slots.len(), 1);
        let width = slots[0].width;
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
                .all(|command| {
                    !matches!(
                        command.op,
                        pcb_ir::geom::path::PathOp::ArcTo | pcb_ir::geom::path::PathOp::CubicTo
                    )
                }),
            "actual source polygons must not be smoothed into curves"
        );
    }

    #[test]
    fn stated_width_must_match_the_outline() {
        let accuracy = GeometryAccuracy::default();

        let ipc = slot_fixture(
            r#"<Location x="10" y="20"/>
              <Oval width="1.8" height="0.6"/>"#,
        );
        let imported = import_design(&ipc, accuracy).unwrap();
        let oval = collect_drilled(&imported, ArtworkScope::Board, None, accuracy)
            .unwrap()
            .1
            .remove(0);
        assert!(slot_width(0.9, oval.width).is_err());
    }
}
