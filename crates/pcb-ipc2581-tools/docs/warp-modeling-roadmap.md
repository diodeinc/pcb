# Warp modelling: theory, metrics, and what to build

This document works out the physics that copper balancing is implicitly
optimising, shows exactly where the current implementation sits relative to
that physics, and proposes what to build.

Two constraints shape everything below.

**Analysis comes before optimisation.** Predicting and reasoning about warp is
independent of the solver: the forward model can be built and exercised without
changing a line of the optimiser, and until it exists there is no defensible
way to judge whether a solver change helped. Most of the work here is analysis.
The optimiser is touched last, on purpose.

**We cannot measure panels.** There is no metrology loop — no measured bow to
correlate against, no calibration constant to fit. This is a hard limit, and it
determines what the model can honestly claim. We can *verify* that the
mathematics is implemented correctly against closed-form cases. We cannot
*validate* that it predicts reality. Those are different things, and the
difference is load-bearing for how the output should be labelled and used.

---

## Part I — Theory

### 1. Where copper density enters

A panel is a laminate. Classical lamination theory relates its in-plane force
resultants $\mathbf{N}$ and bending moment resultants $\mathbf{M}$ to midplane
strain $\boldsymbol{\varepsilon}^0$ and curvature $\boldsymbol{\kappa}$ through
the ABD matrices:

$$
\begin{bmatrix}\mathbf{N}\\ \mathbf{M}\end{bmatrix}
=\begin{bmatrix}\mathbf{A} & \mathbf{B}\\ \mathbf{B} & \mathbf{D}\end{bmatrix}
\begin{bmatrix}\boldsymbol{\varepsilon}^0\\ \boldsymbol{\kappa}\end{bmatrix},
\qquad
\mathbf{A}=\int\bar{\mathbf{Q}}\,dz,\quad
\mathbf{B}=\int\bar{\mathbf{Q}}\,z\,dz,\quad
\mathbf{D}=\int\bar{\mathbf{Q}}\,z^{2}\,dz
$$

where $\bar{\mathbf{Q}}(z)$ is the transformed reduced stiffness at height $z$
above the midplane. $\mathbf{B}$ is the bending–extension coupling and vanishes
for a laminate symmetric about the midplane.

A temperature change $\Delta T$ enters as equivalent thermal resultants. Note
the moment arm $z$ in the second integral — it is what makes $\mathbf{M}^{T}$ a
*moment*, and it is the term most often dropped when the pair is written
informally:

$$
\mathbf{N}^{T}=\Delta T\int\bar{\mathbf{Q}}\,\boldsymbol{\alpha}\,dz,
\qquad
\boxed{\;\mathbf{M}^{T}=\Delta T\int\bar{\mathbf{Q}}\,\boldsymbol{\alpha}\,z\,dz\;}
$$

**This is where copper density enters the physics**, and it enters through
*both* factors. A copper-rich region is stiffer ($\bar{\mathbf{Q}}$) *and* has a
different effective expansion ($\boldsymbol{\alpha}$). The driver is the product
$\bar{\mathbf{Q}}\boldsymbol{\alpha}$, not copper area alone.

### 2. Reducing to a copper-fraction model

Let $\rho_l(x)$ be the copper area fraction of layer $l$ at panel position $x$.
Copper and resin sit side by side in-plane, so they share strain, and the Voigt
(iso-strain) average is the appropriate in-plane homogenisation:

$$
\bar{\mathbf{Q}}_l\boldsymbol{\alpha}_l
=\rho_l\,\bar{\mathbf{Q}}^{\mathrm{cu}}\boldsymbol{\alpha}^{\mathrm{cu}}
+(1-\rho_l)\,\bar{\mathbf{Q}}^{\mathrm{res}}\boldsymbol{\alpha}^{\mathrm{res}}
$$

Substituting into $\mathbf{M}^{T}$ and discretising over layers of thickness
$t_l$ at height $z_l$:

$$
\mathbf{M}^{T}(x)=\underbrace{\Delta T\sum_l t_l z_l\,
\bar{\mathbf{Q}}^{\mathrm{res}}\boldsymbol{\alpha}^{\mathrm{res}}}_{\text{bare stack, zero if symmetric}}
+\;\Delta T\,\underbrace{\bigl[\bar{\mathbf{Q}}^{\mathrm{cu}}\boldsymbol{\alpha}^{\mathrm{cu}}
-\bar{\mathbf{Q}}^{\mathrm{res}}\boldsymbol{\alpha}^{\mathrm{res}}\bigr]}_{\textstyle \boldsymbol{\Lambda}\;(\text{constant})}
\sum_l t_l z_l\,\rho_l(x)
$$

The copper-driven part is **linear in $\rho_l$**, and its geometric factor is

$$
m(x)\;=\;\sum_l \underbrace{t_l z_l}_{w_l}\,\rho_l(x)
$$

which is **exactly the quantity the solver already computes.** The current
metric is the copper contribution to $\mathbf{M}^{T}$ with the constant material
prefactor $\Delta T\,\boldsymbol{\Lambda}$ stripped off.

That is a stronger result than it may look, and it matters more given we cannot
measure. For comparing two panelisations of one design on one stackup, the
prefactor cancels and the existing geometric metric is *exactly right*. What it
cannot do is produce a number in physical units, or capture the tensor character
of $\boldsymbol{\Lambda}$ when copper is directional.

### 3. From resultants to curvature

Free thermal loading of an unconstrained panel gives

$$
\boldsymbol{\kappa}=\bigl(\mathbf{D}-\mathbf{B}\mathbf{A}^{-1}\mathbf{B}\bigr)^{-1}
\bigl(\mathbf{M}^{T}-\mathbf{B}\mathbf{A}^{-1}\mathbf{N}^{T}\bigr)
$$

Two consequences:

- Minimising $\mathbf{M}^{T}$ is **not identical** to minimising curvature. When
  $\mathbf{B}\neq 0$, the in-plane resultant $\mathbf{N}^{T}$ feeds curvature
  too. For a near-symmetric stack $\mathbf{B}\approx 0$ and
  $\boldsymbol{\kappa}\approx\mathbf{D}^{-1}\mathbf{M}^{T}$.
- $\mathbf{M}^{T}$ is a **three-vector** $(M_{xx},M_{yy},M_{xy})$, not a scalar.
  Our $m(x)$ is its isotropic part only.

Because $\mathbf{A},\mathbf{B},\mathbf{D}$ themselves depend on $\rho_l(x)$, the
model linearises about the nominal (mean-copper) stack so the stiffnesses are
constant and only $\mathbf{M}^{T}$ varies with position. This is valid while
copper fractions vary modestly, which is the regime balancing operates in.

### 4. From a curvature field to warp — where scale enters

$\boldsymbol{\kappa}(x)$ is not what a fab measures. They measure out-of-plane
deflection. For a Kirchhoff plate carrying a spatially varying isotropic misfit
moment, the standard thermoelastic result is

$$
D\,\nabla^{4}w=-\frac{1}{1-\nu}\nabla^{2}M^{T},
\qquad D=\frac{Eh^{3}}{12(1-\nu^{2})}
$$

In Fourier space, with $\hat{m}(k)$ the transform of the moment field:

$$
\hat{w}(k)=\frac{\hat{m}(k)}{(1-\nu)D\,k^{2}}\;\;\propto\;\; \hat{m}(k)\,\lambda^{2}
$$

**Deflection response scales as wavelength squared.** The sharper statement:
curvature response $k^{2}\hat{w}=\hat{m}/((1-\nu)D)$ is *scale-free*; it is
deflection that carries $\lambda^{2}$, and deflection is what is measured.

The uniform component is handled by the boundary rather than the interior
equation — a free plate under uniform $M^{T}$ takes spherical curvature
$\kappa_0=M^{T}/(D(1+\nu))$, deflecting by $\approx\kappa_0 L^{2}/8$ over span
$L$. So the panel-scale component also carries $L^{2}$.

Over our range this is decisive: between a 5 mm feature and a 500 mm panel the
mechanical weighting differs by $(L/\lambda)^2 = 10^{4}$.

### 5. What the fab actually measures

[IPC-TM-650 2.4.22](https://www.electronics.org/sites/default/files/test_methods_docs/2.4.22c.pdf)
defines the two acceptance quantities geometrically:

| Quantity | Shape | Normalisation |
|---|---|---|
| **Bow** | "roughly cylindrical or spherical curvature", all four corners in one plane | $R_L = L\cdot B/100$, $R_W = W\cdot B/100$ |
| **Twist** | deformation "parallel to a diagonal", one corner lifted out of the plane of the other three | $R = 2\cdot D_{\text{diag}}\cdot T/100$ |

Acceptance per IPC-6012: **0.75 %** for boards carrying surface-mount
components, 1.5 % otherwise.

These are precisely the low-order modes of the deflection surface — bow is the
$x^2,y^2$ content, twist the $xy$ content. The standard is already a modal
decomposition, which is a strong hint about the right metric.

### 6. What copper can and cannot cause

Twist curvature $\kappa_{xy}$ is driven by the $B_{16},B_{26}$ coupling terms.
For a **cross-ply laminate — 0°/90° plies only — $A_{16}=A_{26}=B_{16}=B_{26}=D_{16}=D_{26}=0$**;
off-axis plies are required to make them non-zero, with bend–twist coupling
peaking near 30°.

A PCB layup is 0/90 glass weave. Therefore:

> An isotropic through-thickness copper-density asymmetry produces **bow**, and
> structurally **cannot produce twist**.

Twist originates in weave skew, layup asymmetry, and *directional* copper (a
field of parallel traces is orthotropic; a plane is not). Balancing with
isotropic hex perforation cannot address any of them. This bounds what this
entire effort can achieve and should be said out loud rather than discovered
later.

### 7. What copper balancing is and is not

It is tempting to unify every copper-evening technique as "minimise
$\mathbf{M}^{T}$". That over-reaches — these techniques share geometry but not
objectives:

| Technique | Primary objective |
|---|---|
| Balancing pours, dummy copper | $\mathbf{M}^{T}$ — warp |
| **Thieving** | **Plating current-density uniformity** — thieving patterns act as current robbers, cutting plating variation from 20–30 % to under 10 % |
| **Cross-hatching** | Flex compliance, impedance, adhesion — *and* balance |
| Density evening generally | **Etch loading** — etchant works faster in sparse regions, over-etching isolated traces |

Our objective already reflects this: it carries a local density-match term *and*
a moment term. The local term does etch and plating work at a scale where the
moment term is mechanically irrelevant, and vice versa. Collapsing them would
lose real physics.

---

## Part II — Gap analysis

| Dimension | Current state | Gap |
|---|---|---|
| Moment quantity $m=\sum w_l\rho_l$ | ✅ exactly the copper part of $\mathbf{M}^T$ | — |
| Material prefactor $\Delta T\boldsymbol{\Lambda}$ | ❌ stripped | no physical units |
| Layer weights $w_l=z_l t_l$ | ⚠️ geometric | should carry $\bar{\mathbf{Q}}\boldsymbol{\alpha}$; wrong across mixed copper weights |
| Neutral axis | ⚠️ geometric midplane | true axis is the stiffness centroid |
| $\mathbf{M}^T\rightarrow\boldsymbol{\kappa}$ | ❌ absent | no $\mathbf{D}^{-1}$, no $\mathbf{B}$ coupling |
| $\boldsymbol{\kappa}\rightarrow w$ (plate response) | ❌ absent | flat in $\lambda$ where physics is $\lambda^{2}$ |
| IPC bow/twist extraction | ❌ absent | no comparison to spec |
| Anisotropy (directional copper) | ❌ isotropic scalar | a plane and parallel traces at equal density are not equivalent |
| Convergence certificate | ❌ absent | cannot tell whether 512 iterations is right |
| Objective scale weighting | ❌ pointwise $m^2$ | over-weights short $\lambda$ by $(L/\lambda)^2$ |
| **Empirical validation** | ❌ **impossible for us** | **see Part IV** |

---

## Part III — What to build

Three groups. Within each, the pieces are largely independent; across them, the
forward model has to exist before anything reads from it.

### A. The forward model

Turn the geometric comparator into a physical estimator by restoring the terms
Part I identifies as missing.

**A1 — Material properties per layer.** A table keyed off stackup material
names: copper $E\approx117$ GPa, $\nu=0.34$, $\alpha=17$ ppm/K; FR-4 in-plane
$E\approx25$ GPa, $\alpha\approx14$–17 ppm/K. Fall back to documented defaults
with a warning when the stackup does not name a material.

**A2 — Elastic layer weights and a real neutral axis.** Replace $w_l=z_l t_l$
with $w_l=z_l t_l\Lambda$, and the geometric midplane with the modulus-weighted
centroid. This is where mixed copper weights (1 oz outers, 2 oz planes) stop
being silently mis-weighted.

**A3 — ABD assembly and the curvature map.** Build
$\mathbf{A},\mathbf{B},\mathbf{D}$ for the nominal stack; produce
$\mathbf{M}^{T}(x)$ and $\boldsymbol{\kappa}(x)$.

**A4 — Plate response and the modal response table.** Rather than deriving
free-edge plate solutions analytically — the boundary conditions are the fiddly
part, and harmonic components of $m$ produce no interior forcing, so the naive
$1/k^2$ reading needs care on a finite plate — compute the response numerically
**once per panel geometry**: for each basis mode $\varphi_k$, solve the plate
problem for unit $m=\varphi_k$, fit bow % and twist % from the resulting
surface, store a small table. Panel sizes change rarely, so it is cached.

**A5 — Simulated IPC measurement.** Fit a plane through the panel corners, take
max deviation, report **bow %** and **twist %** exactly as 2.4.22 defines them.
The metric is then a simulation of the acceptance test rather than an invented
score.

**A6 — The report.**

Two percentages are a lossy projection of data the forward model has already
computed. Every stage of the chain is a full field over the panel — per-layer
density $\rho_l(x)$, the moment field $m(x)$, the curvature field, the
deflection surface $w(x)$ — and the scalars are the last step that throws all
of it away. **The fields are already there; only the rendering is missing.**

The tool should emit a self-contained HTML report alongside the scalars, using
the SVG rendering the crate already does. The scalars stay on stdout for
scripting.

| View | Shows | Form | Colour job |
|---|---|---|---|
| **Predicted panel shape** | $w(x)$, the surface a flatness scanner would see | heat map + contours, panel geometry overlaid | **diverging**, zero at the neutral midpoint |
| **Moment field** | $m(x)$ — *where* the imbalance is | heat map + contours | **diverging**, zero-centred |
| **Per-layer copper** | $\rho_l(x)$ for each layer | small multiples, one per layer | **sequential**, shared scale across layers |
| **Through-stack profile** | each layer at its true $z$, bar length = density, sign of $w_l\rho_l$ | horizontal bars positioned by $z$ | diverging by contribution sign |
| **Modal breakdown** | $a_k$ raw vs after the $\lambda^2$ response | grouped horizontal bars, baseline at zero | two series, legend + direct labels |
| **Deflection by scale** | share of predicted deflection per spatial-scale band | single-series bar, log-scaled x | sequential |
| **Before / after** | any of the above, pinned vs balanced | side by side | **shared scale across both** |

The through-stack profile is the one a PCB engineer will read first — it makes
mirror-pair asymmetry visible at a glance, which is the thing balancing is
actually manipulating. The deflection-by-scale view is the one that makes §4
concrete: it should show most of the warp living in the longest-wavelength band,
and it is the chart that would have prevented us reading a flat RMS as if it
meant something.

Overlay board outlines, frame and gutters on every field map. Without them you
cannot tell whether an imbalance sits over a board (immutable) or over fillable
panel material (actionable), which is the first question anyone will ask.

**Design constraints**, mostly forced by the data rather than taste:

- **Diverging palettes for every signed field**, two hues with a **neutral grey
  midpoint pinned at zero** and a symmetric range. No rainbow colormaps — the
  classic scientific-visualisation failure, and it destroys the sign reading
  these fields exist to convey.
- **Sequential — one hue, light to dark — for unsigned magnitude** ($\rho_l$).
- **Shared colour scale** across per-layer small multiples and across any
  before/after pair. An independently scaled comparison is a lie.
- **Contours alongside the fill.** Redundant magnitude encoding, which keeps the
  maps readable under colour-vision deficiency and in print.
- **No dual-axis charts.** Two measures of different scale become two charts or
  are indexed to a common base.
- Legend whenever there are two or more series; direct labels when there are
  few; text in ink tokens, never in a series colour.
- Dark mode chosen from the same ramps and validated against the dark surface,
  not an automatic inversion.
- A table view of the scalars and modal coefficients, so nothing is available
  only as colour.
- Run a palette validator over the chosen ramps rather than eyeballing
  colour-blind separation.

*Deliverable:* `pcbc ipc2581 warp <panel> [--report warp.html]` — estimated
bow % and twist % against the IPC limit on stdout, and the full field report
when asked.

### B. Knowing what the model is worth

This group replaces what would ordinarily be empirical validation. It cannot
substitute for it, and should not be described as if it does.

**B1 — Verification against closed-form cases.** The mathematics can be checked
even when the physics cannot:

- a symmetric stack with symmetric copper → $\mathbf{M}^{T}=0$ → zero bow
- uniform copper everywhere → pure bow, zero higher modes
- mirroring the stack → curvature flips sign
- doubling $\Delta T$ → doubles curvature (linearity)
- a single copper layer on one face of a substrate → compare against
  **Stoney's formula** for thin-film-on-substrate curvature, a genuine
  independent closed form

These catch sign errors, neutral-axis mistakes, and unit slips — the failure
modes most likely to survive code review.

**B2 — Uncertainty propagation.** The prefactor is uncertain: effective
$\Delta T$ depends on where stress relaxation begins relative to $T_g$, and
FR-4 modulus is strongly temperature-dependent. Propagate the plausible ranges
and **report an interval, never a point estimate.**

The structure of that uncertainty is what makes the tool useful anyway:
$\Delta T\boldsymbol{\Lambda}$ is a *common factor* across panelisations of one
stackup. So

- **ratios and rankings are robust** — "A bows less than B" survives a ±40 %
  prefactor error
- **absolute compliance judgements are not** — "this panel is under 0.75 %"
  does not

The tool should therefore present itself as a comparator with a physical scale
attached, not as a predictor of what a fab will measure.

**B3 — Cross-checks against literature.** Published warpage studies give
order-of-magnitude expectations for panel bow at known copper asymmetries. Not
validation, but enough to catch an answer that is wrong by orders of magnitude.

**B4 — The authority bound.** Apply the forward model to the extreme legal
copper trade (band limit over the fillable region) to compute the *maximum
achievable* $\Delta\text{bow}$ by any frame-copper strategy. Cheap, and it gates
everything in group C: if the ceiling is a small fraction of typical panel bow,
the optimisation work is not worth doing and we should say so.

### C. Acting on it

Only after A, and only if B4 says the authority is worth using.

#### C1 — The structural fact that makes this tractable

Warp depends on the copper field through a handful of numbers.

The deflection response rolls off as $\lambda^{2}$ (§4) and the acceptance
criteria are low-order modes of the deflection surface (§5). So pick an
orthogonal basis $\{\varphi_k\}$ over the panel — $1,\;x,\;y,\;xy,\;x^2-y^2,\;x^2+y^2$
suffices — and the entire warp objective is a function of

$$
a_k=\langle m,\varphi_k\rangle=\sum_l w_l\underbrace{\langle \rho_l,\varphi_k\rangle}_{c_{lk}}
$$

**Six numbers.** All ~485 000 radii enter the warp objective only through six
linear functionals. The decision variables that matter are the $c_{lk}$ — six
modes × six layers = 36 scalars — and the objective sees only their weighted
sum.

This is why no better large-scale solver is needed. The part of the problem
concerning warp is *tiny*; the expensive part concerns etch uniformity and is
already solved adequately.

#### C2 — A lever we do not currently have

Today the band constrains one functional per layer: the total, $c_{l0}$. Higher
modes are unconstrained and unused — the solver has never been asked to put
*more copper on the left of the frame than the right*.

That tilt is exactly what a spatially varying moment needs. Extending from
"constrain the sum" to "constrain the low-order modal coefficients" is a genuine
new actuator, not a re-weighting of the old one.

#### C3 — Two-level formulation

**Outer problem — tiny, solve it properly.** Choose modal targets $c^{*}_{lk}$
minimising estimated warp:

$$
\min_{c}\;\; \mathrm{bow\%}(a)^2+\mathrm{twist\%}(a)^2
\quad\text{s.t.}\quad
a_k=\sum_l w_l c_{lk},\qquad c_{lk}\in\mathcal{C}_{lk}
$$

36 variables, box constraints, convex quadratic. Small enough that an
off-the-shelf QP solver *is* appropriate here — or solve it directly, since at
this size the cost is negligible either way. Note the inversion: a QP solver is
wrong for the 485 k-variable problem because its Hessian is matrix-free and
enormous; at 36 variables that objection evaporates.

**Inner problem — large, keep what we have.** Given $c^{*}$, run the existing
spatial solve with modal coefficients constrained instead of only the sum. The
projection generalises from "box ∩ sum-band ∩ one joint equality" to "box ∩
*several* banded linear functionals" — the same dual-multiplier bisection, over
a handful of multipliers rather than one.

Constraining only $k=0$ reproduces today's behaviour exactly, which makes this a
strict generalisation with a safe rollout path.

#### C4 — Bound the degradation, minimise the warp

Rather than weighting two incommensurable terms with a $\mu$ nobody can
interpret, keep the shape the band already has: **constrain the thing we
understand, minimise the thing we care about.**

The feasible set $\mathcal{C}_{lk}$ encodes "no layer's density may stray more
than $\varepsilon$ from its target, in any low-order mode." That is a statable
guarantee, it is what `stack_flex_density` already means for $k=0$, and it makes
`stack_moment_weight` unnecessary — warp becomes the sole objective inside a
bounded set rather than one half of a weighted sum.

This matters more without measurement: a hard bound on how far we move from the
board's own density is a guarantee we can state regardless of whether the warp
model is quantitatively right.

#### C5 — Simpler options, and when they suffice

We do not need optimality. Ranked by effort:

| Strategy | What it does | When it is enough |
|---|---|---|
| **Mirror-pair trade** | greedily trade density between the pair contributing most to $a_0$, until the band binds | if the forward model shows warp is almost entirely uniform bow — likely |
| **One-shot modal correction** | run today's solve, measure residual $a_k$, apply a single corrective shift | if the correction is small enough not to disturb the local term |
| **Two-level (C3)** | outer modal solve, inner spatial solve | the recommended target |
| **Full joint solve** | warp functional folded into $J$, one big projected gradient | not recommended — the warp gradient is rank-6 against a full-rank local term, badly conditioned, and it discards the structure that makes this easy |

The honest expectation is that **mirror-pair trade captures most of the
available benefit**, because §4 says the uniform mode dominates warp
quadratically. Build C3 only if the modal breakdown justifies it.

#### C6 — What we explicitly do not need

- **Optimality.** Radii quantise to a 5 µm grid; solving below that is waste.
- **Tight convergence.** Same reason. A convergence *certificate* is worth
  having — we currently cannot tell whether 512 iterations is five times too
  many or twice too few — but a tight tolerance is not.
- **A general solver for the big problem.** Matrix-free, ~485 k variables,
  trivial projection, low accuracy: first-order projected gradient is the right
  tool and we already have it.

#### C7 — Suggested order within group C

1. Report the projected-gradient norm as a diagnostic. Measurement, not
   optimisation; can land any time.
2. Report $a_k$ per panel from the modal table. Still analysis — it says which
   modes actually carry the warp.
3. Mirror-pair trade against $a_0$. Smallest change that could work.
4. Generalise the band projection to several modal functionals only if (2)
   shows meaningful warp outside $a_0$.

---

## Part IV — Limits we accept

**No empirical validation.** We cannot measure panels, so the model can be
verified but not validated. Consequences, stated plainly:

- The output is a **modelled estimate with an uncertainty interval**, not a
  prediction of fab measurement. It must never be presented as the latter.
- Absolute compliance against the 0.75 % limit is **not** something this tool
  can assert.
- We can never close the loop on whether an optimisation actually reduced warp
  in the world. Any benefit from group C remains theoretical.
- That argues for conservatism: keep the bounded-degradation framing of C4, keep
  default settings gentle, and treat the tool primarily as a **relative
  comparator** — the role in which it is on firmest ground, per §2.

If measurement ever becomes available — even a handful of panels spanning a
deliberate range of copper asymmetry — it would collapse most of this
uncertainty at once, and it is the single highest-value thing that could change.

**Full viscoelastic thermal history.** Cure→cool→reflow with resin relaxation is
FEA territory. If it becomes necessary, export the stackup and copper fields to
a solver rather than reimplementing one.

**Board-level warp after depanelisation.** Copper balancing writes to frames and
gutters; a delivered board's warp is dominated by copper inside it, which
balancing never touches. This effort targets *panel* flatness.

**Twist reduction.** Per §6, unreachable with isotropic frame copper. Diagnosis
is in scope; correction is not.

---

## Sources

- [IPC-TM-650 2.4.22 — Bow and Twist (Percentage)](https://www.electronics.org/sites/default/files/test_methods_docs/2.4.22c.pdf)
- [Eurocircuits — Bow and twist definitions and IPC-6012 limits](https://www.eurocircuits.com/technical-guidelines/understanding-manufacturing-tolerances-on-a-pcb/bow-and-twist-on-a-pcb/)
- [Roylance, *Mechanics of Materials* — Laminated Composite Plates (ABD matrices, thermal resultants)](https://eng.libretexts.org/Bookshelves/Mechanical_Engineering/Mechanics_of_Materials_(Roylance)/04:_Bending/4.04:_Laminated_Composite_Plates)
- [NASA RP-1351 — Basic Mechanics of Laminated Composite Plates](https://ntrs.nasa.gov/api/citations/19950009349/downloads/19950009349.pdf)
- [Bend–twist coupling and the $B_{16}/B_{26}$ terms in unsymmetric laminates](https://pmc.ncbi.nlm.nih.gov/articles/PMC9657157/)
- [Rapid PCB warpage modelling via CLT-driven anisotropic viscoelastic homogenisation, *Composites Part A* (2026)](https://www.sciencedirect.com/science/article/abs/pii/S1359835X26001181)
- [Multiscale characterisation of the mechanical behaviour of a PCB](https://www.sciencedirect.com/science/article/abs/pii/S2352492822018098)
- [JLCPCB — copper thieving and plating current density](https://jlcpcb.com/blog/copper-thieving-pcb-balance)
- [Sierra Circuits — balanced copper distribution](https://www.protoexpress.com/blog/balanced-copper-distribution-and-copper-weight-in-pcbs/)
- [OSQP — an operator splitting solver for quadratic programs](https://web.stanford.edu/~boyd/papers/pdf/osqp.pdf)
- [Diamond & Boyd — Matrix-Free Convex Optimization Modeling](https://web.stanford.edu/~boyd/papers/pdf/abs_ops.pdf)
- Companion documents: [board-array-copper-balancing.md](board-array-copper-balancing.md),
  [fab-panel-copper-balancing.md](fab-panel-copper-balancing.md)
