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

[profiles.standard]
name = "1 oz rigid standard"
technologies = ["rigid"]

[profiles.standard.support]
copper_layers = { minimum = 2, maximum = 10 }

[[rules.drilling.hole_diameter]]
id = "drilling.via_hole"
select = { hole = "via" }
limit = { minimum = "0.2 mm", preferred = "0.25 mm" }
```

See the [file-backed example](../examples/dfm-pdk.toml) for conditional limits
and more rule families, and the [PDK types](../src/commands/dfm/pdk.rs) for the
complete accepted fields. The example is illustrative, not a qualified process.

One PDK file is a kit with named profiles. A built-in name such as
`jlcpcb-1oz` selects both a kit and one profile. A built-in name without a
profile, such as `jlc`, and a custom file select the profile from the design:
the first executable profile whose `profile.support` the design meets, trying
`default_profile` first and then the other profiles in name order. The report
records the selected profile and why each earlier profile did not fit. A kit
of one executable profile always runs it, and its support rules report what
the design lacks. When the design meets no profile's support, the default
profile runs and its support rules fail on what the design lacks. When the
design does not state what the support of a profile tried earlier reads, the
default profile runs and its support rules report the missing data as
`incomplete`. An executable profile lowers only rules whose `profiles` list
contains it; an omitted list means every profile. A `metadata_only` profile
fails closed before checking and is never selected automatically. This lets a
kit publish a standard taxonomy without claiming that missing numeric rules
establish compliance.

The schema gives each kind of constraint one place:

- `rules.assembly.diagnostic` enables a required component-data predicate.
  Supported diagnostics are `missing_population`, `conflicting_population`,
  `missing_reference_designator`, `missing_package`,
  `missing_physical_terminations`, and `nonstandard_bottom_rotation`. These
  categorical rules always require zero diagnostics and therefore take no
  numeric limit. `nonstandard_bottom_rotation` is a warning: it flags
  bottom-side parts whose rotation pcb corrected for a known KiCad 9.0.8–9.0.9
  or 10.0.0–10.0.4 exporter defect.
- `profile.support` is the hard eligibility envelope for the whole profile,
  and the bounds a kit selects its profile by. It accepts `copper_layers`
  (a count range), `outer_copper_weight` and `inner_copper_weight` (lists of
  allowed weights), `board_thickness` (a thickness limit as in
  `rules.stackup.board_thickness`), `surface_finish` and `soldermask_color`
  (lists of allowed values). A design outside any bound fails profile
  qualification. The engine emits these checks as reserved
  `profile.support.*` report rules; PDK authors do not write layer-count DFM
  rules.
- Every check also runs the reserved `board.outline` rule, whatever the PDK:
  a checked layout with no profile (the board for `--layout-target board`,
  the array or panel for `board-array`) has nothing to fabricate, so the rule
  fails with one finding. Rules that measure against the outline, such as
  board size and board-edge clearance, are then `not_applicable`.
- `rules.stackup` holds limits on the physical stackup.
  `board_thickness` lists the fab's `nominal` finished thicknesses; the
  design conforms when its thickness is within `tolerance` or
  `tolerance_percent` of the nominal value, whichever is larger, of one of
  them. `copper_weight` selects an `outer` or `inner` layer position and
  lists the weights `allowed` there. `dielectric_gaps` states a `tolerance`
  for matching the design against the PDK's top-level `[[stackup_builds]]`
  (`copper_layers`, `board_thickness`, `outer_copper_weight`,
  `inner_copper_weight`, and `templates` of `name`, `default`, and
  `dielectric_gaps_mm`), each build with inner layers. JLCPCB's are
  generated into `pdks/jlcpcb.toml` by `pdks/update_jlcpcb_stackups.py`.
- `rules.board` holds limits on the whole board: `surface_finish` and
  `soldermask_color` list the values `allowed`, and `size` states a
  `minimum`, a `maximum`, or both as two board extents.
  Surface finishes are `hasl` (tin-lead), `lead_free_hasl`, `enig`,
  `enepig`, `immersion_gold`, `immersion_silver`, `immersion_tin`, `osp`,
  `hard_gold`, `nickel`, and `bare_copper`. Soldermask colours are names,
  compared without regard to case or repeated spaces.
- `select` identifies the physical subjects a rule measures. Hole rules select
  `via`, `pth`, or `npth`; slots select `plated` or `nonplated`; hole-pair rules
  state both classes. Hole-aspect-ratio rules accept only plated `via` or `pth`
  holes; selecting `npth` is a schema error. A
  `rules.copper.hole_clearance` selector chooses the drill class measured to
  unrelated final copper.
- `limit` defines an unconditional dimensional minimum, preferred value, or
  both, or one required aspect-ratio maximum.
- `cases` defines named conditional limits when one value is not enough. A
  rule uses either `limit` or `cases`, never both. Stackup and board rules
  may condition cases only on `copper_layers`.
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

A copper-weight condition names a standard weight (⅛, ¼, ⅓, ½, then whole
ounces), while a stackup states a thickness that only approximates one: 1 oz
is 34.3 µm nominal (IPC-4562A), is allowed down to 90 % as foil, typically
finishes near 88 %, and plates up on outer layers. A layer names the nearest
standard weight whose window holds it: from 85 % of nominal up to 0.1 oz
above, plus 0.45 oz of plating on outer layers. So 1.4 mil is 1 oz, 0.07 mm
is 2 oz, a 0.0152 mm inner layer is 0.5 oz, and 1.46 oz is 1 oz outside but
no standard weight inside. A layer that names none matches no condition and
no allowed weight. PDK weights must name a standard weight; two cases naming
the same one overlap. `pcb ipc info --format json` reports the same
`outer_copper_nominal_oz` and `inner_copper_nominal_oz`.

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

Checks measure source primitives and stackup facts where sufficient, and final
composed geometry where polarity or material removal changes the result. The
[rule semantics](#rule-semantics) define each measured quantity; bounds and
spatial indices accelerate checks but never substitute for a measurement.

Geometric checks produce an aggregate measurement per subject and retain its
failing measured sites. Measurements carry the uncertainty of the flattened
boundaries they were measured against (one flattening tolerance per tessellated
curve; zero for stated primitives and analytic shapes). A pair of witness
points does not always encode a length: widths, diameters, and annular
enclosures retain their own measurement constructions. The engine fails a
minimum only when the measured value falls short beyond its own uncertainty,
so curve tessellation by itself cannot manufacture a violation. A value that
falls short by less than its uncertainty is neither a violation nor proof the
limit is met: the report's `unresolved` table lists it with its rule, value,
uncertainty, location, and layers, `summary.unresolved` counts them, and the CLI reports how
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

Morphological opening/closing only finds width and soldermask-web candidates.
Their measured width is an inscribed disk diameter on the medial axis of the
unsnapped prepared boundary; the minimum and evidence share that measurement.
Axis flattening is for drawing, not measuring. This measures resolved contacts
in prepared geometry, not intended curves reconstructed from polygonal input.
Source uncertainty contributes to scalar width but does not certify contact
direction, source-branch correspondence, or topology.

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

Each Step's findings apply at its placements; the report lists these in
[frames](#frames) instead of repeating findings for identical placements.

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
- Explicit IPC `NetShort` declarations permit validated net-tie contacts on the
  containing copper layer: a matching component graphic/pad contact permits
  that graphic to meet the declared nets; a pad-only tie permits only the pad
  pair. The declaration must name two distinct nets and match actual contact.
  It does not merge nets or exempt remote shorts, third-net clearances, or
  other layout occurrences. Unsupported or unmatched declarations leave the
  affected ownership-dependent checks `incomplete`, not silently passing.
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
- Board-property rules (`rules.stackup`, `rules.board`, and the support
  bounds other than layer count) judge properties of the whole board. Each
  permits zero nonconforming properties and reports one finding for each
  property that is not allowed, with a `count` measurement of 1 against 0.
  - Copper weight compares standard weights, as weight conditions do. A layer
    that states no weight or thickness uses the profile's copper-weight
    default, recorded as an assumption; without one the rule is
    `incomplete`.
  - Board thickness is the finished thickness: IPC `overallThickness`
    includes foil and soldermask. A stackup that states no thickness leaves
    the rule `incomplete`; the profile thickness default is not used.
  - Dielectric gaps are the dielectric thicknesses between adjacent copper
    layers. The design's build is its layer count, the listed thickness
    nearest its finished thickness, and its outer and inner copper weights;
    it conforms when a template for that build has every gap within
    `tolerance`. A build with no template fails. A layer count with no build
    is `not_applicable`; missing thicknesses or weights leave the rule
    `incomplete`.
  - Surface finish reads the `SurfaceFinish` spec of each coating layer in
    the stackup, once per spec; unreferenced specs are ignored. IPC `S` is
    solder leveling without stated lead content: it conforms when both
    `hasl` and `lead_free_hasl` are allowed, does not conform when neither
    is, and leaves the rule `incomplete` when only one is. Soldermask
    colour reads the colour term, `Color : <name>` property, or RGB of each
    soldermask layer's stackup spec; an RGB or custom `#rrggbb[aa]` colour,
    as KiCad writes it, takes the nearest named colour. A finish or colour the file does not
    state (KiCad writes `Not specified`) is left to the fab, so the rule is
    `not_applicable`. IPC `OTHER` finishes are never allowed.
  - Board size measures the extents of the checked layout: the board for
    `--layout-target board`, the array or panel for `board-array`. Extents
    and limits are both compared long side to long side. A layout without a
    profile has nothing to size, so the rule is `not_applicable`;
    `board.outline` fails instead.

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

`pcb dfm` exports the board's saved KiCad layout to temporary IPC-2581, without
synchronizing it first, and checks the canonical board. Use `pcb ipc dfm check`
to inspect existing manufacturing files.

`--pdk` defaults to `standard` and accepts an exact built-in name or a TOML
path. `standard`, `jlc`, `jlcpcb-1oz`, and `ipc-1a` through
`ipc-3c` are bundled. `ipc` selects the opinionated Class 2 / Producibility Level B default.
Use `./standard` to select a same-named file. Both sources use the same parser
and checks. `--layout-target` accepts
`board` or `board-array` and defaults to `board-array`.

The bundled TOML files own the exact thresholds and source citations:

- [`standard`](../pdks/standard.toml) supports 2–10 copper layers and reports
  assembly-readiness errors. It selects `standard` for 1 oz outer copper or
  `two-ounce` for 2 oz; mixed or unsupported weights fail qualification.
  Both profiles retain stronger Diode limits and tighten weaker ones to the
  most relaxed JLCPCB 1 oz values. This is Diode policy, not a qualified
  manufacturer's process.
- [`jlcpcb`](../pdks/jlcpcb.toml) covers a subset of JLCPCB's rigid FR-4 service
  for 2–32 copper layers. `jlc` selects `one-ounce` or `two-ounce` by finished
  outer copper weight; `jlcpcb-1oz` fixes `one-ounce`. Track width and spacing
  depend on each layer's weight. Profiles also check board thickness, finish,
  soldermask color, and size; outer copper outside 1 oz or 2 oz fails support.
- [`ipc`](../pdks/ipc.toml) preserves Classes 1–3 crossed with Producibility
  Levels A–C as executable **partial Diode baselines**, not IPC profile matrices.
  Aspect-ratio, hole-to-copper, and hole/slot-to-edge limits vary by level, not
  class. Diode chose these opinionated values using IPC design topics as context;
  they are not licensed IPC numeric matrices, do not prove full IPC compliance,
  and do not imply IPC certification. A pass covers only the selected profile's
  `coverage` metadata and enabled checks.

The JLCPCB kit deliberately omits the one-layer NPTH-only service, heavier copper,
thicker boards for 12 or more layers, thickness-dependent size limits,
the HASL exclusion for boards of 0.4 mm and thinner,
local 3 mil BGA exceptions, dielectric layup templates, and panel spacing,
which depends on routing, mouse-bite, or V-cut process. Its soldermask-web checks
are advisory warnings, not certification of the manufacturer's color-specific
minimums. Plated-slot clearance and enclosure remain outside the kit's coverage.

Both copper-weight profiles in the standard and JLC kits use these tight
dimensional review bands. Below the minimum is an error; from the minimum to
below the target is a warning; the target or above passes.

| Kit | Check | Minimum (mm) | Target (mm) |
| --- | --- | --- | --- |
| Standard | Via drill diameter | 0.190 | 0.200 |
| Standard | Via annular ring | 0.115 | 0.125 |
| Standard | NPTH-to-copper | 0.190 | 0.200 |
| Standard | Routed-edge-to-copper | 0.371 | 0.381 (15 mil) |
| JLC | NPTH-, nonplated-slot-, and routed-edge-to-copper | 0.190 | 0.200 |

These 0.01 mm bands are Diode review policy, not manufacturer-qualified
fabrication tolerances. JLC's published clearance minimum remains 0.20 mm;
a warning still calls for review of that requirement. Other limits and existing
advisory tiers are unchanged.

Two JLC source ambiguities affect the kit:

- NPTH diameter retains the [published 0.50 mm minimum](https://jlcpcb.com/capabilities/pcb-capabilities).
  [Support calls it blockage guidance](https://jlcpcb.com/help/answers/detail/861-Regarding-the-Minimum-NPTH-Diameter)
  but establishes no smaller supported minimum.
- The 0.45 mm mixed-hole/NPTH-pair spacing is inferred, so it only warns.
  [Mouse bites allow smaller NPTH gaps](https://jlcpcb.com/blog/mouse-bite-panelization-guide);
  [POFV spacing requirements](https://jlcpcb.com/news/free-via-in-pad-6-20-layer-pcbs-pofv)
  are process-specific. Neither establishes ordinary mixed-pair limits.

All nine IPC profiles pin earlier Diode `standard.toml` limits, cited as
`diode-standard`; later standard changes do not propagate. These are Diode
baselines, not IPC requirements.

Hole-diameter checks do not model a separate drill-tool diameter, plating
allowance, or manufacturing tolerance, and cannot verify the exporter's
finished-size interpretation.

Standard and all nine IPC profiles use preferred warning tiers for nominal
plated-slot enclosure and slot-to-copper clearance. Standard additionally
requires 0.20 mm nonplated-slot clearance. Custom PDKs can supply other
required minimums.

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
  exact TOML `source`, SHA-256, and the selected profile's `support`: the
  `copper_layers` range (`exact`, `minimum`, and `maximum`) and the other
  support bounds as the PDK states them. `profile_selection` names the
  `profile`, its `mode` (`explicit` or `automatic`), and the `rejected`
  profiles tried before it, each with the `reasons` it does not fit.
- `summary`: rule counts by status, `findings`, `errors` and `warnings` by
  severity, and `unresolved`, the measurements no rule could decide.
- `rules`: one result per lowered rule with the fields of the `rules` table
  below, except that the rule is `id` and its limit is one `limit` object of
  `pdk_value`, `normalized_value` and `normalized_unit`.

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
why a rule was not evaluated. A board-property rule's limit is 0 in unit
`nonconforming`, and `limit_pdk_value` states the values it allows. `subject`, `quantity`, `method` and `comparison`
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
| `circle` | `center_x`, `center_y` (integer nm); `diameter` (mm) |
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

New columns, records, and `kind`, `role`, `status`, rule, and method values may
be added within a version. Unknown required semantics must produce an explicit
unsupported state, never a guessed rendering or fabricated pass. Removing or
changing existing meanings requires a new schema version.
