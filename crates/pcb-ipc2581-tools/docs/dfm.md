# DFM PDK, waiver, and report formats

`pcb ipc dfm check` checks an IPC-2581 design against a built-in or file-backed
TOML fabrication PDK. It writes diagnostic JSON or a portable
[report bundle](dfm-bundle.md) for external viewers.

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
  --pdk standard --waivers waivers.toml \
  --layout-target board-array --output dfm-report.json

pcb dfm board.zen --pdk standard --format bundle --output board.dfm.tar.zst
```

`pcb dfm` prepares and synchronizes the board's KiCad layout before exporting
temporary IPC-2581 and checking the canonical board. Use `pcb ipc dfm check`
to inspect existing manufacturing files without changing their source layout.

`--pdk` accepts an exact built-in name or a TOML path. `standard` is bundled
with `pcb` and reported as `builtin:standard`; use `./standard` to select a
same-named file. Both use the same parser and checks. `--layout-target` accepts
`board` or `board-array` and defaults to `board-array`.

`--format json` is the default and writes to stdout when `--output` is omitted.
`--include-geometry` adds the native scene; otherwise JSON contains diagnostics
without artwork. `--format bundle` requires `--output` and always includes the
scene, checked IPC XML, resolved PDK, and optional waivers. See the
[bundle contract](dfm-bundle.md) for archive layout and limits. Neither format
requires Node.js or a browser to produce. Including geometry does not change
findings, measurements, IDs, waivers, scope, or verdict.

A completed report is written before the command returns a failing status.
Only unwaived error findings fail its verdict. Preparation and output errors
also return a failing status. Preparation errors emit an explicit
[incomplete report](#incomplete-reports) when geometry or bundle output was
requested; ordinary JSON emits no report in that case. Bundles retain any
captured inputs and replace output atomically. I/O, serialization, or size-limit
failures can prevent output, leaving a previous artifact untouched; callers must
check exit status. Report output must not overwrite a source or use a KiCad
board path.

`SOURCE_DATE_EPOCH` fixes both `generated_at` and the date used for waiver
expiry. Pinning an old epoch reproduces that date's verdict, including waivers
that would have expired today.

## JSON report

A complete report has these fields:

- `schema_version`: integer report schema version.
- `generated_at`: RFC 3339 generation time.
- `verdict`: `pass` or `fail`.
- `tool`, `input`, `pdk`, and `waivers`: producer version plus input paths
  and SHA-256 identities; `waivers` also carries applied, expired, and
  unmatched entries and is `null` when no waiver file was given.
- `layout_target`: `board` or `board_array`.
- `layout`: the selected step, actual layout kind, checked coordinate frame,
  bounds, and physical occurrences. Occurrences retain their parent, source
  step, repeat indices, and transform into the checked frame; see
  [coordinates and topology](#coordinates-and-topology).
- `coordinate_system`: the unit, axis convention, and origin for all report
  geometry: `mm`, `x_right_y_up`, and `ipc_2581_design` in version 1.
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
- `scene`: present in bundles and with `--include-geometry` JSON; see
  [native scene](#native-scene).

`rule.finding_count` includes waived findings, and `waived_count` counts that
subset. A rule whose findings are all waived reports `pass`; active findings
determine `warning` or `fail` from rule severity. Skipped rules retain `skipped`
and their reason rather than becoming a pass from zero counts. Similarly,
`summary.findings` includes all findings, `summary.waived` counts the waived
subset, and `summary.errors`/`warnings` count only unwaived findings. A complete
verdict fails exactly when `summary.errors > 0`.

### Findings

- `id` hashes the rule, subjects, layers, and measured location. It remains
  stable while those facts are unchanged. Moving the representative point
  creates a new finding; its old waiver becomes `unmatched`. The measured
  value, added sites, presentation grouping, and extended provenance do not
  affect identity: a violation that shrinks or grows in place keeps its waiver.
- `rule_id`, `severity`, `title`, and `message` identify and explain the
  violation; `waived` and `waiver_reason` record acceptance.
- `measurement` carries `actual_mm`, `required_mm`, and signed `margin_mm` for
  geometry, or the corresponding `actual_count`, `required_count`, and
  `margin_count` for discrete counts. A nonnegative margin satisfies the limit.
  Signed annular enclosure can be negative. Do not clamp measurements or
  recompute the verdict from rounded display values.
- `location` carries a representative point, bounding box, and role-labelled
  witnesses. Nonspatial findings use `{point: null, bounding_box: null,
  witnesses: []}`, never a null `location`.
- `layers` identify every involved manufacturing layer: name, IPC-2581
  layer function, and `side` (`top`, `inner`, `bottom`) where the stackup
  determines it.
- `subjects` preserve role, kind, component, pin, net, padstack, and source
  indices when IPC-2581 provides them. `provenance` identifies the source
  definition and physical occurrence separately from the legacy flattened
  `source` locator; `drill_span` records the applicable copper-layer span.
  Unavailable fields remain `null` so consumers see one stable shape.
- `evidence` records `kind`, `role`, and applicable circle, segment, or bounds
  fields; unused fields remain `null`. `paths` contains closed region rings or
  open paths, preserving the checked material's winding and holes. Optional
  `display` retains native constructions for rendering, as specified below.
- `sites` retain individual failing regions or layers with their measurement,
  `measurement_kind`, `witnesses`, uncertainty, bounds, layers, subjects, and
  evidence. Nonspatial findings use `sites: []`. Site bounds describe the
  finding; viewers add their own camera padding. Witness-point separation is
  not necessarily the measured width or diameter.
- `group_key`, when available, groups proven equivalent causes for display.
  It does not replace the finding id or change the waiver unit. Every finding
  and physical occurrence remains accessible.

Check-owned sites, measurements, witnesses, and evidence paths are authoritative.
The optional `evidence.display` construction uses the same world millimeters:

| `kind` | Fields and rendering |
| --- | --- |
| `path` | SVG `paths` and `fill_rule`; fill each path separately, then union |
| `round_stroke` | Centerline `paths` and physical `width_mm`; round caps and joins |
| `circle_intersection` | `first` and `second` circles, each with `center` and `diameter` |
| `circle_minus_layer` | `center`, `diameter`, and exact copper `layer`; subtract that layer's composed native image from the circle |

Display constructions do not affect finding, site, or repeat-group identity.
`circle_minus_layer` needs its named scene pass even when the pass is hidden as
artwork. JSON without a scene can use the measured paths; a supplied scene
missing a required operand is invalid. Do not fit curves to measured polygons,
invent precision by changing tessellation tolerance, or infer an inscribed
width or radial enclosure from witness separation.

### Coordinates and topology

All geometry uses millimeters with X right and Y up. `layout.kind` distinguishes
`board`, `board_array`, and `fab_panel`; the `board_array` target also selects
fabrication panels. Board scope uses the canonical board's local frame
(`selected_board`). Array and fabrication-panel scope use `root_layout`,
including nested repeats. A canonical board check does not certify every
design in a mixed fabrication panel.

Each occurrence's cumulative `[a,b,c,d,tx,ty]` transform maps definition-local
coordinates to the checked frame: `x' = a*x + c*y + tx`,
`y' = b*x + d*y + ty`. Site, evidence, and scene coordinates are already placed;
do not transform them again. A parent occurrence filter includes descendants.

### Native scene

Scene version 1 contains `schema_version: 1`, full-layout `bounds`, and `passes`.
Each pass has `label`, semantic `feature`, exact `layer` name or `null` for shared
context, display `color`, and a full-layout native `svg` string. All passes
share the same viewport; sites never crop or duplicate the artwork.

Select passes using the rule's `view.features` and the selected **site's** exact
layer names, including shared `layer: null` passes. Finding-level layers are
only a summary. Every spatial site requires a matching pass for each feature
except `stackup`. An empty pass represents empty context; an absent required
pass makes the export incomplete. A scene with no spatial findings may have
an empty pass list.

The SVG root applies one PCB IR display Y flip. When inserting its contents
into a shared world-coordinate scene, remove that flip and apply the viewer's
camera convention. Preserve nested rotations, mirrors, aperture instances,
ordered polarity, masks, nonzero winding, holes, and final cutouts. Namespace
IDs and fragment references per compiled pass/view.

### Incomplete reports

An incomplete geometry or bundle export has `verdict: "incomplete"`, `schema_version`,
`generated_at`, `tool`, `input: {path}`, `pdk: {path}`, `layout_target`, and
`error: {message}`. It has no `summary`, `rules`, `findings`, or `scene`. Consumers
must handle this verdict before requiring complete-report fields; it is never
a pass, a clean board, or a skipped check.

### Schema evolution

Report and scene versions are independent; both currently use integer `1`.
New fields and new `kind`, `role`, `status`, rule, and method values may be
added within a version. Unknown optional fields can be ignored; unknown required
semantics must produce an explicit unsupported state, never a guessed rendering
or fabricated pass. Removing or changing existing field meanings requires a
new schema version.
