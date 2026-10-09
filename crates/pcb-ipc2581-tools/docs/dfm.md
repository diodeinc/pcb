# DFM PDK and report formats

`pcb ipc dfm check` checks an IPC-2581 design against a built-in or file-backed
TOML fabrication PDK. It prints a JSON summary and can write the full report:
one self-contained SQLite database with diagnostics, native vector geometry,
and the exact PDK source.

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

[[rules.assembly.diagnostic]]
id = "assembly.missing_population"
select = { diagnostic = "missing_population" }

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

- `rules.assembly.diagnostic` enables a required component-data predicate.
  Supported diagnostics are `missing_population`, `conflicting_population`,
  `missing_reference_designator`, `missing_package`,
  `missing_physical_terminations`, and `nonstandard_bottom_rotation`. These
  categorical rules always require zero diagnostics and therefore take no
  numeric limit. `nonstandard_bottom_rotation` is a warning: it flags
  bottom-side parts whose rotation pcb corrected for a known KiCad 9.0.8–9.0.9
  or 10.0.0–10.0.4 exporter defect.
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
layer; non-applicable case rules are reported as `not_applicable`. Cases need
not cover every design, but what they leave out is outside the capability the
PDK states: when the design holds subjects for the rule and no case applies to
its stackup, or to one of its copper layers, one more result under the authored
rule id reports `incomplete` and names what no case matched. It carries the
strictest severity the cases declare, so an uncovered required limit fails the
verdict instead of leaving the layer silently unchecked. A condition
that needs stackup context leaves its rule `incomplete` when the IPC-2581 file
has no unambiguous physical stackup. The `technologies` list remains descriptive
metadata because imported designs do not yet state rigid, flex, and HDI
technology reliably enough for qualification.

A copper-weight condition names a nominal weight, while a stackup states a
thickness that only approximates one: 1 oz is 34.3 µm nominal (IPC-4562A), is
allowed down to 90 % as foil, typically finishes near 88 %, and plates up on
outer layers. A layer therefore matches the condition whose standard weight
(⅛, ¼, ⅓, ½, then whole ounces) is nearest its own as a ratio, so 1.4 mil is
1 oz, 0.07 mm is 2 oz, and a 0.0152 mm finished inner layer is 0.5 oz. Two
cases naming the same standard weight overlap.

Layer counts are positive integers. Every dimensional minimum is a positive
string containing a number and `mm`, `mil`, `mils`, or `um`; copper weight is a
positive `oz` string. A hole-aspect-ratio `limit.maximum` is a positive finite
unitless number; zero, nonfinite, string-valued, and otherwise malformed ratios
are rejected. Units can be mixed. Checks normalize lengths to
millimeters and retain both source spelling and normalized value.

Every direct dimensional limit or case has a `minimum`, a `preferred` tier, or
both:

- The minimum lowers to an **error**-severity rule whose id is the
  authored rule id, or `<id>.<case>` for a named case. Error findings fail the
  verdict.
- A preferred tier lowers to a second, **warning**-severity rule under
  `<id>.preferred` or `<id>.<case>.preferred`. Warning findings are reported
  and counted but do not fail the verdict. It may stand alone when the PDK has
  no binding minimum. When both tiers are present, the preferred value must
  exceed the minimum, and a subject that fails both is reported once, as the
  required tier's error; the preferred rule still reports `warning`.

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
so curve tessellation by itself cannot manufacture a violation. A value that
falls short by less than its uncertainty is neither a violation nor proof the
limit is met: the rule lists it under `unresolved` with its value, uncertainty,
location, and layers, `summary.unresolved` counts them, and the CLI reports how
many measurements were within measurement uncertainty. They do not affect the
verdict. Copper-width and soldermask-web candidates are extracted only where
they are certainly below the limit, so those two rules list none. An aspect
ratio exceeds its maximum only when the drilled depth exceeds what the maximum
allows for that diameter by more than the same comparison epsilon, so a span
summed from decimal layer thicknesses does not fail a limit it sits on. Profile
copper-layer qualification instead compares one exact integer with the
configured support bounds.

Aspect ratio is a scalar rather than a geometric distance. Its spatial site and
circle evidence identify the hole, while its measurement records the actual
unitless ratio, maximum, drilled-span thickness, finished diameter, and
thickness source. Witness separation does not encode the ratio.

Morphological opening and closing are deliberately candidate stages for width
and soldermask-web checks. Each candidate residue is measured on the medial
axis of the unsnapped prepared boundary, as an inscribed disk diameter. The
checker constructs the analytic point/point, point/line, and line/line
bisectors of nearby facing segments: on distinct rings, or on one ring
neither adjacent nor turned within a quarter turn of each other, the same
judgement that selects the candidates. Quadratic roots delimit valid
contact domains, nearest-boundary intervals, residue crossings, and radius
limits. Vertices and spans share this construction; no snap-grid repair,
sample-dependent disk pruning, or angular tuning margin decides eligibility.
The contacts must be equidistant, globally nearest, and strictly obtuse from
the center beyond numerical roundoff. Endpoint normal cones exclude corner
contacts shadowed by adjoining edges. The disk radius must exceed its recorded
boundary uncertainty. The minimum and its evidence come from the same analytic
axis; flattening is used only to draw it, not to measure it.
This is a resolved-contact measurement of the prepared geometry, not a
reconstruction of intended curves from an already polygonal input. Source
boundary uncertainty contributes to scalar width, but does not bound contact
direction or certify source-branch correspondence or topology. Bounds and
spatial indices can prove work unnecessary, but cannot emit a finding without
the geometric measurement.

Copper-clearance ownership is retained through that same ordered artwork
composition. Dark features add material to their owner; clears and final
cutouts subtract material from every owner already painted. The evaluator
then measures only pairs of distinct owners. A net is scoped by its
materialized Step occurrence, so repeated boards do not become electrically
connected merely because they reuse the same net names.

Hole- and slot-to-copper clearance use the same attributed composition; the
[rule semantics](#rule-semantics) state which copper a drilled feature owns. A
drill or rout layer that declares no `Span` is through-board, exactly as import
reads it. A declared span that cannot be resolved in the physical stackup
leaves the rule `incomplete` rather than guessing which copper layers the
drill intersects.

`--layout-target board` checks the canonical board step. `board-array` checks
the root layout and every nested repeat. A layout is a few Step definitions
placed many times, so every Step is checked once, in its own coordinates,
together with everything it places: a board, the cell that carries it, the
array of cells, the fabrication panel of arrays. A measurement belongs to the
lowest Step that holds all of its subjects:

- What one Step's content decides on its own — a hole's diameter, aspect
  ratio, annular ring and distance to its own Step's profile, a slot's width
  and enclosure, the width of the Step's own copper, the distance between two
  of its features — is measured in that Step's frame, once, however often the
  layout repeats it.
- What takes two Steps — a rail's tooling hole against board copper, a cell's
  mouse-bite holes or routed slots against the board they carry, copper,
  holes or mask openings of neighbouring boards — is measured in the frame of
  the Step that places both, between its own content and each thing it places
  and across placements, never again inside one placement.
- A V-score line crosses every board along it, so it is a reference for the
  Step that draws it and for every Step placed under that one. Each measures
  its own copper against the line wherever the line comes within the rule's
  limit of that copper, in its own frame, once for all the placements the
  line crosses alike. A board profile is measured against the copper of its
  own Step.

A frame holds of what its Step places only what such a measurement can reach:
placed copper within the largest conductor clearance limit of something
outside its placement, and placed mask openings within the web check's reach
of something outside theirs, together with the openings that chain to them.
Bounds decide this conservatively, so nothing a rule could measure is left
out; everything else a Step places is measured in its own frame and only
counted here.

The same evaluators therefore run on a lone board, a board array or a
fabrication panel without a second DFM code path: a lone board is a layout of
one Step placed once. Rules are rigid-motion invariant, so a Step's findings
hold wherever it is placed; the report lists those placements once per Step
(see [frames](#frames)) instead of repeating findings.

## Rule semantics

- Profile copper-layer qualification requires exactly one physical stackup.
  Every declared copper layer must occur exactly once in it; missing or
  ambiguous stackup data leaves every rule that reads the stackup `incomplete`
  rather than guessing from artwork layer names.
- Hole diameter rules measure every drilled hole of the rule's class. Slot
  width rules measure routed slots of the selected plating class. A slot's width is settled
  at extraction: the stated primitive width when present — exact, and
  verified against the materialized outline — and otherwise the outline's
  narrowest local width. A hole whose plating class or diameter is missing, a
  square hole, which no circular measurement describes, or a slot whose stated
  width its outline contradicts, is never silently discarded or measured as
  something it is not: it leaves every hole rule, or every slot rule,
  `incomplete`.
- Hole aspect ratio is physical drilled-span thickness divided by finished
  circular hole diameter. A through hole uses IPC-2581 `overallThickness` when
  it is positive and finite, otherwise a complete sum of physical stackup
  layer thicknesses. A resolved blind or buried hole sums only the depth its
  drill removes. A blind hole enters at its outer layer and terminates on its
  target land, so its depth runs from the capture land foil to the target land,
  as IPC-T-50M measures a microvia: the entry copper, the dielectric, and any
  intermediate copper, but not the target copper it lands on. A buried hole is
  drilled through its whole sub-stack, both terminal layers included. Routed slots and
  NPTH holes are never subjects. If IPC thickness is incomplete, a selected
  profile's `defaults.board_thickness` may be assumed only for a declared
  through hole; the rule reports that assumption. An incomplete resolved span,
  or an incomplete through span without that default, leaves the rule
  `incomplete` with the precise missing-data reason rather than claiming a pass.
- Hole-to-hole clearance measures edge-to-edge distance between hole pairs
  whose drill spans share board depth. Blind and buried vias on disjoint
  spans do not interact, and neither do vias stacked on a shared terminal
  layer (L1–L2 over L2–L3): each drills only the dielectric between its own
  terminal layers.
- Hole- and slot-to-board-edge clearance measure true edge-to-edge distance
  from each circular hole or materialized routed-slot outline to the boundary
  of its enclosing physical board profile. Profile cutouts are board edges.
  The profile is that of the Step that owns the feature, so a repeated board
  never measures against another board or its panel outline. A feature
  owned by a panel or array step, such as a rail tooling hole, is measured to
  that panel's or array's own profile. A feature crossing or outside its
  material has zero clearance.
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
  The rule requires one physical stackup and a resolvable slot span; missing
  data leaves it `incomplete` rather than assuming a passing whole-stack check.
  Layers use physical stackup order, not XML declaration order. Copper is
  required around the opening, not inside the routed cavity:
  the query fills that cavity, then measures the minimum distance from the slot
  boundary to the resulting copper boundary, including other copper cutouts.
  Absent or breached surrounding copper has zero enclosure. The existing
  flattening tolerance heals quantization seams where the cavity is filled;
  enclosure within that tolerance is reported as zero with geometric uncertainty.
  One finding per slot retains all failing layer sites, with
  `clearance` measurements for positive boundary distances and `missing_copper`
  for zero enclosure. This is not a residual ring prediction after routing,
  plating, or registration tolerances.
- Hole-to-copper clearance measures the edge-to-edge distance from each
  circular drill to the nearest unrelated final copper owner on every copper
  layer in its declared span. Via and PTH copper is exempt only when net or
  physical-land identity proves that it belongs to the hole, by the same rule
  as plated slots: its own occurrence-scoped net, or a resolved land with the
  same stated padstack and no contradictory net, whose net it then owns on
  every layer. A land linked only because the drill overlaps it proves nothing,
  so a drill through foreign copper is reported. Other-net, auxiliary, and
  unattributed functional copper remains an offender. NPTH copper is never
  exempt, a same-named net included.
- Slot-to-copper clearance (`rules.copper.slot_clearance`) measures the true
  materialized filled slot outline, including its ends, against unrelated
  final copper on its physical span. Touching or overlapping copper has zero
  clearance. Select `plated` or `nonplated` with `select.plating`. Plated slots
  exempt only occurrence-scoped own-net copper or resolved physical lands
  with matching stated padstack identity and no contradictory stated net;
  netless functional and foreign copper remain offenders. Nonplated slots
  exempt nothing. The rule requires one unambiguous physical stackup and a
  resolvable layer span; missing data leaves it `incomplete`. This eligibility
  requirement also applies to preferred warning tiers: missing data is an
  incomplete check, not a geometric shortfall or a valid pass.
  Copper layer declaration order does not determine the physical span.
- Copper feature-width rules report narrow copper piece by piece after final
  polarity composition. Copper-clearance rules measure the shortest
  boundary distance between distinct final conductor images. Same-net
  notches and same-net islands are not clearance subjects; touching or
  overlapping distinct conductors have zero clearance. Functional copper
  without a net leaves this rule `incomplete` instead of being guessed into an
  electrical domain; rules that measure the composed image are unaffected. Fiducials, copper-balance
  support, and netless pads remain explicit auxiliary conductors.
- Soldermask-web rules report mask webs — gaps between mask openings —
  narrower than the limit. Morphology finds candidates; the medial-axis
  width decides each finding.
- V-score and board-edge clearance measure the shortest distance from the
  centerlines or profile outlines (cutouts included) to each layer's
  composed copper image. A centerline is measured over its drawn extent, to
  the copper of the Step that draws it and of every Step placed under it.
- Board-array spacing measures boundary-to-boundary distance between the
  sibling board arrays one Step places, as a fabrication panel does; it
  requires `--layout-target board-array` and at least two arrays.

A rule that measures nothing never reports a vacuous `pass`; `skip_reason`
says why, under one of two statuses:

- `not_applicable`: the design holds nothing for the rule to measure — its
  subject pool is empty (no holes of its class, no copper layers, no V-score
  lines), its case conditions do not match this stackup, or the pool yields no
  eligible measurements (`checked` would be zero).
- `incomplete`: the rule applies, or might, but something it reads could not be
  built, resolved, or measured. The problem is local: it blocks the rules that
  read the affected data and every other rule is still evaluated and reported.
  An `incomplete` rule of error severity fails the verdict, because a limit
  that was not measured is never reported as met. The CLI names each one as
  `not evaluated: <rule>: <reason>`.

## CLI

```bash
pcb ipc dfm check board.xml -o board.dfm
pcb dfm board.zen -o board.dfm
```

`pcb dfm` prepares and synchronizes the board's KiCad layout before exporting
temporary IPC-2581 and checking the canonical board. Use `pcb ipc dfm check`
to inspect existing manufacturing files without changing their source layout.

`--pdk` defaults to `standard` and accepts an exact built-in name or a TOML
path. `standard`, `jlcpcb-1oz` (`jlc`), and `ipc-1a` through `ipc-3c` are
bundled. `ipc` selects the opinionated Class 2 / Producibility Level B default.
Use `./standard` to select a same-named file. Both sources use the same parser
and checks. `--layout-target` accepts
`board` or `board-array` and defaults to `board-array`.

The `standard` PDK prefers 0.40 mm plated and nonplated slot-to-copper
clearance. Shortfalls produce warnings, not a failed manufacturing verdict.
This is conservative Diode routing guidance, not a manufacturer capability
or an IPC requirement. Slot clearance is not enabled in `jlcpcb-1oz` without
a manufacturer source. It also fails on the same component-data errors that
make the assembly report incomplete, with one finding per affected component.

The `jlcpcb-1oz` PDK, also available as `jlc`, executes the public rigid FR-4
capability table for 2-32 copper layers and 1 oz outer copper. It deliberately
omits the one-layer NPTH-only service, 2 oz rules, local 3 mil BGA exceptions,
and panel spacing, which depends on the chosen routing, mouse-bite, or V-cut
process. JLCPCB publishes a 0.10 mm soldermask web for standard colors and 0.13
mm for black or white. This PDK uses Diode's advisory 0.10 mm preferred web:
shortfalls produce warnings, not a failed manufacturing verdict. Passing this
advisory check does not certify the manufacturer's color-specific minimum.

The nine `ipc-1a` through `ipc-3c` built-ins preserve performance Classes 1-3
crossed with Producibility Levels A-C, but they are executable **partial Diode
baselines**, not IPC profile matrices. They check maximum via and PTH aspect
ratio at 6.0 for Level A, 8.0 for Level B, and 10.0 for Level C. They also
check via, PTH, and NPTH hole-to-copper clearance at 0.25 mm for Level A,
0.20 mm for Level B, and 0.15 mm for Level C. They check via, PTH, NPTH,
plated-slot, and nonplated-slot clearance to the board edge at 0.50 mm for
Level A, 0.40 mm for Level B, and 0.30 mm for Level C. Plated and nonplated
slot-to-copper clearance prefers 0.50 / 0.40 / 0.30 mm for A/B/C and warns
on shortfalls, deliberately allowing more routing margin than for circular
drills. These values apply across all three performance classes. Each profile
assumes 1.6 mm board thickness for the through-hole aspect-ratio fallback
described above. Diode chose these opinionated values using IPC design topics
as context; they are not licensed IPC numeric matrices, do not prove full IPC
compliance, and do not imply IPC certification. A pass covers only the checks
listed in the selected profile's `coverage` metadata.

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
| Copper-to-board-edge clearance | Required minimum 15 mil (0.381 mm) | Each layer's composed copper image against board edges, including cutouts |
| Soldermask web | Preferred 4 mil (0.1016 mm), warning only | Webs between final composed mask openings, not mask-to-pad registration; warnings do not fail the verdict |

Hole-diameter checks do not model a separate drill-tool diameter, plating
allowance, or manufacturing tolerance, and cannot verify the exporter's
finished-size interpretation.

Standard and all nine IPC profiles warn below 0.25 mm nominal plated-slot
copper enclosure through a preferred tier. Custom PDKs can supply a required
minimum. JLCPCB profiles do not enable this check.

Every run prints its [summary](#summary) as one line of JSON on stdout. With
`-o` / `--output` it first writes the full [report](#report-database), a
SQLite database; `.dfm` is the recommended suffix. Every complete report
includes the checked material and PDK source for viewing without companion
files.

A completed report is written before the command returns a failing status.
Error findings fail its verdict, and so does a required rule that could not be
evaluated. Preparation and output errors also return a failing status.
Preparation errors produce an explicit [incomplete report](#incomplete-reports).
File output is replaced atomically, including incomplete reports. I/O failures
can prevent output and leave a previous artifact untouched; callers must check
exit status. Output must not overwrite a source or use a KiCad board path.

`SOURCE_DATE_EPOCH` fixes `generated_at`. The same input bytes, source labels,
options, and epoch produce an identical summary and a byte-identical database.
A separate `.zen` layout/export operation may change its IPC bytes or temporary
source label.

## Summary

The summary is a JSON object with `schema_version`, `generated_at`, `verdict`
(`pass` or `fail`), `tool`, `input`, `pdk`, `layout_target`, `summary`, and
`rules`, each as described for the [report](#report-database). It holds no
findings or geometry.

- `input`: original IPC input path, SHA-256, and byte size; see
  [source identity](#source-identity).
- `pdk`: resolved kit and profile metadata, assumptions, source citation, path,
  exact TOML `source`, SHA-256, and the selected profile's
  `support.copper_layers` range (`exact`, `minimum`, and `maximum`).
- `summary`: rule counts by status, `findings`, `errors` and `warnings` by
  severity, and `unresolved`, the measurements no rule could decide.
- `rules`: one result per lowered rule, as in the `rules` table below.

A complete verdict fails exactly when `summary.errors > 0` or a rule of error
severity is `incomplete`.

## Report database

The report is one SQLite 3 database. Its header's `application_id` is
`0x44464D52` (`DFMR`), and its `user_version` is the report schema version,
currently `3`. Open it with any SQLite client; `json_each` and `json_extract`
read its JSON columns. Coordinates are integer nanometres in the
[frame](#frames) of their finding, X right and Y up. Every distinct layer,
subject, role, note and shape is stored once and referred to by id.

| Table | One row per | Columns |
| --- | --- | --- |
| `report` | report-level record | `key`, JSON `value`: `schema_version`, `generated_at`, `verdict`, `tool`, `input`, `pdk`, `layout_target`, `coordinate_system`, `layout`, `summary`, `frames`, `scene` (its `bounds`) |
| `rules` | lowered rule | `rule_id`, `title`, `finding_title`, `severity`, `tier`, `status`, `comparison`, `limit_value`, `limit_unit`, `limit_pdk_value`, `subject`, `quantity`, `method`, `checked`, `finding_count`, `skip_reason`, JSON `assumptions` and `view` |
| `unresolved` | measurement below a limit by less than its uncertainty | `rule`, `frame`, `actual_mm`, `uncertainty_mm`, `x`, `y`, JSON layer names |
| `findings` | violation | `finding_id`, `rule`, `frame`, JSON `measurement`, `message`, location `x`, `y`, bounds `min_x` … `max_y`, `layers`, `subjects` |
| `sites` | failing region or layer of a finding | `finding`, `position`, `site_id`, JSON `measurement`, `measurement_kind`, `uncertainty_mm`, bounds, `note` id, `layers`, `subjects`, `witnesses`, `evidence` |
| `layers` | layer | `name`, IPC-2581 `function`, `side` (`top`, `inner`, `bottom`) |
| `subjects` | subject | `role`, `kind`, `name`, `reference_designator`, `pin`, `net`, `padstack_ref`, JSON `locator` and `drill_span` |
| `roles` | witness or evidence role | `name` |
| `notes` | site note | `text` |
| `shapes` | evidence or scene geometry | `kind`, `center_x`/`center_y`/`diameter`, `start_x` … `end_y`, bounds, `paths`, `width_mm` |
| `scene` | layer of checked material | `label`, `feature`, `layer`, `color` |
| `draws` | shape a scene layer draws | `pass`, `frame`, `shape` |

`layers` and `subjects` columns are JSON arrays of ids, `witnesses` an array
of `[role id, x, y]`, and `evidence` an array of `[role id, shape id]`. The view
`finding_summary` joins each finding with its rule, actual value, limit,
margin, layer names, nets and site count:

```bash
sqlite3 board.dfm "SELECT rule_id, count(*), min(actual) FROM finding_summary GROUP BY rule_id"
```

### Rules

A direct limit uses its authored id; a named case uses `<id>.<case>`; a
preferred tier appends `.preferred` to either form. `status` is `pass`,
`warning`, `fail`, `not_applicable`, or `incomplete`, and `skip_reason` says
why a rule was not evaluated. `subject`, `quantity`, `method` and `comparison`
(`minimum` or `maximum`) are the measurement contract every finding of the rule
shares; `finding_title` is what each finding of it says is wrong. `checked`
counts the subjects decided across the physical layout: each Step's own
subjects once per placement of that Step. `finding_count` counts the rule's
findings. `view` specifies the diagnostic family, whether it is spatial, and
its semantic rendering features; `assumptions` lists profile defaults actually
used. A rule that one Step's design could not certify is `incomplete` and still
lists and counts what the other Steps found.

### Findings

A finding says what is wrong and where; its sites hold the geometry.

- `finding_id` hashes the rule, the subjects' stable identity, the layers, and
  where the finding is in its frame, in whole micrometres. A drilled subject
  is placed by where the source drills it; only a finding without one is
  placed by its measured point. A board's findings therefore keep their ids
  however it is panelized. Generated primitive names, padstack ids, set and
  feature indices, raw coordinates, evidence geometry and the measured value
  never enter an id, so an equivalent re-export or a noise-level change does
  not re-key a finding.
- `measurement` carries `actual_mm`, `required_mm`, and signed `margin_mm` for
  geometry, or the corresponding `actual_count`, `required_count`, and
  `margin_count` for discrete counts. Aspect-ratio measurements instead carry
  `actual_ratio`, `maximum_ratio`, signed `margin_ratio`,
  `drilled_span_thickness_mm`, `finished_hole_diameter_mm`, and
  `thickness_source`. A nonnegative margin satisfies the limit. Signed annular
  enclosure can be negative. Lengths are written to the nanometre.
- `x`, `y` and the bounds locate the finding; nonspatial findings leave them
  null and have no sites.
- `subjects` preserve role, kind, component, pin, net, padstack, and the
  source `locator` when IPC-2581 provides them: its `step`, `layer`, set and
  feature indices, and `instance_index`, which is null for the frame's own
  Step and otherwise names, in `layout.instances`, the occurrence under the
  frame's first placement. `drill_span` records the applicable copper-layer
  span.
- Sites carry their measurement, `measurement_kind`, uncertainty, bounds,
  layers, subjects, witnesses and evidence. Site bounds describe the finding;
  viewers add their own camera padding. `outside_board` identifies a drilled
  feature that crosses or lies outside its physical board material and so has
  zero clearance. Witness-point separation is not necessarily the measured
  width or diameter; scalar aspect-ratio sites have no witnesses.

Each piece of evidence has exactly one form, its shape's `kind`:

| `kind` | Geometry |
| --- | --- |
| `circle` | `center_x`, `center_y`, `diameter` (mm) |
| `segment` | `start_x` … `end_y` |
| `bounds` | `min_x` … `max_y` |
| `path` | open polylines in `paths` |
| `region` | closed rings in `paths`, filled nonzero like the checked material, holes included; a `width_mm` also draws their outline round to that width, as a rounded pad's inner rectangle |
| `stroke` | round-capped, round-joined polylines in `paths` of physical `width_mm` |

`paths` holds zigzag LEB128 varints: the path count, each path's point count,
then every point's x and y in nanometres, each as the difference from the
previous point (the first from zero). Paths omit vertices within 0.1 µm of
the path without them. Measurements, witnesses and evidence are authoritative:
do not fit curves to them, change their tessellation, or infer a width or
enclosure from witness separation.

### Coordinates and topology

`layout.kind` distinguishes `board`, `board_array`, and `fab_panel`; the
`board_array` target also selects fabrication panels. Board scope uses the
canonical board's local frame (`selected_board`). Array and fabrication-panel
scope use `root_layout`, including nested repeats. A canonical board check does
not certify every design in a mixed fabrication panel.

Each occurrence's cumulative `[a,b,c,d,tx,ty]` transform maps definition-local
coordinates to the checked frame: `x' = a*x + c*y + tx`,
`y' = b*x + d*y + ty`. A parent occurrence filter includes descendants.

### Frames

`frames` lists every Step the checked layout places, the checked root first.
Each entry has the Step's `step` name and its `placements`: one per occurrence
of that Step, with `instance`, its index in `layout.instances` (`null` for the
checked frame itself), and the occurrence's `transform` as above. A lone board,
or the `board` target, has one frame with the single placement
`{instance: null, transform: [1,0,0,1,0,0]}`. After these, a Step has a
further frame for each smaller set of its placements that some finding holds
at: a V-score line that crosses only the boards of one row is found at those
placements only.

A finding is measured once, in the coordinates of the Step its `frame` names,
and occurs at every placement of that frame, as does the Step's material in
the scene. To show an occurrence, apply that placement's transform. The root frame's transform is the
identity, so its findings are already placed. Ids and counts are per finding,
not per placement.

### Scene

Every complete report has a scene: the checked material findings are drawn
over. The `scene` record in `report` holds the full-layout `bounds` (in
millimetres), and the `scene` table holds its layers. Each has a `label`,
semantic `feature`, exact `layer` name or null for shared context, and display
`color`. `draws` lists each layer's shapes, each in the coordinates of the
Step whose `frame` it names; draw it at every placement of that frame. Each
Step draws only its own material, once.

The material is dark: draw every shape filled (or stroked) in the layer's
color, in any order. Negative polarity is already resolved into it, and holes
and slots are the drill layers'. Pads keep their exact standard shapes: a round
pad is a `circle`, an oval a `stroke`, and a rounded rectangle a `region` with
`width_mm`.

Select layers using the rule's `view.features` and the selected site's exact
layer names, including shared null-layer ones. Every spatial site requires a
matching layer for each feature except `stackup`. An empty layer represents
empty context; an absent required layer makes the export incomplete.

### Source identity

`pdk.source` contains the exact resolved UTF-8 TOML used for evaluation,
including comments, unit spelling, CRLF line endings, and any final newline.
`pdk.sha256` is the SHA-256 of its UTF-8 bytes, as 64 lowercase hexadecimal
characters. Consumers verify this hash; a checksum detects corruption, not
authenticity or fabrication approval.

`input.sha256` and `size_bytes` identify the original on-disk IPC input bytes,
including compression for a `.xml.zst` input. For `pcb dfm`, they identify its
temporary exported IPC input. The XML is not included. All `path` fields are
descriptive provenance; never fetch paths or open files on the consumer's
machine to render or validate the report.

### Incomplete reports

An incomplete run prints, and writes into the `report` table, only
`verdict: "incomplete"`, `schema_version`, `generated_at`, `tool`,
`input: {path}`, `pdk: {path}`, `layout_target`, and `error: {message}`. Its
other tables are empty. Consumers must handle this verdict before requiring
complete-report records; it is never a pass, a clean board, or a skipped check.

### Reader safety

The database, its JSON and TOML are untrusted even when the PDK hash matches.
Open it read-only, without loading extensions or running SQL it contains, and
never inject its text as markup. Reports can
contain private board data, local paths, components, nets and PDK comments: a
file picker or drop action authorizes local inspection only, not uploads or
telemetry.

### Schema evolution

Report version 3 is a SQLite database instead of JSON. The scene is shapes,
not SVG, and evidence shares them. Findings keep only their identity, location
and subjects, and their sites hold all geometry; evidence has one form, with
the clearance band a `stroke`;
the subject `source`/`provenance` pair is one `locator`; and waivers are gone.
A required error stands for a preferred-tier finding of the same subject.
New columns, records, and `kind`, `role`, `status`, rule, and method values may
be added within a version. Unknown required semantics must produce an explicit
unsupported state, never a guessed rendering or fabricated pass. Removing or
changing existing meanings requires a new schema version.
