# pcb-sch

Schematic data model and utilities used by the Zener toolchain.

## `pcb:sch` Position Comments

`pcb-sch` parses and writes schematic placement comments stored in `.zen` files.

Canonical line format:

```text
# pcb:sch <id> x=<f64> y=<f64> rot=<f64> [mirror=<x|y>]
```

Fields:

- `id`: symbol/comment key (single token, no spaces)
- `x`, `y`: schematic coordinates
- `rot`: rotation in degrees
- `mirror` (optional): mirror axis (`x` or `y`)

Examples:

```text
# pcb:sch R1 x=100.0000 y=200.0000 rot=0
# pcb:sch U1 x=150.0000 y=80.0000 rot=90 mirror=x
```

## Parsing Behavior

- Only the trailing contiguous `pcb:sch` block at end-of-file is parsed (blank lines allowed).
- Parsing stops when a non-empty, non-`pcb:sch` line is encountered while scanning upward from EOF.
- Malformed lines in that trailing block are ignored.

## Netlist Serialization

Position comments are surfaced in netlist output under:

- `instances.<instance>.symbol_positions`

Position object shape:

```json
{
  "x": 100,
  "y": 200,
  "rotation": 90,
  "mirror": "x"
}
```

Notes:

- `mirror` is optional and omitted when unset.
- Symbol keys are stable IDs such as `comp:R1` and `sym:VCC#1`.
