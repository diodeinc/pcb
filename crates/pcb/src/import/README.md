# KiCad Import (Developer Notes)

This folder (`crates/pcb/src/import/`) implements the KiCad import pipeline used by:

`pcb import <out-dir> --kicad-project <path-to-kicad-project>`

The main entrypoint is `crates/pcb/src/import/mod.rs`.

## Pipeline

The import flow is organized into phases:

- `discover`: identify a KiCad project and select a board entry.
- `validate`: run KiCad ERC/DRC (and other checks) to catch obvious issues early.
- `extract`: read KiCad schematic/layout artifacts into an IR (including sheet placements).
- `hierarchy`: decide how to map KiCad sheet/module structure into Zener module structure.
- `semantic`: classify parts (e.g. safe passive promotion candidates).
- `materialize`: copy/patch source files into the output tree (layout, project metadata, etc.).
- `generate`: emit `.zen` files (board/modules/components) and schematic placement comments.
- `report`: persist the extraction report for iteration/debugging.

## Power/Ground Nets

KiCad connectivity is sourced from the netlist export, but we classify net *kinds* (power/ground)
from explicit schematic intent:

- Extract all schematic `(power)` symbol instances and read their `Value` property (declares the
  global net name).
- Decide `Ground` vs `Power` from the symbol identity (`lib_id`) and join to netlist nets by exact
  name match.
- Codegen then emits `Power("...")` / `Ground("...")` instead of `Net("...")` only when
  classification is high-confidence; otherwise we fall back to `Net`.

## Schematic Placement (`# pcb:sch`)

Zener schematics persist placement as line comments at the bottom of `.zen` files:

`# pcb:sch <fully-qualified-key> x=<..> y=<..> rot=<..> [mirror=x|y]`

The editor’s schematic viewer consumes these comments.

### Coordinate systems

- KiCad sheet placement (`(at x y rot)`): translation is stored in **mm** in a **Y-down** sheet
  coordinate system.
- KiCad symbol geometry (`lib_symbols`): local geometry uses a **Y-up** coordinate system.
- `# pcb:sch` persisted values: `x/y` are stored in **0.1mm** units in **Y-down**; rotation is
  stored in degrees **clockwise-positive**; `mirror` is the **axis of reflection** (`x` flips Y,
  `y` flips X).

### Transform order

Both KiCad and the editor apply transforms in symbol-local space as:

1. rotate
2. mirror

about the symbol origin, then translate.

### Internal representation

In import code we treat an instance placement as an affine transform:

`p' = A * p + t`

where `A` is the 2x2 linear transform (rotation and/or mirror) and `t` is translation.

Implementation uses `glam`'s double-precision types:

- `DVec2` for translations/points
- `DMat2` for linear transforms
- `DAffine2` for full poses when needed

This keeps the rotation+mirror math explicit and makes it harder to introduce sign/order bugs.

### Mapping model

The importer primarily operates in “KiCad semantics” and performs a final conversion right before
writing `# pcb:sch` comments. The detailed math and rationale live in:

- `docs/specs/kicad-import.md` ("Schematic Placement Mapping")
- implementation: `crates/pcb/src/import/generate/schematic_placement.rs`

Important subtlety: when promoting passives (substituting the KiCad symbol with a stdlib
Device:R/C symbol), the symbol geometry changes. For these cases, we preserve **visual placement**
by aligning the transformed **visual AABB top-left** (axis-aligned bbox in world space after
rotate+mirror) between the source and target symbol, then converting that target origin into the
editor’s stored-anchor format.

### Symbol geometry sources

Placement conversion needs per-symbol bounds and (for passive promotion) pin-direction info.
Geometry is derived from:

1. embedded schematic `lib_symbols` (preferred)
2. KiCad global symbol libraries (if `KICAD_SYMBOL_DIR` or a platform default is available)

The bounds extractor intentionally matches the editor’s bbox expansion so small constant offsets
don’t accumulate into visible drift.
