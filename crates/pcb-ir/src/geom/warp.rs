//! Panel warp estimated from the through-stack copper distribution.
//!
//! Copper balancing is implicitly minimizing the thermal moment resultant of
//! classical lamination theory. This module makes that explicit: it turns a
//! stackup plus per-layer copper density fields into an estimated deflection
//! surface, and reads bow off it the way IPC-TM-650 2.4.22 does.
//!
//! The chain, and the assumptions at each link:
//!
//! 1. Each layer is homogenized by copper fraction under the Voigt (iso-strain)
//!    rule, which is the right average for phases sharing in-plane strain.
//! 2. The copper-driven thermal moment is linear in that fraction, leaving a
//!    geometric field `m(x) = sum_l t_l z_l rho_l(x)` scaled by one material
//!    constant, and the plate that answers it is the stack at the copper each
//!    layer carries on average. See [`ThermalStack::response`].
//! 3. The panel is a free plate carrying that thermal moment, and its surface
//!    is the plate's equilibrium: bending energy against the work the moment
//!    does on curvature, minimized over polynomial surfaces. Free edges are
//!    the natural boundary conditions of that minimization, so the edges are
//!    part of the solution rather than ignored by it. See [`estimate_warp`].
//! 4. Deflection answers the moment through two integrations. That is what
//!    introduces the wavelength-squared weighting that makes long-wavelength
//!    imbalance dominate warp — the reason a flat norm over the moment field
//!    misreads the problem.
//!
//! No twist is estimated, because equilibrium leaves none. A free panel
//! carries no load, so its moment resultants do no work on any virtual
//! deflection, and taking `xy` as that deflection leaves `integral(M_xy) = 0`
//! over the panel. A thermal moment is isotropic and has no `xy` component,
//! which makes this `integral(w_xy) = 0` for the surface itself — and the
//! integral of `w_xy` over a rectangle is identically the alternating sum of
//! its corner heights, the quantity 2.4.22 reads as twist. The solved surface
//! inherits this exactly, `xy` being one of its polynomials. Copper therefore
//! cannot twist a free panel at this order however it is distributed. What
//! twists real panels is weave skew and unbalanced layup, which make the plate
//! itself anisotropic and are outside this model.
//!
//! The model is **verified, not validated**: the tests below check it against
//! closed forms, symmetry, and linearity. Nothing here has been compared
//! against a measured panel, so results are estimates whose absolute scale
//! carries the uncertainty of the assumed temperature drop and moduli. Ratios
//! between panelizations of one stackup are far more trustworthy than absolute
//! values, because the temperature drop and the moduli are common to both and
//! all but cancel.
//!
//! What is modelled is the elastic expansion mismatch between copper and
//! laminate below the glass transition, and nothing else. That mismatch is
//! about 1 ppm/K, and on its own it does not reproduce the fabricator rule of
//! keeping mirrored layers within 10-15 % copper of each other: the IPC-6012
//! bow limit is reached only at a mismatch no panel can carry. Resin cure
//! shrinkage and the several-times-larger resin expansion above the glass
//! transition are outside this model, so the absolute figure is the elastic
//! contribution to warp and not the panel's total.

use crate::geom::{BBox, Point};

/// Effective drop from where the laminate stops relaxing to room temperature.
///
/// Stress locks in around the glass transition rather than at the lamination
/// peak, because above it the resin is rubbery and relieves what it
/// accumulates. Standard FR-4 transitions near 130-140 C, so this is that less
/// room temperature. It is the single largest uncertainty in the absolute
/// result and it cancels entirely when comparing two panelizations of one
/// stackup.
pub const LAMINATE_RELAXATION_DROP_K: f64 = 110.0;

/// Isotropic elastic and thermal properties of one material under plane stress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Young's modulus, GPa.
    pub modulus_gpa: f64,
    pub poisson: f64,
    /// In-plane coefficient of thermal expansion, ppm per kelvin.
    pub cte_ppm_per_k: f64,
}

impl Material {
    /// Electrodeposited copper foil.
    pub const COPPER: Self = Self {
        modulus_gpa: 117.0,
        poisson: 0.34,
        cte_ppm_per_k: 17.0,
    };

    /// Woven-glass epoxy laminate, in-plane, below the glass transition.
    ///
    /// In-plane properties are dominated by the glass weave and are far less
    /// compliant than the through-thickness direction, which this model does
    /// not use.
    pub const LAMINATE: Self = Self {
        modulus_gpa: 25.0,
        poisson: 0.20,
        cte_ppm_per_k: 16.0,
    };

    /// Stiffness against equal strain in both directions, `E / (1 - nu)`, in
    /// GPa.
    ///
    /// This is the constant relating a fully constrained equibiaxial thermal
    /// strain to the stress it produces, which is what a misfit between layers
    /// generates, and the one a spherical curvature bends against.
    fn biaxial_modulus_gpa(self) -> f64 {
        self.modulus_gpa / (1.0 - self.poisson)
    }

    /// Stiffness against equal and opposite strains, `E / (1 + nu)`, in GPa:
    /// twice the shear modulus. Cylindrical-difference and twist curvatures
    /// bend against this one.
    fn deviatoric_modulus_gpa(self) -> f64 {
        self.modulus_gpa / (1.0 + self.poisson)
    }

    /// Thermal stress per kelvin, `E alpha / (1 - nu)`, in GPa per kelvin.
    fn thermal_stress_gpa_per_k(self) -> f64 {
        self.biaxial_modulus_gpa() * self.cte_ppm_per_k * 1e-6
    }
}

/// One physical layer of the stackup, in stack order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackLayer {
    pub thickness_mm: f64,
    pub material: Material,
    /// Whether this layer's copper coverage varies across the panel. Dielectric
    /// layers are fully present everywhere; conductor layers are present only
    /// where their artwork puts copper.
    pub is_conductor: bool,
}

/// A stackup reduced to what warp depends on.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalStack {
    layers: Vec<StackLayer>,
    /// Height of each layer's mid-surface above the geometric mid-plane,
    /// millimeters.
    lever_arms_mm: Vec<f64>,
    total_thickness_mm: f64,
}

/// A layer's contribution to the copper-driven moment, per unit copper
/// fraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConductorWeight {
    /// `z_l`, the layer's height above the mid-plane in millimeters. Signed by
    /// which side of it the layer sits on.
    pub lever_arm_mm: f64,
    /// `t_l z_l`, millimeters squared: the moment a fully copper-covered layer
    /// contributes, carrying the same sign.
    pub moment_arm_mm2: f64,
}

/// How the free panel answers its copper moment, at the copper it carries.
///
/// Every layer here is isotropic, so the laminate's stretching, coupling and
/// bending stiffnesses share their principal shapes: equal strain in both
/// directions, which meets `E / (1 - nu)`, and equal and opposite strain,
/// which meets `E / (1 + nu)`. Each shape bends about its own neutral axis,
/// leaving `D - B^2 / A` assembled from its own modulus, and those two
/// rigidities are the whole plate: `D (1 + nu)` and `D (1 - nu)` of the
/// equivalent homogeneous one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateResponse {
    /// Thermal moment per kelvin per unit of the geometric copper field
    /// `sum_l t_l z_l rho_l`, in GPa per kelvin. With a temperature drop and
    /// the field's mm^2 it is a moment resultant in GPa mm^2 per unit width.
    pub moment_coefficient_gpa_per_k: f64,
    /// Moment per unit of spherical curvature, `D (1 + nu)`, in GPa mm^3. A
    /// thermal moment is the same in every direction and drives this alone.
    pub spherical_rigidity_gpa_mm3: f64,
    /// Moment per unit of cylindrical-difference or twist curvature,
    /// `D (1 - nu)`, in GPa mm^3.
    pub deviatoric_rigidity_gpa_mm3: f64,
}

impl PlateResponse {
    /// `D` of the equivalent homogeneous plate, in GPa mm^3.
    pub fn flexural_rigidity_gpa_mm3(&self) -> f64 {
        (self.spherical_rigidity_gpa_mm3 + self.deviatoric_rigidity_gpa_mm3) / 2.0
    }
}

impl ThermalStack {
    /// Build from layers given in stack order, outermost first.
    ///
    /// The arms that weigh each layer's copper are measured from the geometric
    /// mid-plane, not the neutral axis. What bends a free plate is
    /// `sum Q (a - a_panel) t z` over every layer, which does not depend on
    /// where `z` is measured from; copper coverage enters it through the
    /// conductor layers alone only when the laminate's own share,
    /// `Q_d (a_d - a_panel) sum t z`, vanishes, and the mid-plane is where
    /// `sum t z` does. About that plane the copper field cancels exactly when
    /// the panel stays flat, at any coverage, so the arms are geometry alone.
    pub fn new(layers: Vec<StackLayer>) -> Option<Self> {
        let usable = |thickness: f64| thickness.is_finite() && thickness > 0.0;
        if layers.is_empty() || !layers.iter().all(|layer| usable(layer.thickness_mm)) {
            return None;
        }
        let total_thickness_mm = layers.iter().map(|layer| layer.thickness_mm).sum::<f64>();
        // Layers arrive outermost first, so depth runs *down* from the top
        // face. Measuring the arm the other way puts positive z up, out of the
        // top of the board: copper on the top face then carries a positive
        // moment, which is the convention the balance solver and the report
        // both read.
        let lever_arms_mm = layers
            .iter()
            .scan(0.0, |depth_mm, layer| {
                let center_mm = *depth_mm + layer.thickness_mm / 2.0;
                *depth_mm += layer.thickness_mm;
                Some(total_thickness_mm / 2.0 - center_mm)
            })
            .collect();
        Some(Self {
            layers,
            lever_arms_mm,
            total_thickness_mm,
        })
    }

    pub fn total_thickness_mm(&self) -> f64 {
        self.total_thickness_mm
    }

    /// The plate's response with each conductor at its measured mean copper
    /// fraction, `coverage`, in stack order, and `displaced` filling what the
    /// copper leaves of its layer.
    ///
    /// Each layer is homogenized at that fraction under the Voigt rule, and a
    /// free plate answers a temperature change with a membrane strain and a
    /// curvature, `[N_T; M_T] = [A B; B D] [e0; kappa]`, which leaves
    /// `kappa (D - B^2 / A) = M_T - (B / A) N_T`. With `a = N_T / (A dT)`, the
    /// expansion of the free stack as one membrane, the right side is each
    /// layer's thermal stress against that expansion rather than against a
    /// rigid frame, `dT sum Q (a_l - a) t z`, and under the Voigt rule that is
    /// `Q_c (a_c - a) - Q_d (a_d - a)` times the geometric copper field, plus
    /// the share of the build with its copper taken out: one dielectric
    /// throughout, `Q_d (a_d - a) sum t z`, which vanishes about the
    /// mid-plane. Materials that expand alike cannot bend the panel however
    /// unevenly they are distributed, and the coefficient vanishes when they
    /// do.
    ///
    /// Nothing in that is linearized: for copper spread evenly over each
    /// layer it is lamination theory exactly, at any coverage and for builds
    /// that are not symmetric. What is held fixed is the stiffness and the
    /// membrane expansion *across* the panel, at the panel's means, while the
    /// copper field varies. The curvature scale `coefficient / rigidity` falls
    /// by 9-12 % for each tenth of coverage added to every layer of a
    /// conventional six-layer build, so a region that far from the mean has
    /// its own share of the curvature misjudged by that much. The error is
    /// the product of two departures from the mean -- coverage and moment --
    /// and second order in the bow.
    ///
    /// `None` when `coverage` does not name every conductor or the materials
    /// leave the plate without stiffness.
    pub fn response(&self, displaced: Material, coverage: &[f64]) -> Option<PlateResponse> {
        if coverage.len()
            != self
                .layers
                .iter()
                .filter(|layer| layer.is_conductor)
                .count()
        {
            return None;
        }
        // Dielectric layers are wholly their own material.
        let mut coverage = coverage.iter();
        let present = self
            .layers
            .iter()
            .map(|layer| match layer.is_conductor {
                true => coverage.next().map_or(0.0, |cover| cover.clamp(0.0, 1.0)),
                false => 1.0,
            })
            .collect::<Vec<_>>();
        // `[A, B, D]` of one property through the thickness, about the
        // mid-plane: its resultant, first moment, and second moment with each
        // layer's own `t^3 / 12`.
        let resultants = |property: fn(Material) -> f64| {
            self.layers
                .iter()
                .zip(&self.lever_arms_mm)
                .zip(&present)
                .fold([0.0; 3], |[a, b, d], ((layer, arm), present)| {
                    let sheet = (present * property(layer.material)
                        + (1.0 - present) * property(displaced))
                        * layer.thickness_mm;
                    [
                        a + sheet,
                        b + sheet * arm,
                        d + sheet * (arm * arm + layer.thickness_mm.powi(2) / 12.0),
                    ]
                })
        };
        let about_neutral_axis = |[a, b, d]: [f64; 3]| d - b * b / a;
        let biaxial = resultants(Material::biaxial_modulus_gpa);
        let membrane_cte_per_k = resultants(Material::thermal_stress_gpa_per_k)[0] / biaxial[0];
        let misfit_stress_gpa_per_k = |material: Material| {
            material.thermal_stress_gpa_per_k()
                - material.biaxial_modulus_gpa() * membrane_cte_per_k
        };
        let response = PlateResponse {
            moment_coefficient_gpa_per_k: misfit_stress_gpa_per_k(Material::COPPER)
                - misfit_stress_gpa_per_k(displaced),
            spherical_rigidity_gpa_mm3: about_neutral_axis(biaxial),
            deviatoric_rigidity_gpa_mm3: about_neutral_axis(resultants(
                Material::deviatoric_modulus_gpa,
            )),
        };
        let stiff = |rigidity: f64| rigidity.is_finite() && rigidity > 0.0;
        (response.moment_coefficient_gpa_per_k.is_finite()
            && stiff(response.spherical_rigidity_gpa_mm3)
            && stiff(response.deviatoric_rigidity_gpa_mm3))
        .then_some(response)
    }

    /// Per-conductor moment arms `t_l z_l`, signed about the mid-plane.
    ///
    /// The copper-balance solver draws its stack weights from here, so the
    /// moment it flattens is the moment the warp estimate measures. They are
    /// geometry alone: no material constant and no coverage enters them, and
    /// the field they weigh is zero exactly where the panel stays flat.
    pub fn conductor_weights(&self) -> Vec<ConductorWeight> {
        self.layers
            .iter()
            .zip(&self.lever_arms_mm)
            .filter(|(layer, _)| layer.is_conductor)
            .map(|(layer, arm)| ConductorWeight {
                lever_arm_mm: *arm,
                moment_arm_mm2: layer.thickness_mm * arm,
            })
            .collect()
    }
}

/// A scalar field over the panel: the mean of each cell of a regular grid,
/// row-major from the bottom-left cell, the way
/// [`ContourSet::grid_coverage`](crate::geom::ContourSet::grid_coverage)
/// measures copper.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelField {
    pub bounds: BBox,
    pub columns: usize,
    pub rows: usize,
    pub values: Vec<f64>,
}

impl PanelField {
    /// Requires a panel with extent in both directions and one value per cell.
    /// Guaranteeing that here is what lets everything downstream divide by the
    /// sides and index the grid without re-checking.
    pub fn new(bounds: BBox, columns: usize, rows: usize, values: Vec<f64>) -> Option<Self> {
        let sized = bounds.is_valid() && bounds.width() > 0.0 && bounds.height() > 0.0;
        (sized && columns > 0 && rows > 0 && values.len() == columns * rows).then_some(Self {
            bounds,
            columns,
            rows,
            values,
        })
    }

    /// Centre of the cell `values[index]` belongs to.
    pub fn cell_center(&self, index: usize) -> Point {
        let (column, row) = (index % self.columns, index / self.columns);
        Point::new(
            self.bounds.min.x + self.bounds.width() * (column as f64 + 0.5) / self.columns as f64,
            self.bounds.min.y + self.bounds.height() * (row as f64 + 0.5) / self.rows as f64,
        )
    }
}

/// Bow as IPC-TM-650 2.4.22 defines it, plus the surface it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpEstimate {
    /// Height of the surface at the centre of each cell of the moment field,
    /// millimeters, measured from the plane through the panel corners.
    pub deflection: PanelField,
    /// Largest single departure from that plane, millimeters. A surface that
    /// rises along one axis and dips along the other reports the larger lobe,
    /// not their sum.
    pub bow_mm: f64,
    /// Bow as a percentage of the panel dimension it is worst against.
    pub bow_percent: f64,
}

/// Highest total degree of the polynomial surfaces the deflection is sought
/// among.
///
/// Truncation leaves out what the omitted shapes would have deflected, and
/// deflection is the moment integrated twice: a moment rippling `n` half-waves
/// across a side deflects `8 / (pi n)^2` of what the same amplitude spread
/// evenly does. A polynomial spends about `pi / 2` of its degrees on each
/// half-wave, so degree 12 carries ripples to `n = 8`, and what it leaves out
/// deflects under 1.3 % of its own amplitude's even bow. On copper that changes
/// abruptly -- a heavy half, a heavy corner, rails around a striped array --
/// bow lands within 0.3 % of a degree-28 surface's, which the tests hold it
/// to. The response this surface is scaled by is itself good only to several
/// percent wherever coverage departs from its mean, so a richer surface would
/// resolve detail the model does not have. The solve is dense in
/// `(12 + 1) (12 + 2) / 2 - 3 = 88` unknowns.
const SURFACE_DEGREE: usize = 12;

/// Estimate warp from the geometric copper moment field.
///
/// `moment_field` is `sum_l t_l z_l rho_l(x)` in mm^2 — the quantity the copper
/// balance solver already computes — and `response` is the stack's at the mean
/// coverage of those same layers. `temperature_drop_k` is the effective
/// excursion from where the laminate stops relaxing down to room temperature,
/// which is the single largest source of uncertainty in the absolute result.
///
/// The surface is the Ritz solution of the free plate: over every polynomial
/// `w` up to `SURFACE_DEGREE`, the one that leaves the plate's bending
/// energy, `D/2 (w_xx + w_yy)^2 - D (1 - nu) (w_xx w_yy - w_xy^2)`, less the
/// moment's work on its curvature, `M (w_xx + w_yy)`, stationary over the
/// panel, with `M` the thermal moment held constant over each cell. Free edges
/// are that functional's natural boundary conditions, so nothing is imposed
/// on the boundary, and a uniform moment returns its spherical cap exactly
/// because the cap is one of the polynomials.
pub fn estimate_warp(
    response: &PlateResponse,
    moment_field: &PanelField,
    temperature_drop_k: f64,
) -> WarpEstimate {
    let bounds = moment_field.bounds;
    let half_sides_mm = [bounds.width() / 2.0, bounds.height() / 2.0];
    // GPa/K * K * mm^2 -> GPa mm^2, a moment resultant per unit width.
    let moment_scale = response.moment_coefficient_gpa_per_k * temperature_drop_k;
    let surface = PlateSurface::solve(
        response,
        half_sides_mm,
        moment_field,
        moment_scale,
        SURFACE_DEGREE,
    );

    // Read at every cell's edges and centre, in both directions: the panel's
    // corners, its edges and its middle are all among the stations whatever
    // the cell count, and those are where a bowed panel peaks.
    let stations = [moment_field.columns, moment_field.rows].map(|cells| 2 * cells + 1);
    let heights = surface.heights(stations);
    // Bow is the largest departure a corner-seated panel makes from the table,
    // normalized by the dimension it is measured along — the shorter one gives
    // the larger percentage, so that is the one reported. The corners sit at
    // zero by construction, so that departure is the levelled surface's
    // largest magnitude on either side, not its range: a surface rising along
    // one axis and dipping along the other seats on whichever lobe it rests
    // and shows the other.
    let bow_mm = heights
        .iter()
        .fold(0.0_f64, |bow, height| bow.max(height.abs()));
    let bow_percent = 100.0 * bow_mm / bounds.width().min(bounds.height());

    let deflection = (0..moment_field.rows)
        .flat_map(|row| {
            let heights = &heights;
            (0..moment_field.columns)
                .map(move |column| heights[(2 * row + 1) * stations[0] + 2 * column + 1])
        })
        .collect();
    WarpEstimate {
        deflection: PanelField {
            values: deflection,
            ..*moment_field
        },
        bow_mm,
        bow_percent,
    }
}

/// A polynomial deflection surface, `sum c_ij P_i(x) P_j(y)` in Legendre
/// polynomials of each side's own `[-1, 1]` coordinate.
///
/// Legendre products rather than monomials because they are orthogonal over
/// the rectangle, which is what keeps the stiffness matrix well conditioned at
/// this degree.
struct PlateSurface {
    degree: usize,
    /// `c_ij` at `i * (degree + 1) + j`, millimeters; zero past the total
    /// degree.
    coefficients: Vec<f64>,
}

impl PlateSurface {
    /// The free plate's equilibrium under `moment_scale * moment_field`,
    /// seated on its corners.
    fn solve(
        response: &PlateResponse,
        half_sides_mm: [f64; 2],
        moment_field: &PanelField,
        moment_scale: f64,
        degree: usize,
    ) -> Self {
        let order = degree + 1;
        let [a, b] = half_sides_mm;
        // Constants and tilts bend nothing, so they carry no energy and the
        // moment does no work on them: equilibrium leaves them free, and they
        // are kept for the corner plane below.
        let shapes = (0..order)
            .flat_map(|i| (0..order - i).map(move |j| (i, j)))
            .filter(|(i, j)| i + j >= 2)
            .collect::<Vec<_>>();

        // Energy: `D (u_xx v_xx + u_yy v_yy) + D nu (u_xx v_yy + u_yy v_xx)
        // + 2 D (1 - nu) u_xy v_xy`, which separates into products of one
        // Gram matrix per side. `x = a xi` brings a power of the half-side
        // with every derivative, and the area element one of each.
        let grams = LegendreGrams::new(degree);
        let bend = response.flexural_rigidity_gpa_mm3();
        let cross =
            (response.spherical_rigidity_gpa_mm3 - response.deviatoric_rigidity_gpa_mm3) / 2.0;
        let twist = 2.0 * response.deviatoric_rigidity_gpa_mm3;
        let stiffness = shapes
            .iter()
            .flat_map(|&(i, j)| {
                let grams = &grams;
                shapes.iter().map(move |&(k, l)| {
                    bend * (b / a.powi(3) * grams.curvature[i][k] * grams.mass[j][l]
                        + a / b.powi(3) * grams.mass[i][k] * grams.curvature[j][l])
                        + (cross
                            * (grams.curvature_mass[i][k] * grams.curvature_mass[l][j]
                                + grams.curvature_mass[k][i] * grams.curvature_mass[j][l])
                            + twist * grams.slope[i][k] * grams.slope[j][l])
                            / (a * b)
                })
            })
            .collect::<Vec<_>>();

        // Work of the moment on each shape's curvature, exact for a moment
        // held constant over each cell: `integral(P'')` over a cell is the
        // step in `P'` across it. Columns are summed first, so the double sum
        // over cells costs one pass per side.
        let columns = AxisCells::new(degree, moment_field.columns);
        let rows = AxisCells::new(degree, moment_field.rows);
        let along_columns = |table: &[Vec<f64>]| {
            table
                .iter()
                .map(|weights| {
                    moment_field
                        .values
                        .chunks_exact(moment_field.columns)
                        .map(|row| row.iter().zip(weights).map(|(m, w)| m * w).sum::<f64>())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let (row_steps, row_areas) = (
            along_columns(&columns.slope_steps),
            along_columns(&columns.areas),
        );
        let dot =
            |left: &[f64], right: &[f64]| left.iter().zip(right).map(|(l, r)| l * r).sum::<f64>();
        let work = shapes
            .iter()
            .map(|&(i, j)| {
                moment_scale
                    * (b / a * dot(&row_steps[i], &rows.areas[j])
                        + a / b * dot(&row_areas[i], &rows.slope_steps[j]))
            })
            .collect::<Vec<_>>();

        let mut coefficients = vec![0.0; order * order];
        for ((i, j), amplitude) in shapes.iter().zip(solve_positive_definite(stiffness, work)) {
            coefficients[i * order + j] = amplitude;
        }

        // 2.4.22 measures from the plane the panel seats on: through its
        // corners. `P_i(+-1) = (+-1)^i`, so the corner heights are signed sums
        // of the coefficients, and the free constant and tilts are exactly
        // that plane. The `xy` shape is in the basis and the moment does no
        // work on it, so the fourth corner lies in the plane of the other
        // three.
        let corner = |sx: f64, sy: f64| {
            (0..order)
                .flat_map(|i| (0..order).map(move |j| (i, j)))
                .map(|(i, j)| coefficients[i * order + j] * sx.powi(i as i32) * sy.powi(j as i32))
                .sum::<f64>()
        };
        let (pp, pm, mp, mm) = (
            corner(1.0, 1.0),
            corner(1.0, -1.0),
            corner(-1.0, 1.0),
            corner(-1.0, -1.0),
        );
        coefficients[0] = -(pp + pm + mp + mm) / 4.0;
        coefficients[order] = -(pp + pm - mp - mm) / 4.0;
        coefficients[1] = -(pp - pm + mp - mm) / 4.0;
        Self {
            degree,
            coefficients,
        }
    }

    /// Heights on a lattice of evenly spaced stations spanning each side, edge
    /// to edge, row-major from the bottom-left corner.
    fn heights(&self, stations: [usize; 2]) -> Vec<f64> {
        let order = self.degree + 1;
        let [across, up] = stations.map(|count| {
            (0..count)
                .map(|station| {
                    legendre(self.degree, 2.0 * station as f64 / (count - 1) as f64 - 1.0).0
                })
                .collect::<Vec<_>>()
        });
        up.iter()
            .flat_map(|y| {
                // Collapse the y direction once per row of stations.
                let along = (0..order)
                    .map(|i| {
                        (0..order)
                            .map(|j| self.coefficients[i * order + j] * y[j])
                            .sum::<f64>()
                    })
                    .collect::<Vec<_>>();
                across
                    .iter()
                    .map(move |x| along.iter().zip(x).map(|(c, p)| c * p).sum::<f64>())
            })
            .collect()
    }
}

/// `P_0..=P_degree` and their first derivatives at `x`, by the three-term
/// recurrences.
fn legendre(degree: usize, x: f64) -> (Vec<f64>, Vec<f64>) {
    let mut values = vec![1.0, x];
    let mut slopes = vec![0.0, 1.0];
    for n in 1..degree {
        let weight = n as f64;
        values
            .push(((2.0 * weight + 1.0) * x * values[n] - weight * values[n - 1]) / (weight + 1.0));
        slopes.push(slopes[n - 1] + (2.0 * weight + 1.0) * values[n]);
    }
    values.truncate(degree + 1);
    slopes.truncate(degree + 1);
    (values, slopes)
}

/// What a moment held constant over each cell needs of the Legendre
/// polynomials along one side split into `cells`. Row `i` belongs to `P_i`.
struct AxisCells {
    /// `integral(P_i)` over each cell.
    areas: Vec<Vec<f64>>,
    /// `integral(P_i'')` over each cell: the step in `P_i'` across it.
    slope_steps: Vec<Vec<f64>>,
}

impl AxisCells {
    fn new(degree: usize, cells: usize) -> Self {
        // `(2 i + 1) integral(P_i) = P_(i+1) - P_(i-1)`, and `P_1` for `P_0`.
        let (primitives, slopes): (Vec<_>, Vec<_>) = (0..=cells)
            .map(|edge| {
                let (values, slopes) = legendre(degree + 1, 2.0 * edge as f64 / cells as f64 - 1.0);
                let primitives = (0..=degree)
                    .map(|i| match i {
                        0 => values[1],
                        _ => (values[i + 1] - values[i - 1]) / (2 * i + 1) as f64,
                    })
                    .collect::<Vec<_>>();
                (primitives, slopes)
            })
            .unzip();
        let steps = |at_edges: &[Vec<f64>]| {
            (0..=degree)
                .map(|i| {
                    at_edges
                        .windows(2)
                        .map(|pair| pair[1][i] - pair[0][i])
                        .collect()
                })
                .collect()
        };
        Self {
            areas: steps(&primitives),
            slope_steps: steps(&slopes),
        }
    }
}

/// Integrals over `[-1, 1]` of products of Legendre polynomials and their
/// derivatives, exact.
///
/// `P_n' = sum (2 k + 1) P_k` over `k = n - 1, n - 3, ...`, so every
/// derivative is itself a Legendre series with integer coefficients, and
/// orthogonality, `integral(P_k^2) = 2 / (2 k + 1)`, turns each product
/// integral into a sum over those coefficients. No quadrature is involved.
struct LegendreGrams {
    /// `integral(P_i P_j)`.
    mass: Vec<Vec<f64>>,
    /// `integral(P_i' P_j')`.
    slope: Vec<Vec<f64>>,
    /// `integral(P_i'' P_j'')`.
    curvature: Vec<Vec<f64>>,
    /// `integral(P_i'' P_j)`.
    curvature_mass: Vec<Vec<f64>>,
}

impl LegendreGrams {
    fn new(degree: usize) -> Self {
        let order = degree + 1;
        let series = |of: &dyn Fn(usize, usize) -> f64| {
            (0..order)
                .map(|n| (0..order).map(|k| of(n, k)).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        let value = series(&|n, k| f64::from(n == k));
        let first = series(&|n, k| {
            if k < n && (n - k) % 2 == 1 {
                (2 * k + 1) as f64
            } else {
                0.0
            }
        });
        let second = series(&|n, k| (0..order).map(|m| first[n][m] * first[m][k]).sum());
        let gram = |left: &[Vec<f64>], right: &[Vec<f64>]| {
            series(&|i, j| {
                (0..order)
                    .map(|k| left[i][k] * right[j][k] * 2.0 / (2 * k + 1) as f64)
                    .sum()
            })
        };
        Self {
            mass: gram(&value, &value),
            slope: gram(&first, &first),
            curvature: gram(&second, &second),
            curvature_mass: gram(&second, &value),
        }
    }
}

/// Solve `matrix x = rhs` for a symmetric positive-definite `matrix`, stored
/// row-major.
///
/// Cholesky factorization, after scaling the matrix to a unit diagonal: the
/// shapes' energies span many orders of magnitude, and that spread is
/// conditioning the scaling removes for free.
fn solve_positive_definite(mut matrix: Vec<f64>, mut rhs: Vec<f64>) -> Vec<f64> {
    let n = rhs.len();
    let scale = (0..n)
        .map(|i| 1.0 / matrix[i * n + i].sqrt())
        .collect::<Vec<_>>();
    for i in 0..n {
        rhs[i] *= scale[i];
        for j in 0..n {
            matrix[i * n + j] *= scale[i] * scale[j];
        }
    }
    // `L` overwrites the lower triangle a column at a time.
    for j in 0..n {
        for k in 0..j {
            let factor = matrix[j * n + k];
            for i in j..n {
                matrix[i * n + j] -= matrix[i * n + k] * factor;
            }
        }
        let pivot = matrix[j * n + j].sqrt();
        for i in j..n {
            matrix[i * n + j] /= pivot;
        }
    }
    // `L y = rhs`, then `L^T x = y`.
    for i in 0..n {
        let known = (0..i).map(|k| matrix[i * n + k] * rhs[k]).sum::<f64>();
        rhs[i] = (rhs[i] - known) / matrix[i * n + i];
    }
    for i in (0..n).rev() {
        let known = (i + 1..n).map(|k| matrix[k * n + i] * rhs[k]).sum::<f64>();
        rhs[i] = (rhs[i] - known) / matrix[i * n + i];
    }
    rhs.iter().zip(scale).map(|(x, scale)| x * scale).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laminate(thickness_mm: f64) -> StackLayer {
        StackLayer {
            thickness_mm,
            material: Material::LAMINATE,
            is_conductor: false,
        }
    }

    fn copper(thickness_mm: f64) -> StackLayer {
        StackLayer {
            thickness_mm,
            material: Material::COPPER,
            is_conductor: true,
        }
    }

    fn symmetric_four_layer() -> ThermalStack {
        ThermalStack::new(vec![
            copper(0.035),
            laminate(0.5),
            copper(0.035),
            laminate(0.5),
            copper(0.035),
            laminate(0.5),
            copper(0.035),
        ])
        .unwrap()
    }

    /// A field over `columns x rows` cells, from its value at each cell's
    /// centre in the panel's own `[-1, 1]` coordinates.
    fn field(
        bounds: BBox,
        (columns, rows): (usize, usize),
        value: impl Fn(f64, f64) -> f64,
    ) -> PanelField {
        let along = |cell: usize, cells: usize| (2 * cell + 1) as f64 / cells as f64 - 1.0;
        let values = (0..rows)
            .flat_map(|row| {
                let value = &value;
                (0..columns).map(move |column| value(along(column, columns), along(row, rows)))
            })
            .collect();
        PanelField::new(bounds, columns, rows, values).unwrap()
    }

    fn uniform_field(bounds: BBox, value: f64) -> PanelField {
        field(bounds, (20, 20), |_, _| value)
    }

    /// A conventional 1.6 mm six-layer build: six 1 oz foils, thin outer
    /// prepregs, thicker cores.
    fn six_layer_panel() -> ThermalStack {
        ThermalStack::new(vec![
            copper(0.035),
            laminate(0.2),
            copper(0.035),
            laminate(0.3),
            copper(0.035),
            laminate(0.39),
            copper(0.035),
            laminate(0.3),
            copper(0.035),
            laminate(0.2),
            copper(0.035),
        ])
        .unwrap()
    }

    fn panel() -> BBox {
        BBox::new(Point::new(0.0, 0.0), Point::new(400.0, 500.0))
    }

    /// Every conductor whole, the laminate filling nothing.
    fn whole(stack: &ThermalStack) -> PlateResponse {
        let conductors = stack.conductor_weights().len();
        stack
            .response(Material::LAMINATE, &vec![1.0; conductors])
            .unwrap()
    }

    /// Curvature per kelvin of a free plate with each conductor evenly covered,
    /// from classical lamination theory solved outright: every layer's plane
    /// stress stiffness `Q11`, `Q12` mixed by area, the full `[A B; B D]`
    /// system in both directions, and the thermal resultants on its right.
    fn exact_curvature_per_k(layers: &[StackLayer], displaced: Material, coverage: &[f64]) -> f64 {
        let total = layers.iter().map(|layer| layer.thickness_mm).sum::<f64>();
        let mut coverage = coverage.iter();
        let mut system = [[0.0; 4]; 4];
        let mut thermal = [0.0; 4];
        let mut top = 0.0;
        for layer in layers {
            let (t, z) = (
                layer.thickness_mm,
                total / 2.0 - (top + layer.thickness_mm / 2.0),
            );
            top += t;
            let present = if layer.is_conductor {
                *coverage.next().unwrap()
            } else {
                1.0
            };
            let mixed = |property: fn(Material) -> f64| {
                present * property(layer.material) + (1.0 - present) * property(displaced)
            };
            let q11 = mixed(|m| m.modulus_gpa / (1.0 - m.poisson * m.poisson));
            let q12 = mixed(|m| m.poisson * m.modulus_gpa / (1.0 - m.poisson * m.poisson));
            let stress = mixed(|m| m.modulus_gpa * m.cte_ppm_per_k * 1e-6 / (1.0 - m.poisson));
            // Blocks A, B, B and D, each `[[Q11, Q12], [Q12, Q11]]`.
            for (row, column, weight) in [
                (0, 0, t),
                (0, 2, t * z),
                (2, 0, t * z),
                (2, 2, t * (z * z + t * t / 12.0)),
            ] {
                system[row][column] += q11 * weight;
                system[row + 1][column + 1] += q11 * weight;
                system[row][column + 1] += q12 * weight;
                system[row + 1][column] += q12 * weight;
            }
            for (row, weight) in [(0, t), (1, t), (2, t * z), (3, t * z)] {
                thermal[row] += stress * weight;
            }
        }
        let strains = solve_positive_definite(system.concat(), thermal.to_vec());
        assert!((strains[2] - strains[3]).abs() <= 1e-12 * strains[2].abs());
        strains[2]
    }

    fn model_curvature_per_k(layers: &[StackLayer], displaced: Material, coverage: &[f64]) -> f64 {
        let stack = ThermalStack::new(layers.to_vec()).unwrap();
        let response = stack.response(displaced, coverage).unwrap();
        let field = stack
            .conductor_weights()
            .iter()
            .zip(coverage)
            .map(|(weight, cover)| weight.moment_arm_mm2 * cover)
            .sum::<f64>();
        response.moment_coefficient_gpa_per_k * field / response.spherical_rigidity_gpa_mm3
    }

    /// Copper spread evenly over each layer is the case lamination theory
    /// solves in closed form, and the response evaluated at that copper has to
    /// reproduce it: sparse or dense, balanced or not, symmetric build or not.
    #[test]
    fn evenly_covered_layers_bend_exactly_as_lamination_theory_says() {
        let six = vec![
            copper(0.035),
            laminate(0.2),
            copper(0.035),
            laminate(0.3),
            copper(0.035),
            laminate(0.39),
            copper(0.035),
            laminate(0.3),
            copper(0.035),
            laminate(0.2),
            copper(0.035),
        ];
        let lopsided = vec![
            copper(0.105),
            laminate(0.3),
            copper(0.035),
            laminate(1.0),
            copper(0.035),
        ];
        let resin = Material {
            cte_ppm_per_k: 60.0,
            ..Material::LAMINATE
        };
        for displaced in [Material::LAMINATE, resin] {
            for level in [0.2, 0.35, 0.5, 0.7, 0.9] {
                for (layers, tilt) in [
                    (&six, vec![0.1, -0.05, 0.0, 0.02, 0.0, -0.1]),
                    (&six, vec![0.1, 0.1, 0.1, 0.0, 0.0, 0.0]),
                    (&lopsided, vec![0.0, 0.1, -0.1]),
                    (&lopsided, vec![-0.1, 0.0, 0.1]),
                ] {
                    let layers = layers
                        .iter()
                        .map(|layer| StackLayer {
                            material: if layer.is_conductor {
                                layer.material
                            } else {
                                displaced
                            },
                            ..*layer
                        })
                        .collect::<Vec<_>>();
                    let coverage = tilt.iter().map(|tilt| level + tilt).collect::<Vec<_>>();
                    let exact = exact_curvature_per_k(&layers, displaced, &coverage);
                    let model = model_curvature_per_k(&layers, displaced, &coverage);
                    assert!(exact.abs() > 1e-9, "{level} {tilt:?}: nothing to compare");
                    assert!(
                        (model - exact).abs() <= 1e-9 * exact.abs(),
                        "{level} {tilt:?}: {model} != {exact}"
                    );
                }
            }
        }
    }

    /// What the response holds fixed across a panel is how far the curvature
    /// scale moves with coverage, so that is the figure its documentation
    /// quotes: about a tenth per tenth of coverage on a conventional build.
    #[test]
    fn the_curvature_scale_falls_about_a_tenth_per_tenth_of_coverage() {
        let stack = six_layer_panel();
        let scale = |level: f64| {
            let response = stack.response(Material::LAMINATE, &[level; 6]).unwrap();
            response.moment_coefficient_gpa_per_k / response.spherical_rigidity_gpa_mm3
        };
        for level in [0.2, 0.4, 0.6, 0.8] {
            let step = scale(level + 0.1) / scale(level);
            assert!((0.87..0.92).contains(&step), "{level}: {step}");
        }
        // Which is why the response is evaluated at the measured copper: whole
        // foils would understate a half-covered panel's curvature by a third.
        assert!(scale(1.0) / scale(0.5) < 0.65);
    }

    /// Mixed copper weights move the neutral axis, but the coverage that
    /// leaves the panel flat is the one that cancels about the mid-plane, at
    /// any coverage and whatever the dielectric: the arms balancing draws on
    /// are geometry alone.
    #[test]
    fn the_copper_field_cancels_exactly_where_an_asymmetric_build_stays_flat() {
        let resin = Material {
            cte_ppm_per_k: 60.0,
            ..Material::LAMINATE
        };
        for dielectric in [Material::LAMINATE, resin] {
            let core = StackLayer {
                thickness_mm: 1.0,
                material: dielectric,
                is_conductor: false,
            };
            let layers = vec![copper(0.105), core, copper(0.035)];
            let weights = ThermalStack::new(layers.clone())
                .unwrap()
                .conductor_weights();
            // Thin the heavy foil until the field cancels against the light one.
            let heavy = -weights[1].moment_arm_mm2 / weights[0].moment_arm_mm2;
            assert!(heavy > 0.0 && heavy < 1.0);

            let whole = exact_curvature_per_k(&layers, dielectric, &[1.0, 1.0]).abs();
            for light in [0.3, 0.6, 1.0] {
                let balanced =
                    exact_curvature_per_k(&layers, dielectric, &[heavy * light, light]).abs();
                assert!(balanced <= 1e-12 * whole, "{light}: {balanced} of {whole}");
            }
        }

        // Balanced foils sit symmetrically about the middle.
        let even = ThermalStack::new(vec![copper(0.035), laminate(1.0), copper(0.035)])
            .unwrap()
            .conductor_weights();
        assert!((even[0].moment_arm_mm2 + even[1].moment_arm_mm2).abs() <= 1e-12);
    }

    /// Equal copper on mirrored layers cancels: the field is zero and so is the
    /// warp. This is the case balancing is trying to reach.
    #[test]
    fn a_balanced_panel_is_predicted_flat() {
        let stack = symmetric_four_layer();
        let field = uniform_field(panel(), 0.0);
        let warp = estimate_warp(&whole(&stack), &field, 150.0);

        assert!(warp.bow_mm <= 1e-12, "{:?}", warp.bow_mm);
        assert!(warp.bow_percent <= 1e-12);
    }

    /// A uniform moment bends a panel of any shape into the same spherical cap,
    /// so the surface follows from the curvature alone and bow from the
    /// panel's own diagonal. The cap is one of the polynomials, so the solve
    /// has to return it exactly, and the stations bow is read at have to catch
    /// its middle whether the cell counts are even or odd.
    #[test]
    fn a_uniform_moment_bends_any_panel_to_its_spherical_cap() {
        let response = whole(&symmetric_four_layer());
        let moment = 0.01;
        let curvature = response.moment_coefficient_gpa_per_k * 150.0 * moment
            / response.spherical_rigidity_gpa_mm3;
        for (width, height, cells) in [
            (400.0, 400.0, (20, 20)),
            (400.0, 800.0, (7, 13)),
            (800.0, 200.0, (96, 24)),
        ] {
            let bounds = BBox::new(
                Point::new(10.0, -20.0),
                Point::new(10.0 + width, height - 20.0),
            );
            let warp = estimate_warp(&response, &field(bounds, cells, |_, _| moment), 150.0);

            // w = kappa (X^2 + Y^2) / 2 levelled onto the corners rises from the
            // centre to the corners by an eighth of the squared diagonal.
            let expected = curvature * (width * width + height * height) / 8.0;
            assert!(
                (warp.bow_mm - expected).abs() <= 1e-9 * expected,
                "{width} x {height}: {} != {expected}",
                warp.bow_mm,
            );
            for (index, height) in warp.deflection.values.iter().enumerate() {
                let offset = warp.deflection.cell_center(index) - bounds.center();
                let cap = curvature * (offset.x * offset.x + offset.y * offset.y) / 2.0 - expected;
                assert!((height - cap).abs() <= 1e-9 * expected, "{offset:?}");
            }
        }
    }

    /// An astigmatic surface rises along one axis and dips along the other,
    /// both sides of the corner plane. Bow is the larger lobe -- the departure
    /// a seated panel actually shows -- not the two lobes summed.
    #[test]
    fn an_astigmatic_surface_reports_its_larger_lobe_as_bow() {
        let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(400.0, 400.0));
        let moment = field(bounds, (20, 20), |x, y| 0.01 * (x * x - y * y));
        let warp = estimate_warp(&whole(&symmetric_four_layer()), &moment, 150.0);

        let (low, high) = warp
            .deflection
            .values
            .iter()
            .fold((f64::MAX, f64::MIN), |(low, high), value| {
                (low.min(*value), high.max(*value))
            });
        // On a square panel the two lobes mirror each other, and both peak on
        // an edge, half a cell past the outermost cell centres.
        assert!(warp.bow_mm > 0.0);
        assert!((high + low).abs() <= 1e-9 * warp.bow_mm, "{low} {high}");
        assert!(high <= warp.bow_mm && high > 0.8 * warp.bow_mm, "{high}");
    }

    /// A free panel carries no load, so its moments do no work on the twist
    /// shape `xy`, and a thermal moment has no twisting component to do any
    /// either. The integral of `w_xy` over the panel is therefore zero, and
    /// that integral is the alternating sum of the corner heights: however
    /// the copper is distributed, the fourth corner seats with the other
    /// three.
    #[test]
    fn no_copper_distribution_lifts_a_corner_off_the_plane_of_the_other_three() {
        let response = whole(&symmetric_four_layer());
        for (width, height) in [(400.0, 400.0), (500.0, 200.0), (100.0, 400.0)] {
            let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(width, height));
            // Lopsided every way at once: a heavy quadrant, a diagonal ramp.
            let moment = field(bounds, (24, 18), |x, y| {
                0.01 * (f64::from(x > 0.2 && y < -0.1) + 0.5 * x * y + 0.3 * x - 0.2 * y * y * y)
            });
            let surface = PlateSurface::solve(
                &response,
                [width / 2.0, height / 2.0],
                &moment,
                1.0,
                SURFACE_DEGREE,
            );
            let bow = bow_of(&surface);
            assert!(bow > 0.0);
            for corner in surface.heights([2, 2]) {
                assert!(corner.abs() <= 1e-10 * bow, "{width} x {height}: {corner}");
            }
        }
    }

    /// A saddle-shaped imbalance seats on all four corners and bows between
    /// them. Turning the panel a quarter turn turns the saddle with it, so the
    /// bow has to come out the same.
    #[test]
    fn a_saddle_imbalance_bows_the_same_whichever_way_the_panel_is_turned() {
        let response = whole(&symmetric_four_layer());
        let saddle = |width: f64, height: f64, cells| {
            let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(width, height));
            estimate_warp(&response, &field(bounds, cells, |x, y| 0.01 * x * y), 150.0).bow_mm
        };
        let (upright, turned) = (
            saddle(400.0, 800.0, (16, 32)),
            saddle(800.0, 400.0, (32, 16)),
        );
        assert!(upright > 0.0);
        assert!(
            (upright - turned).abs() <= 1e-9 * upright,
            "{upright} != {turned}"
        );
    }

    /// Long-wavelength imbalance deflects far more than short-wavelength
    /// imbalance of the same amplitude — the result that makes a flat norm over
    /// the moment field the wrong thing to minimize.
    #[test]
    fn long_wavelength_imbalance_dominates_deflection() {
        let response = whole(&symmetric_four_layer());
        let ripple = |cycles: f64| {
            let moment = field(panel(), (80, 100), |x, _| {
                0.01 * (std::f64::consts::PI * cycles * (x + 1.0)).cos()
            });
            estimate_warp(&response, &moment, 150.0).bow_mm
        };

        // One cycle across the panel against three: same amplitude, and the
        // deflection falls with the square of the wavelength.
        assert!(
            ripple(1.0) > 5.0 * ripple(3.0),
            "{} {}",
            ripple(1.0),
            ripple(3.0)
        );
    }

    fn bow_of(surface: &PlateSurface) -> f64 {
        surface
            .heights([193, 193])
            .into_iter()
            .fold(0.0_f64, |bow, height| bow.max(height.abs()))
    }

    /// Moments that vary across the panel have no closed form on a free plate,
    /// so the assembly is held to bows an independent Ritz solve of the same
    /// plate returned: five by three, `nu = 0.3`, unit `M / D`, shapes in units
    /// of the longer half-side. A particular integral of
    /// `laplacian(w) = 2 M / (D (1 + nu))` that ignores the free edges puts the
    /// tilt at 0.098 and the saddle at 0.030, so these tell the two apart.
    #[test]
    fn low_order_imbalances_bow_a_free_plate_as_an_independent_solve_found() {
        let response = PlateResponse {
            moment_coefficient_gpa_per_k: 1.0,
            spherical_rigidity_gpa_mm3: 1.3,
            deviatoric_rigidity_gpa_mm3: 0.7,
        };
        let bounds = BBox::new(Point::new(-1.0, -0.6), Point::new(1.0, 0.6));
        let shapes: [fn(f64, f64) -> f64; 4] = [
            |x, _| x,
            |x, y| x * x - 0.36 * y * y,
            |x, y| x * x + 0.36 * y * y,
            |x, y| 0.6 * x * y,
        ];
        for (shape, expected) in shapes.into_iter().zip([0.105, 0.075, 0.163, 0.016]) {
            let moment = field(bounds, (200, 120), shape);
            let bow = bow_of(&PlateSurface::solve(
                &response,
                [1.0, 0.6],
                &moment,
                1.0,
                SURFACE_DEGREE,
            ));
            assert!((bow - expected).abs() <= 5e-4, "{bow} != {expected}");
        }
    }

    /// The polynomials are a truncation, so what they leave out has to be
    /// small on the fields that are hardest on them: copper that changes
    /// abruptly, the way it does between a board and the rail beside it.
    #[test]
    fn abrupt_copper_bows_as_a_far_richer_surface_says_it_does() {
        let response = whole(&symmetric_four_layer());
        let fields = [
            // One heavy half, one heavy corner, and rails around a striped array.
            field(panel(), (96, 96), |x, _| f64::from(x > 0.0)),
            field(panel(), (96, 96), |x, y| f64::from(x > 0.3 && y > 0.5)),
            field(panel(), (96, 96), |x, y| {
                f64::from(x.abs() > 0.9 || y.abs() > 0.92)
                    + 0.5 * f64::from((4.0 * x).rem_euclid(2.0) > 1.0)
            }),
        ];
        for moment in &fields {
            let bow = |degree| {
                bow_of(&PlateSurface::solve(
                    &response,
                    [200.0, 250.0],
                    moment,
                    1.0,
                    degree,
                ))
            };
            let (chosen, rich) = (bow(SURFACE_DEGREE), bow(28));
            assert!((chosen - rich).abs() <= 3e-3 * rich, "{chosen} != {rich}");
        }
    }

    /// Materials that expand alike cannot bend a panel however unevenly they
    /// are distributed: a bimetal's curvature is proportional to the difference
    /// of its expansions. Stiffness contrast alone must drive nothing.
    #[test]
    fn equal_expansion_predicts_no_warp_for_any_copper_imbalance() {
        let matched = Material {
            cte_ppm_per_k: Material::COPPER.cte_ppm_per_k,
            ..Material::LAMINATE
        };
        let core = |thickness_mm| StackLayer {
            thickness_mm,
            material: matched,
            is_conductor: false,
        };
        let stack = ThermalStack::new(vec![
            copper(0.035),
            core(0.5),
            copper(0.035),
            core(0.5),
            copper(0.035),
        ])
        .unwrap();
        let outer_arm = stack.conductor_weights()[0].moment_arm_mm2;
        for mismatch in [0.1, 0.5, 1.0] {
            let response = stack
                .response(matched, &[1.0, 1.0, 1.0 - mismatch])
                .unwrap();
            assert!(response.moment_coefficient_gpa_per_k.abs() <= 1e-15);
            let field = uniform_field(panel(), mismatch * outer_arm);
            let warp = estimate_warp(&response, &field, LAMINATE_RELAXATION_DROP_K);
            assert!(warp.bow_mm <= 1e-9, "{mismatch}: {}", warp.bow_mm);
        }
    }

    /// A film far thinner than its substrate is the one bimetal with a closed
    /// form that needs no stiffness bookkeeping: Stoney's
    /// `kappa = 6 Q_f t_f (a_f - a_s) dT / (Q_s h^2)`. A foil on one face only
    /// has to reproduce it.
    #[test]
    fn a_thin_foil_on_one_face_bends_to_stoneys_curvature() {
        let (foil_mm, core_mm) = (1e-5, 1.6);
        let stack =
            ThermalStack::new(vec![copper(foil_mm), laminate(core_mm), copper(foil_mm)]).unwrap();
        let bounds = panel();
        // Top foil whole, bottom foil etched away.
        let response = stack.response(Material::LAMINATE, &[1.0, 0.0]).unwrap();
        let field = uniform_field(bounds, stack.conductor_weights()[0].moment_arm_mm2);
        let warp = estimate_warp(&response, &field, LAMINATE_RELAXATION_DROP_K);

        let (film, substrate) = (Material::COPPER, Material::LAMINATE);
        let stoney = 6.0
            * film.biaxial_modulus_gpa()
            * foil_mm
            * (film.cte_ppm_per_k - substrate.cte_ppm_per_k)
            * 1e-6
            * LAMINATE_RELAXATION_DROP_K
            / (substrate.biaxial_modulus_gpa() * core_mm * core_mm);
        // A spherical cap seated on its corners rises an eighth of the squared
        // diagonal per unit curvature.
        let diagonal_squared = bounds.width().powi(2) + bounds.height().powi(2);
        let curvature = 8.0 * warp.bow_mm / diagonal_squared;
        assert!(
            (curvature - stoney).abs() <= 1e-3 * stoney,
            "{curvature} != {stoney}"
        );
    }

    /// Fabricators advise keeping mirrored layers within 10-15 % copper
    /// coverage of each other, and IPC-6012 accepts 0.75 % bow. The elastic
    /// mismatch modelled here does not connect the two: copper and laminate
    /// expand within about 1 ppm/K of each other below the glass transition,
    /// and on a production panel that reaches the limit only at a mismatch no
    /// panel can carry. The rule guards against what this model leaves out.
    #[test]
    fn elastic_mismatch_alone_does_not_reproduce_the_fabricator_copper_rule() {
        let stack = six_layer_panel();
        let outer_arm = stack.conductor_weights()[0].moment_arm_mm2;
        let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(457.2, 609.6));
        // Half copper throughout, the outer pair split evenly about it.
        let bow_at = |mismatch: f64| {
            let (top, bottom) = (0.5 + mismatch / 2.0, 0.5 - mismatch / 2.0);
            let response = stack
                .response(Material::LAMINATE, &[top, 0.5, 0.5, 0.5, 0.5, bottom])
                .unwrap();
            estimate_warp(
                &response,
                &uniform_field(bounds, mismatch * outer_arm),
                LAMINATE_RELAXATION_DROP_K,
            )
            .bow_percent
        };

        // An outer foil whole on one face and absent from the other is the
        // most a mirrored pair can differ by, and it stays under the limit.
        assert!(bow_at(1.0) < 0.75, "{} %", bow_at(1.0));
        // Imbalance only softens the plate, by pulling its neutral axis off
        // the middle, so the advised band sits at least as far under the limit
        // as it sits under total mismatch.
        assert!(bow_at(0.15) > 0.0 && bow_at(0.15) <= 0.15 * bow_at(1.0));
    }

    /// On a build symmetric about its mid-plane the material constant scales
    /// every deflection alike. It cancels from any comparison between two
    /// copper distributions, and the lever arms balancing draws on never see
    /// it.
    #[test]
    fn the_material_constant_cancels_between_copper_distributions() {
        let resin = Material {
            cte_ppm_per_k: 60.0,
            ..Material::LAMINATE
        };
        let build = |dielectric: Material| {
            let core = |thickness_mm| StackLayer {
                thickness_mm,
                material: dielectric,
                is_conductor: false,
            };
            ThermalStack::new(vec![
                copper(0.035),
                core(0.2),
                copper(0.035),
                core(1.06),
                copper(0.035),
                core(0.2),
                copper(0.035),
            ])
            .unwrap()
        };
        let shaped = |scale: f64| {
            field(panel(), (20, 20), |x, y| {
                scale * (0.01 + 0.004 * x - 0.003 * x * y + 0.002 * y * y)
            })
        };

        let response =
            |dielectric: Material| build(dielectric).response(dielectric, &[0.5; 4]).unwrap();
        let ratio = |dielectric: Material| {
            let bow = |field| estimate_warp(&response(dielectric), &field, 110.0).bow_mm;
            bow(shaped(0.4)) / bow(shaped(1.0))
        };
        // Two constants well apart, or the comparison shows nothing.
        let coefficient = |dielectric: Material| response(dielectric).moment_coefficient_gpa_per_k;
        assert!(coefficient(resin).abs() > 10.0 * coefficient(Material::LAMINATE).abs());
        assert!((ratio(Material::LAMINATE) - ratio(resin)).abs() <= 1e-9);
        assert_eq!(
            build(Material::LAMINATE).conductor_weights(),
            build(resin).conductor_weights()
        );
    }
}
