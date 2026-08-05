# Board-array copper balancing

This document defines copper balancing for an assembly panel created by
`board-array create`. Fabrication-panel balancing is a later, independent
pass, documented in
[fab-panel-copper-balancing.md](fab-panel-copper-balancing.md).

## Motivation and scope

The immutable board images can leave the surrounding array rails much more or
less copper-dense than the boards. Generated, non-functional copper should:

- reduce spatial copper-density changes that make etching and plating less
  uniform;
- avoid adding asymmetric copper volume that increases bow or twist during
  fabrication and assembly heating; and
- remain simple, deterministic, and manufacturable.

Board copper is never changed. Generated copper is confined to the certified
safe region. The result is best effort because copper can only be added and the
safe region, tooling, routing, and minimum-feature rules limit the feasible
area.

## Existing area and geometry model

For copper layer `l`, let:

- `P` be the retained assembly-panel region;
- `B` be the union of repeated board footprints with area `A_B`;
- `C_l` be the fixed copper region;
- `S_l` be the initially empty safe region; and
- `d_l = area(C_l intersect B) / A_B` be the board-density target.

The density domain `D_l` is the part of `P` that holds copper or could hold
copper, and `A_Dl` is its area:

```text
D_l = B union S_l union C_l
```

The requested generated area is:

```text
A*_l = d_l A_Dl - area(C_l)
```

Panel material that is permanently bare — board clearance, rails, V-score
relief, tooling holes, gaps too narrow to hold a void — is excluded from `D_l`
and so from both sides of the ratio. Charging the request against all of `P`
instead would ask for `d_l` times that bare area on top of the real request,
and the solver could only spend it by over-filling the region it can reach:
a thin frame saturates to a solid pour whose local density far exceeds the
board density it exists to match. Excluding it makes `A*_l` reduce to
`d_l area(S_l)` whenever `C_l` lies inside `B` — fill the fillable at the
board's own density.

`S_l` is certified independently from board geometry, through-stack
operations, and panel features whose existing IR span reaches layer `l`.

The solver first projects `A*_l` onto the area attainable by no fill, solid
fill, or a perforated fill. Perforated copper uses a common 1.35 mm
triangular lattice of slightly rounded hexagonal voids. Void radii stay between
0.20 and 0.65 mm. The lattice and radius bound preserve at least 0.20 mm between
voids; clipping preserves a 0.20 mm copper web at the safe-region boundary.

The spatial solve keeps the per-layer attainable area selected by this first
projection, but replaces the single interior radius with one radius field.

## Local density field

Use the void lattice as a shared sampling grid on every copper layer. The
staggered columns tile the plane exactly with equal-area rectangles centered
on the sites, one column pitch wide and one pitch tall. At panel lattice site
`j`, measure the fixed-copper coverage of that tile:

```text
c_lj = area(C_l ∩ tile_j) / area(tile_j)
```

estimated by a stratified 3 × 3 subsample grid inside the tile, so copper
narrower than the lattice pitch contributes its true area fraction instead of
aliasing to zero or one. Smooth the coverage with a normalized Gaussian with
`sigma = 5 mm`, truncated at `3 sigma`:

```text
rho_fixed_li =
    sum_j K(distance(x_i, x_j)) c_lj
    --------------------------------
    sum_j K(distance(x_i, x_j))
```

The denominator normalizes panel edges instead of treating space outside the
panel as empty copper area. The resulting field varies over about 10-15 mm,
not at trace or 1.35 mm lattice scale. The kernel is a fixed manufacturing
profile value, not a user-facing board-array option.

The local deficit is:

```text
q_li = d_l - rho_fixed_li
```

A positive value requests more generated copper near `x_i`; a negative value
requests less. The deficit is non-uniform when the fixed board copper or panel
shape is non-uniform. The constrained solution can also become non-uniform
when tooling, routing, or other protected features remove balancing area from
one rail.

## Through-stack metric

The IPC-2581 physical stackup supplies the ordered layer thicknesses. Let:

- `t_l` be conductor thickness;
- `z_l` be the signed distance from the stack mid-plane to the conductor
  layer's center; and
- `w_l = z_l t_l`.

Layer positions are the cumulative centers of the ordered physical stackup.
The metric requires positive conductor thicknesses and enough layer thickness
data to locate every conductor. When those data are absent, all stack weights
are zero: the same solver still optimizes local density, but it does not invent
nominal copper weights or equal layer spacing.

For a proposed generated fill, let `rho_li` be the modeled final local density
and:

```text
M_i = sum_l w_l rho_li
```

`M_i` is a first-moment proxy for the copper the panel actually carries about
its mid-plane at `x_i`. Equal density on mirrored layers cancels; thicker
copper and copper farther from the mid-plane contribute more. Normalize the RMS
metric by `W = sum_l abs(w_l)`:

```text
E_stack = sqrt(weighted_mean_i(M_i^2)) / W
```

Penalizing this field pointwise is what covers both warp modes at once. Bow
follows the field's mean, twist follows its variation, and squaring `M_i` at
every site charges for both — so there is one term here rather than separate
bow and twist machinery.

The reference is zero copper moment, not the board's own per-layer density.
Reading `rho_li` rather than `rho_li - d_l` is the whole difference: the
subtracted form is zero exactly when every layer sits on its target, so a
correctly balanced panel would report no asymmetry left to remove while still
carrying whatever imbalance the boards were designed with. The optimizer
therefore does repair immutable board content from the rails, which is
deliberate — bow answers to the panel-integrated moment, so copper spent
anywhere on the panel counts against it.

That repair is bounded, because it is spending density the boards did not ask
for. Two profile settings govern it, with separate jobs:

- `stack_moment_weight` scales this term against the local density match, and
  says how hard the solver tries. Equal weighting is far too timid to be
  useful: with `L` layers the local term carries `L` squared errors against
  this one, so a six-layer stack removes only about a sixth of a uniform
  moment.
- `stack_flex_density` caps how far any layer's density may end up from its
  own target, whatever the weight asks for. It is the guarantee that survives
  a pathological board: a two-layer panel poured 0.9 against 0.3 wants a 30%
  correction, and the cap is what refuses it.

Board-array creation prints a per-layer balance summary and the stack moment
before and after, warns when stackup thickness data is missing (the solver then
balances each layer independently and neither setting does anything), and the
per-layer report retains the signed stack weight.

## One spatial solve

For every full interior void, use squared radius `x_lk = r_lk^2` as the solver
variable. This is the natural coordinate because the rounded hexagons are
self-similar. Let:

- `c_l` be the fixed-copper indicator on the full panel lattice;
- `s_l` be the safe-region indicator on that lattice;
- `p_l` be the fixed clipped edge-void indicator;
- `P_l` scatter layer `l`'s admitted full-void variables onto the lattice;
- `H` be the normalized Gaussian convolution; and
- `beta = a_h / A_cell`, the rounded-hex area factor divided by lattice-cell
  area.

The perforated layer-density field is the affine map:

```text
rho_l(x_l) = H(c_l + s_l - p_l - beta P_l x_l)
```

The safe region and existing copper must be disjoint. A layer has variables
only at the full-void sites admitted by its safe region. It has no value, and
needs no default value, at another layer's sites. Convolution carries the
effect of each admitted void to nearby evaluation points.

The operator consumes the full 1.35 mm fabrication lattice. For runtime, the
objective is evaluated on a deterministic subset spaced approximately one
5 mm kernel sigma apart. This sampling does not change safe geometry, output
void sites, radius constraints, or total copper area.

Choose every layer's field together by minimizing the dimensionless convex
energy:

```text
J(x) = sum_l mean_i(e_li^2)
     + lambda mean_i((M_i / W)^2)
```

where `lambda` is `stack_moment_weight`.

Each perforated layer carries a box constraint and a band on its total, and one
equality couples the bands across the stack:

```text
r_min^2 <= x_li <= r_max^2
X_l - D_l <= sum_i x_li <= X_l + D_l
sum_l (sum_i x_li - X_l) / A_l = 0
```

`X_l` is computed from the selected total void area after subtracting the
already-clipped edge-fragment area. Edge fragments keep their projected
geometry; they are boundary constraints, not optimizer variables.

`D_l` is `stack_flex_density` converted from density to squared radius through
the density-domain area `A_l`, so the band is symmetric in copper. Without it
each `sum_i x_li` is pinned and the layer can only redistribute; the band is
what lets a layer take on or give back copper to flatten `M_i`.

The joint equality is what keeps that useful. It says the stack's density
deviations cancel — copper moves between layers rather than being created.
Freeing each total independently instead frees a direction the stack cannot
see: a symmetric stackup's weights sum to zero, so every layer drifting the
same way leaves `M_i` untouched, and the local term will happily spend the
whole band on exactly that drift, since bare clearance reads as a deficit on
every layer at once.

The objective is a convex quadratic and the feasible set is convex, so
projected gradient converges as before. Its gradient uses the exact transpose
`-beta P_l^T H^T` of the forward density operator. Projection is two-level:
one linear equality over per-layer convex sets dualizes to a single
multiplier, so each layer projects its own commonly shifted proposal onto its
box and band, and the multiplier is found by bisection. A layer pinned at a
band edge stops responding to the multiplier, so scoring a trial costs one
clamped sum per layer rather than a nested projection. Setting
`stack_flex_density` to zero makes every band degenerate and recovers the
pinned per-layer projection exactly. A constant-radius pattern is the result
when the measured fields and constraints are spatially symmetric; it is not a
separate mode or fallback.

## Fill-to-geometry map

The rounded hexagons are self-similar because their corner radius is a fixed
fraction `k = 0.15` of hexagon radius `r`. A full void has area:

```text
A_void(r) = [3 sqrt(3) / 2 - (2 sqrt(3) - pi) k^2] r^2
```

The full-cell area equality is therefore exact in squared-radius space. Edge
sites use the layer's projected radius, intersect the rounded hexagon with the
boundary-web region, and retain the existing fragment qualification that
rejects clipped voids unable to contain the minimum disk.

All layers use the same lattice coordinate system and evaluation field. The
existing maximum radius continues to guarantee the inter-void web for every
neighboring radius pair.

## Implementation boundaries

Keep the numerical and geometric core independent of IPC-2581:

```text
stackup -> physical conductor weights
fixed contours + lattice -> smoothed density fields
fields + required areas + weights -> bounded fill fields
fill fields + safe geometry -> rounded-hex void contours
```

These should be deterministic functions over immutable inputs in `pcb-ir`.
The IPC-2581 board-array adapter should only extract stackup and copper
geometry, invoke the core, serialize contours, and report metrics.

Initial verification should cover:

- constant density produces constant radii;
- a left-to-right density gradient produces the opposite smooth radius
  gradient without changing generated area;
- equal mirrored layers cancel in `E_stack`;
- a stack carrying a copper moment ends with a smaller one, by layers trading
  density in opposite directions rather than all drifting the same way;
- no layer's achieved density leaves `stack_flex_density` of its target,
  however large the moment; and zero flex reproduces the pinned result;
- conductor thickness and mid-plane distance scale the metric correctly;
- translation and reflection preserve objective values; and
- clipped edge voids preserve the existing web and minimum-fragment rules; and
- distinct layer-safe regions never contribute sites to the wrong layer.

Fabrication-panel layout and balancing (see
[fab-panel-copper-balancing.md](fab-panel-copper-balancing.md)),
electroplating simulation, laminate material stiffness, and changes inside a
board footprint are out of scope.
