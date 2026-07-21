# pcb-sch

`pcb-sch` provides the schematic and netlist data model used by the Zener
toolchain. It also reads and writes schematic positions stored in trailing
`pcb:sch` comments in `.zen` files.

## Schematic position comments

Each comment records the position and orientation of one schematic symbol:

```text
# pcb:sch <id> x=<f64> y=<f64> rot=<f64> [mirror=<x|y>]
```

`id` is a single-token symbol key. `x` and `y` are schematic coordinates, `rot`
is the rotation in degrees, and the optional `mirror` field specifies the `x` or
`y` reflection axis.

The parser reads only the final contiguous comment block. Blank lines may occur
inside that block. Scanning stops at the first nonempty line that is not a
`pcb:sch` comment, and malformed comments are ignored.

Netlist serialization writes positions to
`instances.<instance>.symbol_positions`. The `mirror` property is omitted when
the symbol is not mirrored.
