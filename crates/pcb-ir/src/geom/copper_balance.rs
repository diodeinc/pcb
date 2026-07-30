//! Dense copper balancing over an explicitly supplied safe region.
//!
//! This module intentionally does not discover safe panel regions or inspect
//! PCB layer semantics. Callers supply the addable region and the measured
//! per-layer areas; the solver chooses the closest manufacturable copper area
//! and generates a deterministic perforated plane.

use std::f64::consts::PI;

use crate::geom::path::transform_cmds;
use crate::geom::{Affine2, ContourBuf, ContourSet, PathCmd, Point};

const NUMERIC_EPSILON: f64 = 1e-9;
const SQRT_3: f64 = 1.732_050_807_568_877_2;
const HEXAGON_CORNER_RADIUS_RATIO: f64 = 0.15;
// A sharp regular hexagon has area 3√3 R² / 2. Rounding each 120° corner
// inward by fillet radius kR removes (2√3 - π)k²R² across all six corners.
const ROUNDED_HEXAGON_AREA_FACTOR: f64 = 3.0 * SQRT_3 / 2.0
    - (2.0 * SQRT_3 - PI) * HEXAGON_CORNER_RADIUS_RATIO * HEXAGON_CORNER_RADIUS_RATIO;

/// Fixed geometry constraints for a dense perforated copper plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceProfile {
    pub pitch_mm: f64,
    pub min_void_radius_mm: f64,
    pub max_void_radius_mm: f64,
    pub min_copper_web_mm: f64,
    pub boundary_web_mm: f64,
    pub min_centers_per_component: usize,
}

impl DenseCopperBalanceProfile {
    /// Conservative first-party defaults for conventional rigid boards.
    pub const V1: Self = Self {
        pitch_mm: 2.30,
        min_void_radius_mm: 0.25,
        max_void_radius_mm: 1.00,
        min_copper_web_mm: 0.30,
        boundary_web_mm: 0.30,
        min_centers_per_component: 4,
    };

    pub fn row_pitch_mm(self) -> f64 {
        self.pitch_mm * 3.0_f64.sqrt() / 2.0
    }

    pub fn center_boundary_clearance_mm(self) -> f64 {
        self.max_void_radius_mm + self.boundary_web_mm
    }

    /// Minimum vertex-to-vertex web along the triangular lattice axes.
    ///
    /// The underlying regular hexagon has vertices at 0°, 60°, and their
    /// opposites, so its circumradius lies directly on every nearest-neighbor
    /// axis. The rounded corners stay inside that conservative envelope.
    pub fn nearest_neighbor_web_mm(self) -> f64 {
        self.pitch_mm - 2.0 * self.max_void_radius_mm
    }

    pub fn validate(self) -> Result<(), DenseCopperBalanceError> {
        for (name, value) in [
            ("pitch", self.pitch_mm),
            ("minimum void radius", self.min_void_radius_mm),
            ("maximum void radius", self.max_void_radius_mm),
            ("minimum copper web", self.min_copper_web_mm),
            ("boundary copper web", self.boundary_web_mm),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(DenseCopperBalanceError::InvalidProfile(format!(
                    "{name} must be finite and greater than zero"
                )));
            }
        }
        if self.min_void_radius_mm > self.max_void_radius_mm {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "minimum void radius exceeds maximum void radius".to_string(),
            ));
        }
        if self.nearest_neighbor_web_mm() + NUMERIC_EPSILON < self.min_copper_web_mm {
            return Err(DenseCopperBalanceError::InvalidProfile(format!(
                "pitch leaves {} mm between maximum-radius voids, below the {} mm minimum web",
                self.nearest_neighbor_web_mm(),
                self.min_copper_web_mm
            )));
        }
        if self.min_centers_per_component == 0 {
            return Err(DenseCopperBalanceError::InvalidProfile(
                "minimum centers per component must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

impl Default for DenseCopperBalanceProfile {
    fn default() -> Self {
        Self::V1
    }
}

/// Area-only inputs to the analytic solver.
///
/// `usable_area_mm2` must not overlap `existing_copper_area_mm2`; callers
/// should remove existing/protected copper while constructing the safe region.
/// The density target and retained-area denominator cover the whole board
/// array. Existing copper and non-usable empty area are fixed; only the usable
/// balancing area can be changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceAreas {
    /// Entire retained board-array area used as the density denominator.
    pub retained_area_mm2: f64,
    /// Fixed copper anywhere in that retained area on the layer being balanced.
    pub existing_copper_area_mm2: f64,
    pub target_density: f64,
    /// Safe, initially empty subset where generated copper may be added.
    pub usable_area_mm2: f64,
    pub void_count: usize,
}

/// The selected topology and, for a perforated plane, its analytic radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DenseCopperBalanceMode {
    None,
    Solid,
    /// A solid fill perforated by slightly rounded regular flat-top hexagons.
    ///
    /// `void_radius_mm` is each hexagon's circumradius.
    Perforated {
        void_radius_mm: f64,
    },
}

/// Result of projecting the requested copper area onto the manufacturable set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenseCopperBalanceSolution {
    pub mode: DenseCopperBalanceMode,
    pub desired_added_area_mm2: f64,
    pub generated_area_mm2: f64,
    pub initial_density: f64,
    pub achieved_density: f64,
    pub target_density: f64,
    pub residual_error: f64,
}

/// Geometry inputs once a safe region is available.
#[derive(Debug, Clone, Copy)]
pub struct DenseCopperBalanceRequest<'a> {
    /// Safe, initially empty subset of the retained board-array area.
    pub safe_region: &'a ContourSet,
    /// Entire retained board-array area used as the density denominator.
    pub retained_area_mm2: f64,
    /// Fixed copper anywhere in that retained area on this layer.
    pub existing_copper_area_mm2: f64,
    pub target_density: f64,
    pub lattice_origin: Point,
}

/// Analytic selection plus the actual regularized copper geometry.
#[derive(Debug, Clone)]
pub struct DenseCopperBalanceResult {
    pub solution: DenseCopperBalanceSolution,
    pub copper: ContourSet,
    /// Fixed lattice centers eligible at the profile's maximum void radius.
    pub lattice_centers: Vec<Point>,
    pub usable_area_mm2: f64,
    pub eligible_component_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenseCopperBalanceError {
    InvalidProfile(String),
    InvalidInput(String),
}

impl std::fmt::Display for DenseCopperBalanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProfile(message) => write!(f, "invalid copper balance profile: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid copper balance input: {message}"),
        }
    }
}

impl std::error::Error for DenseCopperBalanceError {}

/// Solve the area problem without constructing geometry.
///
/// The feasible added-copper set is `{0} ∪ [low, high] ∪ {usable_area}`.
/// Projection onto that set gives either no fill, one continuously solved
/// void radius, or a solid fill. Because zero is feasible, the selected
/// result cannot be worse than leaving the panel unchanged.
pub fn solve_dense_copper_balance(
    profile: DenseCopperBalanceProfile,
    areas: DenseCopperBalanceAreas,
) -> Result<DenseCopperBalanceSolution, DenseCopperBalanceError> {
    profile.validate()?;
    validate_areas(profile, areas)?;

    let initial_density = areas.existing_copper_area_mm2 / areas.retained_area_mm2;
    let desired_added_area =
        areas.target_density * areas.retained_area_mm2 - areas.existing_copper_area_mm2;

    let mut best = ProjectedArea {
        mode: DenseCopperBalanceMode::None,
        area_mm2: 0.0,
        error_mm2: desired_added_area.abs(),
    };

    consider_projected_area(
        &mut best,
        DenseCopperBalanceMode::Solid,
        areas.usable_area_mm2,
        desired_added_area,
    );

    if areas.void_count > 0 && areas.usable_area_mm2 > 0.0 {
        let void_area_factor = areas.void_count as f64 * ROUNDED_HEXAGON_AREA_FACTOR;
        let low = (areas.usable_area_mm2 - void_area_factor * profile.max_void_radius_mm.powi(2))
            .max(0.0);
        let high = (areas.usable_area_mm2 - void_area_factor * profile.min_void_radius_mm.powi(2))
            .min(areas.usable_area_mm2);
        let projected = desired_added_area.clamp(low, high);
        let radius = ((areas.usable_area_mm2 - projected) / void_area_factor)
            .max(0.0)
            .sqrt()
            .clamp(profile.min_void_radius_mm, profile.max_void_radius_mm);
        consider_projected_area(
            &mut best,
            DenseCopperBalanceMode::Perforated {
                void_radius_mm: radius,
            },
            projected,
            desired_added_area,
        );
    }

    Ok(solution_from_projected_area(
        areas,
        desired_added_area,
        initial_density,
        best,
    ))
}

/// Generate a dense copper balance region from an explicit safe region.
pub fn generate_dense_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<DenseCopperBalanceResult, DenseCopperBalanceError> {
    profile.validate()?;
    validate_request(profile, request)?;

    let mut usable = ContourSet::empty(request.safe_region.tolerance);
    let mut lattice_centers = Vec::new();
    let mut eligible_component_count = 0;

    for component in request.safe_region.connected_components() {
        let centers = triangular_lattice_centers(&component, request.lattice_origin, profile);
        if centers.len() < profile.min_centers_per_component {
            continue;
        }
        usable.union_assign(&component);
        lattice_centers.extend(centers);
        eligible_component_count += 1;
    }

    let areas = DenseCopperBalanceAreas {
        retained_area_mm2: request.retained_area_mm2,
        existing_copper_area_mm2: request.existing_copper_area_mm2,
        target_density: request.target_density,
        usable_area_mm2: usable.area(),
        void_count: lattice_centers.len(),
    };
    let mut solution = solve_dense_copper_balance(profile, areas)?;

    let copper = match solution.mode {
        DenseCopperBalanceMode::None => ContourSet::empty(request.safe_region.tolerance),
        DenseCopperBalanceMode::Solid => usable.clone(),
        DenseCopperBalanceMode::Perforated { void_radius_mm } => {
            let voids = hexagon_set(
                &lattice_centers,
                void_radius_mm,
                request.safe_region.tolerance,
            );
            usable.difference(&voids)
        }
    };

    solution.generated_area_mm2 = copper.area();
    solution.achieved_density = (request.existing_copper_area_mm2 + solution.generated_area_mm2)
        / request.retained_area_mm2;
    solution.residual_error = (solution.achieved_density - request.target_density).abs();

    // Preserve the mathematical never-worsen contract across geometric
    // regularization and numeric precision.
    if solution.residual_error
        > (solution.initial_density - request.target_density).abs() + NUMERIC_EPSILON
    {
        solution = none_solution(areas, solution.desired_added_area_mm2);
        return Ok(DenseCopperBalanceResult {
            solution,
            copper: ContourSet::empty(request.safe_region.tolerance),
            lattice_centers,
            usable_area_mm2: usable.area(),
            eligible_component_count,
        });
    }

    Ok(DenseCopperBalanceResult {
        solution,
        copper,
        lattice_centers,
        usable_area_mm2: usable.area(),
        eligible_component_count,
    })
}

#[derive(Debug, Clone, Copy)]
struct ProjectedArea {
    mode: DenseCopperBalanceMode,
    area_mm2: f64,
    error_mm2: f64,
}

fn consider_projected_area(
    best: &mut ProjectedArea,
    mode: DenseCopperBalanceMode,
    area_mm2: f64,
    desired_area_mm2: f64,
) {
    let candidate = ProjectedArea {
        mode,
        area_mm2,
        error_mm2: (area_mm2 - desired_area_mm2).abs(),
    };
    if candidate.error_mm2 + NUMERIC_EPSILON < best.error_mm2
        || ((candidate.error_mm2 - best.error_mm2).abs() <= NUMERIC_EPSILON
            && candidate.area_mm2 < best.area_mm2)
    {
        *best = candidate;
    }
}

fn solution_from_projected_area(
    areas: DenseCopperBalanceAreas,
    desired_added_area_mm2: f64,
    initial_density: f64,
    projected: ProjectedArea,
) -> DenseCopperBalanceSolution {
    let achieved_density =
        (areas.existing_copper_area_mm2 + projected.area_mm2) / areas.retained_area_mm2;
    DenseCopperBalanceSolution {
        mode: projected.mode,
        desired_added_area_mm2,
        generated_area_mm2: projected.area_mm2,
        initial_density,
        achieved_density,
        target_density: areas.target_density,
        residual_error: (achieved_density - areas.target_density).abs(),
    }
}

fn none_solution(
    areas: DenseCopperBalanceAreas,
    desired_added_area_mm2: f64,
) -> DenseCopperBalanceSolution {
    let initial_density = areas.existing_copper_area_mm2 / areas.retained_area_mm2;
    DenseCopperBalanceSolution {
        mode: DenseCopperBalanceMode::None,
        desired_added_area_mm2,
        generated_area_mm2: 0.0,
        initial_density,
        achieved_density: initial_density,
        target_density: areas.target_density,
        residual_error: (initial_density - areas.target_density).abs(),
    }
}

fn validate_areas(
    profile: DenseCopperBalanceProfile,
    areas: DenseCopperBalanceAreas,
) -> Result<(), DenseCopperBalanceError> {
    if !areas.retained_area_mm2.is_finite() || areas.retained_area_mm2 <= 0.0 {
        return Err(DenseCopperBalanceError::InvalidInput(
            "retained area must be finite and greater than zero".to_string(),
        ));
    }
    if !areas.existing_copper_area_mm2.is_finite()
        || areas.existing_copper_area_mm2 < 0.0
        || areas.existing_copper_area_mm2 > areas.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper area must be between zero and retained area".to_string(),
        ));
    }
    if !areas.target_density.is_finite() || !(0.0..=1.0).contains(&areas.target_density) {
        return Err(DenseCopperBalanceError::InvalidInput(
            "target density must be between zero and one".to_string(),
        ));
    }
    if !areas.usable_area_mm2.is_finite()
        || areas.usable_area_mm2 < 0.0
        || areas.usable_area_mm2 > areas.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "usable area must be between zero and retained area".to_string(),
        ));
    }
    if areas.existing_copper_area_mm2 + areas.usable_area_mm2
        > areas.retained_area_mm2 + NUMERIC_EPSILON
    {
        return Err(DenseCopperBalanceError::InvalidInput(
            "existing copper and usable regions must be disjoint within retained area".to_string(),
        ));
    }
    let maximum_void_area =
        areas.void_count as f64 * ROUNDED_HEXAGON_AREA_FACTOR * profile.max_void_radius_mm.powi(2);
    if maximum_void_area > areas.usable_area_mm2 + NUMERIC_EPSILON {
        return Err(DenseCopperBalanceError::InvalidInput(
            "maximum-radius voids exceed the usable region area".to_string(),
        ));
    }
    Ok(())
}

fn validate_request(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<(), DenseCopperBalanceError> {
    if !request.lattice_origin.is_finite() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "lattice origin must be finite".to_string(),
        ));
    }
    if !request.safe_region.bbox.is_valid() {
        return Err(DenseCopperBalanceError::InvalidInput(
            "safe region has invalid bounds".to_string(),
        ));
    }
    validate_areas(
        profile,
        DenseCopperBalanceAreas {
            retained_area_mm2: request.retained_area_mm2,
            existing_copper_area_mm2: request.existing_copper_area_mm2,
            target_density: request.target_density,
            usable_area_mm2: request.safe_region.area(),
            void_count: 0,
        },
    )
}

fn triangular_lattice_centers(
    region: &ContourSet,
    origin: Point,
    profile: DenseCopperBalanceProfile,
) -> Vec<Point> {
    if region.is_empty() {
        return Vec::new();
    }

    let row_pitch = profile.row_pitch_mm();
    let keep_in_radius = profile.center_boundary_clearance_mm();
    let first_row = ((region.bbox.min.y - origin.y) / row_pitch).floor() as i64 - 1;
    let last_row = ((region.bbox.max.y - origin.y) / row_pitch).ceil() as i64 + 1;
    let mut centers = Vec::new();

    for row in first_row..=last_row {
        let y = origin.y + row as f64 * row_pitch;
        let row_offset = if row.rem_euclid(2) == 0 {
            0.0
        } else {
            profile.pitch_mm / 2.0
        };
        let row_origin_x = origin.x + row_offset;
        let first_column =
            ((region.bbox.min.x - row_origin_x) / profile.pitch_mm).floor() as i64 - 1;
        let last_column = ((region.bbox.max.x - row_origin_x) / profile.pitch_mm).ceil() as i64 + 1;

        for column in first_column..=last_column {
            let center = Point::new(row_origin_x + column as f64 * profile.pitch_mm, y);
            if region.contains_disk(center, keep_in_radius) {
                centers.push(center);
            }
        }
    }

    centers
}

fn hexagon_set(centers: &[Point], radius: f64, tolerance: f64) -> ContourSet {
    let Some(hexagon) = rounded_hexagon(radius) else {
        return ContourSet::empty(tolerance);
    };
    let contours = centers
        .iter()
        .map(|center| transform_cmds(hexagon.cmds.iter().copied(), Affine2::translation(*center)))
        .collect::<Vec<_>>();
    ContourSet::from_filled_contours(&contours, tolerance)
}

fn rounded_hexagon(radius: f64) -> Option<ContourBuf> {
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }

    let corner_radius = radius * HEXAGON_CORNER_RADIUS_RATIO;
    let tangent_distance = corner_radius / SQRT_3;
    let center_inset = 2.0 * corner_radius / SQRT_3;
    let vertices = (0..6)
        .map(|index| {
            let angle = index as f64 * PI / 3.0;
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect::<Vec<_>>();

    let corner = |index: usize| {
        let previous = vertices[(index + 5) % 6];
        let vertex = vertices[index];
        let next = vertices[(index + 1) % 6];
        let incoming = vertex + (previous - vertex) * (tangent_distance / radius);
        let outgoing = vertex + (next - vertex) * (tangent_distance / radius);
        let center = vertex * ((radius - center_inset) / radius);
        (incoming, outgoing, center)
    };

    let (first_incoming, _, _) = corner(0);
    let mut commands = Vec::with_capacity(14);
    commands.push(PathCmd::move_to(first_incoming));
    for index in 0..6 {
        let (incoming, outgoing, center) = corner(index);
        if index > 0 {
            commands.push(PathCmd::line_to(incoming));
        }
        commands.push(PathCmd::arc_to(outgoing, center, false));
    }
    commands.push(PathCmd::close());
    Some(ContourBuf::new(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{BBox, PathOp, Point, tol};

    fn areas(target_density: f64) -> DenseCopperBalanceAreas {
        DenseCopperBalanceAreas {
            retained_area_mm2: 1_000.0,
            existing_copper_area_mm2: 0.0,
            target_density,
            usable_area_mm2: 1_000.0,
            void_count: 200,
        }
    }

    #[test]
    fn solves_continuous_hexagon_circumradius_exactly() {
        let solution =
            solve_dense_copper_balance(DenseCopperBalanceProfile::V1, areas(0.60)).unwrap();

        let DenseCopperBalanceMode::Perforated { void_radius_mm } = solution.mode else {
            panic!("expected perforated balance");
        };
        let expected_radius = (400.0 / (200.0 * ROUNDED_HEXAGON_AREA_FACTOR)).sqrt();
        assert!((void_radius_mm - expected_radius).abs() <= 1e-12);
        assert!((solution.generated_area_mm2 - 600.0).abs() <= 1e-9);
        assert!((solution.achieved_density - 0.60).abs() <= 1e-12);
        assert!(solution.residual_error <= 1e-12);
    }

    #[test]
    fn projects_low_unreachable_density_to_closest_feasible_area() {
        let none = solve_dense_copper_balance(DenseCopperBalanceProfile::V1, areas(0.10)).unwrap();
        assert_eq!(none.mode, DenseCopperBalanceMode::None);

        let bounded =
            solve_dense_copper_balance(DenseCopperBalanceProfile::V1, areas(0.30)).unwrap();
        assert_eq!(
            bounded.mode,
            DenseCopperBalanceMode::Perforated {
                void_radius_mm: 1.0
            }
        );
        assert!(bounded.residual_error < 0.30);
    }

    #[test]
    fn projects_high_unreachable_density_to_solid_fill() {
        let solution =
            solve_dense_copper_balance(DenseCopperBalanceProfile::V1, areas(0.99)).unwrap();

        assert_eq!(solution.mode, DenseCopperBalanceMode::Solid);
        assert_eq!(solution.generated_area_mm2, 1_000.0);
        assert!((solution.residual_error - 0.01).abs() <= 1e-12);
    }

    #[test]
    fn existing_copper_can_make_no_fill_optimal() {
        let solution = solve_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceAreas {
                retained_area_mm2: 1_000.0,
                existing_copper_area_mm2: 700.0,
                target_density: 0.60,
                usable_area_mm2: 200.0,
                void_count: 20,
            },
        )
        .unwrap();

        assert_eq!(solution.mode, DenseCopperBalanceMode::None);
        assert_eq!(solution.initial_density, solution.achieved_density);
    }

    #[test]
    fn target_is_global_while_only_usable_area_is_controllable() {
        let solution = solve_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceAreas {
                retained_area_mm2: 1_000.0,
                existing_copper_area_mm2: 200.0,
                target_density: 0.90,
                usable_area_mm2: 300.0,
                void_count: 50,
            },
        )
        .unwrap();

        assert_eq!(solution.mode, DenseCopperBalanceMode::Solid);
        assert_eq!(solution.generated_area_mm2, 300.0);
        assert_eq!(solution.achieved_density, 0.50);
        assert_eq!(solution.residual_error, 0.40);
    }

    #[test]
    fn geometry_uses_one_fixed_triangular_lattice_and_matches_area() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let result = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                retained_area_mm2: 200.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.75,
                lattice_origin: Point::new(10.0, 5.0),
            },
        )
        .unwrap();

        let DenseCopperBalanceMode::Perforated { void_radius_mm } = result.solution.mode else {
            panic!("expected perforated balance");
        };
        assert!((0.25..=1.0).contains(&void_radius_mm));
        assert_eq!(result.eligible_component_count, 1);
        assert!(result.lattice_centers.len() >= 4);
        assert!((result.solution.achieved_density - 0.75).abs() <= 2e-3);
        assert!(result.solution.residual_error < (result.solution.initial_density - 0.75).abs());
        assert_eq!(result.copper.rings.len(), result.lattice_centers.len() + 1);

        for center in &result.lattice_centers {
            assert!(safe_region.contains_disk(
                *center,
                DenseCopperBalanceProfile::V1.center_boundary_clearance_mm()
            ));
        }
        for (index, left) in result.lattice_centers.iter().enumerate() {
            for right in &result.lattice_centers[index + 1..] {
                assert!(left.distance_to(*right) + 1e-9 >= DenseCopperBalanceProfile::V1.pitch_mm);
            }
        }
    }

    #[test]
    fn geometry_skips_components_too_small_for_a_uniform_pattern() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(2.0, 2.0)),
            tol::REGION_MM,
        );
        let result = generate_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                retained_area_mm2: 4.0,
                existing_copper_area_mm2: 0.0,
                target_density: 1.0,
                lattice_origin: Point::new(1.0, 1.0),
            },
        )
        .unwrap();

        assert_eq!(result.solution.mode, DenseCopperBalanceMode::None);
        assert!(result.copper.is_empty());
        assert!(result.lattice_centers.is_empty());
        assert_eq!(result.eligible_component_count, 0);
    }

    #[test]
    fn rounded_hexagon_uses_six_scaled_corner_arcs_and_tracks_analytic_area() {
        let radius = 0.8;
        let hexagon = rounded_hexagon(radius).unwrap();
        let arcs = hexagon
            .cmds
            .iter()
            .filter(|command| command.op == PathOp::ArcTo)
            .collect::<Vec<_>>();
        assert_eq!(arcs.len(), 6);
        for arc in arcs {
            assert!(
                (arc.p0.distance_to(arc.p1) - radius * HEXAGON_CORNER_RADIUS_RATIO).abs() <= 1e-12
            );
        }

        let region = ContourSet::from_filled_contours(&[hexagon], tol::REGION_MM);
        let expected_area = ROUNDED_HEXAGON_AREA_FACTOR * radius.powi(2);
        assert!(
            (region.area() - expected_area).abs() <= expected_area * 2e-3,
            "geometric area {}, analytic area {}",
            region.area(),
            expected_area
        );
    }
}
