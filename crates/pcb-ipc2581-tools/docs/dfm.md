# DFM PDK, waiver, and report formats

`pcb ipc dfm check` turns fabrication capabilities from a built-in or file-backed
TOML PDK into geometry checks over an IPC-2581 design and writes JSON. Optional
full-scene vector geometry makes the same report interactive in the standalone
[DFM viewer](../../../apps/dfm-viewer).

## PDK

The PDK is strict and versioned. Unknown fields, bare numeric lengths, and
unsupported schema versions are errors — refusing an unknown capability key
is deliberate, because silently ignoring a capability the fab requires would
green-light unchecked boards. A PDK that configures no rules is an error
rather than a passing report.

```toml
schema_version = 1

[pdk]
id = "example-fab-standard"
name = "Example fabrication standard"
revision = "1"
# manufacturer = "Example Fabricator"
# process = "standard"

[capabilities.stackup]
minimum_copper_layer_count = 2
maximum_copper_layer_count = 10

[capabilities.drilling]
minimum_via_hole_diameter = "0.2 mm"
minimum_pth_hole_diameter = "0.2 mm"
minimum_npth_hole_diameter = "0.2 mm"
minimum_slot_width = "0.8 mm"
minimum_hole_to_hole_clearance = "10 mil"

[capabilities.copper]
minimum_via_annular_ring = { minimum = "100 um", preferred = "0.125 mm" }
minimum_pth_annular_ring = "0.125 mm"
minimum_feature_width = "0.09 mm"
minimum_copper_clearance = "0.09 mm"
minimum_board_edge_clearance = "0.25 mm"
minimum_vscore_to_copper_clearance = "15 mil"

[capabilities.soldermask]
minimum_web = "3 mil"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
```

Layer counts are positive integers and refer specifically to conductive layers
in the physical stackup. When both bounds are configured, the minimum must not
exceed the maximum. Every dimensional limit is a positive string containing a
number and `mm`, `mil`, `mils`, or `um`. Units can be mixed in one PDK. Checks
normalize lengths to millimeters and the report retains both the source
spelling and normalized value.

Every dimensional capability takes either a bare length — the binding minimum
— or a table with a `preferred` tier the fab would rather see met:

- The minimum lowers to an **error**-severity rule whose id is the
  capability path, e.g. `copper.minimum_via_annular_ring`. Error findings
  fail the verdict.
- A preferred tier lowers to a second, **warning**-severity rule under
  `<capability>.preferred`. Warning findings are reported and counted but do
  not fail the verdict. The preferred value must exceed the minimum.

Rule ids are the PDK capability paths, so every reported limit can be traced
to its PDK line verbatim.

## Evaluation model

Each rule has one verdict-producing evaluator. The evaluator uses the
highest-level representation that still states the measured quantity exactly,
and lowers only when fabrication composition can change that quantity. A
high-level pass is never allowed to suppress a later authoritative failure.

| Rules | Authoritative representation | Acceleration only |
| --- | --- | --- |
| Copper layer count | Conductive layers in the one physical IPC stackup | None |
| Hole diameter | Materialized IPC drill primitive and plating class | None |
| Nominal slot width | IPC slot primitive width | None |
| Outline slot width | Materialized filled route outline, then its narrowest maximal inscribed disk | None |
| Hole-to-hole clearance | Materialized drill circles and overlapping drill spans | Sorted bounds prune pairs already proven clear |
| Annular ring | Drill circle and final composed copper image on each applicable layer | Batched containment and an indexed copper boundary |
| Copper width | Final composed copper image, medial-axis width of each residue | Guarded opening localizes candidates |
| Copper clearance | Final composed copper attributed to occurrence-scoped electrical conductors | Sorted bounds prune conductor components already proven clear |
| Soldermask web | Final composed mask-opening image, medial-axis width of each residue | Guarded closing localizes candidates |
| V-score and board-edge clearance | Materialized line/profile geometry against final composed copper | Indexed copper boundaries |
| Board-array spacing | Materialized filled array profiles | Bounding boxes prune pairs already proven clear |

Geometric checks produce an aggregate measurement per subject and retain its
failing measured sites. Measurements carry the uncertainty of the flattened
boundaries they were measured against (one flattening tolerance per tessellated
curve; zero for stated primitives and analytic shapes). A pair of witness
points does not always encode a length: widths, diameters, and annular
enclosures retain their own measurement constructions. The engine fails a
minimum only when the measured value falls short beyond its own uncertainty,
so curve tessellation by itself cannot manufacture a violation. Layer-count
checks instead compare one exact integer with the configured minimum or
maximum.

Morphological opening and closing are deliberately candidate stages for width
and soldermask-web checks. Each candidate residue is measured on the medial
axis of the boundary around it — the Voronoi diagram of the nearby boundary
segments — as the diameter of its narrowest maximal inscribed disk. Disks
tangent only to incident segments are corner spokes and carry no width, so
one-sided residue (the bite an isolated corner sheds) measures nothing; disks
that a larger disk contains within the flattening tolerance are branches the
tessellation sprouted and are pruned, so a flattened arc measures its diameter
while a tapered spur still measures its tip. Bounds and spatial indices have
the same one-way contract: they can prove work unnecessary, but they cannot
emit a finding without the exact geometric measurement.

Copper-clearance ownership is retained through that same ordered artwork
composition. Dark features add material to their owner; clears and final
cutouts subtract material from every owner already painted. The evaluator
then measures only pairs of distinct owners. A net is scoped by its
materialized Step occurrence, so repeated boards do not become electrically
connected merely because they reuse the same net names.

`--layout-target board` extracts the canonical board step. `board-array`
materializes the root layout and every nested repeat, so the same evaluators
operate on a board array or on a fabrication panel without a second DFM code
path. Instance transforms therefore affect only extracted coordinates and
subject multiplicity. The board-array-spacing rule is intentionally narrower:
it compares direct sibling array instances of a fabrication panel and skips
when fewer than two exist.

## Rule semantics

- Copper layer count requires exactly one physical stackup. Every declared
  copper layer must occur exactly once in it; missing or ambiguous stackup data
  fails extraction rather than guessing from artwork layer names.
- Hole diameter rules measure every drilled hole of the rule's class;
  `minimum_slot_width` measures every routed slot. A slot's width is settled
  at extraction: the stated primitive width when present — exact, and
  verified against the materialized outline — and otherwise the outline's
  narrowest local width. Drill extraction fails rather than silently
  discarding a hole whose plating class or diameter is missing, or a slot
  whose stated width its outline contradicts.
- Hole-to-hole clearance measures edge-to-edge distance between hole pairs
  whose drill spans overlap; stacked blind and buried vias on disjoint spans
  do not interact.
- Annular ring measures the radial copper enclosure of each via or PTH hole
  on every applicable layer. A genuine intermediate plane anti-pad with no
  matching source land has no ring to measure. Both terminal layers, and any
  layer with a matching source land, must retain copper at the hole center;
  missing copper there is a zero-enclosure failure. One finding per hole
  reports the worst layer.
- `minimum_feature_width` reports narrow copper piece by piece after final
  polarity composition. `minimum_copper_clearance` measures the shortest
  boundary distance between distinct final conductor images. Same-net
  notches and same-net islands are not clearance subjects; touching or
  overlapping distinct conductors have zero clearance. Functional copper
  without a net fails extraction when this rule is configured instead of
  being guessed into an electrical domain. Fiducials and copper-balance
  support remain explicit auxiliary conductors.
- `soldermask.minimum_web` reports mask webs — gaps between mask openings —
  narrower than the limit. Morphology finds candidates; the medial-axis
  width decides each finding.
- V-score and board-edge clearance measure the shortest distance from the
  centerlines or profile outlines (cutouts included) to each layer's
  composed copper image.
- Board-array spacing measures boundary-to-boundary distance between the
  sibling board arrays a fabrication panel places; it requires
  `--layout-target board-array` and at least two arrays.

A rule that measures nothing reports `skipped` with a reason instead of a
vacuous `pass` — whether its subject pool is empty (no holes of its class,
no copper layers, no V-score lines) or the pool yields no eligible
measurements (`checked` would be zero).

## Waivers

A waiver file accepts specific findings by their stable ids without
silencing the rule:

```toml
[[waiver]]
finding = "dfm-9c41f2ab8d10"
reason = "edge plating intentional, approved by fab 2026-08"
expires = "2027-01-01"   # optional, YYYY-MM-DD
```

Waived findings stay in the report — marked `waived` with the reason,
counted in the summary — and are excluded from the verdict. Waivers that
name no finding in the run, and waivers past their expiry, are listed in the
report's `waivers` block instead of being applied, so the file cannot rot
silently.

## CLI

```bash
pcb ipc dfm check fabrication-panel.xml \
  --pdk standard \
  --waivers waivers.toml \
  --layout-target board-array \
  --output dfm-report.json
```

For an interactive report, include vector geometry and load the JSON in the
standalone viewer:

```bash
pcb ipc dfm check fabrication-panel.xml \
  --pdk standard \
  --layout-target board-array \
  --include-geometry --output panel.dfm.json

pcb dfm board.zen --pdk standard --include-geometry --output board.dfm.json

# From the repository root:
npm --prefix apps/dfm-viewer ci
npm --prefix apps/dfm-viewer run dev
```

Open the local URL printed by Vite and load the generated JSON. The viewer is
built separately from the Rust CLI; exporting a report never requires Node.js.

`pcb dfm` prepares and synchronizes the board's KiCad layout before exporting
temporary IPC-2581 and checking the canonical board. Use `pcb ipc dfm check`
to inspect existing manufacturing files without changing their source layout.

`--pdk` accepts an exact built-in name or a TOML file path. `standard` is
bundled with `pcb`; use a path such as `./standard` when a file has the same
name as a built-in. Reports identify built-ins with paths such as
`builtin:standard`. Custom PDK files otherwise use the same parser, rules, and
report pipeline.

`--layout-target` is `board` or `board-array` and defaults to `board-array`.
Omit `--output` to write JSON to stdout. Ordinary JSON stays lightweight and
does not render artwork. `--include-geometry` adds a `scene` without changing
findings, measurements, stable finding ids, waivers, scope, or verdict.

A completed report is written before the command returns a failing status.
Its verdict fails only on unwaived error findings. Input, configuration,
extraction, and output errors also return a failing status. With
`--include-geometry`, preparation errors write an explicit `incomplete` JSON
report instead of leaving a stale successful artifact. Ordinary diagnostic
JSON preserves its existing error behavior and emits no report on preparation
failure.

Set `SOURCE_DATE_EPOCH` to make `generated_at` — and with it
the whole report — reproducible. Waiver expiry is evaluated against that same
timestamp on purpose: expiry makes the verdict time-dependent, so pinning the
time must pin expiry too, or a reproduced report could not reproduce its
verdict. A pipeline that pins an old epoch is choosing to reproduce that
date's verdict, expired waivers included.

## Standalone viewer

The viewer is a lightweight Vite web app that loads a local report JSON file.
Report data stays in the browser; there is no board-upload service. A report
without `scene` still carries all diagnostic data, and the viewer explains that
`--include-geometry` is needed for artwork.

Findings are grouped by diagnostic family, with individual findings and sites
available for navigation. Repeated groups keep waived and active findings
separate. Selecting a site updates its detail view, measurement, relevant
layers, and source information. Counts without a physical location use a
stackup explanation instead of a fabricated board marker.

Both the main view and minimap use the same full vector scene. The main view
starts near the finding, then allows panning and zooming across the entire
board, array, or fabrication panel. The minimap distinguishes the finding's
fixed location from the live main-view bounds. Both views have scale bars
computed from their current camera and display size.

The native PCB IR rendering stack preserves source arcs, stroke styles,
polarity, holes, and final cutouts. It exports each semantic layer once, never
a raster overview or cropped copy per finding. Physical outlines remain true
paths. Checks supply native display recipes where the construction is known:
routed-slot contours, round clearance bands, overlapping circles, and required
copper minus a native copper layer. These use SVG curves, strokes, and masks
instead of increasing tessellation across the layout. Material definitions
are reused while navigating and evidence effects are bounded to the viewport.
Original measured paths and their flattening uncertainty remain independent
of this display geometry; arbitrary polygons are never smoothed. The required
and actual dimensions describe a distance, diameter, width disk, or count as
specified by the check; the camera cannot change those measurements.

The report retains passed and skipped rules, skip reasons, waived findings,
and stale waiver information. A board view selects one canonical source board;
it does not certify every board design in a mixed fabrication panel. A
`board-array` view includes the root layout and every nested occurrence. The
report's selected scope and occurrence information distinguish board findings
from array or panel support geometry. Filtering by an occurrence includes its
descendants, so selecting an array retains the findings on its nested boards.

## JSON report

The report is a self-contained record of what was checked:

- `schema_version`: integer report schema version.
- `generated_at`: RFC 3339 generation time.
- `verdict`: `pass` or `fail`; fails exactly when unwaived error-severity
  findings exist.
- `tool`, `input`, `pdk`, and `waivers`: producer version plus input paths
  and SHA-256 identities; `waivers` also carries applied, expired, and
  unmatched entries and is `null` when no waiver file was given.
- `layout_target`: `board` or `board_array`.
- `layout`: the selected step, actual layout kind, checked coordinate frame,
  bounds, and physical occurrences. Occurrences retain their parent, source
  step, repeat indices, and transform into the checked frame.
- `coordinate_system`: the unit, axis convention, and origin for all report
  geometry. Version 1 uses millimeters, `x_right_y_up`, and
  `ipc_2581_design`.
- `summary`: rule counts by status and finding counts by severity and
  waiver state.
- `rules`: one result per lowered rule: its id (the capability path, plus
  `.preferred` for warning tiers), severity, source and normalized limit,
  status (`pass`, `warning`, `fail`, or `skipped`), skip reason, and the
  measurement contract shared by all of its findings — `subject` (what one
  checked unit is), `quantity`, `method`, `comparison` (`minimum` or
  `maximum`), and `checked`, the number of measurements evaluated. `view`
  specifies the diagnostic family, whether it is spatial, and its semantic
  rendering features; `tier` distinguishes required and preferred limits.
- `findings`: violations in deterministic rule/location order.
- `scene`: present only with `--include-geometry`. Version 1 contains
  `schema_version: 1`, full-scene `bounds`, and `passes`. Each pass has a
  `label`, semantic `feature`, exact `layer` name or `null` for shared profiles,
  display `color`, and a full `svg` string. Match a site's `layers` and its
  rule's `view.features` to select passes. SVG paths use world millimeters with
  one root Y display flip; all passes share the same viewport. Native artwork
  can contain local aperture and mask references. Evidence and finding bounds
  remain separate from this shared display scene.

Each finding is intentionally a fat record:

- `id` is a content hash of what the finding is about — its rule, subjects,
  layers, and measured location. It is deterministic for a given input, and
  it survives design revisions exactly as long as those identifying facts
  are unchanged. A violation whose representative point moves is a new
  finding with a new id; its old waiver then surfaces as `unmatched` for
  re-review. The measured value is deliberately not part of the identity:
  a waived violation that shrinks or grows in place keeps its waiver.
  Added sites, presentation grouping, and extended provenance do not change
  the legacy finding identity.
- `rule_id`, `severity`, `title`, and `message` identify and explain the
  violation; `waived` and `waiver_reason` record acceptance.
- `measurement` carries `actual_mm`, `required_mm`, and signed `margin_mm` for
  geometry, or the corresponding `actual_count`, `required_count`, and
  `margin_count` for discrete counts. A nonnegative margin passes.
- `location` carries a representative point, bounding box, and role-labelled
  geometric witnesses.
- `layers` identify every involved manufacturing layer: name, IPC-2581
  layer function, and `side` (`top`, `inner`, `bottom`) where the stackup
  determines it.
- `subjects` preserve role, kind, component, pin, net, padstack, and source
  indices when IPC-2581 provides them. `provenance` identifies the source
  definition and physical occurrence separately from the legacy flattened
  `source` locator; `drill_span` records the applicable copper-layer span.
  Unavailable fields remain `null` so consumers see one stable shape.
- `evidence` is a fat geometry record. `kind` and `role` identify the
  evidence; circle, segment, and bounds fields are populated as applicable
  and otherwise remain `null`. `paths` contains closed region rings or open
  paths; regions retain the checked material's winding and holes.
  Optional `display` gives the check's native rendering construction, in the
  same world millimeters, without replacing those measured paths:
  `path` carries independent SVG `paths` and an explicit `fill_rule` (fill each
  separately, then union); `round_stroke` carries centerline `paths` and the
  physical `width_mm` with round caps/joins; `circle_intersection` carries
  `first` and `second` circles (`center`, `diameter`); `circle_minus_layer`
  carries `center`, `diameter`, and the exact copper `layer` name. The last
  recipe requires that layer's scene pass; without it, use the original paths.
  Display recipes do not participate in finding, site, or repeat identities.
- `sites` retain individual failing regions or layers with their measurement,
  `measurement_kind`, `witnesses`, uncertainty, bounds, layers, subjects, and
  evidence.
  Nonspatial count findings have no sites. Site bounds describe the finding,
  not a padded camera window. Witness-point separation is not necessarily
  the measured width or diameter; render the specified construction.
- `group_key`, when available, groups proven equivalent causes for display.
  It does not replace the finding id or change the waiver unit. Every finding
  and physical occurrence remains accessible.

Within schema version 1, new fields and new `kind`, `role`,
`status`, rule, and method values may be added. Removing or changing the
meaning of an existing field requires a new schema version.

An incomplete geometry export has `verdict: "incomplete"`, `schema_version`,
`generated_at`, `tool`, `input: {path}`, `pdk: {path}`, `layout_target`, and
`error: {message}`. It deliberately has no `summary`, `rules`, `findings`, or
`scene`, since the checker did not establish a complete result. Consumers must
handle that verdict before interpreting the complete-report fields.
