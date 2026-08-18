# DFM PDK, waiver, and report formats

`pcb ipc dfm check` turns fabrication capabilities from one TOML PDK into
geometry checks over an IPC-2581 design and writes one JSON report.

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

Every length is a positive string containing a number and `mm`, `mil`,
`mils`, or `um`. Units can be mixed in one PDK. Checks normalize lengths to
millimeters and the report retains both the source spelling and normalized
value.

Every capability takes either a bare length — the binding minimum — or a
table with a `preferred` tier the fab would rather see met:

- The minimum lowers to an **error**-severity rule whose id is the
  capability path, e.g. `copper.minimum_via_annular_ring`. Error findings
  fail the verdict.
- A preferred tier lowers to a second, **warning**-severity rule under
  `<capability>.preferred`. Warning findings are reported and counted but do
  not fail the verdict. The preferred value must exceed the minimum.

Rule ids are the PDK capability paths, so every reported limit can be traced
to its PDK line verbatim.

## Rule semantics

- Hole diameter rules measure every drilled hole of the rule's class;
  `minimum_slot_width` measures every routed slot with a stated width.
- Hole-to-hole clearance measures edge-to-edge distance between hole pairs
  whose drill spans overlap; stacked blind and buried vias on disjoint spans
  do not interact.
- Annular ring measures the radial copper enclosure of each via or PTH hole
  on every spanned copper layer where the hole has a matching land or its
  center lies in copper. Layers where no copper reaches the hole — a plane
  anti-pad, a removed unused land — have no ring to measure. One finding per
  hole reports the worst layer.
- `minimum_feature_width` and `minimum_copper_clearance` are morphological:
  copper narrower than the limit, and gaps in copper narrower than the
  limit, are reported piece by piece per layer. `soldermask.minimum_web`
  reports mask webs — gaps between mask openings — narrower than the limit.
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
  --pdk fab-process.toml \
  --waivers waivers.toml \
  --layout-target board-array \
  --output dfm-report.json
```

`--layout-target` is `board` or `board-array` and defaults to `board-array`.
Omit `--output` to write JSON to stdout. The report is written before the
command returns a failing status, and the exit status fails only on unwaived
error findings. Set `SOURCE_DATE_EPOCH` to make `generated_at` — and with it
the whole report — reproducible. Waiver expiry is evaluated against that same
timestamp on purpose: expiry makes the verdict time-dependent, so pinning the
time must pin expiry too, or a reproduced report could not reproduce its
verdict. A pipeline that pins an old epoch is choosing to reproduce that
date's verdict, expired waivers included.

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
- `coordinate_system`: the unit, axis convention, and origin for all report
  geometry. Version 1 uses millimeters, `x_right_y_up`, and
  `ipc_2581_design`.
- `summary`: rule counts by status and finding counts by severity and
  waiver state.
- `rules`: one result per lowered rule: its id (the capability path, plus
  `.preferred` for warning tiers), severity, source and normalized limit,
  status (`pass`, `warning`, `fail`, or `skipped`), skip reason, and the
  measurement contract shared by all of its findings — `subject` (what one
  checked unit is), `quantity`, `method`, and `checked`, the number of
  measurements evaluated. Every rule requires its measured quantity to be at
  least its limit.
- `findings`: violations in deterministic rule/location order.

Each finding is intentionally a fat record:

- `id` is a content hash of what the finding is about — its rule, subjects,
  layers, and measured location. It is deterministic for a given input, and
  it survives design revisions exactly as long as those identifying facts
  are unchanged. A violation whose representative point moves is a new
  finding with a new id; its old waiver then surfaces as `unmatched` for
  re-review. The measured value is deliberately not part of the identity:
  a waived violation that shrinks or grows in place keeps its waiver.
- `rule_id`, `severity`, `title`, and `message` identify and explain the
  violation; `waived` and `waiver_reason` record acceptance.
- `measurement` carries `actual_mm`, `required_mm`, and the signed
  `margin_mm`; its quantity, method, and comparison live on the rule.
- `location` carries a representative point, bounding box, and role-labelled
  geometric witnesses.
- `layers` identify every involved manufacturing layer: name, IPC-2581
  layer function, and `side` (`top`, `inner`, `bottom`) where the stackup
  determines it.
- `subjects` preserve role, kind, component, pin, net, padstack, and source
  indices when IPC-2581 provides them. Unavailable fields remain `null` so
  consumers see one stable shape.
- `evidence` is a fat geometry record. `kind` and `role` identify the
  evidence; circle, segment, and bounds fields are populated as applicable
  and otherwise remain `null`.

Within schema version 1, new optional fields and new `kind`, `role`,
`status`, rule, and method values may be added. Removing or changing the
meaning of an existing field requires a new schema version.
