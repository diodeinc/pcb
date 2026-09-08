# Courtyard-only eligible-outline review

This example exercises `pcb_ir::geom::attachment::outline::eligible_outline` on
real source boards. It is a review surface, not a tab generator or manufacturing
approval. The library function has no KiCad, corpus, display, population or
material policy: it accepts immutable canonical substrate, declared exclusion
regions (or explicitly missing evidence), an explicit rectangular footprint and
query tolerance. It returns open intervals with original ring/edge/arclength
identity, state, contributing obstacle indices and positional uncertainty.

## Reproduce

Use the seven-source archive from the corpus work (#1241). It contains
`layouts/<Name>.kicad_pcb`, `exports/<Name>.xml`, `provenance/demo-<name>.json`,
`sources.tsv` and `outcomes.tsv`. Do not re-export or replace corpus snapshots to
run this review. The extractor checks each original layout hash; the Rust runner
checks the paired XML hash. Bramble is `demo/b/DM0003.git`, Marlow is
`demo/b/DM0002.git`, and Governor is `demo/b/DM0001.git`; source aliases, original
paths, revisions and hashes remain in the output.

```sh
# KiCad's Python bindings, not the development venv's Python.
/usr/bin/python3 crates/pcb-ir/examples/outline_review/extract.py \
  /tmp/outline-corpus /tmp/outline-courtyards

# Explicit review choices: widths 2/3/5 mm, inward 0.5 mm, outward 2 mm,
# preparation accuracy 0.01 mm. None is a recommended manufacturing value.
cargo run -p pcb-ir --example outline_review -- \
  /tmp/outline-corpus/exports /tmp/outline-courtyards \
  /tmp/outline-review 2,3,5 0.5 2 0.01
```

Serve the output directory as a static HTTP site. `index.html` loads `data.json`;
both are self-contained, with no external scripts, fonts or services. In an Amp
orb, use a supervised service and share its portal, not a localhost URL.

## What the colors mean

- **Green:** every centre in this open interval clears the included courtyards
  with the entire rectangle. Best-effort green is conditional on omitted data.
- **Red:** the contracted footprint intersects an included courtyard throughout
  the interval, beyond the positional comparison band in the polygon model.
- **Amber:** clearance is within that band, or strict evidence is incomplete.
- **Blue rectangle:** the full footprint at the selected centre. Hover, click
  to pin, or select an interval with the keyboard. The board never changes.

Top and bottom courtyards both constrain the answer; visibility toggles do not
change the calculation. Only explicit source `IsDNP()` excludes a footprint.
No designator-name heuristics exempt logos, test points, virtual parts or missing
connector courtyards. Review the missing list and make those decisions upstream.

The source adapter asks KiCad to construct courtyards from original graphics,
accepting one closed, non-self-intersecting, hole-free polygon or circle per
required side. Circles retain their source center/radius until canonical pcb-ir
preparation; KiCad's already-flattened cache does not provide an accuracy bound.
Other source curves are reported as unsupported. It does not parse IPC package
outlines, where KiCad can silently substitute a hull. It reports absent or
unsupported evidence rather than inventing geometry. Closure/provenance validation is **not** full KLC or datasheet
validation. The shape must still satisfy the intended body/land/mating-space
contract in [KLC F5.3](https://klc.kicad.org/footprint/f5/f5.3.html).

## Geometry and limits

For each original straight polygon edge, transform the obstacles into its
tangent/outward-normal frame. Intersect each with the rectangle's normal strip;
project each connected intersection onto the tangent and expand by half the
width. These are continuous configuration-space collision intervals, not sampled
candidate sites. Holes and concavities pass through canonical filled-region
intersection; no whole-obstacle bounding box or convex hull substitutes for them.
Connected-strip projection is the only use of component bounds.

Expanded and contracted rectangles bracket positional uncertainty. The comparison
band includes substrate history, the obstacle's preparation budget (reserving
transform/boolean rounding), and the query's numerical guard. The example sets
that numerical guard to one thousandth of the explicit accuracy budget, and
records it. It uses zero significance to retain narrow rings. Accuracy errors
propagate; prior lost precision is never erased or silently re-budgeted.

Results concern the canonical polygon model, not source-curve tangent, arclength
or topology certification. Corners are not averaged or merged; interval endpoints
carry no guarantee. Internal rings remain indexed. The library does not validate
landing containment in the substrate, frame connectivity, inward-facing edge
eligibility, router entry/access, fracture mechanics or support count. These are
later gates, not conclusions inferred from a green interval.

This particular review includes **courtyards only**. Source rule areas are counted
and disclosed, not silently promoted from copper-placement constraints to tab
keepouts. Copper, drill/rout images, unknown Z spans and heights are not constraints
in this run. Other callers may supply explicit exclusion regions under the same
API; there is no manufacturing clearance, material or height fallback.

## Checks

```sh
cargo nextest run -p pcb-ir -E 'test(geom::attachment)'
cargo test --doc -p pcb-ir
uv run ruff check crates/pcb-ir/examples/outline_review/extract.py
```

Analytical tests cover exact interval lengths, full-footprint versus centre-only
collisions, interior/exterior obstacles, hole and concavity preservation,
translation/rotation/mirror invariance, monotonic footprint/uncertainty behavior,
tangency, internal-ring provenance, missing evidence and unchanged substrate.
Generated reviews are exploratory results, not automatically accepted snapshots.
