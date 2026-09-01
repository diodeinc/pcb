# PCBA assembly report v1

`assembly::build_report` lowers one cached `pcb-ir` imported design into the
deterministic contract designed for native, CLI, and WebAssembly surfaces. The
report carries design and manufacturing facts only. It does not select a part
offer, infer commercial process policy, multiply by order quantity, or contain
prices.

The CLI writes this contract as JSON. It reports the complete board array by
default; pass `--scope board` to select one canonical board instead.

```bash
pcb ipc assembly design.xml --scope board-array > assembly-report.json
```

The report has no generated timestamp or host file path. Repeating a build
with the same IPC input and scope produces identical JSON. Board, package,
component, and termination arrays are ordered by stable identifiers;
diagnostics are ordered by code and subject identity. Identifiers hash
semantic source facts; they do not expose `pcb-ir` vector indices. An exact
duplicate receives a deterministic numeric suffix.

## Units and scope

All lengths and coordinates are millimeters. All angles are degrees. Affine
matrices use `[a, b, c, d, tx, ty]`, where
`x' = a*x + b*y + tx` and `y' = c*x + d*y + ty`. Coordinates use the
IPC-2581 design frame: +X right, +Y up.

`board` selects one canonical board and normalizes it to the design origin.
`board_array` selects the root Step and materializes nested StepRepeat records.
Each board occurrence retains the complete source StepRepeat path, including
grid indices, first origin, pitch, rotation, and mirror.

## Contract

This example shows every v1 field. Identifiers are illustrative.

```json
{
  "schema_version": 1,
  "units": { "length": "mm", "angle": "degree" },
  "source": {
    "format": "ipc_2581",
    "revision": "C",
    "creation_software": "KiCad",
    "software_package": {
      "name": "KiCad",
      "revision": "10.0",
      "vendor": "KiCad"
    }
  },
  "scope": {
    "kind": "board_array",
    "root_step": "panel",
    "coordinate_frame": "ipc_2581_design_x_right_y_up"
  },
  "readiness": "incomplete",
  "summary": {
    "board_occurrences": 1,
    "packages": 1,
    "components": {
      "total": 1,
      "included": 1,
      "excluded": 0,
      "included_populated": 0,
      "included_do_not_populate": 0,
      "included_population_unresolved": 1
    },
    "terminations": {
      "total": 1,
      "on_included_populated_components": 0,
      "surface_on_included_populated_components": 0,
      "through_on_included_populated_components": 0,
      "blind_on_included_populated_components": 0
    },
    "paste": {
      "islands": 1,
      "exactly_linked_to_termination": 1,
      "on_included_populated_components": 0,
      "exactly_linked_on_included_populated_components": 0
    }
  },
  "boards": [
    {
      "id": "board-0123456789ab",
      "step": "board",
      "path": [
        { "step": "panel", "repeat": null },
        {
          "step": "board",
          "repeat": {
            "index_x": 0,
            "index_y": 0,
            "first_x_mm": 10.0,
            "first_y_mm": 20.0,
            "pitch_x_mm": 15.0,
            "pitch_y_mm": 0.0,
            "rotation_degrees": 0.0,
            "mirror": false
          }
        }
      ],
      "transform": [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]
    }
  ],
  "packages": [
    {
      "id": "package-0123456789ab",
      "source_step": "board",
      "name": "QFN-16",
      "package_type": "OTHER",
      "height_mm": 0.8,
      "pins": [
        {
          "view": "primary",
          "number": "1",
          "name": "GND",
          "pin_type": "surface",
          "electrical_type": "electrical",
          "mount_type": "surface_mount_pad",
          "polarity": "minus",
          "location_mm": { "x": 0.0, "y": 0.0 },
          "transform": {
            "x_offset_mm": 0.0,
            "y_offset_mm": 0.0,
            "rotation_degrees": 0.0,
            "mirror": false,
            "face_up": false,
            "scale": 1.0
          }
        }
      ]
    }
  ],
  "components": [
    {
      "id": "component-0123456789ab",
      "board_id": "board-0123456789ab",
      "source_step": "board",
      "layout_path": [
        { "step": "panel", "repeat": null },
        {
          "step": "board",
          "repeat": {
            "index_x": 0,
            "index_y": 0,
            "first_x_mm": 10.0,
            "first_y_mm": 20.0,
            "pitch_x_mm": 15.0,
            "pitch_y_mm": 0.0,
            "rotation_degrees": 0.0,
            "mirror": false
          }
        }
      ],
      "reference_designator": "U1",
      "part": "controller",
      "package_id": "package-0123456789ab",
      "package_ref": "QFN-16",
      "bom": {
        "bom": "bom",
        "oem_design_number": "controller",
        "category": "electrical",
        "quantity": 1,
        "quantity_source": "1",
        "pin_count": 16,
        "pin_count_source": "16",
        "internal_part_number": null,
        "approved_parts": [
          {
            "external_vendor": null,
            "external_mpn": null,
            "qualified": true,
            "chosen": true,
            "manufacturer_part_numbers": ["ABC123"],
            "vendor_refs": ["maker"]
          }
        ]
      },
      "population": "unspecified",
      "side": "top",
      "mount": "smt",
      "assembly_status": "included",
      "exclusion_reason": null,
      "transform": [1.0, 0.0, 0.0, 1.0, 12.0, 22.0],
      "termination_ids": ["termination-0123456789ab"]
    }
  ],
  "terminations": [
    {
      "id": "termination-0123456789ab",
      "component_id": "component-0123456789ab",
      "pin": "1",
      "pin_type": "surface",
      "mount_type": "surface_mount_pad",
      "padstack": "U1-1",
      "location_mm": { "x": 12.0, "y": 22.0 },
      "side": "top",
      "population": "unspecified",
      "lands": [{ "layer": "TOP" }],
      "paste_islands": [
        {
          "layer": "PASTE",
          "side": "top",
          "location_mm": { "x": 12.0, "y": 22.0 }
        }
      ]
    }
  ],
  "diagnostics": [
    {
      "id": "assembly-diagnostic-0123456789ab",
      "severity": "error",
      "code": "missing_population",
      "subject": {
        "kind": "component",
        "id": "component-0123456789ab",
        "reference_designator": "U1"
      },
      "message": "component 'U1' has no explicit population state"
    }
  ]
}
```

`null` means that the IPC source did not supply or resolve that fact. Empty
arrays mean a genuinely empty source-backed collection. In particular, a
missing BOM `pinCount` remains `null`; it never becomes a default pin or joint
count. `quantity_source` and `pin_count_source` retain the IPC lexical values
alongside any parsed integer.

IPC-2581C defines `RefDes/@populate` as optional without a schema default. The
report records an omitted attribute as `unspecified` and emits a readiness
diagnostic, so an unattended quote can distinguish an explicit population
instruction from an omission. The source XML remains valid IPC-2581C.

Termination `total` and paste `islands` describe all source-backed physical
facts in scope. Fields ending in `on_included_populated_components` are the
safe assembly-work subset; excluded DOCUMENT records, DNP components, and
unresolved population do not contribute to those fields.

Packages contain only definitions referenced by components in the selected
scope. Package pin views remain distinct. Physical terminations include only
pins explicitly marked `electricalType="ELECTRICAL"`; copper replicas collapse
only when component, pin, padstack, and location identities match exactly.
Paste islands link only through that same exact identity. No nearest-object or
overlap heuristic is used.

Components remain present when excluded. A preferred BOM item with
`category="DOCUMENT"` sets `assembly_status` to `excluded` and
`exclusion_reason` to `document_bom_category`. DNP is not an exclusion: it is
the independent `do_not_populate` population state.

Source provenance, repeat parameters, package and pin attributes, BOM/AVL
identity, and component placement attributes are normalized directly from IPC
fields. Occurrence paths and transforms compose the exact StepRepeat hierarchy.
Population combines the component and BOM RefDes instructions and becomes
`conflicting` when explicit instructions disagree. Terminations and paste links
derive only from exact component, pin, padstack, and location identities.
The report does not infer package family, body dimensions, pitch, process, or
commercial policy.

Report construction rejects non-finite numeric evidence instead of allowing
JSON serialization to replace it with `null`.

## Enums

| Field | Values |
| --- | --- |
| `scope.kind` | `board`, `board_array` |
| `readiness` | `ready`, `review_required`, `incomplete` |
| `population` | `unspecified`, `populate`, `do_not_populate`, `conflicting` |
| `side` | `top`, `bottom`, `both`, `internal`, `all`, `none`, `unspecified` |
| `mount` | `smt`, `through_hole`, `embedded`, `press_fit`, `wire_bonded`, `glued`, `clamped`, `socketed`, `formed`, `other` |
| `bom.category` | `electrical`, `programmable`, `mechanical`, `material`, `document` |
| `assembly_status` | `included`, `excluded` |
| `exclusion_reason` | `document_bom_category` |
| package pin `view` | `primary`, `topside` |
| `pin_type` | `through`, `blind`, `surface` |
| pin `electrical_type` | `electrical`, `mechanical`, `undefined` |
| pin `mount_type` | `surface_mount_pin`, `surface_mount_pad`, `through_hole_pin`, `through_hole_hole`, `press_fit`, `non_board`, `hole`, `wire_bond`, `undefined` |
| pin `polarity` | `plus`, `minus`, `anode`, `cathode` |
| diagnostic `severity` | `error`, `warning` |
| diagnostic `code` | `missing_population`, `conflicting_population`, `missing_reference_designator`, `missing_package`, `missing_physical_terminations` |
| diagnostic subject `kind` | `component` |

`ready` means the implemented source-completeness checks found no diagnostics.
`review_required` means only warnings remain. `incomplete` means at least one
error remains. This is assembly-data readiness, not a price or factory-routing
decision. V1 reports errors for an included component with absent/conflicting
population, missing reference designator, a populated component without a
resolved package, or a populated SMT/through-hole component without an exact
physical termination.
