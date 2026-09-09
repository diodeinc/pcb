# pcb-mechanics

Pure geometry-to-mechanics adapter for explicit support candidates (including
calibrated mouse-bite connections). Depends on `pcb-ir` and `pcb-elastic`; neither
foundation crate depends on this integration layer. No importer, eligibility,
tab generation, material inference, fixtures policy, CLI, or manufacturing output.

## Inputs and use

`Assembly::new(plates, tolerances)` gives each board/frame mesh its own Morley
plate DOFs and assembles the immutable base `Model`. Supply an SPD bending tensor
per plate in global axes, and characteristic displacement/slope scales. Mesh
component identities also separate coincident vertices/edges. Normal-slope DOFs
use the right normal of the edge directed from smaller to larger vertex index;
incident CCW triangles receive opposite signs. `Dof` and `element_dofs` map
snapshot-local geometry identities to global response/load/fixture indices.
Meshes must have valid CCW topology from `AnalysisMesh`; approximation/refinement
status remains caller evidence, not a mechanical acceptance criterion.

Each `Connection` names two finite-area footprints and a shared reference point.
`candidate` returns the existing additive numerical candidate, preserving its ID.
Both ends remain flexible: board/frame and board/board connections are identical
operations. There is no implicit ground. Fixtures are explicit selected DOFs;
selection accepts homogeneous Dirichlet constraints only. Numerical nodal loads
can be supplied directly; a footprint resultant `[Fz, Mx, My]` maps with `Pᵀ f`.
The same rows give fitted displacement/rotation diagnostics `P u` after solving.

```rust
use pcb_elastic::{Error, selection::{self, LoadCase, Report}};
use pcb_mechanics::{Assembly, Connection, Dof};

fn choose(
    assembly: &Assembly,
    connections: &[Connection],
    cases: &[LoadCase], // global loads and explicit positive compliance limits
    fixtures: &[Dof],
    max_subsets: usize,
) -> Result<Report, Error> {
    let candidates = connections.iter()
        .map(|c| assembly.candidate(c)).collect::<Result<Vec<_>, _>>()?;
    let fixed = fixtures.iter()
        .map(|&d| assembly.dof(d)).collect::<Result<Vec<_>, _>>()?;
    selection::select(&assembly.model, &candidates, &[], cases, &fixed, max_subsets)
}
```

Pass supplied candidate-ID conflicts instead of `&[]` when applicable. The
existing budgeted exhaustive selector minimizes support count, then worst
normalized compliance. Its unchanged `Report` includes selected IDs, spectral
deformations/compliance/energy/reactions/residuals/modes, independent Cholesky
responses, budget state and unresolved subsets. An unsupported smaller subset
withholds an optimum certificate even if a feasible incumbent exists. Never
interpret absence of an incumbent as infeasibility or a projected singular
response as a physical restraint. This is not continuum/global physical optimality.

## Finite-area connection model

A footprint is a caller-supplied nonoverlapping partition into positive-area
triangular `Patch` pieces. Each piece names its plate, incident element, and three
barycentric vertices inside that element. Use mesh attachment/search APIs to
construct these locations; the weights locate points, not P1 bending values.
Splitting/clipping a physical landing against the mesh belongs to the caller.
Line/point footprints are rejected. Overlapping pieces are not unioned or
detected: repeated area would change the integration measure. A footprint may
span elements, but not physical components. Geometry IDs must be regenerated
after remeshing; boundary provenance and eligibility do not imply a footprint.

For reference r, define the rigid-plane row
`b(x,y) = [1, y-r_y, -(x-r_x)]`. The port is the area-L² projection

`P = (∫ bᵀ b dA)⁻¹ ∫ bᵀ N_e dA`,

where `N_e` is the explicitly sided Morley quadratic displacement row, scattered
to global DOFs. Thus `P u = [w(r), theta_x, theta_y]` of the best-fit plane.
It reproduces affine fields exactly; for those fields physical Kirchhoff
rotations are `[w_y, -w_x]`. For warped fields these are fitted rotations, not
point gradients or an average of element gradients. Degree-three quadrature
integrates both polynomial products exactly. The implementation centers the fit
at its area centroid, scales by sqrt(area), then transports to r; this avoids
conditioning the fit on a distant reference. Extremely thin/ill-conditioned
footprints and coordinate precision still require caller scrutiny.

For two footprints at the **same** reference, `H = P_a - P_b` and the connection
energy is `1/2 (H u)ᵀ C (H u)`. Its stiffness is `Hᵀ C H`, and force pullback
is `Hᵀ f`; these preserve virtual work and common rigid motion even when the
landings are separated. `C` must refer to that reference and the global rotation
axes; transporting/rotating a calibration requires the corresponding congruence
transform, not retaining an unrelated diagonal stiffness.

Units are N and mm: w in mm, rotations/slopes dimensionless, bending tensor in
N·mm, Cww in N/mm, Cwθ in N, Cθθ in N·mm, moments/compliance/energy in N·mm.
`C` is checked with the existing PSD validator using unit mm/radian channel
scales and supplied numerical tolerances; selection additionally validates the
global contribution using the plate DOF scales. No stiffness is inferred from
holes, tab width, thickness, a material name, or a beam approximation.

This is a three-channel elastic port idealization, **not** a rigid area constraint
or a pointwise spring bed: footprint warping orthogonal to the fitted plane is
unrestrained by the connection. Footprint shape/extent, reference and C jointly
define the calibration. Small footprints can be mesh-sensitive. Use physical
validation and shape-regular refinement of a fixed finite footprint. Classical
thin-plate bending only: no membrane, transverse shear, fracture, nonlinear
failure, or mouse-bite breaking certification. Existing tab geometry is unchanged.

## Verification and scale

`cargo nextest run -p pcb-mechanics` checks analytical asymmetric quadratic
area projection under refinement; force/moment virtual work; common motion;
rotated stiffness/normal signs; partial-patch subdivision/reference transport;
invalid footprints and hidden negative C; unsupported/free-frame configurations;
candidate non-accumulation; and selection against independent complete assembly
using physical polynomial interpolation, positive Gauss/Duffy integration and
direct Cholesky. That path shares only mesh and DOF identities with the adapter.

A ν=0 cantilever frame with a floating board and whole-area ports has exact
compliance `F²/Cww + F² L³/(20 D width) = 0.5158461538461538 N·mm` for the test
inputs. Refining the frame (38, 115, 371 total DOFs) gives absolute errors
0.03756668, 0.00911049, 0.00226478. This exercises solved finite-area coupling,
not a single-point convergence claim. These synthetic results do not certify
real PCB accuracy or any other attachment geometry/calibration.

Dense candidate storage is O(m n²), and exhaustive search has exponential subset
count with cubic full solves. No scalability upgrade or large-panel claim is
made. Small synthetic models are the intended current scope. Production wiring
still must supply landing partitions, calibrated stiffness, real fixtures/loads,
limits, geometry evidence, and independent selected-model acceptance.
