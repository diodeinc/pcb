//! V-score relief geometry.
//!
//! A V-score can separate the portions of a board boundary that lie directly on
//! a score line. Boundary material that does not overlap a score line must be
//! routed out. We model that routed material as regularized planar set
//! operations over `ContourSet`.
//!
//! For each board instance:
//!
//! - `B` is the protected finished-board material for the whole array.
//! - `C_i` is the rectangular score cell bounded by the nearest V-score lines.
//! - `B'_i` is the instance board after tolerance-scale score alignment.
//! - `P_i = C_i \ B'_i` is sacrificial dead-space material in that cell.
//! - `D_r` is a disk whose radius is the route-tool radius.
//!
//! Tool awareness is the usual configuration-space construction for a
//! translating disk tool. Tool centers are legal only where the swept disk will
//! not touch protected finished-board material:
//!
//! ```text
//! T_i = (P_i ⊕ D_r) \ (B ⊕ D_r)
//! W_i = (T_i ⊕ D_r) \ B
//! R_i = P_i ∪ W_i
//! ```
//!
//! `T_i` is the legal tool-center region, `W_i` is the swept tool area, and
//! `R_i` is the material-removal region emitted as closed profile contours.

use std::fmt;

use crate::dialects::ipc::{Document, SpecItemKind};
use crate::geom::path::{ContourBuf, PathCmd, PathOp};
use crate::geom::region::{Ring, rings_from_contours, simplify_rings};
use crate::geom::{BBox, ContourSet, FillRule, PaintKind, Point};

pub const DEFAULT_ROUTE_TOOL_DIAMETER_MM: f64 = 1.0;
pub const DEFAULT_RELIEF_TOLERANCE_MM: f64 = 0.01;
pub const DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM: f64 = 0.10;

#[derive(Debug, Clone)]
pub struct VScoreReliefInput {
    pub board_boundaries: Vec<ContourBuf>,
    pub board_cutouts: Vec<ContourBuf>,
    pub score_blockers: Vec<ContourBuf>,
    pub score_lines: Vec<VScoreLine>,
    pub tool_diameter_mm: f64,
    pub tolerance_mm: f64,
}

impl VScoreReliefInput {
    pub fn new(board_boundaries: Vec<ContourBuf>, score_lines: Vec<VScoreLine>) -> Self {
        Self {
            board_boundaries,
            board_cutouts: Vec::new(),
            score_blockers: Vec::new(),
            score_lines,
            tool_diameter_mm: DEFAULT_ROUTE_TOOL_DIAMETER_MM,
            tolerance_mm: DEFAULT_RELIEF_TOLERANCE_MM,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VScoreLine {
    pub start: Point,
    pub end: Point,
    pub width: f64,
}

#[derive(Debug, Clone, Default)]
pub struct VScoreReliefOutput {
    pub relief_contours: Vec<ContourBuf>,
    pub debug: VScoreReliefDebug,
}

#[derive(Debug, Clone, Default)]
pub struct VScoreReliefDebug {
    pub entries: Vec<VScoreReliefDebugEntry>,
    pub merged_relief_contours: Vec<ContourBuf>,
}

#[derive(Debug, Clone)]
pub struct VScoreReliefDebugEntry {
    pub board_boundary: Vec<ContourBuf>,
    pub score_cell: ContourBuf,
    pub dead_space_pockets: Vec<ContourBuf>,
    pub legal_tool_centers: Vec<ContourBuf>,
    pub relief_contours: Vec<ContourBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VScoreReliefError {
    EmptyScoreLines,
    InvalidToolDiameter(f64),
    InvalidTolerance(f64),
    EmptyBoundary,
    InvalidBoundary(&'static str),
}

impl fmt::Display for VScoreReliefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScoreLines => write!(f, "V-score relief score lines are empty"),
            Self::InvalidToolDiameter(value) => {
                write!(
                    f,
                    "V-score relief tool diameter must be positive; got {value}"
                )
            }
            Self::InvalidTolerance(value) => {
                write!(f, "V-score relief tolerance must be positive; got {value}")
            }
            Self::EmptyBoundary => write!(f, "V-score relief boundary is empty"),
            Self::InvalidBoundary(message) => {
                write!(f, "V-score relief boundary is invalid: {message}")
            }
        }
    }
}

impl std::error::Error for VScoreReliefError {}

pub fn vscore_route_reliefs(
    input: &VScoreReliefInput,
) -> Result<Vec<ContourBuf>, VScoreReliefError> {
    Ok(vscore_route_reliefs_inner(input, false)?.relief_contours)
}

pub fn vscore_route_reliefs_with_debug(
    input: &VScoreReliefInput,
) -> Result<VScoreReliefOutput, VScoreReliefError> {
    vscore_route_reliefs_inner(input, true)
}

fn vscore_route_reliefs_inner(
    input: &VScoreReliefInput,
    include_debug: bool,
) -> Result<VScoreReliefOutput, VScoreReliefError> {
    if input.score_lines.is_empty() {
        return Err(VScoreReliefError::EmptyScoreLines);
    }
    if !input.tool_diameter_mm.is_finite() || input.tool_diameter_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidToolDiameter(
            input.tool_diameter_mm,
        ));
    }
    if !input.tolerance_mm.is_finite() || input.tolerance_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidTolerance(input.tolerance_mm));
    }
    if input.board_boundaries.is_empty() {
        return Err(VScoreReliefError::EmptyBoundary);
    }

    let protected_material = protected_board_material(input)?;
    let mut debug = VScoreReliefDebug::default();
    let mut relief_contours = Vec::new();
    for boundary in &input.board_boundaries {
        let Some(boundary_relief) = boundary_pocket_relief(boundary, &protected_material, input)?
        else {
            continue;
        };
        if include_debug {
            debug.entries.push(boundary_relief.debug_entry(boundary));
        }
        relief_contours.extend(boundary_relief.geometry.material_removal.rings);
    }
    let relief_region = ContourSet::new(relief_contours, FillRule::NonZero, input.tolerance_mm);
    let relief_payloads = relief_region.to_contours_with_arcs();
    if include_debug {
        debug.merged_relief_contours = relief_payloads.clone();
    }

    Ok(VScoreReliefOutput {
        relief_contours: relief_payloads,
        debug,
    })
}

pub fn vscore_lines_for<Symbol: PartialEq, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
) -> Vec<VScoreLine> {
    let mut lines = Vec::new();
    for feature in doc
        .features
        .iter()
        .filter(|feature| feature.is_vcut() && feature_has_vcut_spec(doc, feature))
    {
        for path in feature.paths.slice(&doc.arena.paths) {
            if path.paint.kind() != PaintKind::Stroke {
                continue;
            }
            let line_start = lines.len();
            for contour in doc.arena.contours(path.contours) {
                append_contour_line_segments(doc.arena.cmds(*contour), &mut lines);
            }
            if feature.stroke_width > 0.0 {
                for line in &mut lines[line_start..] {
                    line.width = feature.stroke_width;
                }
            }
        }
    }
    lines
}

fn feature_has_vcut_spec<Symbol: PartialEq, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    feature: &crate::dialects::ipc::Feature<Symbol>,
) -> bool {
    let Some(set_index) = feature.set else {
        return false;
    };
    let Some(set) = doc.feature_sets.get(set_index as usize) else {
        return false;
    };
    spec_refs_include_vcut(doc, set.spec_refs)
        || doc
            .layers
            .get(set.layer as usize)
            .is_some_and(|layer| spec_refs_include_vcut(doc, layer.spec_refs))
}

fn spec_refs_include_vcut<Symbol: PartialEq, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    spec_refs: crate::geom::Span,
) -> bool {
    spec_refs
        .slice(&doc.spec_refs)
        .iter()
        .any(|spec_ref| spec_is_vcut(doc, &spec_ref.spec))
}

fn spec_is_vcut<Symbol: PartialEq, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    spec_name: &Symbol,
) -> bool {
    doc.specs
        .iter()
        .find(|spec| &spec.name == spec_name)
        .is_some_and(|spec| {
            spec.items
                .slice(&doc.spec_items)
                .iter()
                .any(|item| item.kind == SpecItemKind::VCut)
        })
}

fn append_contour_line_segments(cmds: &[PathCmd], lines: &mut Vec<VScoreLine>) {
    let mut first = None;
    let mut current = None;
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                first = Some(cmd.p0);
                current = Some(cmd.p0);
            }
            PathOp::LineTo => {
                if let Some(start) = current {
                    lines.push(VScoreLine {
                        start,
                        end: cmd.p0,
                        width: 0.0,
                    });
                }
                current = Some(cmd.p0);
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (first, current)
                    && start.distance_to(end) > 0.0
                {
                    lines.push(VScoreLine {
                        start: end,
                        end: start,
                        width: 0.0,
                    });
                }
                current = first;
            }
            PathOp::ArcTo | PathOp::CubicTo => current = cmd.end_point(),
        }
    }
}

fn boundary_pocket_relief(
    boundary: &ContourBuf,
    base_protected_material: &ContourSet,
    input: &VScoreReliefInput,
) -> Result<Option<BoundaryRelief>, VScoreReliefError> {
    if boundary.bbox.is_empty()
        || boundary.bbox.width() <= input.tolerance_mm
        || boundary.bbox.height() <= input.tolerance_mm
    {
        return Err(VScoreReliefError::InvalidBoundary(
            "boundary has empty bounds",
        ));
    }

    let score_tolerance = input.tolerance_mm.max(DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM);
    let Some(score_cell) =
        score_cell_for_boundary(boundary.bbox, &input.score_lines, score_tolerance)?
    else {
        return Ok(None);
    };
    let score_cell_path = rectangle_payload(score_cell);
    let score_blockers = score_blockers_for_cell(
        &input.score_blockers,
        score_cell,
        score_tolerance,
        input.tolerance_mm,
    );
    let protected_material = if score_blockers.is_empty() {
        base_protected_material.clone()
    } else {
        base_protected_material.difference(&score_blockers)
    };

    let geometry = compute_relief_geometry(
        boundary,
        &protected_material,
        &score_blockers,
        score_cell,
        input.tool_diameter_mm / 2.0,
        score_tolerance,
        input.tolerance_mm,
    );
    let has_dead_space = !geometry.dead_space.is_empty();

    if has_dead_space && geometry.legal_tool_centers.is_empty() {
        return Err(VScoreReliefError::InvalidBoundary(
            "V-score relief pocket is too small for the route tool",
        ));
    }

    Ok(Some(BoundaryRelief {
        score_cell: score_cell_path,
        geometry,
    }))
}

#[derive(Debug, Clone)]
struct BoundaryRelief {
    score_cell: ContourBuf,
    geometry: ReliefGeometry,
}

impl BoundaryRelief {
    fn debug_entry(&self, boundary: &ContourBuf) -> VScoreReliefDebugEntry {
        VScoreReliefDebugEntry {
            board_boundary: vec![boundary.clone()],
            score_cell: self.score_cell.clone(),
            dead_space_pockets: self.geometry.dead_space.to_contours(),
            legal_tool_centers: self.geometry.legal_tool_centers.to_contours(),
            relief_contours: self.geometry.material_removal.to_contours(),
        }
    }
}

#[derive(Debug, Clone)]
struct ReliefGeometry {
    dead_space: ContourSet,
    legal_tool_centers: ContourSet,
    material_removal: ContourSet,
}

/// Compute `P_i`, `T_i`, and `R_i` for one board instance.
///
/// The returned `material_removal` is a filled region. Callers emit its
/// boundary (`∂R_i`) as closed profile contours.
fn compute_relief_geometry(
    boundary: &ContourBuf,
    protected_material: &ContourSet,
    score_blockers: &ContourSet,
    score_cell: BBox,
    tool_radius: f64,
    score_tolerance: f64,
    area_tolerance: f64,
) -> ReliefGeometry {
    // C_i: the V-score cell around this board instance.
    let score_cell_region = ContourSet::rectangle(score_cell, area_tolerance);
    // B_i: this board instance before score-alignment tolerance is applied.
    let current_board = ContourSet::from_contours(
        std::slice::from_ref(boundary),
        FillRule::NonZero,
        area_tolerance,
    );
    // B'_i: absorb tolerance-scale slivers along score-cell edges so tiny
    // source/score mismatches do not become false relief pockets.
    let aligned_board = score_aligned_board_region(current_board, score_cell, score_tolerance)
        .difference(score_blockers);
    // P_i = C_i \ B'_i.
    let dead_space = score_cell_region.difference(&aligned_board);
    let (legal_tool_centers, material_removal) =
        tool_aware_material_removal(&dead_space, protected_material, tool_radius);

    ReliefGeometry {
        dead_space,
        legal_tool_centers,
        material_removal,
    }
}

/// Compute the tool-center free region and resulting material-removal region.
///
/// This is the configuration-space obstacle form of a circular router bit:
/// inflate protected material by the tool radius, subtract that forbidden
/// center region from the sacrificial center window, then inflate the legal
/// centers back into the physical tool sweep.
fn tool_aware_material_removal(
    dead_space: &ContourSet,
    protected_material: &ContourSet,
    tool_radius: f64,
) -> (ContourSet, ContourSet) {
    // T_i = (P_i ⊕ D_r) \ (B ⊕ D_r).
    let sacrificial_center_window = dead_space.disk_dilate(tool_radius);
    let protected_clearance = protected_material.disk_dilate(tool_radius);
    let legal_tool_centers = sacrificial_center_window.difference(&protected_clearance);

    // W_i = (T_i ⊕ D_r) \ B.
    let tool_sweep = legal_tool_centers
        .clone()
        .disk_dilate(tool_radius)
        .difference(protected_material);

    // R_i = P_i ∪ W_i.
    let material_removal = dead_space.union(&tool_sweep);
    (legal_tool_centers, material_removal)
}

/// Finished-board material that the route tool must not touch.
fn protected_board_material(input: &VScoreReliefInput) -> Result<ContourSet, VScoreReliefError> {
    let board_contours = finished_board_contours(&input.board_boundaries)?;
    let mut board_region = ContourSet::new(board_contours, FillRule::NonZero, input.tolerance_mm);
    if !input.board_cutouts.is_empty() {
        let cutout_region =
            ContourSet::from_filled_contours(&input.board_cutouts, input.tolerance_mm);
        board_region = board_region.difference(&cutout_region);
    }
    Ok(board_region)
}

fn score_blockers_for_cell(
    score_blockers: &[ContourBuf],
    score_cell: BBox,
    score_tolerance: f64,
    area_tolerance: f64,
) -> ContourSet {
    if score_blockers.is_empty() {
        return ContourSet::empty(area_tolerance);
    }
    let score_strip = score_cell_strip_region(score_cell, score_tolerance, area_tolerance);
    let selected = score_blockers
        .iter()
        .filter(|payload| payload.bbox.intersects(score_strip.bbox))
        .filter(|payload| {
            !ContourSet::from_filled_contours(std::slice::from_ref(payload), area_tolerance)
                .intersection(&score_strip)
                .is_empty()
        })
        .cloned()
        .collect::<Vec<_>>();
    ContourSet::from_filled_contours(&selected, area_tolerance)
}

/// Snap tolerance-scale boundary slivers onto score-cell edges.
///
/// This regularizes tiny source/export mismatches, such as an edge that should
/// lie on a V-score but is offset by numeric tolerance. Real recesses remain
/// outside the aligned board and become dead-space pockets.
fn score_aligned_board_region(
    board: ContourSet,
    score_cell: BBox,
    score_tolerance: f64,
) -> ContourSet {
    let cell_strip = score_cell_strip_region(score_cell, score_tolerance, board.tolerance);
    if cell_strip.is_empty() {
        return board;
    }
    let dilated_board = board.disk_dilate(score_tolerance);
    let score_slivers = cell_strip.intersection(&dilated_board);
    board.union(&score_slivers)
}

fn score_cell_strip_region(score_cell: BBox, width: f64, tolerance: f64) -> ContourSet {
    let cell = ContourSet::rectangle(score_cell, tolerance);
    if score_cell.width() <= 2.0 * width || score_cell.height() <= 2.0 * width {
        return cell;
    }
    let inner = BBox {
        min: Point::new(score_cell.min.x + width, score_cell.min.y + width),
        max: Point::new(score_cell.max.x - width, score_cell.max.y - width),
    };
    cell.difference(&ContourSet::rectangle(inner, tolerance))
}

fn finished_board_contours(boundaries: &[ContourBuf]) -> Result<Vec<Ring>, VScoreReliefError> {
    let contours = boundaries
        .iter()
        .flat_map(|boundary| rings_from_contours(std::slice::from_ref(boundary)))
        .collect::<Vec<_>>();
    let contours = simplify_rings(contours, FillRule::NonZero);
    if contours.is_empty() {
        Err(VScoreReliefError::InvalidBoundary(
            "boundary does not form a polygon",
        ))
    } else {
        Ok(contours)
    }
}

fn score_cell_for_boundary(
    board_bbox: BBox,
    score_lines: &[VScoreLine],
    tolerance: f64,
) -> Result<Option<BBox>, VScoreReliefError> {
    let left = find_vertical_score_line(
        board_bbox.min.x,
        board_bbox.min.y,
        board_bbox.max.y,
        score_lines,
        tolerance,
    );
    let right = find_vertical_score_line(
        board_bbox.max.x,
        board_bbox.min.y,
        board_bbox.max.y,
        score_lines,
        tolerance,
    );
    let bottom = find_horizontal_score_line(
        board_bbox.min.y,
        board_bbox.min.x,
        board_bbox.max.x,
        score_lines,
        tolerance,
    );
    let top = find_horizontal_score_line(
        board_bbox.max.y,
        board_bbox.min.x,
        board_bbox.max.x,
        score_lines,
        tolerance,
    );

    let (Some(left), Some(right), Some(bottom), Some(top)) = (left, right, bottom, top) else {
        return Ok(None);
    };
    if right - left <= tolerance || top - bottom <= tolerance {
        return Err(VScoreReliefError::InvalidBoundary(
            "V-score cell has empty bounds",
        ));
    }
    Ok(Some(BBox {
        min: Point::new(left, bottom),
        max: Point::new(right, top),
    }))
}

fn find_vertical_score_line(
    x: f64,
    y_min: f64,
    y_max: f64,
    score_lines: &[VScoreLine],
    tolerance: f64,
) -> Option<f64> {
    score_lines
        .iter()
        .filter_map(|line| {
            let score_x = axis_aligned_x(*line, tolerance)?;
            let min_y = line.start.y.min(line.end.y);
            let max_y = line.start.y.max(line.end.y);
            ((score_x - x).abs() <= tolerance
                && min_y <= y_min + tolerance
                && max_y >= y_max - tolerance)
                .then_some(score_x)
        })
        .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
}

fn find_horizontal_score_line(
    y: f64,
    x_min: f64,
    x_max: f64,
    score_lines: &[VScoreLine],
    tolerance: f64,
) -> Option<f64> {
    score_lines
        .iter()
        .filter_map(|line| {
            let score_y = axis_aligned_y(*line, tolerance)?;
            let min_x = line.start.x.min(line.end.x);
            let max_x = line.start.x.max(line.end.x);
            ((score_y - y).abs() <= tolerance
                && min_x <= x_min + tolerance
                && max_x >= x_max - tolerance)
                .then_some(score_y)
        })
        .min_by(|a, b| (a - y).abs().total_cmp(&(b - y).abs()))
}

fn axis_aligned_x(line: VScoreLine, tolerance: f64) -> Option<f64> {
    ((line.start.x - line.end.x).abs() <= tolerance).then_some((line.start.x + line.end.x) / 2.0)
}

fn axis_aligned_y(line: VScoreLine, tolerance: f64) -> Option<f64> {
    ((line.start.y - line.end.y).abs() <= tolerance).then_some((line.start.y + line.end.y) / 2.0)
}

fn rectangle_payload(bbox: BBox) -> ContourBuf {
    ContourBuf::from_parts(
        bbox,
        vec![
            PathCmd::move_to(Point::new(bbox.min.x, bbox.min.y)),
            PathCmd::line_to(Point::new(bbox.max.x, bbox.min.y)),
            PathCmd::line_to(Point::new(bbox.max.x, bbox.max.y)),
            PathCmd::line_to(Point::new(bbox.min.x, bbox.max.y)),
            PathCmd::close(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(cmds: Vec<PathCmd>) -> Vec<ContourBuf> {
        vec![ContourBuf::new(cmds)]
    }

    fn rectangle_score_lines(width: f64, height: f64) -> Vec<VScoreLine> {
        vec![
            VScoreLine {
                start: Point::new(0.0, 0.0),
                end: Point::new(width, 0.0),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(width, 0.0),
                end: Point::new(width, height),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(width, height),
                end: Point::new(0.0, height),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(0.0, height),
                end: Point::new(0.0, 0.0),
                width: 0.025,
            },
        ]
    }

    #[test]
    fn rectangle_boundary_needs_no_reliefs() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();

        assert!(output.relief_contours.is_empty());
        assert_eq!(output.debug.entries.len(), 1);
        assert!(output.debug.entries[0].dead_space_pockets.is_empty());
        assert!(output.debug.entries[0].legal_tool_centers.is_empty());
    }

    #[test]
    fn score_edge_blocker_creates_relief() {
        let mut input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );
        input.score_blockers = vec![rectangle_payload(BBox {
            min: Point::new(0.0, 2.0),
            max: Point::new(1.2, 3.0),
        })];

        let output = vscore_route_reliefs_with_debug(&input).unwrap();

        assert!(!output.relief_contours.is_empty());
        assert!(!output.debug.entries[0].dead_space_pockets.is_empty());
    }

    #[test]
    fn internal_score_blocker_is_ignored() {
        let mut input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );
        input.score_blockers = vec![rectangle_payload(BBox {
            min: Point::new(4.0, 2.0),
            max: Point::new(6.0, 3.0),
        })];

        let output = vscore_route_reliefs_with_debug(&input).unwrap();

        assert!(output.relief_contours.is_empty());
        assert!(output.debug.entries[0].dead_space_pockets.is_empty());
    }

    #[test]
    fn inset_boundary_creates_closed_dead_space_pocket() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(6.0, 5.0)),
                PathCmd::line_to(Point::new(5.0, 3.0)),
                PathCmd::line_to(Point::new(4.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let relief_contours = &output.relief_contours;
        let debug = &output.debug.entries[0];

        assert!(!relief_contours.is_empty());
        assert!(!payloads_bbox(&debug.dead_space_pockets).is_empty());
        assert!(!payloads_bbox(&debug.legal_tool_centers).is_empty());
        assert!(relief_contours.iter().all(|contour| {
            contour
                .cmds
                .last()
                .is_some_and(|cmd| cmd.op == PathOp::Close)
        }));
        let pocket_bbox = payloads_bbox(&debug.dead_space_pockets);
        assert!(pocket_bbox.min.x <= 4.0 + DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM);
        assert_close(pocket_bbox.min.y, 3.0);
        assert!(pocket_bbox.max.x >= 6.0 - DEFAULT_SCORE_ALIGNMENT_TOLERANCE_MM);
        assert_close(pocket_bbox.max.y, 5.0);
        let relief_bbox = payloads_bbox(relief_contours);
        assert!(relief_bbox.max.y > pocket_bbox.max.y);
    }

    #[test]
    fn overlapping_relief_regions_are_merged_before_emit() {
        let boundary = path(vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(10.0, 0.0)),
            PathCmd::line_to(Point::new(10.0, 5.0)),
            PathCmd::line_to(Point::new(6.0, 5.0)),
            PathCmd::line_to(Point::new(5.0, 3.0)),
            PathCmd::line_to(Point::new(4.0, 5.0)),
            PathCmd::line_to(Point::new(0.0, 5.0)),
            PathCmd::close(),
        ]);
        let mut board_boundary = boundary.clone();
        board_boundary.extend(boundary);
        let input = VScoreReliefInput::new(board_boundary, rectangle_score_lines(10.0, 5.0));

        let output = vscore_route_reliefs_with_debug(&input).unwrap();

        assert_eq!(output.debug.entries.len(), 2);
        assert!(!output.relief_contours.is_empty());
        assert!(!output.debug.merged_relief_contours.is_empty());
        assert_eq!(
            payloads_bbox(&output.relief_contours),
            payloads_bbox(&output.debug.merged_relief_contours)
        );
    }

    #[test]
    fn missing_score_cell_side_yields_no_relief_candidate() {
        let mut score_lines = rectangle_score_lines(10.0, 5.0);
        score_lines.remove(2);
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            score_lines,
        );

        assert!(vscore_route_reliefs(&input).unwrap().is_empty());
    }

    #[test]
    fn curved_boundary_creates_closed_dead_space_pocket() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 10.0)),
                PathCmd::line_to(Point::new(4.0, 10.0)),
                PathCmd::arc_to(Point::new(0.0, 6.0), Point::new(4.0, 6.0), false),
                PathCmd::line_to(Point::new(0.0, 0.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let relief_contours = &output.relief_contours;

        assert!(!relief_contours.is_empty());
        assert!(
            relief_contours
                .iter()
                .flat_map(|contour| &contour.cmds)
                .any(|cmd| cmd.op == PathOp::Close)
        );
        let pocket_bbox = payloads_bbox(&output.debug.entries[0].dead_space_pockets);
        assert!(pocket_bbox.min.x >= -DEFAULT_RELIEF_TOLERANCE_MM);
        assert!(pocket_bbox.max.y <= 10.0 + DEFAULT_RELIEF_TOLERANCE_MM);
    }

    #[test]
    fn rounded_corners_smaller_than_tool_radius_still_get_relief() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(9.0, 0.0)),
                PathCmd::arc_to(Point::new(10.0, 1.0), Point::new(9.0, 1.0), false),
                PathCmd::line_to(Point::new(10.0, 9.0)),
                PathCmd::arc_to(Point::new(9.0, 10.0), Point::new(9.0, 9.0), false),
                PathCmd::line_to(Point::new(1.0, 10.0)),
                PathCmd::arc_to(Point::new(0.0, 9.0), Point::new(1.0, 9.0), false),
                PathCmd::line_to(Point::new(0.0, 1.0)),
                PathCmd::arc_to(Point::new(1.0, 0.0), Point::new(1.0, 1.0), false),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let relief_contours = &output.relief_contours;
        let debug = &output.debug.entries[0];

        assert!(!relief_contours.is_empty());
        assert!(!payloads_bbox(&debug.legal_tool_centers).is_empty());
        let pocket_bbox = payloads_bbox(&debug.dead_space_pockets);
        let relief_bbox = payloads_bbox(relief_contours);
        assert!(relief_bbox.min.x < pocket_bbox.min.x);
        assert!(relief_bbox.min.y < pocket_bbox.min.y);
        assert!(relief_bbox.max.x > pocket_bbox.max.x);
        assert!(relief_bbox.max.y > pocket_bbox.max.y);
        // Tool-sweep fillets come out as arcs, not tessellation.
        assert!(
            relief_contours
                .iter()
                .flat_map(|contour| &contour.cmds)
                .any(|cmd| cmd.op == PathOp::ArcTo),
            "route relief boundary should contain arcs"
        );
    }

    #[test]
    fn narrow_pocket_routes_from_sacrificial_margin() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(5.4, 5.0)),
                PathCmd::line_to(Point::new(5.0, 4.0)),
                PathCmd::line_to(Point::new(4.6, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let relief_contours = &output.relief_contours;

        assert!(!relief_contours.is_empty());
        assert!(!payloads_bbox(&output.debug.entries[0].legal_tool_centers).is_empty());
        assert!(!payloads_bbox(relief_contours).is_empty());
    }

    #[test]
    fn slightly_slanted_score_edges_are_treated_as_scored() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.05, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 10.0)),
                PathCmd::line_to(Point::new(0.0, 10.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();

        assert!(output.relief_contours.is_empty());
        assert!(output.debug.entries[0].dead_space_pockets.is_empty());
    }

    #[test]
    fn score_alignment_tolerance_keeps_real_pockets() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 8.0)),
                PathCmd::line_to(Point::new(8.0, 8.0)),
                PathCmd::line_to(Point::new(8.0, 10.0)),
                PathCmd::line_to(Point::new(0.0, 10.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let pocket_bbox = payloads_bbox(&output.debug.entries[0].dead_space_pockets);

        assert!(!output.relief_contours.is_empty());
        assert!(pocket_bbox.min.x < 8.1);
        assert!(pocket_bbox.min.y < 8.1);
        assert_close(pocket_bbox.max.x, 10.0);
        assert_close(pocket_bbox.max.y, 10.0);
    }

    #[test]
    fn stepped_outline_routes_only_unscored_boundary_runs() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 4.0)),
                PathCmd::line_to(Point::new(8.0, 4.0)),
                PathCmd::line_to(Point::new(8.0, 8.0)),
                PathCmd::line_to(Point::new(6.0, 8.0)),
                PathCmd::line_to(Point::new(6.0, 10.0)),
                PathCmd::line_to(Point::new(0.0, 10.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let output = vscore_route_reliefs_with_debug(&input).unwrap();
        let relief_bbox = output
            .relief_contours
            .iter()
            .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox));

        assert!(!relief_bbox.is_empty());
        assert!(
            relief_bbox.min.y > 3.0,
            "bottom score-aligned edge should not be routed"
        );
    }

    #[test]
    fn no_score_lines_errors_instead_of_inferring_bbox_scores() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
            ]),
            Vec::new(),
        );

        assert_eq!(
            vscore_route_reliefs(&input).unwrap_err(),
            VScoreReliefError::EmptyScoreLines
        );
    }

    fn payloads_bbox(payloads: &[ContourBuf]) -> BBox {
        payloads
            .iter()
            .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox))
    }

    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= 1e-9,
            "expected {left} to be close to {right}"
        );
    }
}
