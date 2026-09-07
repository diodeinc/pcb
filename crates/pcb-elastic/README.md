# pcb-elastic

Small deterministic dense linear-elastic analysis, separate from PCB geometry,
meshing, import, fixtures, materials, support selection and manufacturing limits.
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
numerical tolerance. Negative stiffness, invalid input and arithmetic failure are
errors. Small eigenvalues are reported as unsupported, not replaced by stiffness.

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

`cargo nextest run -p pcb-elastic` runs hand-built matrices/meshes only. Tests cover
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
Native tests and WASM compilation establish numerical/software behavior only,
not manufacturing physics or mouse-bite breaking safety.
