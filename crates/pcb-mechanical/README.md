# Experimental whole-panel mechanical translation (ENG-1434)

This library builds a **numerical analysis problem, not manufacturing acceptance**.
ENG-1434's physical measurements and qualified manufacturing preset acceptance
remain **UNFULFILLED**. No laminate/thickness range is qualified. No placement
optimization, CLI enabling, importer, mesher, or fabrication export is added.

## Contract and use

1. Call `Panel::from_physical` with existing `BoardPhysicalView` instances, rigid
   placements, the actual retained frame, and panel-coordinate `TabGeometry`.
   `from_regions` accepts the same canonical `ContourSet` substrate evidence
   for pure in-memory models. Do not reconstruct outlines or import IPC again.
2. Supply one `Laminate`: measured thickness and a symmetric positive definite
   equivalent plane-stress tensor Q in **panel stock axes**, N/mm². The entire
   panel must have that equivalent material. Rotating board artwork in one
   panel does not rotate the stock's weave. Distinct bonded materials, ply
   stacking, unsymmetric laminates and membrane/bending coupling are unsupported.
   Source thickness/material ambiguities remain diagnostics, never fallback FR4.
   The caller must reconcile explicit material input with source metadata.
3. `discretize` uses `pcb_ir::geom::mesh` constrained Delaunay refinement, with
   explicit area/angle and vertex/DOF budgets. Incomplete refinement and DOF
   exhaustion return errors, not a softer model or physical infeasibility.
4. Inspect `Discretization::mesh()` and resolve actual fixture subedges and
   physical element sides **on that snapshot**. Boundary entries retain the
   original final-region ring/segment and parameter intervals. Partial rail
   coverage requires matching boundary subdivision, not snapping or selecting
   every edge of every cell. Remeshing invalidates these indices.
5. Supply `ProcessCase` loads, rail restraints and optional measured bilateral
   tooling springs. `problem` returns element contributions, scaling, consistent
   forces, exact homogeneous Dirichlet DOFs, and unvalidated diagnostics.
   Feed these to `pcb_elastic::Model::new` and `evaluate` with explicit numerical
   tolerances. Check status, unsupported modes, equilibrium and reactions.
   The translator never runs the solver. `observation` returns P2 displacement
   and gradient rows for deflection/rotation comparisons, not pass/fail.

The opinionated experimental model is a homogeneous, small-deflection,
quasistatic Kirchhoff plate with the experimental SparkFunShallow tab geometry.
There is no implicit load amplitude, support-margin knob, tuned tab spring,
stiffness floor, or compliance limit. Analysis controls are library inputs for
verification, not a user-facing manufacturing preset.

## Material counting and kinematics

Let B be the union of nominal board instances and retained frame. For tabs i,
let T_i be full retained stock and H_i the perforation masks. The material is

`S = (B union union_i(T_i minus B)) minus union_i(H_i)`.

Thus tab shoulders/necks are continuum material exactly once. A tab's local
router removal must **not** be subtracted globally: that would erase other
tabs. Subtract holes last so unioning full board/support stock cannot refill
another tab's holes or remove its intrusion. Input tabs must be constructed
against the supplied board/frame stock; this function does not certify their
provenance, obstacle clearance, cutter access, or final panel manufacturability.

Mesh components have separate vertex IDs even when coordinates touch. Vertex
DOFs are transverse w; each unique edge has one normal derivative DOF. Its
canonical normal is `(dy,-dx)/length` directed from lower to higher vertex ID.
CCW element edge signs are +1 in that direction, -1 otherwise. Tab coupling
is shared plate DOFs across actual contiguous material, not penalties or a
beam laid over a plate. The beam API is used only as an independent strip
benchmark; adding it over continuum tab material would double-count stiffness.

`D = Q t³/12`, `kappa = [w,xx, w,yy, 2w,xy]`, and
`U = (1/2) integral(kappa^T D kappa dA)`. Morley integrates this exactly per
triangle. Pressure uses its consistent P2 load integration, not nodal lumping.
Point attachment barycentric weights locate an explicitly selected element
side; the actual rows are quadratic. Moment work is
`Fz*w + Mx*w,y - My*w,x`. Shared-edge P2 traces are nonconforming, not equal;
choose a physical side and verify refinement, including attachment response.

Clamping prescribes boundary vertex w and boundary midpoint normal slope;
simple support prescribes only w. Other boundary edges are free. Linear
tooling springs add `k r_w^T r_w`, with no artificial large penalty. They
assume maintained bilateral contact. Unilateral support, preload, friction,
fixture motion and rail compliance beyond the explicitly modeled plate/spring
are not inferred. Pin/tool compliance must be measured before using that model.

## Defensible process-load proposal, NOT a qualified preset

Authoritative sources inspected on 2026-09-07:

* [ASMPT DEK PCB support solutions](https://smt.asmpt.com/en/products/process-support-products/pcb-support-solutions/)
  describes support bars between conveyors, custom tooling, vacuum carriers,
  and locking support pins for printing/testing; it separately describes
  support for placement vibration. This contradicts assuming that conveyor
  rails alone or independently clamped cells represent every assembly process.
* [IPC-9704A official product description](https://shop.electronics.org/ipcjedec-9704/ipcjedec-9704-standard-only/Revision-a/english)
  calls for objective strain **and strain-rate** measurements during assembly,
  test and operation and notes differing package/solder/laminate failure modes.
  Only the public description was inspected, not the paywalled standard.
  It supplies no universal force or compliance threshold here.

Proposed characterization cases (amplitudes intentionally **not invented**):

| Process | Inputs required before prediction is meaningful |
| --- | --- |
| Conveyor/handling | Actual rail contact lengths, clamp/slip/lift-off behavior, panel orientation, measured acceleration spectrum, substrate areal mass and component masses/locations |
| Stencil printing | Machine squeegee force history, contact footprint and travel, stencil/load transfer, actual support bars/pins/vacuum and measured compliance |
| Placement/test | Nozzle/probe force-time history and footprint, location sequence, underside tooling map, measured vibration; quasistatic approximation needs timescale evidence |

For measured areal mass m in kg/mm² and normal acceleration a in mm/s²,
equivalent quasistatic pressure is `p = m*a/1000` N/mm². Component forces are
`F = mass_kg*a/1000` N, at measured attachment/load-transfer sites, not inferred
from ambiguous body/courtyard outlines. Moving loads require separate cases.
Element pressure is piecewise constant on **whole selected triangles**: refine
and align contact patches explicitly; do not silently rasterize a small nozzle.
Point loads are idealizations with local stress singularities, not contact models.

Require process-owner limits on specified deflections/rotations at process
sites, package/laminate strain and strain-rate limits, uncertainty and margins,
plus measured unloaded flatness. Compare observed deflections and reactions
to instrumented panels under the same load and fixture conditions. Compliance
is useful for comparisons **only with the same force distribution and scale**.
It scales quadratically with force and cannot by itself establish acceptable
printing or package strain. The exploratory **0.05 N·mm is neither adopted
nor replaced with an invented number**. Missing evidence always returns
unvalidated diagnostics; supplying a provenance string does not qualify a case.

## Verification and acceptance gaps

`cargo test -p pcb-mechanical -- --nocapture` runs source-independent geometry,
P2 virtual work, component isolation, beam/plate analytical, fixture, thickness,
whole-panel and perforated-tab sensitivity checks. No snapshots are accepted.
Synthetic unit loads/materials exercise the linear operator, not a real PCB at
those deflections. Scale loads down for small-deflection physics; none are
production load proposals.

The exact nu=0 clamped strip with uniform pressure has compliance
`p² b L⁵/(20D)`. At L=4, b=1, p=D=1 the exact value is 51.2. Shared CDT gives
57.3472 / 52.6203 / 51.4856 at 27 / 85 / 353 DOFs, errors 12.01% / 2.774% /
0.558%; thickness 0.5 / 1 / 2 gives the exact inverse-cube response scaling.
Two disconnected components retain six rigid modes. P2 displacement/gradient
and physical moment virtual work reproduce a quadratic field to 1e-11.

The actual experimental perforated-tab polygon gives unit-force compliance
57.027823 (1193 DOFs, max area 0.99599 mm²) and 56.605078 (1464 DOFs, max area
0.25 mm²), with minimum angles 20.2449°. This is 0.747% two-mesh agreement,
not a certified error bound or proof of asymptotic convergence. The undrilled
comparison gives 54.771371 (1312 DOFs), so perforation increases compliance
3.348% for this particular geometry/fixture/load. These three dense debug
solves take about 557 seconds in the development orb. Run this expensive
check separately with `cargo test -p pcb-mechanical perforated_tab -- --nocapture`;
the remaining five checks use
`cargo nextest run -p pcb-mechanical -E 'not test(perforated_tab)'`.
Native checks and clippy pass; `cargo check -p pcb-mechanical --target
wasm32-unknown-unknown` also passes (compile only, not WASM runtime validation).

For the synthetic two-pad common rail, end-clamped compliance is 31.0803;
end simple support is 50.2105; a 1 N/mm tooling spring gives 0.980473.
Artificially fixing the local rail gives 12.1653, understating whole-panel
compliance by a factor 2.55484. These are fixture-sensitivity examples, not
universal ratios. A local-cell approximation is defensible only after showing
that actual global rail displacements/rotations and neighboring-cell coupling
change the process observations by less than a **process-specified** tolerance,
for all relevant loads. No such production tolerance is currently available.

Outstanding before manufacturing acceptance:

* Lot-, direction- and process-temperature-dependent laminate thickness and Q
  measurements/datasheets, and qualification of the equivalent symmetric model.
* Actual machine load/contact/fixture maps, calibrated compliance, preload,
  acceleration/time histories and the process-specific allowable observations.
* Whole-panel deflection/strain measurements, fixture swaps and multiple panel
  sizes; numerical/geometric refinement studies for each intended geometry.
* Physical tab and break coupons: same stackup, drill/router tolerances, weave
  orientation, load/unload curves, break force/displacement and failure locus,
  residual nubs/delamination/copper damage and variability across samples.
  SparkFunShallow Ø.381/pitch.635/nominal ligament.254 mm with outward.127 and
  intrusion.0635 mm is **experimental**; the offset is not SparkFun-tested.

Omissions: copper stiffening, original small drills/slots (only explicit tab
perforations and substrate profile voids are included), component stiffness,
fracture, stress concentrations, transverse shear, membrane effects and
3D/beam torsion. Plate twisting curvature is included. Thin plate assumptions
are particularly doubtful when substrate thickness is comparable to the
0.254 mm nominal ligaments; refinement cannot cure missing 3D/shear physics.
No stress or break-strength prediction is qualified. Dense solve cost is
O(n³), storage O(n²), with several simultaneous matrices; DOF budgets are
explicit but not an allocation guarantee. Resource exhaustion must not be
replaced by clamped-cell or ad hoc tab-stiffness heuristics.

## Canonical source smoke (native development example)

The corpus owner supplies canonical fixtures; this runner does not clone repos,
inventory sources, export KiCad/IPC, reinterpret overlays, or infer panel tabs.
Run `cargo run --release -p pcb-mechanical --example corpus_smoke -- PATH.json.zst ...`.
It uses `pcb_corpus::load` and the fixture's substrate region, without treating
unknown-span drill/rout or component-envelope overlays as substrate removal.

This is deliberately an **unrestrained free-body diagnostic**, not a fabricated
rail fixture. Synthetic thickness 1 mm, Q=diag(12,12,6) N/mm² and pressure
1e-6 N/mm² exercise the software only. A completed smoke must report
`SingularIncompatible`: the unrestrained substrate cannot equilibrate its net
transverse load. No computed compliance is presented as a supported response;
physical metrics remain unavailable. Mesh targets are 25 mm² / 20°, with 3000
additional vertices and 1500 DOFs as explicit smoke resource budgets. Missing
files, geometry/refinement/resource/numerical failures return nonzero, not a
manufacturing failure or a silently simplified model.

All seven genuine checked-in demo layouts supplied by the corpus owner complete
this software check using its KiCad 10.0.6 IPC-C/mm/precision-6 extractions:

| Canonical fixture | DOFs | Rigid modes | Substrate area (mm²) |
| --- | ---: | ---: | ---: |
| demo-bramble | 1111 | 3 | 3479.489536 |
| demo-demeter | 158 | 3 | 960.000000 |
| demo-feign | 93 | 3 | 606.900000 |
| demo-governor | 190 | 3 | 1281.062500 |
| demo-marlow | 589 | 3 | 763.057173 |
| demo-renfield | 110 | 3 | 697.000000 |
| demo-seward | 75 | 3 | 448.000000 |

Each has the expected incompatible free-body response and meets mesh targets.
Source thickness/material/envelope evidence and exact revisions/layout/XML
hashes remain in the canonical fixtures. The reported source thickness is not
silently substituted into the synthetic model. No actual equivalent laminate,
panel frame/tab layout, fixtures, process loads, or allowable response is
qualified. Seward's source README explicitly warns that the latest hardware
changes still need layout synchronization and bench validation. None of these
results establishes source/layout synchronization, DRC or manufacturing readiness.
