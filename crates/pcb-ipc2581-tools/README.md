# pcb-ipc2581-tools

`pcb-ipc2581-tools` implements the `pcb ipc2581` command group. The `pcb ipc`
alias provides the same commands.

| Command | Purpose |
|---|---|
| `info` | Report board, layer, drill, and stackup metadata. |
| `assembly` | Emit the versioned PCBA assembly report as JSON. |
| `bom` | Export the bill of materials. |
| `cpl` | Export component placement data. |
| `html` | Export an HTML board summary. |
| `outline` | Export a KiCad-compatible DXF outline. |
| `render` | Render one layer as terminal graphics, SVG, or PNG. |
| `dfm check` | Check IPC-2581 geometry against a fabrication PDK and emit self-contained JSON. |
| `gerber` | Export fabrication layers and drill files. |
| `view` | Export a filtered IPC-2581 function-mode document. |
| `board-array create` | Create a rectangular board array. |
| `fab-panel create` | Tile assembly panels into a supported fabrication panel size. |
| `edit bom` | Add approved alternatives to BOM entries. |

Run `pcb ipc2581 <command> --help` for arguments and output options.

Use `assembly` to emit the complete board-array report, or select one canonical
board with `--scope board`:

```bash
pcb ipc assembly design.xml --scope board-array > assembly-report.json
```

Board-array creation balances copper by default; pass `--no-copper-balance`
to disable it. Fabrication-panel creation leaves copper unchanged by default;
pass `--copper-balance` to enable balancing. Both commands accept the inverse
flag as an explicit override, and reject using both flags together.

The mathematical and geometric strategy for automatic copper balancing — both
the board-array and fabrication-panel passes — is documented in
[`docs/copper-balancing.md`](docs/copper-balancing.md).

`edit bom` modifies the input file when `--output` is omitted. Specify an output
path when the source document must remain unchanged.

Use `view --mode fabrication` to create an IPC-2581C fabrication projection.
The projection removes BOM/AVL, package, placement, assembly, solder-paste,
documentation, logical-net, and DFX data. It retains physical construction,
manufacturing artwork, physical nets, panel repeats, and definitions referenced
by the retained geometry.

## DFM process design kits

`dfm check` accepts a built-in process design kit name or a strict, versioned
TOML file. Built-ins include `standard`, `jlcpcb-1oz` (`jlc`), and the nine IPC
Class/Producibility profile identities:

```bash
pcb ipc dfm check fabrication-panel.xml \
  --output dfm-report.json
```

The PDK defaults to `standard`. Pass `--pdk` with a built-in name or a path to
select another process definition.

`standard` currently supports 2 through 10 copper layers.
`jlcpcb-1oz` checks JLCPCB's rigid FR-4 service with 1 oz outer copper; `jlc`
is an alias for the same PDK. It uses the conservative 0.13 mm soldermask-web
limit published for black and white mask across every color. The `ipc` alias
selects the opinionated Class 2 / Producibility Level B default. It and the
explicit `ipc-1a` through `ipc-3c` profiles run Diode's opinionated partial
baseline for plated-hole aspect ratio, via, PTH, and NPTH hole-to-copper
clearance, and via, PTH, NPTH, plated-slot, and nonplated-slot board-edge
clearance. Diode selected these values with IPC design topics as context; they
are not licensed IPC numeric matrices, do not prove full IPC compliance, and
do not imply IPC certification.

Pass a path such as `--pdk ./fab-process.toml` to use a custom PDK. Exact
built-in names take precedence, so prefix a same-named file with `./`.

Complete JSON reports contain diagnostics, native vector artwork, and the exact
PDK source for external viewers. PCB does not generate DFM HTML or host a viewer.

See the [PDK, waiver, and JSON formats](docs/dfm.md) and the
[standard PDK](pdks/standard.toml) for details.

`fab-panel create` supports the common 12 by 18, 16 by 18, 18 by 24, and 21 by
24 inch fabrication panel sizes through `--panel-size`. The default is 18 by 24
inches:

```bash
pcb ipc2581 fab-panel create \
  --panel-size 16x18 \
  --output fabrication-panel.xml \
  assembly-a.xml assembly-a.xml assembly-b.xml
```

By default, the command reserves 25.4 mm on the left and right and 50.8 mm on
the top and bottom of every stock panel. This gives the following packing areas:

| Stock panel | Usable packing area |
| --- | --- |
| 12 by 18 in | 10 by 14 in |
| 16 by 18 in | 14 by 14 in |
| 18 by 24 in | 16 by 20 in |
| 21 by 24 in | 19 by 20 in |

Use `--edge-margin` with one to four CSS-shorthand values to override the
process margins in millimeters. Use `--panel-gap` to override the default 7.62 mm
gap between assembly panels:

```bash
pcb ipc2581 fab-panel create \
  --panel-size 18x24 \
  --edge-margin 50.8 25.4 \
  --panel-gap 7.62 \
  --output fabrication-panel.xml \
  assembly-a.xml assembly-b.xml
```

Use `--emit-usable-area` to omit the reserved process margins from the generated
profile and rebase the usable packing area to the origin. For example, an 18 by
24 inch stock panel with a 25.4 mm margin on every side emits a 16 by 22 inch
profile while retaining the stock size and process margins in metadata:

```bash
pcb ipc2581 fab-panel create \
  --panel-size 18x24 \
  --edge-margin 25.4 \
  --emit-usable-area \
  --output fabrication-panel.xml \
  assembly-a.xml assembly-b.xml
```

All inputs must have identical physical stackups. The first input provides the
fab panel stackup and canonical physical layer definitions. Each input must have
one simple clockwise rounded-rectangle root profile, as generated by
`board-array create`.

The generated panel applies the fabrication projection by default. Gerber
manufacturing export separates profile geometry by purpose:

- `Fab_Panel_Outline.gm1` contains the emitted panel outline: the stock panel by
  default, or its usable packing area with `--emit-usable-area`.
- `Assembly_Panel_Outlines.gm1` contains nominal assembly-panel outlines.
- `Board_Cutouts.gm1` contains assembly-panel and board profile cutouts plus
  generated routing reliefs.

The outline files do not apply cutter compensation. Drill and route features
remain in their XNC files.

Repeat an input path to request more than one copy. The command supports up to
32 assembly panels and fails without writing an output when it cannot find a
layout.

```bash
cargo test -p pcb-ipc2581-tools
```

## In-memory library and WebAssembly

Disable the default `cli` feature for `wasm32-unknown-unknown`. This excludes
terminal output, network availability lookups, native Zstandard, and file
command wrappers; native CLI behavior remains enabled by default.

Import/export and DFM use the same implementations in both environments.
`manufacturing::build_manufacturing_package_from_design` reuses an imported
design; the resulting package exposes individual files and `to_zip()` for an
in-memory archive. `geometry::render::prepare_layer` prepares a layer for the
shared SVG/PNG renderers.

`assembly::build_report` builds the deterministic, schema-versioned PCBA
assembly contract from that same imported design. See the
[assembly report contract](docs/assembly-report.md) for scope, units,
identities, physical evidence, and readiness semantics.

For browser and Node.js bindings, see [`pcb-ipc-wasm`](../pcb-ipc-wasm/README.md).

```bash
cargo check -p pcb-ipc2581-tools --no-default-features --target wasm32-unknown-unknown
```
