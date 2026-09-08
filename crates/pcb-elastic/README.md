# pcb-elastic

Small deterministic dense linear-elastic analysis, separate from PCB geometry,
meshing, import, fixtures, materials and manufacturing limits. Pure support
selection over caller-supplied stiffness contributions lives in `selection`.
`pcb-sim` owns SPICE/process integration; `pcb-ir` owns geometry. This crate takes
only numerical element coordinates and indexed contributions. It does not invent
another outline, mesh, attachment-search or manufacturing representation.

## Contract

`Model::new(scales, base, tolerances)` assembles immutable base stiffness.
`evaluate(optional_supports, loads, prescribed)` returns displacement, fᵀu
compliance, strain energy, Dirichlet reactions, optional support restoring forces,
free residuals and unsupported eigenmodes with effective-load projections.
Repeated calls do not accumulate supports. The solver always assembles and solves
the full system; there is no condensed approximation to validate or hidden pin.
Contributions must be symmetric positive semidefinite within the caller's explicit
numerical tolerance. The assembled base and each support-updated matrix are also
checked before applying constraints: local negative roundoff allowances cannot
accumulate beyond the global cutoff, even in an all-fixed evaluation. An unchanged
base is not checked again. Negative stiffness, invalid input and arithmetic failure
(including overflow of tolerance limits) are errors. Small eigenvalues are reported
as unsupported, not replaced by stiffness.

Let S contain the supplied characteristic DOF displacements. On free DOFs solve
`A q = b`, with `A = S K_ff S`, `b = S (f_f - K_fc u_c)`, `u_f = S q`.
Use the symmetric eigendecomposition and divide only by supported eigenvalues.
Rank cutoff is `rank_absolute + rank_relative * max(abs(eigenvalue))`.
Null-space solutions are minimum-norm in q, **not physical constraints**.
`SingularCompatible` still means nonunique deformation; `SingularIncompatible`
means no static equilibrium, and its displacement/compliance are only projected
diagnostics, never a feasible response. `Inaccurate` means the requested residual
accuracy was not met; inspect modes even with that status.

Residual norm is measured in scaled free coordinates; relative residual is
`||Aq-b|| / (||A||_F ||q|| + ||b||)` (zero for the zero system).
Compatibility separately checks the unsupported load norm against the explicit
absolute plus relative load tolerance, so soft directions cannot hide imbalance.
Original-unit free residuals and Dirichlet reactions are returned separately.
Prescribed nonzero displacements can do work, so fᵀu need not equal twice energy.
The same ordered finite input is deterministic; eigenbasis signs/rotations within
repeated eigenspaces are not a portable identity for comparing modes.

Use consistent units, for example N/mm: vertex w in mm, slope dimensionless,
beam EI in N·mm², plate bending tensor in N·mm, compliance in N·mm. Characteristic
scales for w and slopes must be explicit and physically coherent. Numerical
tolerances are not material properties or manufacturing acceptance thresholds.

## Support selection (ENG-1435)

`selection::select(model, candidates, conflicts, cases, fixed, max_subsets)`
returns candidate IDs, not geometry. A candidate contains an ID and any number
of stiffness contributions; selecting it counts as one support. Conflicts are
unordered pairs of candidate IDs. IDs must be unique; input candidate ordering
does not affect traversal. All inputs, including unreached candidates, are
validated before searching. Validation errors return `Err`, never infeasibility.
Each load case supplies its own finite positive compliance limit. `fixed` names
homogeneous Dirichlet DOFs: moving fixtures are intentionally outside this
objective, since their work changes the interpretation of fᵀu.

The lexicographic objective is exactly:

1. Minimize the number of selected candidates.
2. At that count, minimize the worst normalized compliance
   `max_l (f_lᵀ u_l / compliance_limit_l)`.
3. For exactly equal computed objectives, choose lexicographically smaller IDs.

The case limits therefore explicitly set both feasibility and relative response
importance. There are no inferred loads, physical defaults, distribution rules,
support margins, default 0.05 N·mm limit, or epsilon tie window. Feasibility uses
`compliance <= limit` with no added acceptance tolerance. Tolerances on the
`Model` control numerical rank/residuals, not compliance requirements.

**Admissibility:** require stable, unique static equilibrium for every load case.
Compatible singular systems are not admitted: their minimum-norm displacement
is nonunique and is not a physical restraint. Conservatively, *all* nonstable
statuses (compatible/incompatible singular and inaccurate), evaluation errors,
and independent verification failures remain unresolved, not certified
infeasible. Reports retain subset IDs, load-case indices and failure reasons.
Even when another case exceeds its limit, a numerical failure is retained and
withholds proof. Unsupported models may consequently have a verified feasible
incumbent but no minimum-count certificate.

Search enumerates combinations in increasing cardinality and sorted-ID order,
rejects conflicts, and completes the first layer containing a verified feasible
set. This considers arbitrary coupled replacements, not just single-site moves.
`max_subsets` caps visited subsets, including conflict rejections; it is not a
wall-clock limit and does not include input validation. No heuristic pruning or
local-optimum claim is involved. A call restarts from the beginning; no resumable
state is exposed. Budget exhaustion retains any incumbent and all diagnostics.

**Certificate argument:** the combination iterator visits each subset of a
cardinality once. Every smaller layer is exhausted before the first feasible
layer, and every set in that layer is compared before declaring an optimum.
Conflict rejection is exact. Without unresolved analyses this proves minimum
count and minimum *computed* response at that count. `count_lower_bound` advances
only over completely classified infeasible layers; `n+1` denotes exhaustive
infeasibility. The incumbent count/objective provide achievable upper values.
No response lower bound, numerical error interval, or exact-arithmetic/global
physical optimality certificate is claimed. Near thresholds/ties or poorly
conditioned models require separate numerical scrutiny. `Proof::Unresolved`
means the relevant traversal completed but certification failed;
`Proof::BudgetExhausted` means traversal is unfinished, with numerical failures
reported independently. Absence of an incumbent alone never proves infeasibility.

Every spectrally admissible case also takes an independent full Cholesky path
before a set can become an incumbent. It assembles selected contributions anew
onto the immutable base matrix, eliminates explicit fixed DOFs, factors the full
scaled free system and checks residuals and the original compliance limit.
Both displacement/compliance responses and residuals are returned. This checks
assembly of optional supports and a different solve algorithm; it shares the
already assembled base and input stiffness/scales, and cannot validate those
physical inputs. It is not a second call to `Model::evaluate` or condensation.
The objective uses spectral compliance; Cholesky is an independent admissibility
check, not an averaged objective or a hidden margin. This is a backward residual
check, not a forward-error bound. At PCB integration, reassemble the selected
complete mechanical model independently of candidate generation and verify it;
that integration acceptance remains ENG-1434/ENG-591 work.

**Cost and evidence:** with m candidates, n DOFs and L cases, worst-case traversal
is 2^m subsets and each full solve costs O(n³). Contribution validation also uses
dense spectra. No large-model scalability is claimed. Live numerical storage is
O(L n²) plus candidate data; failure diagnostics can grow as O(B L m) for budget
B. Tests compare 32 independent 8-candidate/4-DOF problems against a separately
assembled bitmask/Cholesky exhaustive oracle, with coupled off-diagonal stiffness,
multiple contributions, two load cases, conflicts, nonunit scales and fixed
DOFs. Tests also cover two-support replacement traps, count priority, ties,
budget boundaries, infeasibility, singularity, overflow and inaccurate solves.

Run the reproducible synthetic benchmark (seed 1435):

```sh
cargo test -p pcb-elastic --release --test selection benchmark_synthetic_stiffness -- --ignored --nocapture
```

Measured in an x86_64 Linux orb, optimized native Rust, two loads per case:

| DOFs | Candidates | Subset budget | Visited / spectral solves | Seconds | Result |
| ---: | ---: | ---: | ---: | ---: | --- |
| 12 | 20 | 2000 | 211 / 422 | 0.012094 | Exhaustive optimum: IDs [23,26], count 2, objective 0.8932730213381614 |
| 32 | 32 | 2000 | 529 / 1058 | 0.221149 | Exhaustive optimum: IDs [41,92], count 2, objective 0.9348780279410369 |
| 64 | 48 | 500 | 500 / 1000 | 1.204119 | Budget exhausted, no incumbent; count lower bound 2 |

All three runs report zero unresolved analyses. Times include selector input
validation, assembled global PSD validation and independent checks, but exclude fixture/base construction and
compilation; these are single-run measurements, not latency guarantees. The
64-DOF run does not prove infeasibility or optimality. Benchmark limits and
rank-one synthetic supports are test data, not manufacturing assumptions.

## Elements and mesh integration

- Euler–Bernoulli bending beam: `[w0, slope0, w1, slope1]`, explicit length/EI.
- Classical Morley nonconforming Kirchhoff triangle: quadratic w, three vertex
  displacements followed by normal slopes at midpoints of edges `(0,1),(1,2),(2,0)`.
  This is the six-DOF nonconforming triangular plate of Morley (1968), implemented
  directly by interpolation of the complete quadratic polynomial and exact
  constant-curvature energy. There is no rectangular production alternative.

References: [Morley's original element](https://doi.org/10.1017/S0001925900004546),
the [DOF definition](https://defelement.org/elements/morley.html), and
[Li, Guan & Mao's convergence analysis](https://doi.org/10.1016/j.cam.2013.12.024).
The basis is built in physical coordinates (shifted/scaled for arithmetic), not
by an incorrect scalar affine pullback of reference normal-derivative DOFs.

For each CCW pcb-ir mesh triangle, pass its three point coordinates. Number one w
per mesh vertex and one normal-slope DOF per mesh edge, separately within physical
components. Pick a canonical normal per edge. Pass +1 or -1 relative to the local
outward normal; neighboring triangles must share the DOF with opposite signs.
No DOF knowledge belongs in pcb-ir. Boundary vertex displacement and edge slope
constraints are caller-selected physical boundary conditions.

`MorleyTriangle::attachment(barycentric)` uses barycentric coordinates **only to
locate** a point, returning the element's quadratic shape rows for `[w, wx, wy]`.
Kirchhoff rotations about x/y are `[wy, -wx]`. For an explicit support stiffness C
on those channels and attachment rows H, assemble `Hᵀ C H`; apply generalized
forces as `Hᵀ f`. Coupling two elements uses the relative-displacement rows from
both elements in one contribution. This preserves virtual work and includes edge
slope DOFs; P1 weights are not bending shape functions.

Morley is nonconforming: only vertex w and midpoint normal slopes are shared,
not complete edge traces. Edge/vertex gradient attachments must name their element
side; no averaging or hidden attachment policy is supplied. Finite attachment
footprints require caller-specified integration and physical modeling. Point
rotational loads/supports can be mesh-sensitive; do not infer physical convergence
from a single point evaluation. Arbitrary triangular outlines are supported, but
the material/fixture model and its response convergence remain ENG-1434's work.

## Verification and limitations

`cargo nextest run -p pcb-elastic` runs hand-built matrices/meshes plus the
support-selection tests. Tests cover
beam analytical tip response and uniform-load refinement; triangle rigid modes,
quadratic curvature/attachment patch and pressure resultant; manufactured clamped
plate refinement on perturbed triangles; thickness scaling; optional supports
against independent full Cholesky solves; prescribed motion, singular compatible
and incompatible loads, invalid stiffness, and coordinate-unit scaling.

The manufactured solution is `w=x²(1-x)²y²(1-y)²`, with prescribed zero boundary
w/normal slope, explicit `q=Δ²w`, unit isotropic rigidity and Poisson ratio 0.3
(test data only). Its exact compliance is `8/3150 + 8/11025`. On perturbed square
triangulations with 2, 4, 8 and 16 subdivisions per side, absolute compliance
errors are 0.00887360, 0.00391602, 0.00118878 and 0.000318664. Final vertex RMS
displacement error is 0.000150938. These demonstrate convergence, not fine-mesh
engineering accuracy. The beam uniform-load compliance error falls by about 16×
per halving of mesh size, reaching 3.39084e-7 at eight elements.

The Morley element has no transverse shear variable and therefore no shear
locking in the thin limit. It is a low-order nonconforming discretization, not a
thick-plate, shell, membrane, torsion, nonlinear fracture or separation model.
Coarse meshes can be substantially too compliant. Shape-regular refinement and
independent physical validation are necessary. Dense O(n²) memory/O(n³) solves
are intended for small foundation models, not a scalable general FEM platform.
Pure-Rust nalgebra requires no BLAS, native process, GPU or platform solver.
PSD validation uses its bounded spectral routine; the public eigenvalues-only
routine has no iteration bound. The workspace optimizes this crate in dev/test
builds, retaining debug assertions and all validation on refinement fixtures.
Native tests and WASM compilation establish numerical/software behavior only,
not manufacturing physics or mouse-bite breaking safety.
