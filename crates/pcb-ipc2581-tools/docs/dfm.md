# DFM PDK and report formats

`pcb ipc dfm check` turns fabrication capabilities from one TOML PDK into
geometry checks over an IPC-2581 design and writes one JSON report.

## PDK

The PDK is strict and versioned. Unknown fields, bare numeric lengths, and
unsupported schema versions are errors. Only capabilities present in the file
become configured rules.

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
minimum_hole_to_hole_clearance = "10 mil"

[capabilities.copper]
minimum_via_annular_ring = "0.125 mm"
minimum_pth_annular_ring = "0.125 mm"
minimum_board_edge_clearance = "0.25 mm"
minimum_vscore_to_copper_clearance = "15 mil"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
```

Every length is a positive string containing a number and either `mm`, `mil`,
or `mils`. Units can be mixed in one PDK. Checks normalize lengths to
millimeters and the report retains both the source spelling and normalized
value.

## CLI

```bash
pcb ipc dfm check fabrication-panel.xml \
  --pdk fab-process.toml \
  --layout-target board-array \
  --output dfm-report.json
```

`--layout-target` is `board` or `board-array` and defaults to `board-array`.
Omit `--output` to write JSON to stdout. The report is written before the
command returns a failing status for findings.

## JSON report

The report is a self-contained record of what was checked:

- `schema_version`: integer report schema version.
- `generated_at`: RFC 3339 generation time.
- `verdict`: `pass` or `fail`.
- `tool`, `input`, and `pdk`: producer version plus input paths and SHA-256
  identities.
- `layout_target`: `board` or `board_array`.
- `coordinate_system`: the unit, axis convention, and origin for all report
  geometry. Version 1 uses millimeters, `x_right_y_up`, and
  `ipc_2581_design`.
- `summary`: rule and finding counts.
- `rules`: one result for every configured PDK capability, including its
  source and normalized limit, status, checked count, finding count, and skip
  reason.
- `findings`: deterministic rule/location-ordered violations.

Each finding is intentionally a fat record:

- `id`, `rule_id`, `severity`, `title`, and `message` identify and explain the
  violation.
- `measurements` carry quantity, actual value, required value, signed margin,
  unit, comparison, and measurement method.
- `location` carries a representative point, bounding box, and role-labelled
  geometric witnesses.
- `layers` identify every involved manufacturing layer.
- `subjects` preserve role, kind, component, footprint, pin, net, padstack,
  and source indices when IPC-2581 provides them. Unavailable fields remain
  `null` so consumers see one stable shape.
- `evidence` is a fat geometry record. `kind` and `role` identify the evidence;
  circle, segment, and bounds fields are populated as applicable and otherwise
  remain `null`.

Within schema version 1, new optional fields and new `kind`, `role`, rule, and
method values may be added. Removing or changing the meaning of an existing
field requires a new schema version.
