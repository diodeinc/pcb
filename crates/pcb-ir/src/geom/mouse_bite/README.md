# Experimental single-tab geometry

`geom::mouse_bite::build(Attachment)` is pure, in millimeters, and uses the
existing `ContourSet`, stroke, transform, and `attachment` query APIs. It accepts
explicit stock, board, support, boundary station, support anchor and board
witness. It does **not** choose support locations/counts, loads, clamps, laminate,
thickness, or panel dimensions. A tab has one perforated board interface; its
other end stays on support.

The [board-array adapter](../../../../pcb-ipc2581-tools/README.md#panels-and-copper-balancing)
uses this builder for `pcb ipc board-array create --mouse-bite`, selecting tabs
and emitting routed profile cutouts and NPTH holes. Manufacturing output does
not establish physical acceptance of the experimental preset.

## Preset and evidence

Authoritative source: Nick Poole, SparkFun, *Building a Better Mouse Bite*, March
2022, [white paper](https://cdn.sparkfun.com/assets/home_page_posts/4/4/0/0/Mousebites_Whitepaper_Final.pdf),
especially pages 5–9 and 11; [author's experiment description](https://news.sparkfun.com/4400).
The paper's recommendations, **not** its introductory industry survey, are:

| Dimension | `SparkFunShallow` | Provenance |
|---|---:|---|
| NPTH diameter | 0.381 mm (0.015 in) | Paper p9 |
| Center pitch | 0.635 mm (0.025 in) | Paper p9; applied as a straight chord between consecutive centers along the offset polygon, so the web is the same on curves |
| Nominal straight ligament | 0.254 mm | Pitch − diameter; the all-pairs chord ligament may not fall below it |
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

The straight 2 mm neck connects the explicit anchor to the supplied site.
`routed_void` owns what the router removes: the ideal void less the necks,
opened with the 0.5 mm cutter disk around them, which leaves rounded shoulders,
and exact along its walls everywhere else. The builder certifies that
construction locally and a panel generator emits the same one.
The board's outward disk offset supplies a cyclic polygon break row, preserving
all intervening vertices, with five centers each one straight pitch from the
last along it. This avoids inventing smooth-curve tangents, curvature
thresholds, or arc-length error bounds, and keeps the web between holes a
straight-line distance on curves.

Outputs keep retained substrate, routed removal, full drill masks, the straight
neck and analytic NPTH centers/diameters distinct. Routed removal and full drill
masks may overlap intentionally. Material is stock minus their union. Shoulders
are retained material, **not** the full router sweep; use routed removal for
routing-footprint obstacle checks. Adapters must map NPTH to existing
manufacturing tooling with appropriate layer/provenance data.

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
obstacle and release checks. Disk opening establishes a local swept-disk shape,
not a globally reachable toolpath. Check actual fixture and tool Z access
separately.

Canonical preparation, offsets and stroke expansion inherit each input region's
`Resolution` and fallible `GeometryAccuracy` budget. Generated geometry uses the
tightest input budget without widening it; contours retain their accumulated
uncertainty. Budget failures propagate through `QueryError::Accuracy`. Stored
geometry uncertainty remains a floor even with zero additional query allowance.
Repeated overlays can leave sub-micron slivers; bounded overlap/penetration
tests are not exact predicates or topology guarantees for source curves.

## Reproducible coupons and outstanding physical acceptance

Run `cargo run -p pcb-ir --example mouse_bite_coupon -- /tmp/coupons`.
It writes straight/curved overview and detail SVGs, separate routed/retained
polygon paths, virtual-break paths, and analytic NPTH CSVs. These are geometry
review/interchange artifacts, **not** fabrication-ready Gerbers or IPC. The
[coupon source](../../../examples/mouse_bite_coupon.rs) owns dimensions, datums,
and query allowances; preserve its units and datums in any fab adapter.

Physical acceptance is **unfulfilled**. No coupons from this preset have been
fabricated or measured. The intended initial evaluation matrix is rigid FR-4
at 0.8, 1.0, 1.2 and 1.6 mm, both geometries, both bending directions, at least
five replicates per cell and material/fabrication lot. This is a proposed test
range, not a supported production range. SparkFun primarily tested 1.6 mm PCB
and compared 0.8 mm; it does not establish validation of this offset, specific
laminate grades, copper stackups, or every thickness in between.

Protocol before claiming acceptance:

1. Agree on application-specific acceptance thresholds with manufacturing and
   have the fab approve drill/routing geometry, registration, and clearances.
   Freeze revision, laminate/weave, stackup, copper, finish, and process; measure
   actual hole diameter, pitch, neck, offset, and thickness.
2. Test both bending directions on separate coupons with a recorded, calibrated
   fixture/load protocol. Save force–displacement and peak force; derive moment
   from the **measured** lever arm, not an assumed clamp model.
3. Use separate samples for handling/reflow/support-load tests, including the
   actual populated panel and fixture; record sag, permanent set, early breaks,
   and component damage. A single-tab coupon cannot qualify the full panel.
4. Measure nubs/indentation, cracks, delamination, copper tear-out, debris, and
   cleanup. Save calibrated fracture photos and raw measurements with specimen,
   lot, revision, material, conditioning, and fixture/load traceability.

Neither connected ligaments, simulated virtual release, nor linear elastic
compliance establishes fracture load, crack trajectory, safe assembly support,
damage-free release, or acceptable residual nubs. Those require measured evidence.
