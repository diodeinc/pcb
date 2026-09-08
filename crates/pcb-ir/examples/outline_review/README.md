# Boundary-following courtyard review

`pcb_ir::geom::attachment::outline::eligible_outline` partitions an immutable
polygon boundary into clearance intervals for a band. Width is **boundary
arclength**: the band crosses vertices and the cyclic seam. Each edge supplies
its inward/outward normal strip; triangular bevel joins connect adjacent strips.
The blue selection renders that union, not a tangent rectangle or a finished tab.

## Geometry contract

Pointwise strip/obstacle intersections project to boundary stations. Vertex-join
intersections contribute point stations. Expanding those stations by half the
width gives continuous blocked centre intervals, without sampling. Closed edge
contacts supplement regularized booleans so tangencies cannot silently disappear.

The inward band is constructed **before** checking it against the substrate.
Its missing material is not clipped away and accepted. Expanded/contracted
depths and arclength spans bracket positional uncertainty. Definite landing
failure requires resolved missing material (void erosion by the comparison band);
thin voids and contact with another board edge remain unknown. Inward zero
explicitly disables landing checks. Local offset collapse/reversal and spans
covering a whole ring are unknown, not mechanical rejections. No radius threshold
is guessed; a gentle rounded corner can pass even with many short polygon edges.

The comparison band includes substrate history, the largest input preparation
budget and caller tolerances. The example uses a numerical guard of accuracy/1000
and zero significance. Errors propagate; source precision is never reset.
Results retain original component/ring/edge/arclength identity. Open interval
endpoints have no guarantee; source-curve topology/arclength are not certified.

- **Green:** the complete band clears supplied exclusions and its landing lies on
  material, subject to the selected evidence policy.
- **Red:** a courtyard or resolved missing substrate blocks the band.
- **Amber:** positional uncertainty, unsupported band geometry or missing evidence.

This is a first-stage filter, not support/frame discovery. Internal rings remain
identified, not approved for external-frame attachment. Actual tab construction,
perforation fit, global connectivity, cutter entry/access and strength are later
gates. Rule areas are counted but not interpreted. Copper, drills/routing, heights,
optical/probe access and material properties are not constraints in this review.

## Source contract

The adapter reads original KiCad graphics in board coordinates: one closed,
non-self-intersecting, hole-free polygon or circle per required side. Segments
must join exactly; rectangles/polygons retain source vertices and circles retain
centre/radius. Unsupported curves, mixed shapes, gaps and multiple loops remain
missing evidence. **No courtyard cache, inset, endpoint repair, hull or IPC
package-outline fallback.** Source validation is not datasheet or full
[KLC F5.3](https://klc.kicad.org/footprint/f5/f5.3.html) compliance validation.

Both board sides constrain clearance; visibility toggles only affect display.
DNP footprints are omitted. Set the custom KiCad footprint field `PhysicalRole`
to `component` for an installed body or `board-feature` for a bare-board feature.
Only the latter omits a body exclusion. A test-point post is still a component.
No reference-name, BOM, placement, board-only or model-presence heuristic applies.
Missing/invalid roles are unknown in strict mode; best-effort retains available
courtyards conditionally. Source JSON v2 preserves raw roles, UUIDs and hashes.
Library annotation and native pad-role export work is tracked in ENG-1487.

## Reproduce

Use the original seven-board archive from corpus work #1241: `layouts/`,
`exports/`, `provenance/`, `sources.tsv` and `outcomes.tsv`. Layout/XML hashes are
checked; originals are never changed. Bramble is `demo/b/DM0003.git`, Governor
is `demo/b/DM0001.git`, Marlow is `demo/b/DM0002.git`; aliases, paths and revisions
remain in the output. None of these originals declares `PhysicalRole`.

```sh
/usr/bin/python3 crates/pcb-ir/examples/outline_review/extract.py \
  /tmp/outline-corpus /tmp/outline-courtyards

# Explicit review inputs, not manufacturing recommendations:
# arclength widths 2/3/5, inward 0.2, outward 2, accuracy 0.01 (all mm).
cargo run -p pcb-ir --example outline_review -- \
  /tmp/outline-corpus/exports /tmp/outline-courtyards \
  /tmp/outline-review 2,3,5 0.2 2 0.01

cargo nextest run -p pcb-ir -E 'test(geom::attachment)'
cargo test --doc -p pcb-ir
/usr/bin/python3 -m unittest discover -s crates/pcb-ir/examples/outline_review -v
uv run ruff check crates/pcb-ir/examples/outline_review/
```

Serve the generated `index.html` and `data.json` together over HTTP. In an orb,
use a supervised service and share its portal. The page has no external assets.
Generated reviews are exploratory artifacts, not automatically accepted snapshots.
