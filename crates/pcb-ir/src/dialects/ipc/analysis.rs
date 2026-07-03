//! Derived views over the IPC layout graph.
//!
//! Nothing in this module is independent IR state: board/panel bounds,
//! profile occurrences, and simple-array descriptions are all computed from
//! the layout graph on demand.

use crate::dialects::ipc::Document;
use crate::dialects::ipc::layout::{
    LayoutInstance, LayoutMargins, LayoutRepeat, LayoutStep, LayoutStepKind, StepProfile,
};
use crate::geom::{Affine2, BBox};

/// Which geometry a layer extraction materializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Canonical board-step geometry only.
    Board,
    /// Root array-step geometry only, with no repeated child board materialization.
    ArrayLocal,
    /// Root array and nested non-board array support geometry, excluding board materialization.
    ArraySupport,
    /// Root array-step geometry plus repeated child board/sub-array geometry in array coordinates.
    ArrayFlattened,
    /// Root-step geometry plus the symbolic layout graph, without repeated feature materialization.
    LayoutSymbolic,
}

impl View {
    pub fn profile_set(self) -> ProfileSet {
        match self {
            Self::Board => ProfileSet::BoardOutlines,
            Self::ArrayLocal => ProfileSet::RootOnly,
            Self::ArraySupport => ProfileSet::RootOnly,
            Self::ArrayFlattened => ProfileSet::FabricationOutlines,
            Self::LayoutSymbolic => ProfileSet::LayoutBoundaries,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSet {
    /// The canonical board step profile only.
    BoardOutlines,
    /// Physical outlines for manufacturing exports.
    ///
    /// For a panel root, this means the root panel profile plus final board
    /// instance profiles. Nested panel boundaries are intentionally excluded.
    FabricationOutlines,
    /// Every placed profile boundary in the layout graph, including nested
    /// panel/subpanel boundaries.
    LayoutBoundaries,
    /// Only the root step profile.
    RootOnly,
}

pub fn board_bbox<Symbol, LayerFunction>(doc: &Document<Symbol, LayerFunction>) -> Option<BBox> {
    layout_steps_by_kind(doc, LayoutStepKind::Board)
        .map(|(_, step)| step.bbox)
        .find(|bbox| !bbox.is_empty())
}

pub fn panel_bbox<Symbol, LayerFunction>(doc: &Document<Symbol, LayerFunction>) -> Option<BBox> {
    root_panel_step(doc)
        .map(|(_, step)| step.bbox)
        .filter(|bbox| !bbox.is_empty())
        .or_else(|| {
            layout_steps_by_kind(doc, LayoutStepKind::Panel)
                .map(|(_, step)| step.bbox)
                .find(|bbox| !bbox.is_empty())
        })
}

pub fn root_step<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Option<(u32, &LayoutStep<Symbol>)> {
    let index = doc.layout.root_step?;
    doc.layout
        .steps
        .get(index as usize)
        .map(|step| (index, step))
}

pub fn root_panel_step<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Option<(u32, &LayoutStep<Symbol>)> {
    root_step(doc).filter(|(_, step)| step.kind == LayoutStepKind::Panel)
}

pub fn layout_steps_by_kind<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    kind: LayoutStepKind,
) -> impl Iterator<Item = (u32, &LayoutStep<Symbol>)> {
    doc.layout
        .steps
        .iter()
        .enumerate()
        .filter_map(move |(index, step)| (step.kind == kind).then_some((index as u32, step)))
}

pub fn layout_instances_by_kind<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    kind: LayoutStepKind,
) -> impl Iterator<Item = (u32, &LayoutInstance<Symbol>)> {
    doc.layout
        .instances
        .iter()
        .enumerate()
        .filter_map(move |(index, instance)| {
            let step = doc.layout.steps.get(instance.child_step as usize)?;
            (step.kind == kind).then_some((index as u32, instance))
        })
}

pub fn layout_child_repeats<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    parent_step: u32,
    parent_instance: Option<u32>,
) -> impl Iterator<Item = (u32, &LayoutRepeat<Symbol>)> {
    doc.layout
        .repeats
        .iter()
        .enumerate()
        .filter_map(move |(index, repeat)| {
            (repeat.parent_step == parent_step && repeat.parent_instance == parent_instance)
                .then_some((index as u32, repeat))
        })
}

pub fn layout_repeat_instances<'a, Symbol, LayerFunction>(
    doc: &'a Document<Symbol, LayerFunction>,
    repeat: &LayoutRepeat<Symbol>,
) -> impl Iterator<Item = (u32, &'a LayoutInstance<Symbol>)> {
    repeat.instances.indices().filter_map(move |index| {
        doc.layout
            .instances
            .get(index as usize)
            .map(|instance| (index, instance))
    })
}

pub fn board_step_count<Symbol, LayerFunction>(doc: &Document<Symbol, LayerFunction>) -> usize {
    layout_steps_by_kind(doc, LayoutStepKind::Board).count()
}

pub fn board_instance_count<Symbol, LayerFunction>(doc: &Document<Symbol, LayerFunction>) -> usize {
    layout_instances_by_kind(doc, LayoutStepKind::Board).count()
}

pub fn panel_step_count<Symbol, LayerFunction>(doc: &Document<Symbol, LayerFunction>) -> usize {
    layout_steps_by_kind(doc, LayoutStepKind::Panel).count()
}

/// Derived description of a simple rectangular board array.
///
/// This is not independent IR state. It is a convenience view over the IPC
/// layout graph for the common `array -> board_cell -> board` or
/// `array -> board` shapes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimpleBoardArrayLayout {
    pub array_step: u32,
    pub array_repeat: u32,
    pub board_cell_step: Option<u32>,
    pub board_cell_repeat: Option<u32>,
    pub board_step: u32,
    pub columns: u32,
    pub rows: u32,
    pub array_bbox: BBox,
    pub repeated_bbox: BBox,
    pub board_bbox: BBox,
    pub board_width: f64,
    pub board_height: f64,
    pub pitch_x: Option<f64>,
    pub pitch_y: Option<f64>,
    pub board_margin: Option<LayoutMargins>,
    pub edge_rail_width: Option<f64>,
    pub edge_rail: LayoutMargins,
    pub margins: LayoutMargins,
}

const SIMPLE_BOARD_ARRAY_EPSILON: f64 = 1e-6;

pub fn simple_board_array_layout<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Option<SimpleBoardArrayLayout> {
    let (array_step_index, array_step) = root_panel_step(doc)?;
    let array_bbox = array_step.bbox;
    if array_bbox.is_empty() || array_bbox.width() <= 0.0 || array_bbox.height() <= 0.0 {
        return None;
    }

    let mut root_repeats = layout_child_repeats(doc, array_step_index, None);
    let (array_repeat_index, array_repeat) = root_repeats.next()?;
    if root_repeats.next().is_some()
        || array_repeat.nx == 0
        || array_repeat.ny == 0
        || !simple_array_nearly_zero(array_repeat.angle)
        || array_repeat.mirror
    {
        return None;
    }

    let child_step = doc.layout.steps.get(array_repeat.child_step as usize)?;
    match child_step.kind {
        LayoutStepKind::Board => {
            simple_direct_board_array(doc, array_step_index, array_repeat_index, array_repeat)
        }
        LayoutStepKind::Panel => {
            simple_board_cell_array(doc, array_step_index, array_repeat_index, array_repeat)
        }
        _ => None,
    }
}

fn simple_direct_board_array<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    array_step_index: u32,
    array_repeat_index: u32,
    repeat: &LayoutRepeat<Symbol>,
) -> Option<SimpleBoardArrayLayout> {
    let array_step = doc.layout.steps.get(array_step_index as usize)?;
    let board_step = doc.layout.steps.get(repeat.child_step as usize)?;
    if board_step.kind != LayoutStepKind::Board {
        return None;
    }
    let (board_width, board_height) = simple_step_dimensions(board_step)?;
    let instance_count = layout_repeat_instances(doc, repeat).count() as u32;
    if instance_count != repeat.nx.saturating_mul(repeat.ny) || repeat.bbox.is_empty() {
        return None;
    }

    let pitch_x = (repeat.nx > 1)
        .then_some(repeat.dx)
        .filter(|pitch| simple_valid_pitch(*pitch, board_width));
    let pitch_y = (repeat.ny > 1)
        .then_some(repeat.dy)
        .filter(|pitch| simple_valid_pitch(*pitch, board_height));
    if (repeat.nx > 1 && pitch_x.is_none()) || (repeat.ny > 1 && pitch_y.is_none()) {
        return None;
    }

    let margins = simple_margins_between(repeat.bbox, array_step.bbox)?;
    let horizontal_gap = pitch_x.map(|pitch| simple_clamp_zero(pitch - board_width));
    let vertical_gap = pitch_y.map(|pitch| simple_clamp_zero(pitch - board_height));
    let edge_rail_width = simple_edge_rail_width(margins, horizontal_gap, vertical_gap);
    let board_margin =
        edge_rail_width.and_then(|edge| simple_board_margin_from_margins(margins, edge));
    let edge_rail = edge_rail_width.map(simple_margins_all).unwrap_or(margins);

    Some(SimpleBoardArrayLayout {
        array_step: array_step_index,
        array_repeat: array_repeat_index,
        board_cell_step: None,
        board_cell_repeat: None,
        board_step: repeat.child_step,
        columns: repeat.nx,
        rows: repeat.ny,
        array_bbox: array_step.bbox,
        repeated_bbox: repeat.bbox,
        board_bbox: board_step.bbox,
        board_width,
        board_height,
        pitch_x,
        pitch_y,
        board_margin,
        edge_rail_width,
        edge_rail,
        margins,
    })
}

fn simple_board_cell_array<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    array_step_index: u32,
    array_repeat_index: u32,
    repeat: &LayoutRepeat<Symbol>,
) -> Option<SimpleBoardArrayLayout> {
    let array_step = doc.layout.steps.get(array_step_index as usize)?;
    let board_cell_step = doc.layout.steps.get(repeat.child_step as usize)?;
    let (cell_width, cell_height) = simple_step_dimensions(board_cell_step)?;
    let pitch_x = (repeat.nx > 1)
        .then_some(repeat.dx)
        .filter(|pitch| simple_valid_pitch(*pitch, cell_width));
    let pitch_y = (repeat.ny > 1)
        .then_some(repeat.dy)
        .filter(|pitch| simple_valid_pitch(*pitch, cell_height));
    if (repeat.nx > 1 && pitch_x.is_none()) || (repeat.ny > 1 && pitch_y.is_none()) {
        return None;
    }

    let (first_cell_instance, _) = layout_repeat_instances(doc, repeat).next()?;
    let mut board_repeats = layout_child_repeats(doc, repeat.child_step, Some(first_cell_instance));
    let (board_repeat_index, board_repeat) = board_repeats.next()?;
    if board_repeats.next().is_some()
        || board_repeat.nx != 1
        || board_repeat.ny != 1
        || !simple_array_nearly_zero(board_repeat.dx)
        || !simple_array_nearly_zero(board_repeat.dy)
        || !simple_array_nearly_zero(board_repeat.angle)
        || board_repeat.mirror
    {
        return None;
    }

    let board_step = doc.layout.steps.get(board_repeat.child_step as usize)?;
    if board_step.kind != LayoutStepKind::Board {
        return None;
    }
    let (board_width, board_height) = simple_step_dimensions(board_step)?;
    if cell_width + SIMPLE_BOARD_ARRAY_EPSILON < board_width
        || cell_height + SIMPLE_BOARD_ARRAY_EPSILON < board_height
    {
        return None;
    }

    let board_left = board_repeat.x + board_step.bbox.min.x - board_cell_step.bbox.min.x;
    let board_bottom = board_repeat.y + board_step.bbox.min.y - board_cell_step.bbox.min.y;
    let board_margin = simple_board_margin_from_cell(
        board_left,
        board_bottom,
        board_width,
        board_height,
        cell_width,
        cell_height,
    )?;

    let edge_margins = simple_margins_between(repeat.bbox, array_step.bbox)?;
    let edge_rail_width = simple_average_if_consistent(vec![
        edge_margins.left,
        edge_margins.right,
        edge_margins.bottom,
        edge_margins.top,
    ]);

    Some(SimpleBoardArrayLayout {
        array_step: array_step_index,
        array_repeat: array_repeat_index,
        board_cell_step: Some(repeat.child_step),
        board_cell_repeat: Some(board_repeat_index),
        board_step: board_repeat.child_step,
        columns: repeat.nx,
        rows: repeat.ny,
        array_bbox: array_step.bbox,
        repeated_bbox: repeat.bbox,
        board_bbox: board_step.bbox,
        board_width,
        board_height,
        pitch_x,
        pitch_y,
        board_margin: Some(board_margin),
        edge_rail_width,
        edge_rail: edge_margins,
        margins: simple_margins_between(repeat.bbox, array_step.bbox)?,
    })
}

fn simple_margins_all(value: f64) -> LayoutMargins {
    LayoutMargins {
        top: value,
        right: value,
        bottom: value,
        left: value,
    }
}

fn simple_step_dimensions<Symbol>(step: &LayoutStep<Symbol>) -> Option<(f64, f64)> {
    (!step.bbox.is_empty() && step.bbox.width() > 0.0 && step.bbox.height() > 0.0)
        .then_some((step.bbox.width(), step.bbox.height()))
}

fn simple_valid_pitch(pitch: f64, span: f64) -> bool {
    pitch.is_finite() && pitch + SIMPLE_BOARD_ARRAY_EPSILON >= span && pitch > 0.0
}

fn simple_board_margin_from_cell(
    board_left: f64,
    board_bottom: f64,
    board_width: f64,
    board_height: f64,
    cell_width: f64,
    cell_height: f64,
) -> Option<LayoutMargins> {
    let left = board_left;
    let bottom = board_bottom;
    let right = cell_width - board_left - board_width;
    let top = cell_height - board_bottom - board_height;
    if [left, right, bottom, top]
        .iter()
        .any(|value| !value.is_finite() || *value < -SIMPLE_BOARD_ARRAY_EPSILON)
    {
        return None;
    }

    Some(LayoutMargins {
        top: simple_clamp_zero(top),
        right: simple_clamp_zero(right),
        bottom: simple_clamp_zero(bottom),
        left: simple_clamp_zero(left),
    })
}

fn simple_margins_between(inner: BBox, outer: BBox) -> Option<LayoutMargins> {
    let left = simple_clamp_zero(inner.min.x - outer.min.x);
    let right = simple_clamp_zero(outer.max.x - inner.max.x);
    let bottom = simple_clamp_zero(inner.min.y - outer.min.y);
    let top = simple_clamp_zero(outer.max.y - inner.max.y);

    if [left, right, bottom, top]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
    {
        Some(LayoutMargins {
            top,
            right,
            bottom,
            left,
        })
    } else {
        None
    }
}

fn simple_edge_rail_width(
    margins: LayoutMargins,
    horizontal_gap: Option<f64>,
    vertical_gap: Option<f64>,
) -> Option<f64> {
    let mut candidates = Vec::new();
    if let Some(gap) = horizontal_gap {
        candidates.push((margins.left + margins.right - gap) / 2.0);
    }
    if let Some(gap) = vertical_gap {
        candidates.push((margins.bottom + margins.top - gap) / 2.0);
    }

    simple_average_if_consistent(candidates)
}

fn simple_board_margin_from_margins(
    margins: LayoutMargins,
    edge_rail_width: f64,
) -> Option<LayoutMargins> {
    let left = margins.left - edge_rail_width;
    let right = margins.right - edge_rail_width;
    let bottom = margins.bottom - edge_rail_width;
    let top = margins.top - edge_rail_width;
    if [left, right, bottom, top]
        .iter()
        .any(|value| !value.is_finite() || *value < -SIMPLE_BOARD_ARRAY_EPSILON)
    {
        return None;
    }

    Some(LayoutMargins {
        top: simple_clamp_zero(top),
        right: simple_clamp_zero(right),
        bottom: simple_clamp_zero(bottom),
        left: simple_clamp_zero(left),
    })
}

fn simple_average_if_consistent(candidates: Vec<f64>) -> Option<f64> {
    if candidates.is_empty()
        || candidates
            .iter()
            .any(|candidate| !candidate.is_finite() || *candidate < -SIMPLE_BOARD_ARRAY_EPSILON)
    {
        return None;
    }

    let average = candidates.iter().sum::<f64>() / candidates.len() as f64;
    candidates
        .iter()
        .all(|candidate| simple_array_nearly_equal(*candidate, average))
        .then_some(simple_clamp_zero(average))
}

fn simple_array_nearly_zero(value: f64) -> bool {
    value.abs() <= SIMPLE_BOARD_ARRAY_EPSILON
}

fn simple_array_nearly_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= SIMPLE_BOARD_ARRAY_EPSILON
}

fn simple_clamp_zero(value: f64) -> f64 {
    if simple_array_nearly_zero(value) {
        0.0
    } else {
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileOccurrenceRole {
    Unplaced,
    RootBoard,
    RootPanel,
    RootStep,
    BoardDefinition,
    BoardInstance,
    PanelInstance,
    StepInstance,
}

#[derive(Debug, Clone, Copy)]
pub struct ProfileOccurrence<'a> {
    pub profile_index: u32,
    pub profile: &'a StepProfile,
    pub step: Option<u32>,
    pub instance: Option<u32>,
    pub transform: Affine2,
    pub role: ProfileOccurrenceRole,
    pub depth: u32,
}

pub fn profile_occurrences_for<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    profile_set: ProfileSet,
) -> Vec<ProfileOccurrence<'_>> {
    if profile_set == ProfileSet::BoardOutlines {
        return board_profile_occurrences(doc);
    }

    let Some((root_index, root)) = root_step(doc) else {
        return doc
            .profiles
            .iter()
            .enumerate()
            .map(|(profile_index, profile)| ProfileOccurrence {
                profile_index: profile_index as u32,
                profile,
                step: None,
                instance: None,
                transform: Affine2::IDENTITY,
                role: ProfileOccurrenceRole::Unplaced,
                depth: 0,
            })
            .collect();
    };

    let mut occurrences = Vec::new();
    push_profile_occurrences(
        &mut occurrences,
        doc,
        ProfileOccurrenceSpec {
            profiles: root.profiles,
            step: Some(root_index),
            instance: None,
            transform: Affine2::IDENTITY,
            role: root_profile_role(root.kind),
            depth: 0,
        },
    );

    if profile_set == ProfileSet::RootOnly {
        return occurrences;
    }

    for (instance_index, instance) in doc.layout.instances.iter().enumerate() {
        let Some(step) = doc.layout.steps.get(instance.child_step as usize) else {
            continue;
        };
        if !include_instance_profiles(profile_set, root.kind, step.kind) {
            continue;
        }

        push_profile_occurrences(
            &mut occurrences,
            doc,
            ProfileOccurrenceSpec {
                profiles: step.profiles,
                step: Some(instance.child_step),
                instance: Some(instance_index as u32),
                transform: instance.transform,
                role: instance_profile_role(step.kind),
                depth: instance_depth(doc, instance_index as u32),
            },
        );
    }
    occurrences
}

#[derive(Debug, Clone, Copy)]
struct ProfileOccurrenceSpec {
    profiles: crate::geom::Span,
    step: Option<u32>,
    instance: Option<u32>,
    transform: Affine2,
    role: ProfileOccurrenceRole,
    depth: u32,
}

fn push_profile_occurrences<'a, Symbol, LayerFunction>(
    occurrences: &mut Vec<ProfileOccurrence<'a>>,
    doc: &'a Document<Symbol, LayerFunction>,
    spec: ProfileOccurrenceSpec,
) {
    for profile_index in spec.profiles.indices() {
        let Some(profile) = doc.profiles.get(profile_index as usize) else {
            continue;
        };
        occurrences.push(ProfileOccurrence {
            profile_index,
            profile,
            step: spec.step,
            instance: spec.instance,
            transform: spec.transform,
            role: spec.role,
            depth: spec.depth,
        });
    }
}

fn include_instance_profiles(
    profile_set: ProfileSet,
    root_kind: LayoutStepKind,
    child_kind: LayoutStepKind,
) -> bool {
    match profile_set {
        ProfileSet::FabricationOutlines => {
            root_kind == LayoutStepKind::Panel && child_kind == LayoutStepKind::Board
        }
        ProfileSet::LayoutBoundaries => true,
        ProfileSet::BoardOutlines | ProfileSet::RootOnly => false,
    }
}

fn board_profile_occurrences<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Vec<ProfileOccurrence<'_>> {
    let Some((step_index, step)) = layout_steps_by_kind(doc, LayoutStepKind::Board).next() else {
        return Vec::new();
    };
    let role = if root_step(doc).is_some_and(|(root_index, _)| root_index == step_index) {
        ProfileOccurrenceRole::RootBoard
    } else {
        ProfileOccurrenceRole::BoardDefinition
    };
    let mut occurrences = Vec::new();
    push_profile_occurrences(
        &mut occurrences,
        doc,
        ProfileOccurrenceSpec {
            profiles: step.profiles,
            step: Some(step_index),
            instance: None,
            transform: Affine2::IDENTITY,
            role,
            depth: 0,
        },
    );
    occurrences
}

fn root_profile_role(kind: LayoutStepKind) -> ProfileOccurrenceRole {
    match kind {
        LayoutStepKind::Board => ProfileOccurrenceRole::RootBoard,
        LayoutStepKind::Panel => ProfileOccurrenceRole::RootPanel,
        _ => ProfileOccurrenceRole::RootStep,
    }
}

fn instance_profile_role(kind: LayoutStepKind) -> ProfileOccurrenceRole {
    match kind {
        LayoutStepKind::Board => ProfileOccurrenceRole::BoardInstance,
        LayoutStepKind::Panel => ProfileOccurrenceRole::PanelInstance,
        _ => ProfileOccurrenceRole::StepInstance,
    }
}

pub(crate) fn instance_depth<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    instance_index: u32,
) -> u32 {
    let mut depth = 1;
    let mut remaining = doc.layout.instances.len();
    let mut parent = doc
        .layout
        .instances
        .get(instance_index as usize)
        .and_then(|instance| instance.parent_instance);

    while let Some(parent_index) = parent {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        depth += 1;
        parent = doc
            .layout
            .instances
            .get(parent_index as usize)
            .and_then(|instance| instance.parent_instance);
    }
    depth
}
