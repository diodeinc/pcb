# Fab-panel copper balancing

This document defines copper balancing for a fabrication panel created by
`fab-panel create`. It is the fabrication-panel pass that
[board-array copper balancing](board-array-copper-balancing.md) reserves as
out of scope; the density model, solver, and generated geometry are shared
with that pass, so only the differences are described here.

## Balancing region

The placed assembly panels are immutable, and the fabrication-panel step adds
no per-layer geometry of its own. The balancing region is therefore just the
gutters: the usable stock region between the reserved process margins, minus
the placed assembly-panel outlines with the standard clearance, regularized
and certified by the same construction as the board-array pass. One safe
region serves every copper layer.

The reserved process margins stay bare. They are excluded from the density
domain entirely — not merely from the safe region — so the solver never
chases a deficit it cannot fill at the margin boundary.

As a defensive measure, any copper found outside the placed panel outlines
joins the shared obstacle set, shrinking the certified safe region instead of
failing the solve. A correctly generated fabrication panel has no such
copper.

## Density targets

Each copper layer targets the aggregate copper density measured inside the
placed assembly-panel outlines. Because the assembly panels were themselves
balanced during board-array creation, this extends their already-uniform
density into the gutters. Mixed panel types weight the target by their
footprint areas, since the denominator is the union of all placements.

With domain `U` (the usable region), footprints `F`, and fixed copper `C_l`,
the requested generated area reduces to filling the gutters at the panel
density:

```text
d_l  = area(C_l ∩ F) / area(F)
A*_l = d_l area(U) - area(C_l) = d_l area(U \ F)
```

## Through-stack metric

`fab-panel create` requires every source to carry one identical physical
stackup, so the signed stack weights come from that shared stackup exactly as
in the board-array pass. When thickness data is missing, layers balance
independently and the summary warns, as before.

## Lattice

The void lattice originates at the usable region's minimum corner. It is not
phase-aligned with the lattices inside the source assembly panels; the
sources are immutable and the gutters are solver-fresh territory, so no
alignment is needed and determinism is preserved for identical inputs.

## Flow

Balancing runs unconditionally inside `fab-panel create`: the panel is
generated provisionally without balance copper, the provisional document
supplies the composed per-layer copper images and placed-panel outlines, and
the final document is written with the generated copper as positive
`LayerFeature` contours in the fabrication-panel step. A per-layer summary is
printed, matching the board-array report.
