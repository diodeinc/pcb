# Copper balancing

Copper balancing generates non-functional copper on every conductor layer of a
panel: an assembly panel from `board-array create`, or a fabrication panel from
`fab-panel create`. Both passes share one density model, one solver, and one
generated geometry; this document defines all of it. Board and placed-panel
copper is never changed — generated copper is confined to a certified safe
region, and the result is best effort because copper can only be added.

## What balancing is for

Two physical objectives, at two different scales:

- **Etch and plating uniformity** — local. Etchant works faster in sparse
  regions and plating current crowds onto isolated copper, so each layer's fill
  should hold the density of the boards beside it. This is the local
  density-match term, and it works at the 10–15 mm scale of the smoothing
  kernel.
- **Warp** — global. Copper carried asymmetrically about the stack's mid-plane
  is a thermal moment that bows the panel during lamination cooldown. Deflection
  response grows with the square of wavelength, so only the broadest variation
  of the copper field matters; detail at lattice or trace scale is mechanically
  inert.

These are distinct objectives that happen to share geometry. Collapsing them
into one term loses real physics, so the solver keeps them apart: the local
term is minimised, the through-stack correction is a bounded step settled in
closed form, and the bound between them is the one deliberate trade
(see [Knobs](#knobs)).

`pcbc ipc2581 warp` closes the loop: it estimates a panel's bow and twist from
the same stackup and copper distribution, against the IPC-6012 limit, so the
effect of any balancing change is measurable in seconds.

## Geometry model

For copper layer `l`, let:

- `P` be the retained panel region;
- `F` be the union of immutable footprints with area `A_F` — repeated board
  profiles for a board array, placed assembly-panel outlines for a fab panel;
- `C_l` be the fixed copper region;
- `S_l` be the certified, initially empty safe region; and
- `d_l = area(C_l ∩ F) / A_F` be the layer's density target: the density its
  own boards already carry.

The density domain `D_l` is the part of `P` that holds copper or could hold
copper:

```text
D_l = F ∪ S_l ∪ C_l
A*_l = d_l area(D_l) - area(C_l)
```

`A*_l` is the requested generated area. Panel material that is permanently
bare — board clearance, rails, V-score relief, tooling holes, process margins,
gaps too narrow to hold a void — is excluded from `D_l` and so from both sides
of the ratio. Charging the request against all of `P` would ask for `d_l` times
that bare area on top of the real request, and the solver could only spend it
by over-filling the region it can reach: a thin frame saturates to a solid pour
whose local density far exceeds the board density it exists to match. With the
exclusion, `A*_l` reduces to `d_l area(S_l)` whenever `C_l` lies inside `F` —
fill the fillable at the boards' own density.

The safe region is certified independently from board geometry, through-stack
operations, and panel features whose IR span reaches layer `l`; the
construction and its clearance/regularization radii are specified in
[`../examples/board-array-balancing-region.md`](../examples/board-array-balancing-region.md).

The solver first projects `A*_l` onto the area attainable by no fill, solid
fill, or a perforated fill at one uniform void radius. The spatial solve then
keeps each layer's selected copper area and replaces the single radius with a
radius field.

## Local density field

The void lattice doubles as the sampling grid on every copper layer. The
staggered columns tile the plane exactly with equal-area rectangles centered on
the sites, one column pitch wide and one pitch tall. At site `j`, the fixed
copper's coverage of that tile is measured, and the coverage field is smoothed
with a normalized Gaussian (`sigma = 5 mm`, truncated at `3 sigma`):

```text
rho_fixed_li = sum_j K(|x_i - x_j|) c_lj / sum_j K(|x_i - x_j|)
```

The denominator normalizes panel edges instead of treating space outside the
panel as empty copper. The resulting field varies over 10–15 mm, not at trace
or lattice scale. The local deficit `d_l - rho_fixed_li` is what the spatial
solve works down: positive requests more generated copper near `x_i`, negative
requests less.

## Through-stack balance

The IPC-2581 physical stackup supplies ordered layer thicknesses. With `t_l`
the conductor thickness and `z_l` the signed distance from the stack mid-plane
to the conductor's center, each layer carries a signed weight `w_l = z_l t_l`
— mirrored layers carry equal and opposite weights, so equal copper on them
cancels. A stackup without enough thickness data to locate every conductor
yields all-zero weights: the solver still balances each layer's density, it
just has no moment to correct and does not invent nominal spacing to get one.

The panel's copper moment is linear in the copper each layer carries, so the
correction that flattens it is a closed form, settled once before the spatial
iteration rather than traded against the local term inside it:

- Each perforated layer is granted a bounded **step**: its fill may run up to
  `stack_flex_density` denser or sparser than its own target, measured over
  the lattice it actually controls. The bound is therefore the density step
  across the boundary between a board and the frame beside it — the quantity
  etch loading cares about.
- Steps are spent lever-arm-weighted: the layer with the longest arm spends
  its whole step, the rest spend in proportion, and a layer the stackup gave
  no weight does not move.
- The total spend is clamped to exactly the moment-nulling value. The
  correction strictly shrinks the moment, never overshoots, and a symmetric
  panel spends nothing.

Each layer's copper area is then pinned at its stepped value and the spatial
iteration below redistributes *within* the layer only. The moment does not
appear in the iterated objective at all: the local term does etch and plating
work at a scale the moment cannot feel, and the moment was already answered at
the only scale the plate response can feel.

The reference is zero copper moment, not the boards' own imbalance. The
correction therefore does repair immutable board content from the rails —
deliberately, since bow answers to the panel-integrated moment — but only up to
the step bound. A panel whose boards are drawn 20 points apart front to back
cannot be fixed from the frame within any tolerable etch step; the balance
summary reports the moment before and after, and `pcbc ipc2581 warp` reports
what the residual is worth in millimeters of bow.

## One spatial solve

For every full interior void, the solver variable is the squared radius
`x_lk = r_lk^2` — the natural coordinate, because the rounded hexagons are
self-similar and void area is linear in it. The perforated layer-density field
is the affine map:

```text
rho_l(x_l) = H(c_l + s_l - p_l - beta P_l x_l)
```

with `H` the normalized Gaussian convolution, `P_l` the scatter of layer `l`'s
admitted void variables onto the lattice, `p_l` the fixed clipped edge-void
indicator, and `beta` the rounded-hex area factor over lattice-cell area. A
layer has variables only at sites its own safe region admitted.

The objective is each layer's mean squared local deficit, and the feasible set
is a box with a pinned sum per layer:

```text
minimize   sum_l mean_i((rho_li - d_l)^2)
subject to r_min^2 <= x_lk <= r_max^2
           sum_k x_lk = X_l
```

`X_l` is the layer's stepped copper area from the through-stack settlement,
converted to squared-radius space, after subtracting already-clipped
edge-fragment area (edge fragments keep their projected geometry; they are
boundary constraints, not variables). Projected gradient converges on this
convex problem; the gradient uses the exact transpose `-beta P_l^T H^T` of the
forward operator, and projection onto box-with-pinned-sum is a single
water-filling bisection per layer. For runtime, the objective is evaluated on a
deterministic site subset spaced about one kernel sigma apart; this changes no
safe geometry, void sites, radius constraints, or copper area. A
constant-radius pattern is what falls out when the measured fields are
symmetric — it is not a separate mode.

Converged radii snap to 20 uniformly spaced void-area levels. Interior sites
use the nearest level; an admitted boundary site rounds upward so it cannot
lose the minimum disk that made it manufacturable. The whole layer therefore
uses at most 20 exact rounded-hex templates rather than thousands of unique
shapes.

## Fill geometry

Perforated copper uses a common triangular lattice of slightly rounded
hexagonal voids; the corner radius is a fixed fraction `k = 0.15` of hexagon
radius, which is what makes void area exactly quadratic in radius. The lattice
pitch and maximum radius together guarantee the minimum copper web between any
neighboring radius pair. Edge candidates too small to contain the minimum disk
are rejected. Output clears every admitted site with the same full rounded-hex
template, then restores `usable - voidable` as one positive boundary web. This
is exactly the clipped geometry used by the solve, while keeping the repeated
voids as flashes.

## Pass-specific inputs

Everything above is shared. The passes differ only in what they feed it:

**Board array** (`board-array create`): `F` is the repeated board footprints.
Existing copper outside the footprints (frame fiducials, array support) joins
the obstacle set and stays in the density domain — real copper, even where no
generated copper may go. Creation prints a per-layer balance summary with the
signed stack weight, the achieved and target densities, and the stack moment
before and after; it warns when stackup thickness data is missing.

**Fab panel** (`fab-panel create`): `F` is the placed assembly-panel outlines,
so the target extends the already-balanced panels' aggregate density into the
gutters; mixed panel types weight it by footprint area. One safe region serves
every layer — the gutters between process margins, minus placed outlines with
clearance. The process margins stay bare and are excluded from the density
domain entirely. All sources must carry one identical physical stackup. The
gutter lattice originates at the usable region's minimum corner and is not
phase-aligned with the immutable source panels' lattices; none is needed for
determinism. Balancing runs on an explicit flag: the panel is generated
provisionally, the provisional document supplies composed copper images and
outlines, and the final document carries an ordered plane, rounded-hex void,
and boundary-web image.

## Knobs

All of these live in `DenseCopperBalanceProfile` (`pcb-ir`); `V1` is the only
profile in use and none are user-facing options today. The intent is to keep it
that way until the fundamentals are validated — the values below are defaults
chosen from process rules of thumb, not tuning.

| Knob | Default | What it controls | What tuning it would trade |
|---|---|---|---|
| `stack_flex_density` | 0.05 | **The one deliberate trade.** How far a layer's fill may step off its own board density to flatten the stack moment. | Warp authority against etch uniformity. Zero pins every layer to its board density (no warp correction). Raising it cancels more of an asymmetric board's moment at a larger board↔frame density step; fabricators tolerate roughly 10–15 % mirrored-layer mismatch, and plating already varies by under 10 % across a panel, so 0.05 sits comfortably inside both. |
| `density_sigma_mm` | 5.0 | Smoothing scale of the density field the local term matches. | What "local" means for etch: smaller chases finer density structure with more radius variation; larger yields flatter fill that ignores mid-scale gradients. |
| `pitch_mm` | 1.35 | Void lattice pitch. | Fill resolution against feature count and solve size. Bounded below by web rules. |
| `min_void_radius_mm` | 0.20 | Smallest manufacturable void. | Densest expressible fill (smaller voids → higher maximum density). |
| `max_void_radius_mm` | 0.65 | Largest void; with the pitch, guarantees the inter-void web. | Sparsest expressible fill (larger voids → lower minimum density). |
| `min_copper_web_mm` | 0.20 | Minimum copper between neighboring voids. | Process rule; not a tuning knob. |
| `boundary_web_mm` | 0.20 | Copper web preserved at the safe-region boundary. | Process rule; not a tuning knob. |
| `void_area_levels` | 20 | Uniform squared-radius levels shared by every rounded-hex void. | Template count against fill-area precision. |

Related constants outside the profile, with the same posture:

| Constant | Default | Where |
|---|---|---|
| Balancing-region clearance / regularization radii | 0.5 mm | safe-region construction |
| Warp model temperature drop (`LAMINATE_RELAXATION_DROP_K`) | 110 K | `pcb-ir::geom::warp` — the largest uncertainty in the *absolute* warp figure; cancels when comparing panelizations of one stackup |
| Warp material constants | textbook Cu / FR-4 | `pcb-ir::geom::warp` — calibrated only by consistency: the model puts the 0.75 % IPC bow limit at ~13 % mirror-pair mismatch, inside the 10–15 % fabricators quote |

Removed knobs, for the record: `stack_moment_weight` (the local-versus-moment
exchange rate) is gone — the moment left the iterated objective when its
correction became a closed-form bounded step, so there is no longer a weight to
choose, only the bound above.

## Implementation boundaries

The numerical and geometric core stays independent of IPC-2581, as
deterministic functions over immutable inputs in `pcb-ir`:

```text
stackup -> physical conductor weights
fixed contours + lattice -> smoothed density fields
fields + stepped areas -> bounded fill fields
fill fields + safe geometry -> rounded-hex void contours
```

The IPC-2581 adapters extract stackup and copper geometry, invoke the core,
serialize the plane, declared lattice, and boundary web, and report metrics.
Electroplating simulation, laminate anisotropy, and changes inside a board
footprint are out of scope.
