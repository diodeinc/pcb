# Warp modelling: theory, metrics, and a four-phase plan

This document works out the physics that copper balancing is implicitly
optimising, shows exactly where the current implementation sits relative to
that physics, and proposes a plan to close the gap.

**The ordering principle is that measurement comes first.** Prediction and
measurement are independent of the solver: we can build and validate a forward
model without changing a single line of the optimiser, and until that model is
validated against measured panels there is no defensible way to judge whether
any solver change helped. Three of the four phases below are analysis. The
optimiser is touched last, on purpose.

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
the moment arm $z$ in the second integral — this is what makes
$\mathbf{M}^{T}$ a *moment*, and it is the term most often dropped when the
pair is written informally:

$$
\mathbf{N}^{T}=\Delta T\int\bar{\mathbf{Q}}\,\boldsymbol{\alpha}\,dz,
\qquad
\boxed{\;\mathbf{M}^{T}=\Delta T\int\bar{\mathbf{Q}}\,\boldsymbol{\alpha}\,z\,dz\;}
$$

**This is where copper density enters the physics**, and it enters through
*both* factors. A copper-rich region is stiffer ($\bar{\mathbf{Q}}$) *and* has
a different effective expansion ($\boldsymbol{\alpha}$). The driver is the
product $\bar{\mathbf{Q}}\boldsymbol{\alpha}$, not copper area alone.

### 2. Reducing to a copper-fraction model

Let $\rho_l(x)$ be the copper area fraction of layer $l$ at panel position $x$,
smoothed at a scale below which the laminate homogenises. Copper and resin sit
side by side in-plane, so they share strain, and the Voigt (iso-strain) average
is the appropriate in-plane homogenisation:

$$
\bar{\mathbf{Q}}_l(x)=\rho_l(x)\,\bar{\mathbf{Q}}^{\mathrm{cu}}
+\bigl(1-\rho_l(x)\bigr)\bar{\mathbf{Q}}^{\mathrm{res}}
$$

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
metric is the copper contribution to $\mathbf{M}^{T}$ with the constant
material prefactor $\Delta T\,\boldsymbol{\Lambda}$ stripped off.

That is a stronger result than it may look. For comparing two panelisations of
one design on one stackup, the prefactor cancels and the existing geometric
metric is *exactly right*. What it cannot do is produce a number in physical
units, or capture the tensor character of $\boldsymbol{\Lambda}$ when copper is
directional.

### 3. From resultants to curvature

Free thermal loading of an unconstrained panel gives

$$
\boldsymbol{\kappa}=\bigl(\mathbf{D}-\mathbf{B}\mathbf{A}^{-1}\mathbf{B}\bigr)^{-1}
\bigl(\mathbf{M}^{T}-\mathbf{B}\mathbf{A}^{-1}\mathbf{N}^{T}\bigr)
$$

Two consequences worth stating plainly:

- Minimising $\mathbf{M}^{T}$ is **not identical** to minimising curvature. When
  $\mathbf{B}\neq 0$, the in-plane resultant $\mathbf{N}^{T}$ feeds curvature
  too. For a near-symmetric stack $\mathbf{B}\approx 0$ and
  $\boldsymbol{\kappa}\approx\mathbf{D}^{-1}\mathbf{M}^{T}$, which is the
  approximation we rely on.
- $\mathbf{M}^{T}$ is a **three-vector** $(M_{xx},M_{yy},M_{xy})$, not a scalar.
  Our $m(x)$ is its isotropic part only.

Because $\mathbf{A},\mathbf{B},\mathbf{D}$ themselves depend on $\rho_l(x)$,
the honest model linearises about the nominal (mean-copper) stack so that the
stiffnesses are constant and only $\mathbf{M}^{T}$ varies with position. This
is valid while copper fractions vary modestly, which is the regime balancing
operates in.

### 4. From a curvature field to measured warp — where scale enters

$\boldsymbol{\kappa}(x)$ is not what a fab measures. They measure out-of-plane
deflection. For a Kirchhoff plate carrying a spatially varying isotropic misfit
moment, the standard thermoelastic result is

$$
D\,\nabla^{4}w=-\frac{1}{1-\nu}\nabla^{2}M^{T},
\qquad D=\frac{Eh^{3}}{12(1-\nu^{2})}
$$

In Fourier space, with $\hat{m}(k)$ the transform of the moment field:

$$
D\,k^{4}\hat{w}=\frac{k^{2}}{1-\nu}\hat{m}
\quad\Longrightarrow\quad
\hat{w}(k)=\frac{\hat{m}(k)}{(1-\nu)D\,k^{2}}
\;\;\propto\;\; \hat{m}(k)\,\lambda^{2}
$$

**Deflection response scales as wavelength squared.** Note the sharper
statement: curvature response $k^{2}\hat{w}=\hat{m}/((1-\nu)D)$ is *scale-free*;
it is deflection that carries $\lambda^{2}$, and deflection is what is measured.

The uniform component is handled by the boundary rather than the interior
equation — a free plate under uniform $M^{T}$ takes spherical curvature
$\kappa_0=M^{T}/(D(1+\nu))$, deflecting by $\approx\kappa_0 L^{2}/8$ over a span
$L$. So the panel-scale component also carries $L^{2}$.

Over our range this is decisive. Between a 5 mm feature and a 500 mm panel the
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
$x^2,y^2$ content, twist is the $xy$ content. The standard is already a modal
decomposition, which is a strong hint about the right metric.

### 6. What copper can and cannot cause

Twist curvature $\kappa_{xy}$ is driven by the $B_{16},B_{26}$ coupling terms.
For a **cross-ply laminate — 0°/90° plies only — $A_{16}=A_{26}=B_{16}=B_{26}=D_{16}=D_{26}=0$**;
off-axis plies are required to make them non-zero, with bend–twist coupling
peaking near 30°.

A PCB layup is 0/90 glass weave. Therefore:

> An isotropic through-thickness copper-density asymmetry produces **bow**, and
> structurally **cannot produce twist**.

Twist originates in weave skew, layup asymmetry, and *directional* copper
(a field of parallel traces is orthotropic, a plane is not). Balancing copper
with isotropic hex perforation cannot address any of them. This bounds what
this entire effort can achieve, and it should be said out loud rather than
discovered later.

### 7. What copper balancing is and is not

It is tempting to unify every copper-evening technique as "minimise
$\mathbf{M}^{T}$". That over-reaches — these techniques share geometry but not
objectives:

| Technique | Primary objective |
|---|---|
| Balancing pours, dummy copper | $\mathbf{M}^{T}$ — warp |
| **Thieving** | **Plating current-density uniformity** — thieving patterns act as current robbers, cutting plating variation from 20–30 % to under 10 % |
| **Cross-hatching** | Flex compliance, impedance, adhesion — *and* balance |
| Density evening generally | **Etch loading** — etchant works faster in sparse regions, causing over-etch of isolated traces |

Our objective already reflects this: it carries a local density-match term
*and* a moment term. The local term is doing etch and plating work at a scale
where the moment term is mechanically irrelevant, and vice versa. Collapsing
them would lose real physics.

---

## Part II — Gap analysis

| Dimension | Current state | Gap | Phase |
|---|---|---|---|
| Moment quantity $m=\sum w_l\rho_l$ | ✅ exactly the copper part of $\mathbf{M}^T$ | — | — |
| Material prefactor $\Delta T\boldsymbol{\Lambda}$ | ❌ stripped | no physical units; cannot compare to 0.75 % | 1 |
| Layer weights $w_l=z_l t_l$ | ⚠️ geometric | should carry $\bar{\mathbf{Q}}\boldsymbol{\alpha}$; wrong across mixed copper weights | 1 |
| Neutral axis | ⚠️ geometric midplane | true axis is the stiffness centroid | 1 |
| $\mathbf{M}^T\rightarrow\boldsymbol{\kappa}$ | ❌ absent | no $\mathbf{D}^{-1}$, no $\mathbf{B}$ coupling | 1 |
| $\boldsymbol{\kappa}\rightarrow w$ (plate response) | ❌ absent | flat in $\lambda$ where physics is $\lambda^{2}$ | 1 |
| IPC bow/twist extraction | ❌ absent | no comparison to spec | 1 |
| **Validation against measurement** | ❌ **none** | **every number is untested theory** | **2** |
| Anisotropy (directional copper) | ❌ isotropic scalar | a plane and parallel traces at equal density are not equivalent | 3 |
| Thermal history / viscoelasticity | ❌ single elastic $\Delta T$ | resin relaxes; cure→cool→reflow | out of scope |
| Objective scale weighting | ❌ pointwise $m^2$ | over-weights short $\lambda$ by $(L/\lambda)^2$ | 4 |

---

## Part III — The four phases

### Phase 1 — Build the forward model

**Analysis only. No solver changes.**

Turn the geometric comparator into a physical predictor by restoring the terms
Part I identifies as missing, then simulate the IPC measurement.

1. **Material properties per layer.** A table keyed off stackup material names:
   copper $E\approx117$ GPa, $\nu=0.34$, $\alpha=17$ ppm/K; FR-4 in-plane
   $E\approx25$ GPa, $\alpha\approx14$–17 ppm/K. Fall back to defaults with a
   warning when the stackup does not name a material.
2. **Stiffness-weighted neutral axis and elastic layer weights.** Replace
   $w_l=z_l t_l$ with $w_l = z_l t_l \Lambda$, and the geometric midplane with
   the modulus-weighted centroid.
3. **Assemble $\mathbf{A},\mathbf{B},\mathbf{D}$** for the nominal stack, and
   the field $\mathbf{M}^{T}(x)$.
4. **Solve for the deflection field** $w(x)$ — either the plate equation on the
   evaluation grid, or a projection onto low-order modes with the closed-form
   response per mode. The modal route is cheaper and adequate given the
   $\lambda^{2}$ roll-off.
5. **Simulate the IPC measurement.** Fit a plane through the panel corners,
   take max deviation, report **bow %** and **twist %** exactly as 2.4.22
   defines them.

**Also in Phase 1 — the authority bound.** Apply the same forward model to the
extreme legal copper trade (band limit over the fillable region) to compute the
*maximum achievable* $\Delta\text{bow\%}$ by any frame-copper strategy. This is
cheap and it gates Phase 4: if the ceiling is 0.02 % against panels sitting at
0.5 %, the optimiser work is not worth doing and we stop here.

**Deliverable.** `pcbc ipc2581 warp <panel>` reporting predicted bow % and
twist % against the IPC limit, plus the authority bound.

**Exit criteria.** A number, in physical units, for any panel — with explicit
uncertainty from the assumed $\Delta T$ and moduli.

### Phase 2 — Validate and calibrate

**This is the gate. Nothing downstream is trustworthy without it.**

Everything in Phase 1 is theory with plausible constants. The model has
essentially one lumped free parameter — the effective $\Delta T$ absorbing cure
temperature, stress relaxation, and modulus temperature dependence — and it must
be fitted, not assumed.

1. **Measure real panels** per IPC-TM-650 2.4.22, or with a flatness scanner
   giving the full surface (better: it yields the whole $w$ field, not just two
   scalars).
2. **Span a range deliberately.** Vary copper asymmetry across the sample —
   include a deliberately unbalanced panel and a well-balanced one. A
   correlation fitted over a narrow range tells you nothing.
3. **Correlate predicted against measured**, fit the lumped constant, and state
   the residual as a confidence interval.
4. **Decompose the residual.** If it is dominated by twist, that is the
   anisotropy gap and Phase 3 is justified. If it is scattered and large, the
   linearisation or the elastic assumption is failing and the model needs
   rethinking rather than enriching.

**Deliverable.** A predicted-vs-measured plot over $N\gtrsim 10$ panels, a
fitted constant, and an error bar.

**Exit criteria.** Predicted bow within a stated tolerance of measured across
the range. If this fails, **stop and revisit Part I** — do not proceed to
enrich a model that does not yet track reality.

### Phase 3 — Enrich the model where the residuals demand it

Driven by Phase 2 residuals, not by ambition. Candidates in likely order:

1. **Anisotropy — restore the tensor.** Extract copper fraction *and dominant
   trace orientation* per cell (a structure tensor over the copper image gives
   orientation and coherence), homogenise each cell to an orthotropic lamina,
   and assemble the full ABD. This yields $M_{xx},M_{yy},M_{xy}$ separately —
   distinguishing cylindrical from spherical bow, and making twist *visible*.
   This is the approach taken in the recent
   [CLT-driven PCB warpage literature](https://www.sciencedirect.com/science/article/abs/pii/S1359835X26001181).
   Note it makes twist **diagnosable, not fixable** — see §6.
2. **Bending–extension coupling.** Carry $\mathbf{B}$ properly rather than
   assuming near-symmetry, for genuinely asymmetric stackups.
3. **Temperature-dependent moduli.** FR-4 modulus falls substantially through
   $T_g$; a single room-temperature $E$ is a crude approximation over a
   150 K excursion.

**Exit criteria.** Residual reduced to the measurement noise floor, or a
reasoned decision that the remaining error is not worth further modelling.

### Phase 4 — Optimise against a validated objective

Only now does the solver change, and only if the Phase 1 authority bound said
it was worth reaching.

1. **Replace the objective's moment term.** Swap pointwise $m_i^{2}$ for the
   validated warp functional — the $\lambda^{2}$-weighted modal norm, i.e.
   penalise low-order coefficients of $\mathbf{M}^{T}$ weighted by their
   deflection response. Practically this is a handful of dot products against
   an orthonormal polynomial basis over the panel.
2. **Keep the local density term at 5 mm.** It is serving etch loading and
   plating uniformity, which are genuinely short-scale (§7). The two terms
   should have *different* kernels — that they currently share one is the
   central error.
3. **Retune the band.** With the objective correctly scaled the trade will be
   spent differently; `stack_flex_density` and `stack_moment_weight` need
   re-deriving against predicted bow % rather than a dimensionless residual.
4. **Re-measure.** Close the loop from Phase 2 on optimised panels.

**Exit criteria.** A measured reduction in bow on real panels, not a
dimensionless number moving.

---

## Part IV — Deliberately out of scope

- **Full viscoelastic thermal history.** Cure→cool→reflow with resin
  relaxation is FEA territory. If it becomes necessary, export the stackup and
  copper fields to a solver rather than reimplementing one here.
- **Board-level warp after depanelisation.** Copper balancing writes to frames
  and gutters; a delivered board's warp is dominated by copper inside it, which
  balancing never touches. This effort targets *panel* flatness during
  fabrication and assembly.
- **Twist reduction.** Per §6, unreachable with isotropic frame copper.
  Diagnosis is in scope; correction is not.

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
- Companion documents: [board-array-copper-balancing.md](board-array-copper-balancing.md),
  [fab-panel-copper-balancing.md](fab-panel-copper-balancing.md)
