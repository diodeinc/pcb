# Board-array copper balancing

This document defines copper balancing for an assembly panel created by
`board-array create`. Fabrication-panel balancing is a later, independent pass.

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

- `P` be the retained assembly-panel region with area `A_P`;
- `B` be the union of repeated board footprints with area `A_B`;
- `C_l` be the fixed copper region;
- `S_l` be the initially empty safe region; and
- `d_l = area(C_l intersect B) / A_B` be the board-density target.

The requested generated area is:

```text
A*_l = d_l A_P - area(C_l)
```

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

Use the void lattice as a shared sampling grid on every copper layer. At panel
lattice site `j`, sample the fixed-copper indicator:

```text
c_lj = 1 when x_j is in C_l, otherwise 0
```

This is midpoint quadrature over the lattice's equal-area hexagonal Voronoi
cells. Smooth the samples with a normalized Gaussian with `sigma = 5 mm`,
truncated at `3 sigma`:

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
e_li = rho_li - d_l
m_i  = sum_l w_l e_li
```

`m_i` is a first-moment proxy for the stack asymmetry introduced by
panelization at `x_i`. Equal residual density on mirrored layers cancels;
thicker copper and copper farther from the mid-plane contribute more. Normalize
the RMS metric by `W = sum_l abs(w_l)`:

```text
E_stack = sqrt(weighted_mean_i(m_i^2)) / W
```

The reference is the board's own per-layer density, not zero copper moment.
The optimizer therefore avoids adding new stack asymmetry without trying to
repair immutable board content from the rails. The per-layer report exposes
the physical stack weight when it is available.

## One spatial solve

For every full interior void, use squared radius `x_li = r_li^2` as the solver
variable. This is the natural coordinate because the rounded hexagons are
self-similar. Let `s_li` be the smoothed safe-region indicator at the site. If
`a_h` is the rounded-hex area factor and `A_cell` is lattice-cell area, the
local generated-copper model is affine:

```text
g_li = s_li (1 - a_h x_li / A_cell)
rho_li = rho_fixed_li + g_li
```

The field varies only over the 5 mm kernel scale, so this uses the local radius
as constant within that kernel. `s_li` prevents fixed copper near a safe-region
boundary and generated copper from both claiming the same neighborhood area.

Choose every layer's field together by minimizing the dimensionless convex
energy:

```text
J(x) = sum_l mean_i(e_li^2)
     + mean_i((m_i / W)^2)
```

subject to one box constraint and one equality per perforated layer:

```text
sum_i x_li = X_l
r_min^2 <= x_li <= r_max^2
```

`X_l` is computed from the selected total void area after subtracting the
already-clipped edge-fragment area. The equality therefore redistributes the
full-void area without changing the layer-density result. Edge fragments keep
their projected geometry; they are boundary constraints, not optimizer
variables.

The objective is a convex quadratic. A fixed-count projected-gradient solve
uses its analytic gradient, then projects each layer onto the intersection of
the box and sum constraints with one-dimensional bisection. A constant-radius
pattern is the result when the measured fields and constraints are spatially
symmetric; it is not a separate mode or fallback.

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

All layers use the same lattice coordinate system. Each layer owns only the
sites admitted by `S_l`; the union of those sites supplies common coordinates
for local-density and stack-error evaluation. The existing maximum radius
continues to guarantee the inter-void web for every neighboring radius pair.

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
- conductor thickness and mid-plane distance scale the metric correctly;
- translation and reflection preserve objective values; and
- clipped edge voids preserve the existing web and minimum-fragment rules; and
- distinct layer-safe regions never contribute sites to the wrong layer.

Fabrication-panel layout and balancing, electroplating simulation, laminate
material stiffness, and changes inside a board footprint are out of scope.
