# KiCad Import (Developer Notes)

This folder (`crates/pcb/src/import/`) implements the KiCad import pipeline used by:

`pcb import <path-to-project.kicad_pro> <output-dir>`

In addition to generating Zener sources, import writes a portable archive of the original KiCad
project sources to the target board directory as `<board>.kicad.archive.zip`.

Entrypoint is `crates/pcb/src/import/mod.rs` (orchestrated by `crates/pcb/src/import/flow.rs`).

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

## Identifiers

Import uses KiCad's stable, cross-artifact UUID-based identity for instances.

- `KiCadUuidPathKey` = (`sheetpath.tstamps`, `symbol_uuid`)
- `sheetpath.tstamps` is a `/.../` chain of sheet UUIDs. Root is `/`.

Joins (schematic/netlist/layout) should use this key rather than refdes.

## Output Layout

For an imported board named `<board>`:

- `boards/<board>/<board>.zen` (root board module)
- `boards/<board>/modules/<SheetName>/<SheetName>.zen` (sheet modules)
- `boards/<board>/components/.../*.zen` (imported components)
- `boards/<board>/layout/<board>/layout.kicad_pcb` (patched KiCad PCB)
- `boards/<board>/.kicad.import.extraction.json` (extraction report)

## Footprint De-Instancing (.kicad_pcb -> .kicad_mod)

Import cannot assume the original footprint library `.kicad_mod` files exist at import time.
We therefore generate standalone `.kicad_mod` footprints by de-instancing the `(footprint ...)` blocks
embedded in the PCB file.

Model:
- Instance pose: translation `t = (tx, ty)` and rotation `theta` from the root `(at tx ty theta)`.
- `is_back` derived from the root `(layer "B.*")`.
- Mirror across X axis (flip Y): `M(x, y) = (x, -y)` and rotation `R(theta)`.

Normalization rules (high level):
- Drop instance-only fields: root `at/path/sheetname/sheetfile/uuid/locked/property`, per-pad `net/uuid`.
- Local geometry points (pads + `fp_*` graphics) are footprint-local but mirrored when `is_back`:
  - `p_file = p_local` (front)
  - `p_file = M(p_local)` (back)
- Footprint-embedded `zone` polygon `(xy ...)` points in `.kicad_pcb` are serialized in **absolute board coordinates**
  with the instance pose applied:
  - `p_file = t + R(theta) * p_local` (front)
  - `p_file = t + R(theta) * M(p_local)` (back)
  Invert to recover `.kicad_mod` footprint-local points:
  - `p_local = R(-theta) * (p_file - t)` (front)
  - `p_local = M(R(-theta) * (p_file - t))` (back)
- Pad angles in `.kicad_pcb` pad `(at x y ANGLE)` are serialized as board-absolute; normalize to pad-local:
  - `a_local = a_file - theta` (front)
  - `a_local = theta - a_file` (back)
- When `is_back`, swap all `layer`/`layers` strings `B.* <-> F.*` and remove `mirror` from `(justify ...)`.

Implementation: `crates/pcb-sexpr/src/board.rs` `transform_board_instance_footprint_to_standalone(...)`.

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
- implementation: `crates/pcb/src/import/generate/schematic_comments.rs` (comment collection + emission)
- implementation: `crates/pcb/src/import/generate/schematic_types.rs` (shared comment data)
- implementation: `crates/pcb/src/import/generate/schematic_placement.rs` (KiCad->editor anchor mapping)

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
