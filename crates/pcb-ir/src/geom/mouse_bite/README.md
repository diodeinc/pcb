# Experimental single-tab geometry

`geom::mouse_bite::build(Attachment)` is pure, in millimeters, and uses the
existing `ContourSet`, stroke, transform, and `attachment` query APIs. It accepts
explicit stock, board, support, boundary station, support anchor and board
witness. It does **not** choose support locations/counts, loads, clamps, laminate,
thickness, or panel dimensions. No CLI panelization or manufacturing export is
enabled. A tab has one perforated board interface; its other end stays on support.

## Preset and evidence

Authoritative source: Nick Poole, SparkFun, *Building a Better Mouse Bite*, March
2022, [white paper](https://cdn.sparkfun.com/assets/home_page_posts/4/4/0/0/Mousebites_Whitepaper_Final.pdf),
especially pages 5–9 and 11; [author's experiment description](https://news.sparkfun.com/4400).
The paper's recommendations, **not** its introductory industry survey, are:

| Dimension | `SparkFunShallow` | Provenance |
|---|---:|---|
| NPTH diameter | 0.381 mm (0.015 in) | Paper p9 |
| Center pitch | 0.635 mm (0.025 in) | Paper p9; applied to offset polygon arc length |
| Nominal straight ligament | 0.254 mm | Pitch − diameter; curved chord ligaments are measured separately |
| Outward center offset | 0.127 mm (0.005 in) | **Experimental project adaptation**, not a SparkFun measurement |
| Nominal straight board intrusion | 0.0635 mm | Radius − outward offset, not a damage-zone prediction |
| Hole count / straight neck width | 5 / 2.0 mm | Project construction choice; 2.54 mm center span overlaps shoulders |
| Router radius | 0.5 mm | Paper p8 example fab's 1 mm minimum slot, not a universal fab capability |

Do not misattribute the outward offset: p9 recommends holes **centered over the
board edge** for cosmetic use, or recessed inward tangent holes for an
uninterrupted outer dimension. Page 6 explicitly says outward holes were not
tested. This adaptation leaves less nominal intrusion than centered holes but
can leave protruding nubs. The 0.127 mm offset deliberately leaves one sixth of
the hole diameter inside a straight board, rather than setting a nearly tangent
feature below the geometry resolution. It is an experimental starting point,
not an optimized or physically validated value. SparkFun's p9/p11 maximum 3 mm
tab width is for depaneling-tool compatibility; this library does not certify a
tool's jaw access, support shape, or curved-tab compatibility.

## Geometry contract and limits

The straight 2 mm neck connects the explicit anchor to the supplied site. Opening
the ideal routing void with the 0.5 mm cutter disk leaves rounded shoulders.
The board's outward disk offset supplies a cyclic polygon break row, preserving
all intervening vertices and five equally spaced stations. This avoids inventing
smooth-curve tangents, curvature thresholds, or arc-length error bounds.

Outputs keep retained substrate, routed removal, full drill masks, analytic
NPTH centers/diameters, attachment footprint and shoulder material distinct.
Routed removal and full drill masks may overlap intentionally. Material is stock
minus their union. `shoulders` is retained material, **not** the full router sweep;
use routed removal for routing-footprint obstacle checks. Adapters must map NPTH
to existing manufacturing tooling with appropriate layer/provenance data.

Construction rejects disconnected stock results, missing/uncertain inter-hole
ligaments, and incomplete release under a 0.002 mm virtual break sweep. All-pairs
drill clearance prevents folded rows from silently overlapping nonadjacent holes.
`after_break(width, witnesses, tolerance)` exposes the shared material query for
other explicit geometric probes; the probe width is **not** a physical kerf.
Witnesses in an uncertainty band never count as successful connection/release.

Validated synthetic cases are a straight edge and a convex 10 mm-radius source
circle represented by canonical flattened polygons, with a 3 mm board/support
gap. Arbitrary concave, oblique, tight-radius, holed or obstructed attachments are
not manufacturing-qualified. The builder's checks do not replace full-panel
`check_footprints` / `cutter_reachability` with explicit obstacles and entry points.
Disk opening establishes a local swept-disk shape, not a globally reachable
toolpath. Check actual fixture and tool Z access separately.

Canonical preparation, offsets and stroke expansion inherit each input region's
`Resolution` and fallible `GeometryAccuracy` budget. Generated geometry uses the
tightest input budget without widening it; contours retain their accumulated
uncertainty. Budget failures propagate through `QueryError::Accuracy`. The coupon
example uses the default 0.01 mm approximation budget with zero significance,
plus query allowances of 0.015 mm boundary and 0.000001 mm numerical uncertainty.
Stored geometry uncertainty is a floor even when a query supplies zero additional
boundary allowance; neither allowance certifies source-curve topology. Repeated overlay on curved
polygons can leave sub-micron slivers: tests bound both overlap area (<0.000001
mm²) and penetration (<0.000001 mm), rather than claiming exact predicates.
No topology guarantee extends to the pre-flattened source curves.

## Reproducible coupons and outstanding physical acceptance

Run `cargo run -p pcb-ir --example mouse_bite_coupon -- /tmp/coupons`.
It writes straight/curved overview and detail SVGs, separate routed/retained
polygon path data, virtual-break path data, and analytic NPTH CSVs. SVGs show
substrate green, shoulders darker green, routed removal gray, holes white,
nominal board boundary dashed black and proposed break row red. These are
geometry review/interchange artifacts, **not** fabrication-ready Gerbers or IPC.
The straight board is 20×20 mm; the curved board is a 20 mm diameter disk centered
at (0, −10); support spans (−10, 3) to (10, 13); stock spans (−10, −20) to (10, 13).
The single neck runs to (0, 5). Units and these datums must survive any fab adapter.

Physical acceptance is **unfulfilled**. No coupons from this preset have been
fabricated or measured. The intended initial evaluation matrix is rigid FR-4
at 0.8, 1.0, 1.2 and 1.6 mm, both geometries, both bending directions, at least
five replicates per cell and material/fabrication lot. This is a proposed test
range, not a supported production range. SparkFun primarily tested 1.6 mm PCB
and compared 0.8 mm; it does not establish validation of this offset, specific
laminate grades, copper stackups, or every thickness in between.

Protocol before claiming acceptance:

1. Have the fab approve NPTH diameter, drill registration, slot/radius and copper
   clearances; freeze coupon revision, laminate grade/weave, thickness, stackup,
   copper distribution, finish and routing/drilling process. Fabricate both
   geometries. Measure actual diameter, pitch, neck, offset and thickness.
2. Clamp the board side at y = −5 mm with a padded jaw and apply an out-of-plane
   load at the support-side line y = 10 mm with a spreader. Record the actual
   contact positions, clamp torque, load-cell calibration, loading rate and
   displacement. Reverse bending direction on separate coupons; do not reuse
   fractured coupons. Record force–displacement and peak force; calculate moment
   about the break row from the **measured** lever arm, not a guessed clamp model.
3. Before destructive testing, use separate samples to exercise the actual
   intended assembly handling, reflow and support loads. Record sag, permanent
   set, premature breaks and component damage. A single-tab coupon is not a
   substitute for an entire populated panel/fixture test.
4. Photograph both fracture faces with a calibrated scale. Measure maximum nub
   protrusion and indentation relative to the nominal edge, crack extent,
   delamination, copper tear-out and loose fragments; inspect brittle nearby
   components where relevant. Record break effort, debris and required cleanup.
5. Save raw records with specimen/lot/revision IDs, geometry, material/stackup,
   measured dimensions, conditioning, fixture/load history, force/displacement,
   nub/indentation/damage measurements and photos. Establish application-specific
   acceptance thresholds with manufacturing **before** evaluating pass/fail.

Neither connected ligaments, simulated virtual release, nor linear elastic
compliance establishes fracture load, crack trajectory, safe assembly support,
damage-free release, or acceptable residual nubs. Those require measured evidence.
