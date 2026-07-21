# KiCad import pipeline

This module implements:

```bash
pcb import <project.kicad_pro> <output-directory>
```

The command converts a KiCad project into a Zener board repository. It also
writes `<board>.kicad.archive.zip`, which preserves the original KiCad source
files.

`flow.rs` coordinates the pipeline. `mod.rs` defines the module boundary.

## Pipeline

Import runs these phases in order:

1. `discover` locates the KiCad project and selects its board.
2. `validate` runs the applicable KiCad ERC and DRC checks.
3. `extract` converts schematic and layout data into the import IR.
4. `hierarchy` maps KiCad sheets to Zener modules.
5. `semantic` classifies components, including passive-promotion candidates.
6. `materialize` copies and patches source assets into the output repository.
7. `generate` writes board, module, component, and schematic-position sources.
8. `report` writes the extraction report used for diagnostics.

An error stops the import at the current phase. Because the command prepares the
output repository before validation, a failure can leave generated or partial
files. Correct the cause and rerun with `--force` to replace generated output.

## Output

For a board named `<board>`, the output repository contains:

```text
<output-directory>/
├── <board>.zen
├── <board>.kicad.archive.zip
├── modules/<SheetName>/<SheetName>.zen
├── components/.../*.zen
├── layout/<selected-project>.kicad_pro
├── layout/<selected-board>.kicad_pcb
└── .kicad.import.extraction.json
```

The generated layout retains the original KiCad project and board filenames.

## Cross-file identity

Join schematic, netlist, and layout records with `KiCadUuidPathKey`, not with a
reference designator. The key contains the instance's sheet UUID path and symbol
UUID:

```text
KiCadUuidPathKey = (sheetpath.tstamps, symbol_uuid)
```

The root sheet path is `/`. Reference designators can change or collide across
hierarchical sheets and are not stable cross-file identifiers.

## Footprint de-instancing

Import cannot assume that the original `.kicad_mod` libraries are present. It
therefore converts each embedded `(footprint ...)` instance in the board into a
standalone footprint file.

The conversion applies these rules:

1. Remove instance-only fields, including root placement, path, sheet, UUID,
   lock, and property data, plus per-pad nets and UUIDs.
2. Preserve front-side local geometry. Mirror back-side local geometry across
   the X axis and exchange `F.*` and `B.*` layer names.
3. Convert embedded zone polygons from board coordinates back to footprint-local
   coordinates by removing the instance translation and rotation.
4. Convert absolute pad angles to local angles. Front-side pads use
   `a_local = a_file - theta`; back-side pads use
   `a_local = theta - a_file`.
5. Remove `mirror` from back-side text justification after applying the
   geometry transform.

`pcb-sexpr::board::transform_board_instance_footprint_to_standalone` implements
these rules.

## Power and ground classification

The netlist supplies connectivity, while schematic power symbols supply net
intent. Import reads each `(power)` symbol's `Value`, classifies its library
identity as power or ground, and joins it to a net by exact name. Code generation
emits `Power` or `Ground` only for a high-confidence match; otherwise it emits
`Net`.

## Schematic positions

Generated `.zen` files store symbol placement in trailing `pcb:sch` comments:

```text
# pcb:sch <id> x=<value> y=<value> rot=<value> [mirror=<x|y>]
```

The relevant coordinate systems differ:

| Source | Position units | Y axis | Rotation |
|---|---|---|---|
| KiCad sheet placement | mm | Down | KiCad sheet semantics |
| KiCad symbol geometry | mm | Up | Symbol-local |
| `pcb:sch` comment | 0.1 mm | Down | Clockwise-positive degrees |

Both KiCad and the schematic editor rotate and then mirror in symbol-local space
before applying translation. The importer represents this operation as
`p' = A * p + t` with `glam::DMat2` and `glam::DVec2`, then converts to the
stored editor coordinates immediately before writing comments.

Passive promotion can replace a source symbol with a standard resistor or
capacitor symbol that has different bounds. In that case, import aligns the
transformed visual bounding boxes instead of copying the original symbol origin.
This preserves the visible placement.

Embedded schematic `lib_symbols` are the preferred geometry source. Import can
fall back to the KiCad global symbol libraries when `KICAD_SYMBOL_DIR` or a
platform default is available.

The schematic placement implementation is under
`generate/schematic_placement.rs`; comment collection and serialization are
under `generate/schematic_comments.rs` and `generate/schematic_types.rs`.

## Verification

Run the focused importer tests with:

```bash
cargo test -p pcbc import
```
