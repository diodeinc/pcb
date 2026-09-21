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
//! 3. Curvature follows the moment pointwise, `kappa = M / (D (1 + nu))` in
//!    each direction. This is exact for a uniform moment on a free plate and a
//!    quasi-static approximation for a slowly varying one.
//! 4. Deflection is the second integral of curvature. Integrating twice is what
//!    introduces the wavelength-squared weighting that makes long-wavelength
//!    imbalance dominate warp — the reason a flat norm over the moment field
//!    misreads the problem.
//!
//! Step 4 determines the surface only up to a harmonic function. Constants and
//! tilts leave with the corner plane. The `xy` term does not, and it is exactly
//! what a twist reading measures, so equilibrium fixes it rather than a choice
//! of particular solution. A free panel carries no load, so its moment
//! resultants do no work on any virtual deflection, and taking `xy` as that
//! deflection leaves `integral(M_xy) = 0` over the panel. A thermal moment is
//! isotropic and has no `xy` component, which makes this `integral(w_xy) = 0`
//! for the surface itself — and the integral of `w_xy` over a rectangle is
//! identically the alternating sum of its corner heights, the quantity 2.4.22
//! reads as twist. Copper therefore cannot twist a free panel at this order
//! however it is distributed, and no twist is estimated. What twists real
//! panels is weave skew and unbalanced layup, which make the plate itself
//! anisotropic and are outside this model.
//!
//! The pointwise free-edge conditions cannot stand in for that integral. No
//! low-order surface meets them all — the saddle leaves an edge shear whatever
//! its `xy` term — and imposing the corner-force condition alone while the rest
//! stay violated moves the reading as far the other way as ignoring it does.
//!
//! The model is **verified, not validated**: the tests below check it against
//! closed forms, symmetry, and linearity. Nothing here has been compared
//! against a measured panel, so results are estimates whose absolute scale
//! carries the uncertainty of the assumed temperature drop and moduli. Ratios
//! between panelizations of one stackup are far more trustworthy than absolute
//! values, because the material constant is common to both and cancels.
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

/// A scalar field sampled on the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelField {
    pub samples: Vec<Point>,
    pub values: Vec<f64>,
    pub bounds: BBox,
}

impl PanelField {
    /// Requires a panel with extent in both directions and one value per
    /// sample. Guaranteeing that here is what lets everything downstream divide
    /// by the span, normalise coordinates and fit shapes without re-checking.
    pub fn new(samples: Vec<Point>, values: Vec<f64>, bounds: BBox) -> Option<Self> {
        let sized = bounds.is_valid() && bounds.width() > 0.0 && bounds.height() > 0.0;
        (sized && !samples.is_empty() && samples.len() == values.len()).then_some(Self {
            samples,
            values,
            bounds,
        })
    }

    /// The length normalized coordinates are measured in: half the panel's
    /// longer side.
    fn half_span_mm(&self) -> f64 {
        self.bounds.width().max(self.bounds.height()) / 2.0
    }

    /// Panel coordinates in units of the half-span, centered on the panel.
    ///
    /// Both axes carry the same scale. The mode deflections solve
    /// `laplacian(w) = phi` in these coordinates, and that operator only keeps
    /// its form under a change of variables that scales both axes alike —
    /// stretching each to its own side would leave a different operator on
    /// every panel shape. So the longer side spans `[-1, 1]` and the shorter one
    /// spans less.
    fn normalized(&self, point: Point) -> (f64, f64) {
        let center = self.bounds.center();
        let half_span_mm = self.half_span_mm();
        (
            (point.x - center.x) / half_span_mm,
            (point.y - center.y) / half_span_mm,
        )
    }
}

/// The low-order shapes a moment field is decomposed into.
///
/// Deflection response grows with the square of wavelength and the acceptance
/// criteria are low-order shapes of the surface, so the warp a field produces
/// is carried almost entirely by these. Higher-order content is mechanically
/// inert by comparison, which is exactly why a flat norm over the field
/// misreads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    /// Constant. Drives spherical bow, and dominates warp in practice.
    Uniform,
    TiltX,
    TiltY,
    /// `xy`. Opposite corners heavy the same way.
    Saddle,
    /// `x^2 - y^2`. Cylindrical rather than spherical curvature.
    Astigmatic,
    /// `x^2 + y^2`. Curvature that varies from center to edge.
    ///
    /// Not mean-free: no fixed constant is, on a panel of any shape. The
    /// uniform mode carries the mean, which is what it is for.
    Domed,
}

impl PanelMode {
    pub const ALL: [Self; 6] = [
        Self::Uniform,
        Self::TiltX,
        Self::TiltY,
        Self::Saddle,
        Self::Astigmatic,
        Self::Domed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Uniform => "uniform",
            Self::TiltX => "tilt-x",
            Self::TiltY => "tilt-y",
            Self::Saddle => "saddle",
            Self::Astigmatic => "astigmatic",
            Self::Domed => "domed",
        }
    }

    /// The basis shape at normalized panel coordinates.
    fn evaluate(self, x: f64, y: f64) -> f64 {
        match self {
            Self::Uniform => 1.0,
            Self::TiltX => x,
            Self::TiltY => y,
            Self::Saddle => x * y,
            Self::Astigmatic => x * x - y * y,
            Self::Domed => x * x + y * y,
        }
    }

    /// A solution of `laplacian(w) = phi` in normalized coordinates, on a
    /// panel reaching `edge` from its centre.
    ///
    /// Deflection is the second integral of curvature, and this is that
    /// integral mode by mode. Any harmonic function may be added. Constants
    /// and tilts leave with the corner plane, and the `xy` term is the one the
    /// module documentation fixes: its mean `w_xy` over the panel is zero.
    /// Only the saddle has any to remove.
    fn deflection(self, x: f64, y: f64, edge: (f64, f64)) -> f64 {
        match self {
            Self::Uniform => (x * x + y * y) / 4.0,
            Self::TiltX => x * x * x / 6.0,
            Self::TiltY => y * y * y / 6.0,
            // `(x^3 y + x y^3) / 12` is symmetric in x and y, so turning the
            // panel turns the surface with it. Its `w_xy = (x^2 + y^2) / 4`
            // averages a twelfth of the squared corner distance, which the
            // `xy` term takes back out.
            Self::Saddle => x * y * (x * x + y * y - edge.0 * edge.0 - edge.1 * edge.1) / 12.0,
            Self::Astigmatic => (x.powi(4) - y.powi(4)) / 12.0,
            Self::Domed => (x.powi(4) + y.powi(4)) / 12.0,
        }
    }
}

/// How much of a field each low-order shape accounts for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeAmplitude {
    pub mode: PanelMode,
    /// Least-squares coefficient of this shape in the field's own units.
    pub amplitude: f64,
    /// Peak-to-valley deflection this shape alone would produce, millimeters.
    pub deflection_mm: f64,
}

/// Bow as IPC-TM-650 2.4.22 defines it, plus the field it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpEstimate {
    /// Deflection sampled at the same points as the moment field, millimeters,
    /// measured from the plane through the panel corners.
    pub deflection: PanelField,
    /// Largest single departure from that plane, millimeters. A surface that
    /// rises along one axis and dips along the other reports the larger lobe,
    /// not their sum.
    pub bow_mm: f64,
    /// Bow as a percentage of the panel dimension it is worst against.
    pub bow_percent: f64,
    pub modes: Vec<ModeAmplitude>,
}

/// Estimate warp from the geometric copper moment field.
///
/// `moment_field` is `sum_l t_l z_l rho_l(x)` in mm^2 — the quantity the copper
/// balance solver already computes — and `response` is the stack's at the mean
/// coverage of those same layers. `temperature_drop_k` is the effective
/// excursion from where the laminate stops relaxing down to room temperature,
/// which is the single largest source of uncertainty in the absolute result.
pub fn estimate_warp(
    response: &PlateResponse,
    moment_field: &PanelField,
    temperature_drop_k: f64,
) -> WarpEstimate {
    // GPa/K * K * mm^2 -> GPa mm^2, a moment resultant per unit width. An
    // equibiaxial moment `M` bends a free plate into a spherical cap of
    // curvature `M / (D (1 + nu))` in each direction, and the deflection
    // integral takes the Laplacian, their sum, as its source. The second
    // integral of curvature then picks up the squared half-span on the way
    // back to millimetres, since normalized coordinates are measured in
    // half-spans.
    let moment_scale = response.moment_coefficient_gpa_per_k * temperature_drop_k;
    let half_span_mm = moment_field.half_span_mm();
    let deflection_scale =
        moment_scale * 2.0 / response.spherical_rigidity_gpa_mm3 * half_span_mm * half_span_mm;

    let amplitudes = fit(moment_field, &moment_field.values, mode_shapes);
    let edge = moment_field.normalized(moment_field.bounds.max);

    // 2.4.22 measures from the plane the panel seats on: through its corners.
    // Each mode's deflection is known in closed form, so its corner plane is
    // too. No mode leaves an alternating corner residual, so the plane passes
    // through all four.
    let mut corner_planes = [[0.0_f64; 3]; PanelMode::ALL.len()];
    for (index, (mode, amplitude)) in PanelMode::ALL.iter().zip(&amplitudes).enumerate() {
        let corner = |x: f64, y: f64| deflection_scale * amplitude * mode.deflection(x, y, edge);
        let (pp, pm, mp, mm) = (
            corner(edge.0, edge.1),
            corner(edge.0, -edge.1),
            corner(-edge.0, edge.1),
            corner(-edge.0, -edge.1),
        );
        corner_planes[index] = [
            (pp + pm + mp + mm) / 4.0,
            (pp + pm - mp - mm) / (4.0 * edge.0),
            (pp - pm + mp - mm) / (4.0 * edge.1),
        ];
    }

    // One pass over the samples: the total corner-levelled surface and each
    // mode's own extremes.
    let mut deflection = Vec::with_capacity(moment_field.samples.len());
    let mut extremes = [(f64::MAX, f64::MIN); PanelMode::ALL.len()];
    for point in &moment_field.samples {
        let (x, y) = moment_field.normalized(*point);
        let mut total = 0.0;
        for (index, (mode, amplitude)) in PanelMode::ALL.iter().zip(&amplitudes).enumerate() {
            let plane = corner_planes[index];
            let levelled = deflection_scale * amplitude * mode.deflection(x, y, edge)
                - (plane[0] + plane[1] * x + plane[2] * y);
            extremes[index].0 = extremes[index].0.min(levelled);
            extremes[index].1 = extremes[index].1.max(levelled);
            total += levelled;
        }
        deflection.push(total);
    }

    // Bow is the largest departure a corner-seated panel makes from the table,
    // normalized by the dimension it is measured along — the shorter one gives
    // the larger percentage, so that is the one reported. The corners sit at
    // zero by construction, so that departure is the levelled surface's
    // largest magnitude on either side, not its range: a surface rising along
    // one axis and dipping along the other seats on whichever lobe it rests
    // and shows the other.
    let bow_mm = deflection
        .iter()
        .fold(0.0_f64, |bow, height| bow.max(height.abs()));
    let bow_percent = 100.0 * bow_mm
        / moment_field
            .bounds
            .width()
            .min(moment_field.bounds.height());

    let modes = PanelMode::ALL
        .iter()
        .zip(&amplitudes)
        .zip(&extremes)
        .map(|((mode, amplitude), (low, high))| ModeAmplitude {
            mode: *mode,
            amplitude: *amplitude,
            deflection_mm: high - low,
        })
        .collect();

    WarpEstimate {
        deflection: PanelField {
            samples: moment_field.samples.clone(),
            values: deflection,
            bounds: moment_field.bounds,
        },
        bow_mm,
        bow_percent,
        modes,
    }
}

/// The six low-order shapes at a normalised panel position.
fn mode_shapes(x: f64, y: f64) -> [f64; PanelMode::ALL.len()] {
    PanelMode::ALL.map(|mode| mode.evaluate(x, y))
}

/// Least-squares amplitudes of `shapes` in `values`, over the field's samples.
///
/// The shapes are orthogonal over a continuous rectangle but not over a finite
/// sample set, and a panel samples only the region inside its process margins,
/// which is neither. Projecting each shape independently would leak one into
/// another -- a uniform field picking up a spurious domed component. Solving
/// the normal equations is exact for any sample distribution.
fn fit<const N: usize>(
    field: &PanelField,
    values: &[f64],
    shapes: impl Fn(f64, f64) -> [f64; N],
) -> [f64; N] {
    let mut gram = [[0.0_f64; N]; N];
    let mut projection = [0.0_f64; N];
    for (point, value) in field.samples.iter().zip(values) {
        let (x, y) = field.normalized(*point);
        let shape = shapes(x, y);
        for row in 0..N {
            projection[row] += shape[row] * value;
            for column in 0..N {
                gram[row][column] += shape[row] * shape[column];
            }
        }
    }
    solve(gram, projection)
}

/// Gaussian elimination with partial pivoting.
///
/// A shape no sample distinguishes from the others leaves a negligible pivot.
/// Its amplitude is genuinely undetermined, so it is reported as zero and the
/// remaining shapes are still solved for.
fn solve<const N: usize>(mut matrix: [[f64; N]; N], mut rhs: [f64; N]) -> [f64; N] {
    const NEGLIGIBLE: f64 = 1e-12;
    let mut solution = [0.0; N];
    for pivot in 0..N {
        let best = (pivot..N)
            .max_by(|left, right| {
                matrix[*left][pivot]
                    .abs()
                    .total_cmp(&matrix[*right][pivot].abs())
            })
            .expect("the range starts at the pivot row");
        if matrix[best][pivot].abs() <= NEGLIGIBLE {
            continue;
        }
        matrix.swap(pivot, best);
        rhs.swap(pivot, best);
        let (pivot_row, pivot_rhs) = (matrix[pivot], rhs[pivot]);
        for row in pivot + 1..N {
            let factor = matrix[row][pivot] / pivot_row[pivot];
            for (target, source) in matrix[row].iter_mut().zip(&pivot_row).skip(pivot) {
                *target -= factor * source;
            }
            rhs[row] -= factor * pivot_rhs;
        }
    }
    for row in (0..N).rev() {
        if matrix[row][row].abs() <= NEGLIGIBLE {
            continue;
        }
        let known = (row + 1..N)
            .map(|column| matrix[row][column] * solution[column])
            .sum::<f64>();
        solution[row] = (rhs[row] - known) / matrix[row][row];
    }
    solution
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

    fn uniform_field(bounds: BBox, value: f64) -> PanelField {
        let mut samples = Vec::new();
        let mut values = Vec::new();
        for row in 0..21 {
            for column in 0..21 {
                let x = bounds.min.x + bounds.width() * column as f64 / 20.0;
                let y = bounds.min.y + bounds.height() * row as f64 / 20.0;
                samples.push(Point::new(x, y));
                values.push(value);
            }
        }
        PanelField::new(samples, values, bounds).unwrap()
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
        let [_, _, curvature_x, curvature_y] = solve(system, thermal);
        assert!((curvature_x - curvature_y).abs() <= 1e-12 * curvature_x.abs());
        curvature_x
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

    /// A uniform moment bends the panel into a spherical cap: all the
    /// deflection lands in the uniform mode.
    #[test]
    fn a_uniform_moment_produces_pure_bow() {
        let stack = symmetric_four_layer();
        let field = uniform_field(panel(), 0.01);
        let warp = estimate_warp(&whole(&stack), &field, 150.0);

        assert!(warp.bow_mm > 0.0);
        let uniform = warp
            .modes
            .iter()
            .find(|mode| mode.mode == PanelMode::Uniform)
            .unwrap();
        assert!((uniform.amplitude - 0.01).abs() <= 1e-9);
        for mode in &warp.modes {
            if mode.mode != PanelMode::Uniform {
                assert!(mode.amplitude.abs() <= 1e-9, "{mode:?}");
            }
        }
    }

    /// A uniform moment bends a panel of any shape into the same spherical cap,
    /// so bow follows from the curvature and the panel's own diagonal alone.
    /// Nothing about the fit may reintroduce the panel's aspect ratio.
    #[test]
    fn a_rectangular_panel_bows_to_the_spherical_cap_its_curvature_implies() {
        let response = whole(&symmetric_four_layer());
        let moment = 0.01;
        for (width, height) in [(400.0, 400.0), (400.0, 800.0), (800.0, 200.0)] {
            let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(width, height));
            let warp = estimate_warp(&response, &uniform_field(bounds, moment), 150.0);

            // w = kappa (X^2 + Y^2) / 2 levelled onto the corners rises from the
            // centre to the corners by an eighth of the squared diagonal.
            let curvature = response.moment_coefficient_gpa_per_k * 150.0 * moment
                / response.spherical_rigidity_gpa_mm3;
            let expected = curvature * (width * width + height * height) / 8.0;
            assert!(
                (warp.bow_mm - expected).abs() <= 1e-9 * expected,
                "{width} x {height}: {} != {expected}",
                warp.bow_mm,
            );
        }
    }

    /// An astigmatic surface rises along one axis and dips along the other,
    /// both sides of the corner plane. Bow is the larger lobe -- the departure
    /// a seated panel actually shows -- not the two lobes summed.
    #[test]
    fn an_astigmatic_surface_reports_its_larger_lobe_as_bow() {
        let stack = symmetric_four_layer();
        let bounds = BBox::new(Point::new(0.0, 0.0), Point::new(400.0, 400.0));
        let mut field = uniform_field(bounds, 0.0);
        field.values = field
            .samples
            .iter()
            .map(|point| {
                let (x, y) = field.normalized(*point);
                0.01 * (x * x - y * y)
            })
            .collect();
        let warp = estimate_warp(&whole(&stack), &field, 150.0);

        let (low, high) = warp
            .deflection
            .values
            .iter()
            .fold((f64::MAX, f64::MIN), |(low, high), value| {
                (low.min(*value), high.max(*value))
            });
        // On a square panel the two lobes match, so the range is twice the bow.
        assert!(warp.bow_mm > 0.0);
        assert!(
            (warp.bow_mm - high.max(-low)).abs() <= 1e-12,
            "{} != {}",
            warp.bow_mm,
            high.max(-low)
        );
        assert!(
            ((high - low) - 2.0 * warp.bow_mm).abs() <= 1e-9 * warp.bow_mm,
            "range {} vs bow {}",
            high - low,
            warp.bow_mm
        );
    }

    /// The integral of `w_xy` over the panel is the alternating sum of its
    /// corner heights, and equilibrium makes it zero for any isotropic thermal
    /// moment. Every mode has to leave the four corners in one plane, on a
    /// panel of any shape, so that no copper distribution reads as twist.
    #[test]
    fn no_mode_lifts_a_corner_out_of_the_plane_of_the_other_three() {
        for edge in [(1.0, 1.0), (1.0, 0.4), (0.25, 1.0)] {
            for mode in PanelMode::ALL {
                let corner = |x: f64, y: f64| mode.deflection(x * edge.0, y * edge.1, edge);
                let lift =
                    corner(1.0, 1.0) + corner(-1.0, -1.0) - corner(1.0, -1.0) - corner(-1.0, 1.0);
                assert!(lift.abs() <= 1e-15, "{mode:?} on {edge:?}: {lift}");
            }
        }
    }

    /// A saddle-shaped imbalance seats on all four corners and bows between
    /// them. Turning the panel a quarter turn turns the saddle with it, so the
    /// bow has to come out the same.
    #[test]
    fn a_saddle_imbalance_bows_between_coplanar_corners() {
        let stack = symmetric_four_layer();
        let saddle = |bounds| {
            let mut field = uniform_field(bounds, 0.0);
            field.values = field
                .samples
                .iter()
                .map(|point| {
                    let (x, y) = field.normalized(*point);
                    0.01 * x * y
                })
                .collect();
            estimate_warp(&whole(&stack), &field, 150.0)
        };
        let warp = saddle(panel());

        let upright = saddle(BBox::new(Point::new(0.0, 0.0), Point::new(400.0, 800.0)));
        let turned = saddle(BBox::new(Point::new(0.0, 0.0), Point::new(800.0, 400.0)));
        assert!(
            (upright.bow_mm - turned.bow_mm).abs() <= 1e-9 * upright.bow_mm,
            "{} != {}",
            upright.bow_mm,
            turned.bow_mm
        );

        assert!(warp.bow_mm > 0.0);
        let bounds = warp.deflection.bounds;
        for (point, height) in warp.deflection.samples.iter().zip(&warp.deflection.values) {
            let on_corner = (point.x == bounds.min.x || point.x == bounds.max.x)
                && (point.y == bounds.min.y || point.y == bounds.max.y);
            assert!(
                !on_corner || height.abs() <= 1e-12 * warp.bow_mm,
                "{point:?}"
            );
        }
        let saddle = warp
            .modes
            .iter()
            .find(|mode| mode.mode == PanelMode::Saddle)
            .unwrap();
        assert!((saddle.amplitude - 0.01).abs() <= 1e-9);
        let uniform = warp
            .modes
            .iter()
            .find(|mode| mode.mode == PanelMode::Uniform)
            .unwrap();
        assert!(uniform.amplitude.abs() <= 1e-9);
    }

    /// Long-wavelength imbalance deflects far more than short-wavelength
    /// imbalance of the same amplitude — the result that makes a flat norm over
    /// the moment field the wrong thing to minimize.
    #[test]
    fn long_wavelength_imbalance_dominates_deflection() {
        let stack = symmetric_four_layer();
        let bounds = panel();
        let ripple = |cycles: f64| {
            let mut field = uniform_field(bounds, 0.0);
            for (point, value) in field.samples.iter().zip(&mut field.values) {
                let phase =
                    std::f64::consts::TAU * cycles * (point.x - bounds.min.x) / bounds.width();
                *value = 0.01 * phase.cos();
            }
            estimate_warp(&whole(&stack), &field, 150.0).bow_mm
        };

        // One cycle across the panel against five: same amplitude, far more
        // deflection from the longer wave.
        assert!(
            ripple(1.0) > 5.0 * ripple(5.0),
            "{} {}",
            ripple(1.0),
            ripple(5.0)
        );
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
            let mut field = uniform_field(panel(), 0.0);
            field.values = field
                .samples
                .iter()
                .map(|point| {
                    let (x, y) = field.normalized(*point);
                    scale * (0.01 + 0.004 * x - 0.003 * x * y + 0.002 * y * y)
                })
                .collect();
            field
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
