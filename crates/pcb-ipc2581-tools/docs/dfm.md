# DFM PDK, waiver, and report formats

`pcb ipc dfm check` checks an IPC-2581 design against a built-in or file-backed
TOML fabrication PDK. It writes one self-contained JSON report with diagnostics,
native vector geometry, and the exact PDK source for external viewers.

## PDK

The PDK is strict and versioned. Unknown fields, bare numeric lengths, and
unsupported schema versions are errors — refusing an unknown rule key
is deliberate, because silently ignoring a rule the fab requires would
green-light unchecked boards. A profile with neither measurable support bounds
nor DFM rules is an error rather than a passing report.

```toml
schema_version = 2
default_profile = "standard"

[pdk]
id = "example-fab"
name = "Example fabrication kit"
revision = "2"

[sources.capabilities]
title = "Example Fab PCB capabilities"
url = "https://fab.example/capabilities"
accessed = "2026-09-01"

[profiles.standard]
name = "1 oz rigid standard"
technologies = ["rigid"]
source = "capabilities"

[profiles.standard.support]
copper_layers = { minimum = 2, maximum = 10 }

[profiles.standard.defaults]
material = "FR-4"
board_thickness = "1.6 mm"
outer_copper_weight = "1 oz"
inner_copper_weight = "0.5 oz"
soldermask_color = "green"

[[rules.drilling.hole_diameter]]
id = "drilling.via_hole"
select = { hole = "via" }
limit = { minimum = "0.2 mm", preferred = "0.25 mm" }
source = "capabilities"

[[rules.drilling.hole_aspect_ratio]]
id = "drilling.via_aspect_ratio"
select = { hole = "via" }
limit = { maximum = 8.0 }
source = "capabilities"

[[rules.drilling.slot_width]]
id = "drilling.plated_slot"
select = { plating = "plated" }
cases = [
  { id = "2-layer", when = { copper_layers = { exact = 2 } }, limit = { minimum = "0.50 mm" } },
  { id = "multilayer", when = { copper_layers = { minimum = 3, maximum = 10 } }, limit = { minimum = "0.35 mm" } },
]

[[rules.drilling.hole_to_hole_clearance]]
id = "drilling.via_to_pth"
select = { first_hole = "via", second_hole = "pth" }
limit = { minimum = "10 mil" }

[[rules.drilling.hole_to_board_edge_clearance]]
id = "drilling.npth_to_board_edge"
select = { hole = "npth" }
limit = { minimum = "0.30 mm", preferred = "0.40 mm" }

[[rules.drilling.slot_to_board_edge_clearance]]
id = "drilling.nonplated_slot_to_board_edge"
select = { plating = "nonplated" }
limit = { minimum = "0.30 mm" }

[[rules.copper.annular_ring]]
id = "copper.via_annular_ring"
select = { hole = "via" }
limit = { minimum = "100 um", preferred = "0.125 mm" }

[[rules.copper.plated_slot_enclosure]]
id = "copper.plated_slot_enclosure"
limit = { preferred = "0.25 mm" }

[[rules.copper.hole_clearance]]
id = "copper.via_hole_clearance"
select = { hole = "via" }
limit = { minimum = "0.20 mm", preferred = "0.25 mm" }

[[rules.copper.slot_clearance]]
id = "copper.plated_slot_clearance"
select = { plating = "plated" }
limit = { minimum = "0.40 mm", preferred = "0.50 mm" }

[[rules.copper.feature_width]]
id = "copper.feature_width"
cases = [
  { id = "outer-1oz", when = { copper = { position = "outer", weight = "1 oz" } }, limit = { minimum = "0.10 mm" } },
]
```

One PDK file is a kit with named profiles. A built-in name selects both a kit
and one profile; a custom file runs its `default_profile`. An executable
profile lowers only rules whose `profiles` list contains it; an omitted list
means every profile. A `metadata_only` profile fails closed before checking.
This lets a kit publish a standard taxonomy without claiming that missing
numeric rules establish compliance.

The schema gives each kind of constraint one place:

- `profile.support` is the hard eligibility envelope for the whole profile.
  A design outside its copper-layer range fails profile qualification. The
  engine emits these checks as reserved `profile.support.*` report rules;
  PDK authors do not write layer-count DFM rules.
- `select` identifies the physical subjects a rule measures. Hole rules select
  `via`, `pth`, or `npth`; slots select `plated` or `nonplated`; hole-pair rules
  state both classes. Hole-aspect-ratio rules accept only plated `via` or `pth`
  holes; selecting `npth` is a schema error. A
  `rules.copper.hole_clearance` selector chooses the drill class measured to
  unrelated final copper.
- `limit` defines an unconditional dimensional minimum, preferred value, or
  both, or one required aspect-ratio maximum.
- `cases` defines named conditional limits when one value is not enough. A
  rule uses either `limit` or `cases`, never both.
- `profile.defaults` records missing-data assumptions. Copper weight defaults
  are used when a weight-conditioned case has no stated stackup weight.

Case conditions use structured ranges such as
`copper_layers = { exact = 2 }` or `{ minimum = 3, maximum = 10 }`. Copper
rules may also condition on `copper = { position = "outer", weight = "1 oz" }`.
The parser rejects unsupported conditions and any pair of cases whose domains
overlap. Therefore, at most one case from a rule applies to a design or copper
layer; non-applicable case rules are reported as skipped. A condition that
needs stackup context fails extraction when the IPC-2581 file has no
unambiguous physical stackup. The `technologies` list remains descriptive
metadata because imported designs do not yet state rigid, flex, and HDI
technology reliably enough for qualification.

Layer counts are positive integers. Every dimensional minimum is a positive
string containing a number and `mm`, `mil`, `mils`, or `um`; copper weight is a
positive `oz` string. A hole-aspect-ratio `limit.maximum` is a positive finite
unitless number; zero, nonfinite, string-valued, and otherwise malformed ratios
are rejected. Units can be mixed. Checks normalize lengths to
millimeters and retain both source spelling and normalized value. Profile
defaults document an order's assumptions in the report; an outer/inner copper
weight default is also the fallback for a weight-conditioned rule when the
source stackup does not state that weight.

Every direct dimensional limit or case has a `minimum`, a `preferred` tier, or
both:

- The minimum lowers to an **error**-severity rule whose id is the
  authored rule id, or `<id>.<case>` for a named case. Error findings fail the
  verdict.
- A preferred tier lowers to a second, **warning**-severity rule under
  `<id>.preferred` or `<id>.<case>.preferred`. Warning findings are reported
  and counted but do not fail the verdict. It may stand alone when the PDK has
  no binding minimum. When both tiers are present, the preferred value must
  exceed the minimum.

Each direct or named-case hole-aspect-ratio limit instead has one required
**error**-severity `maximum`; values above it fail the rule. It has no preferred
tier.

Rule ids, profile metadata, source citations, and the exact PDK TOML are
retained in the report for auditability.

## Evaluation model

Each rule has one verdict-producing evaluator. The evaluator uses the
highest-level representation that still states the measured quantity exactly,
and lowers only when fabrication composition can change that quantity. A
high-level pass is never allowed to suppress a later authoritative failure.

| Check | Authoritative representation | Acceleration only |
| --- | --- | --- |
| Profile copper-layer support | Conductive layers in the one physical IPC stackup | None |
| Hole diameter | Materialized IPC drill primitive and plating class | None |
| Plated-hole aspect ratio | Physical IPC stackup thickness over the resolved circular drill span, divided by finished hole diameter | None |
| Nominal slot width | IPC slot primitive width | None |
| Outline slot width | Materialized filled route outline, then its narrowest maximal inscribed disk | None |
| Hole-to-hole clearance | Materialized drill circles and overlapping drill spans | Sorted bounds prune pairs already proven clear |
| Hole-to-board-edge clearance | Analytic drill circle against its enclosing physical board profile, including cutouts | Indexed profile boundaries |
| Slot-to-board-edge clearance | Materialized filled route outline against its enclosing physical board profile, including cutouts | Indexed profile boundaries |
| Annular ring | Drill circle and final composed copper image on each applicable layer | Batched containment and an indexed copper boundary |
| Plated-slot enclosure | Minimum distance from the materialized slot boundary to final copper with the cavity filled for the query | Indexed boundaries |
| Hole-to-copper clearance | Analytic drill circle and attributed final composed copper on each layer in its drill span | Indexed attributed-copper boundaries |
| Slot-to-copper clearance | Materialized filled route outline and attributed final composed copper on each layer in its physical span | Indexed region boundaries |
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
so curve tessellation by itself cannot manufacture a violation. Profile
copper-layer qualification instead compares one exact integer with the
configured support bounds.

Aspect ratio is a scalar rather than a geometric distance. Its spatial site and
circle evidence identify the hole, while its measurement records the actual
unitless ratio, maximum, drilled-span thickness, finished diameter, and
thickness source. Witness separation does not encode the ratio.

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

Hole-to-copper clearance uses the same attributed composition. For a via or
PTH, copper proven to belong to the hole's occurrence-scoped net or a resolved
physical land is excluded; other-net, auxiliary, and unattributed functional
copper remains an offender. For an NPTH, every final copper owner is an
offender, including a same-named net. The rule requires a declared through or
resolvable layer span and fails extraction when the span is unavailable rather
than guessing which copper layers the drill intersects.

`--layout-target board` extracts the canonical board step. `board-array`
materializes the root layout and every nested repeat, so the same evaluators
operate on a board array or on a fabrication panel without a second DFM code
path. Instance transforms therefore affect only extracted coordinates and
subject multiplicity. The board-array-spacing rule is intentionally narrower:
it compares direct sibling array instances of a fabrication panel and skips
when fewer than two exist.

## Rule semantics

- Profile copper-layer qualification requires exactly one physical stackup.
  Every declared copper layer must occur exactly once in it; missing or
  ambiguous stackup data fails extraction rather than guessing from artwork
  layer names.
- Hole diameter rules measure every drilled hole of the rule's class. Slot
  width rules measure routed slots of the selected plating class. A slot's width is settled
  at extraction: the stated primitive width when present — exact, and
  verified against the materialized outline — and otherwise the outline's
  narrowest local width. Drill extraction fails rather than silently
  discarding a hole whose plating class or diameter is missing, or a slot
  whose stated width its outline contradicts.
- Hole aspect ratio is physical drilled-span thickness divided by finished
  circular hole diameter. A through hole uses IPC-2581 `overallThickness` when
  it is positive and finite, otherwise a complete sum of physical stackup
  layer thicknesses. A resolved blind or buried hole sums only layers from its
  first copper endpoint through its last endpoint, inclusive. Routed slots and
  NPTH holes are never subjects. If IPC thickness is incomplete, a selected
  profile's `defaults.board_thickness` may be assumed only for a declared
  through hole; the rule reports that assumption. An incomplete resolved span,
  or an incomplete through span without that default, skips the rule with the
  precise missing-data reason rather than claiming a pass.
- Hole-to-hole clearance measures edge-to-edge distance between hole pairs
  whose drill spans overlap; stacked blind and buried vias on disjoint spans
  do not interact.
- Hole- and slot-to-board-edge clearance measure true edge-to-edge distance
  from each circular hole or materialized routed-slot outline to the boundary
  of its enclosing physical board profile. Profile cutouts are board edges.
  The profile must have the feature's exact physical occurrence, so a repeated
  board never measures against another board or its panel outline. A feature
  crossing or outside its board material has zero clearance.
- Annular ring measures the radial copper enclosure of each via or PTH hole
  from its nominal circular geometry on every applicable layer. It is not
  tolerance-aware finished-board acceptance: drill size/position, registration,
  and plating tolerances are not modeled. Geometric flattening uncertainty is
  not a fabrication tolerance. Plated-slot enclosure is outside this check.
  A genuine intermediate plane anti-pad with no matching source land has no
  ring to measure. Both terminal layers, and any layer with a matching source
  land, must retain copper at the hole center;
  missing copper there is a zero-enclosure failure. One finding per hole
  reports the worst layer.
- `rules.copper.plated_slot_enclosure` measures nominal artwork enclosure of
  plated slots only (no selector). It supports the same limits and copper-layer
  cases as annular ring. It measures the actual materialized slot, including
  asymmetric and curved outlines, not a circular or bounding-box proxy. Within
  the slot's span, terminal layers and matching physical source lands require
  copper; an intermediate layer without a land or copper meeting the slot is exempt.
  The rule requires one physical stackup and a declared, resolvable slot span;
  missing data fails extraction rather than assuming a passing whole-stack check.
  Layers use physical stackup order, not XML declaration order. Copper is
  required around the opening, not inside the routed cavity:
  the query fills that cavity, then measures the minimum distance from the slot
  boundary to the resulting copper boundary, including other copper cutouts.
  Absent or breached surrounding copper has zero enclosure. The existing
  flattening tolerance heals quantization seams where the cavity is filled;
  enclosure within that tolerance is reported as zero with geometric uncertainty.
  One finding per slot retains all failing layer sites, with
  `boundary_enclosure` or `missing_copper` measurements. This is not a residual
  ring prediction after routing, plating, or registration tolerances.
- Hole-to-copper clearance measures the edge-to-edge distance from each
  circular drill to the nearest unrelated final copper owner on every copper
  layer in its declared span. Via and PTH copper is exempt only when net or
  physical-land identity proves that it belongs to the hole. NPTH copper is
  never exempt. Missing drill-span identity makes the check incomplete.
- Slot-to-copper clearance (`rules.copper.slot_clearance`) measures the true
  materialized filled slot outline, including its ends, against unrelated
  final copper on its physical span. Touching or overlapping copper has zero
  clearance. Select `plated` or `nonplated` with `select.plating`. Plated slots
  exempt only occurrence-scoped own-net copper or resolved physical lands
  with matching stated padstack identity and no contradictory stated net;
  netless functional and foreign copper remain offenders. Nonplated slots
  exempt nothing. The rule requires one unambiguous physical stackup and a
  declared through or resolvable layer span; missing data fails extraction.
  This eligibility requirement also applies to preferred warning tiers: missing
  data is an incomplete check, not a geometric shortfall or a valid pass.
  Copper layer declaration order does not determine the physical span.
- Copper feature-width rules report narrow copper piece by piece after final
  polarity composition. Copper-clearance rules measure the shortest
  boundary distance between distinct final conductor images. Same-net
  notches and same-net islands are not clearance subjects; touching or
  overlapping distinct conductors have zero clearance. Functional copper
  without a net fails extraction when this rule is configured instead of
  being guessed into an electrical domain. Fiducials, copper-balance
  support, and netless pads remain explicit auxiliary conductors.
- Soldermask-web rules report mask webs — gaps between mask openings —
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
pcb ipc dfm check board.xml -o board.dfm.json
pcb dfm board.zen -o board.dfm.json
pcb dfm board.zen --open
pcb open board.dfm.json
```

`pcb dfm` prepares and synchronizes the board's KiCad layout before exporting
temporary IPC-2581 and checking the canonical board. Use `pcb ipc dfm check`
to inspect existing manufacturing files without changing their source layout.

`--pdk` defaults to `standard` and accepts an exact built-in name or a TOML
path. `standard`, `jlcpcb-1oz` (`jlc`), and `ipc-1a` through `ipc-3c` are
bundled. `ipc` selects the opinionated Class 2 / Producibility Level B default.
Use `./standard` to select a same-named file. Both sources use the same parser
and checks. `--layout-target` accepts
`board` or `board-array` and defaults to `board-array`. Add
`--waivers waivers.toml` to apply a [waiver file](#waivers).

The `standard` PDK prefers 0.40 mm plated and nonplated slot-to-copper
clearance. Shortfalls produce warnings, not a failed manufacturing verdict.
This is conservative Diode routing guidance, not a manufacturer capability
or an IPC requirement. Slot clearance is not enabled in `jlcpcb-1oz` without
a manufacturer source.

The `jlcpcb-1oz` PDK, also available as `jlc`, executes the public rigid FR-4
capability table for 2-32 copper layers and 1 oz outer copper. It deliberately
omits the one-layer NPTH-only service, 2 oz rules, local 3 mil BGA exceptions,
and panel spacing, which depends on the chosen routing, mouse-bite, or V-cut
process. JLCPCB publishes a 0.10 mm soldermask web for standard colors and 0.13
mm for black or white; this combined PDK conservatively uses 0.13 mm for every
color.

The nine `ipc-1a` through `ipc-3c` built-ins preserve performance Classes 1-3
crossed with Producibility Levels A-C, but they are executable **partial Diode
baselines**, not IPC profile matrices. They check maximum via and PTH aspect
ratio at 6.0 for Level A, 8.0 for Level B, and 10.0 for Level C. They also
check via, PTH, and NPTH hole-to-copper clearance at 0.25 mm for Level A,
0.20 mm for Level B, and 0.15 mm for Level C. They check via, PTH, NPTH,
plated-slot, and nonplated-slot clearance to the board edge at 0.50 mm for
Level A, 0.40 mm for Level B, and 0.30 mm for Level C. Plated and nonplated
slot-to-copper clearance prefers 0.50 / 0.40 / 0.30 mm for A/B/C and warns
on shortfalls. Diode deliberately allows more routing margin than for circular
drills; these are opinionated guidance, not IPC requirements.
These values apply across all three performance classes. Each profile assumes 1.6 mm board
thickness for the through-hole aspect-ratio fallback described above. Diode
chose these opinionated values using IPC design topics as context; they are not
licensed IPC numeric matrices, do not prove full IPC compliance, and do not
imply IPC certification. A pass covers only the checks listed in the selected
profile's `coverage` metadata.

All nine IPC profiles also adopt the following thresholds from the bundled
Diode `standard.toml` PDK, cited as `diode-standard`. These general-purpose
baselines apply uniformly across classes and levels; they are not IPC
requirements or qualified capabilities for every technology or copper weight.

| Check | Standard-derived limit | Scope |
| --- | --- | --- |
| Copper feature width | Required minimum 5 mil (0.127 mm) | Local widths of final composed copper on every copper layer, not just nominal trace widths |
| Copper spacing | Required minimum 5 mil (0.127 mm) | Distinct final conductors on every copper layer; fabrication spacing, not voltage-dependent insulation clearance |
| Annular ring | Required minimum 0.125 mm for vias, 7 mil (0.1778 mm) for PTHs | Nominal circular-hole enclosure on applicable copper layers, not tolerance-aware finished-board acceptance |
| Hole diameter | Required minimum 0.20 mm for vias/NPTHs, 15 mil (0.381 mm) for PTHs | Source-declared circular hole diameter, interpreted as finished size |
| Slot width | Required minimum 0.5 mm plated, 31 mil (0.7874 mm) nonplated | IPC primitive width or PCB IR outline minimum width; excludes the optional 0.6 mm plated preferred tier |
| Hole-to-hole clearance | Required minimum 10 mil (0.254 mm) | All six unordered circular via/PTH/NPTH pair classes with overlapping physical drill spans; excludes routed-slot pairs |
| Copper-to-board-edge clearance | Required minimum 15 mil (0.381 mm) | Each layer's composed copper image against board edges, including cutouts; intentional edge plating uses finding-specific [waivers](#waivers) |
| Soldermask web | Preferred 4 mil (0.1016 mm), warning only | Webs between final composed mask openings, not mask-to-pad registration; warnings do not fail the verdict |

Hole-diameter checks do not model a separate drill-tool diameter, plating
allowance, or manufacturing tolerance, and cannot verify the exporter's
finished-size interpretation.

Standard and all nine IPC profiles warn below 0.25 mm nominal plated-slot
copper enclosure through a preferred tier. Custom PDKs can supply a required
minimum. JLCPCB profiles do not enable this check.

Output is UTF-8 JSON on stdout unless `-o` / `--output` is supplied. The
recommended suffix is `.dfm.json`. Every complete report includes the native
scene and PDK source for viewing without companion files. PCB generates no
DFM HTML or viewer application.

`pcb open <report.dfm.json>` opens an existing report in
`https://dfm.diode.computer`. A one-shot local page redirects the same browser
tab with the compressed report in a temporary URL fragment. The viewer removes
the fragment before loading the report, and no report bytes are uploaded.
`pcb dfm --open` performs the same handoff after generating the report. With
`-o`, it saves and opens that file; without `-o`, it uses a temporary report
instead of writing JSON to stdout. Fresh `pass`, `fail`, and `incomplete`
reports are all opened, while the DFM command keeps its normal exit status.
Reports over 16 MiB or encoded URLs over 900 KiB must be selected in the
viewer manually.

A completed report is written before the command returns a failing status.
Only unwaived error findings fail its verdict. Preparation and output errors
also return a failing status. Preparation errors emit an explicit
[incomplete report](#incomplete-reports). File output is replaced atomically,
including incomplete reports. I/O, serialization, or size-limit failures can
prevent output and leave a previous artifact untouched; callers must check exit
status. Output must not overwrite a source or use a KiCad board path.

`SOURCE_DATE_EPOCH` fixes both `generated_at` and the date used for waiver
expiry. Pinning an old epoch reproduces that date's verdict, including waivers
that would have expired today. The same input bytes, source labels, options,
and epoch produce identical JSON. A separate `.zen` layout/export operation
may change its IPC bytes or temporary source label.

## JSON report

A complete report has these fields:

- `schema_version`: integer report schema version.
- `generated_at`: RFC 3339 generation time.
- `verdict`: `pass` or `fail`.
- `tool`: producer name and version.
- `input`: original IPC input path, SHA-256, and byte size.
- `pdk`: resolved kit and profile metadata, assumptions, source citation, path,
  exact TOML `source`, SHA-256, and the selected profile's
  `support.copper_layers` range (`exact`, `minimum`, and `maximum`).
- `waivers`: file path and SHA-256 plus applied, expired, and unmatched entries;
  `null` when no waiver file was given. See [source identity](#source-identity).
- `layout_target`: `board` or `board_array`.
- `layout`: the selected step, actual layout kind, checked coordinate frame,
  bounds, and physical occurrences. Occurrences retain their parent, source
  step, repeat indices, and transform into the checked frame; see
  [coordinates and topology](#coordinates-and-topology).
- `coordinate_system`: the unit, axis convention, and origin for all report
  geometry: `mm`, `x_right_y_up`, and `ipc_2581_design` in version 1.
- `summary`: rule counts by status and finding counts by severity and
  waiver state.
- `rules`: one result per lowered rule. A direct limit uses its authored id; a
  named case uses `<id>.<case>`; a preferred tier appends `.preferred` to
  either form. Each result includes severity, source and normalized limit,
  status (`pass`, `warning`, `fail`, or `skipped`), skip reason, and the
  measurement contract shared by all of its findings — `subject` (what one
  checked unit is), `quantity`, `method`, `comparison` (`minimum` or
  `maximum`), and `checked`, the number of measurements evaluated. `view`
  specifies the diagnostic family, whether it is spatial, and its semantic
  rendering features; `tier` distinguishes required and preferred limits.
  `assumptions` lists profile defaults actually used while evaluating that
  rule, and is empty when no assumption was needed.
- `findings`: violations in deterministic rule/location order.
- `scene`: required native artwork for the complete checked layout; see
  [native scene](#native-scene).

`rule.finding_count` includes waived findings, and `waived_count` counts that
subset. A rule whose findings are all waived reports `pass`; active findings
determine `warning` or `fail` from rule severity. Skipped rules retain `skipped`
and their reason rather than becoming a pass from zero counts. Similarly,
`summary.findings` includes all findings, `summary.waived` counts the waived
subset, and `summary.errors`/`warnings` count only unwaived findings. A complete
verdict fails exactly when `summary.errors > 0`.

### Source identity

`pdk.source` is required in complete reports. It contains the exact resolved
UTF-8 TOML used for evaluation, including comments, unit spelling, CRLF line
endings, and any final newline. Built-in and file-backed PDKs follow the same
contract. Do not reserialize TOML or normalize text. `pdk.sha256` is the SHA-256
of the UTF-8 bytes of that decoded JSON string, encoded as 64 lowercase
hexadecimal characters. Consumers verify this hash; a checksum detects
corruption, not authenticity or fabrication approval.

`input.sha256` and `size_bytes` identify the original on-disk IPC input bytes,
including compression for a `.xml.zst` input. For `pcb dfm`, they identify its
temporary exported IPC input. The XML is not included. Waiver source is also
not included: its hash, applied/expired/unmatched entries, and each finding's
waiver fields preserve what was applied.

All `path` fields are descriptive provenance. They may be absolute,
`builtin:standard`, or no-longer-existing temporary paths. Input and waiver
hashes identify absent source files; never fetch paths or open files on the
consumer's machine to render or validate the report.

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
  `margin_count` for discrete counts. Aspect-ratio measurements instead carry
  `actual_ratio`, `maximum_ratio`, signed `margin_ratio`,
  `drilled_span_thickness_mm`, `finished_hole_diameter_mm`, and
  `thickness_source`. A nonnegative margin satisfies the limit.
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
  finding; viewers add their own camera padding. `outside_board` identifies a
  drilled feature that crosses or lies outside its physical board material and
  therefore has zero clearance. Witness-point separation is not necessarily
  the measured width or diameter. Scalar aspect-ratio sites have no measurement
  witnesses; their circle evidence locates the hole.
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
artwork. A missing required operand makes the scene invalid. Do not fit curves
to measured polygons, invent precision by changing tessellation tolerance, or
infer an inscribed width or radial enclosure from witness separation.

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

Every complete report contains a scene, including reports with only nonspatial
checks. Scene version 1 contains `schema_version: 1`, full-layout `bounds`, and
`passes`.
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

The report is the scene authority. Render its native SVG and check-owned
evidence; do not infer geometry from messages or screenshots, rerun DFM, or
recreate the verdict in a second geometry engine. Measurements, IDs, waivers,
and checked scope do not depend on display constructions.

### Incomplete reports

An incomplete report has `verdict: "incomplete"`, `schema_version`,
`generated_at`, `tool`, `input: {path}`, `pdk: {path}`, `layout_target`, and
`error: {message}`. It has no `summary`, `rules`, `findings`, or `scene`. Consumers
must handle this verdict before requiring complete-report fields; it is never
a pass, a clean board, or a skipped check. Its minimal `pdk` has no source or
hash, even if a PDK was read before the failure.

### Reader limits and safety

Producer and consumer enforce these inclusive limits (1 MiB = 1,048,576 bytes):

| Resource | Limit |
| --- | --- |
| UTF-8 JSON report | 128 MiB |
| UTF-8 bytes of decoded `pdk.source` | 1 MiB |

Readers check file size before allocating or parsing. Decode strict UTF-8 and
parse in a worker so cancellation and malformed input do not block the UI. Validate
versions, finite numbers, ordered bounds, supported coordinate frames, unique
IDs, references, and aggregate counts before rendering. Bound JSON/geometry
complexity and SVG reference traversal independently of byte size.

All JSON, TOML, and SVG are untrusted even when the PDK hash matches. Parse SVG
into an inert allowlisted tree; never inject uploaded markup as HTML. Reject
scripts, event handlers, foreign objects, styles, entities, external resources,
and nonlocal fragment references. Show an explicit invalid or unsupported
report error when safe rendering is unavailable.

Reports can contain private board data, local paths, components, nets, PDK
comments, and waiver reasons. A file picker or drop action authorizes local
inspection only, not backend uploads or telemetry. Do not log payloads, persist
uploads silently, or keep hidden copies after replacement. Terminate workers,
release buffers/object URLs, and ignore stale async results when a new file
replaces the current load.

### Schema evolution

Report and scene versions are independent; both currently use integer `1`.
New fields and new `kind`, `role`, `status`, rule, and method values may be
added within a version. Unknown optional fields can be ignored; unknown required
semantics must produce an explicit unsupported state, never a guessed rendering
or fabricated pass. Removing or changing existing field meanings requires a
new schema version.
