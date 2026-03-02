# Part Type Specification

Status: Draft proposal  
Target: Zener language (`builtin`, `Component`, schematic/netlist conversion)  
Version: 1

## 1. Overview

This spec introduces a first-class `Part` value in Zener, exposed as `builtin.part(...)`.

The goal is to replace ad hoc part dictionaries with a typed object while preserving all existing BOM and netlist behavior.

## 2. New Builtin Value: `Part`

### 2.1 Constructor

`builtin.part(*, mpn, manufacturer, qualifications=[]) -> Part`

Required fields:
- `mpn: str`
- `manufacturer: str`

Optional fields:
- `qualifications: list[str]` (default `[]`)

Validation rules:
- `mpn` and `manufacturer` must be non-empty strings after trim.
- `qualifications` must be a list containing only strings.

### 2.2 Runtime behavior

`Part` exposes:
- `part.mpn`
- `part.manufacturer`
- `part.qualifications`

`Part` values compare equal if all three fields are equal (`qualifications` order-sensitive).

## 3. `Component()` Integration

### 3.1 New optional kwarg

Add:
- `part` (type `Part`, optional)

to `Component(**kwargs)`.

### 3.2 Resolution precedence for primary sourcing fields

Final component `mpn`/`manufacturer` values are resolved in this order:
1. Explicit `mpn=` / `manufacturer=` kwargs.
2. `part.mpn` / `part.manufacturer` from `part=`.
3. Legacy `properties` keys (`mpn`, `Mpn`, `manufacturer`).
4. Symbol metadata fallback (`Manufacturer_Part_Number`, `Manufacturer_Name`).

If explicit fields conflict with `part=`, explicit kwargs win.

### 3.3 Diagnostics

If `part=` is provided and explicitly conflicting `mpn`/`manufacturer` are also provided:
- emit warning `bom.part_conflict`
- include both values and selected value in the diagnostic message

Existing incomplete manufacturer warning is unchanged:
- if final manufacturer exists and final mpn does not, emit warning.

### 3.4 Modifier-time mutability

`Component` modifiers MUST be able to mutate both primary part data and alternatives.

Required behavior:
- `component.part = builtin.part(...)` updates the component's canonical scalar fields (`mpn`, `manufacturer`) to match the assigned part.
- Mutating scalar fields (`component.mpn`, `component.manufacturer`) MUST keep `component.part` in sync when a part exists.
- `component.alternatives` MUST be mutable in modifiers:
  - assignment (`component.alternatives = [...]`) is supported.
  - in-place list mutation (e.g. `component.alternatives.append(...)`) is supported.

## 4. Alternatives Support

`properties["alternatives"]` supports:
- preferred: `list[Part]`
- compatibility: existing list-of-map/list-of-JSON-compatible legacy formats

Normalization target for each alternative:

```json
{
  "mpn": "string",
  "manufacturer": "string",
  "qualifications": ["string"]
}
```

If legacy entries omit `qualifications`, normalize to `[]`.

## 5. Serialization Model

## 5.1 Internal schematic attribute representation

When converting a `Component` to schematic instances:
- keep scalar canonical fields:
  - `mpn` (string)
  - `manufacturer` (string)
- add structured field:
  - `part` (JSON object: `{mpn, manufacturer, qualifications}`)
- `alternatives` should serialize as array of JSON objects in normalized shape above.

This enables typed round-trip behavior without removing existing scalar keys.

## 5.2 JSON form (normative)

Primary part:

```json
"part": {
  "mpn": "RC0603FR-0710KL",
  "manufacturer": "Yageo",
  "qualifications": ["AEC-Q200"]
}
```

Alternatives:

```json
"alternatives": [
  {"mpn": "RC0603FR-0710KL", "manufacturer": "Yageo", "qualifications": []},
  {"mpn": "ERJ-3EKF1001V", "manufacturer": "Panasonic", "qualifications": ["PPAP"]}
]
```

## 6. Schematic JSON Netlist + KiCad Netlist Compatibility (MUST NOT BREAK)

This section is mandatory and constrains implementation choices.

1. Schematic JSON netlist MUST preserve existing scalar keys:
   - `mpn` (string)
   - `manufacturer` (string)
2. Schematic JSON netlist MAY add `part` as a JSON object, but MUST NOT replace or remove scalar `mpn`/`manufacturer`.
3. In schematic JSON netlist, `alternatives` MUST serialize as an array whose entries are JSON objects (not `Part(...)` string reprs).
4. Existing consumers that only read scalar `mpn`/`manufacturer` MUST continue to work unchanged.
5. Existing KiCad netlist S-expression structure MUST remain unchanged.
6. Existing KiCad netlist `value` selection behavior MUST remain unchanged.
7. If `part` is emitted into KiCad netlist component properties, it MUST be serialized as a JSON string payload, not as a syntax/schema change to KiCad netlist format.
8. Alternatives serialization MUST remain parseable by current BOM extraction paths; adding `qualifications` MUST be backward-compatible.

## 7. BOM Behavior

- BOM primary MPN/manufacturer continue to come from scalar component fields.
- BOM alternatives continue to use `{mpn, manufacturer}` matching semantics.
- `qualifications` are preserved metadata in this phase and are not required for matching decisions.

## 8. Backward Compatibility

No breaking changes to:
- `Component(mpn=..., manufacturer=...)`
- component modifiers mutating `component.mpn` / `component.manufacturer`
- legacy `properties["alternatives"]` formats

The `Part` type is additive and optional.

## 9. Example

```python
preferred = builtin.part(
    mpn = "RC0603FR-0710KL",
    manufacturer = "Yageo",
    qualifications = ["AEC-Q200"],
)

alts = [
    builtin.part(mpn = "ERJ-3EKF1001V", manufacturer = "Panasonic"),
    builtin.part(mpn = "RK73H1JTTD1001F", manufacturer = "KOA", qualifications = ["PPAP"]),
]

Component(
    name = "R1",
    symbol = my_symbol,
    pins = {"1": n1, "2": n2},
    part = preferred,
    properties = {
        "alternatives": alts,
    },
)
```
