# Copper balancing

Copper balancing adds non-functional copper to conductor layers of assembly
panels (`board-array create`) and fabrication panels (`fab-panel create`). Board
and placed-panel copper is immutable. Generated copper stays inside a certified
safe region; balancing is best effort because copper can only be added.
See the [command defaults](../README.md#panels-and-copper-balancing) for enabling
or disabling each pass.

## Objectives and limits

The model separates two objectives:

- **Local etch/plating uniformity:** match each layer's surrounding fill to the
  density of its boards at multiple spatial scales.
- **Global warp reduction:** reduce the copper moment about the stack mid-plane,
  allowing a bounded departure from each layer's density target.

`pcb ipc2581 warp` estimates elastic copper-laminate mismatch from the same
stackup and copper distribution. Use it to compare panelizations, not to clear
a panel against IPC-6012 limits. The model does not simulate electroplating,
cure shrinkage, or laminate anisotropy. Its process scales and material constants
are assumptions, not manufacturing qualification or measured bow predictions.

## Density domain

For copper layer `l`, let:

- `P` be the retained panel region;
- `F` be the immutable footprints: repeated board profiles for a board array,
  or placed assembly-panel outlines for a fab panel;
- `C_l` be fixed copper;
- `S_l` be the certified, initially empty safe region; and
- `d_l = area(C_l ∩ F) / area(F)` be the boards' existing layer density.

The density domain includes material that holds copper or could hold copper:

```text
D_l = F ∪ S_l ∪ C_l
A*_l = d_l area(D_l) - area(C_l)
```

`A*_l` requests generated area. Permanently bare material—clearance, reliefs,
tooling holes, process margins, and gaps too narrow for fill—is excluded from
`D_l`. Counting it would inflate the request and overfill the available frame.
When `C_l` lies inside `F`, the request reduces to `d_l area(S_l)`.

The [safe-region contract](../examples/board-array-balancing-region.md) defines
the layer-specific obstacle collection, clearance, and regularization certificate.
The solver projects the request onto attainable no-fill, solid-fill, or
perforated-fill geometry before distributing fill spatially.

## Local density and through-stack balance

Fixed copper coverage is sampled on the void lattice. Normalized Gaussian
kernels measure local density without treating space outside the panel as empty
copper. The objective is mean squared density deficit, equally weighted over
the process scale, successive octaves down to the lattice pitch, and the lattice
tile itself. Including short scales avoids patterns that average correctly at
one scale but alternate between locally saturated and sparse regions.

The physical stackup gives each conductor a signed weight `w_l = z_l t_l`,
where `z_l` is its center's distance from the stack mid-plane and `t_l` its
thickness. Mirrored layers have opposite weights. If thickness data cannot locate
every conductor, density matching still runs, but no stack moment is inferred
from invented spacing; creation warns about the missing data.

Before spatial redistribution, the stack correction can change each perforated
layer's fill density by at most `stack_flex_density`, measured over the lattice
it controls. It is weighted by lever arm and clamped at the moment-nulling amount;
layers with no weight do not move. The reference is zero total copper moment,
not preservation of the boards' original imbalance. Rails can therefore offset
some board imbalance, but only within that bound.

Each layer's selected area is then pinned while void radii redistribute copper
within it. Clipped boundary fragments remain fixed. Radii are quantized to shared
void-area levels with error diffusion to limit density/area drift; boundary
voids round upward to retain their minimum admitted disk. The reported moment
and densities describe emitted geometry, not just the unquantized solve.

For optimizer details, see the owning
[spatial solver](../../pcb-ir/src/geom/copper_balance/spatial.rs).

## Generated geometry

Perforated fill uses a triangular lattice of rounded-hexagonal voids. Pitch and
maximum radius constrain the minimum inter-void copper web; edge candidates
that cannot contain the minimum disk are rejected. A positive boundary web
restores material outside the voidable region. Repeated voids share IPC instances.

Gerber export resolves the lattice to dark-only geometry: interior cells use
shared `cell - void` ring flashes, while boundary cells become region geometry.
This avoids relying on repeated clear objects composited over underlying copper
in CAM importers.

## Pass-specific inputs

**Board array:** `F` is the repeated board footprints. Existing copper outside
them, including fiducials and array support, remains both an obstacle and part
of the density domain. The creation summary reports signed stack weights,
achieved and target layer densities, and stack moment before and after balancing.

**Fab panel:** `F` is the placed assembly-panel outlines. Mixed source types
contribute targets weighted by footprint area, extending their aggregate density
into the gutters. One safe region serves all layers: the usable gutters minus
placed outlines and their clearance. Process margins stay bare and outside the
density domain. Sources require identical physical stackups. The gutter lattice
starts at the usable region's minimum corner; it need not align with source
panels' immutable lattices.

## Process profile

`DenseCopperBalanceProfile::V1` is the only profile in use. Its geometry, scale,
quantization, and stack-flex defaults are maintained in the
[profile definition](../../pcb-ir/src/geom/copper_balance/mod.rs), not exposed
as CLI tuning options. They are first-party rules of thumb for conventional
rigid boards, not universal fabricator capabilities. Confirm the generated
features and residual imbalance against the actual manufacturing process.
