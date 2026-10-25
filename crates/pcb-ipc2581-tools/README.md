# pcb-ipc2581-tools

`pcb-ipc2581-tools` implements the `pcb ipc2581` command group. The `pcb ipc`
alias provides the same commands.

Run `pcb ipc2581 --help` for available inspection, editing, rendering, and
manufacturing commands, and `pcb ipc2581 <command> --help` for their options.

## Geometry accuracy

Global `--accuracy-um N` sets the geometry approximation budget to an integer
1–100 µm (default 10). It applies to IPC-2581 geometry, Gerber operations, and
IPC HTML reports, not comparison or feature-significance tolerances. DFM stays
at 10 µm and copper balancing at 50 µm. KiCad exports, external commands, and
interposer generation are not controlled by this flag. Smaller budgets may cost
more time and memory; coarse approximation can lose small features.

## Assembly and production metadata

`assembly` reports the complete board array by default; `--scope board` selects
one canonical board. See the [assembly report contract](docs/assembly-report.md).

`edit bom` applies path-based manufacturer, MPN, and supplier selections from
JSON. Both `--selections` and `--output` are required; see the
[selection format](src/commands/bom_edit.rs).

```bash
pcb ipc edit bom design.xml --selections selections.json --output selected.xml
```

`edit population` sets `RefDes@populate` on the named designators only:
`false` for `--dnp`, `true` for `--populate`. Every other designator keeps the
population it was authored with; a designator outside the BOM is an error. The
assembly report, `cpl --exclude-dnp`, and board arrays of the edited file follow
the population. An unchanged population leaves the file byte-for-byte intact.

`edit provenance --provenance <json>` records a production run's parties, BOM
header, history, and configuration properties; see the
[JSON example](src/commands/testdata/provenance.json). Board arrays preserve
this provenance, and `info --format json` reports it under `provenance`.

Use `view --mode fabrication` to create an IPC-2581C fabrication projection.
The projection removes BOM/AVL, package, placement, assembly, solder-paste,
documentation, logical-net, and DFX data. It retains physical construction,
manufacturing artwork, physical nets, panel repeats, and definitions referenced
by the retained geometry.

## DFM process design kits

`dfm check` defaults to the `standard` PDK. Use `--pdk` to select a built-in name
or a strict, versioned TOML file:

```bash
pcb ipc dfm check fabrication-panel.xml --pdk standard --output panel.dfm
```

See the [PDK and report formats](docs/dfm.md) for built-in coverage,
manufacturer-specific omissions, advisory checks, and custom PDKs. A passing
report covers only the selected checks, not full IPC compliance or certification.

Complete reports are SQLite databases with diagnostics, native vector artwork,
and the exact PDK source for external viewers. PCB does not generate DFM HTML or
host a viewer.

## Panels and copper balancing

Board-array creation balances copper by default; use `--no-copper-balance`
to disable it. Fabrication-panel creation requires `--copper-balance` to enable
balancing. Both commands accept either flag, but not both together. See the
[copper-balancing strategy](docs/copper-balancing.md).

`board-array plan` lists the grid, board count, and utilization that
`board-array create --sheet` gives on each sheet from A7 to A3. With
`--quantity` (and an optional `--moq`, the minimum number of arrays per order),
it adds the arrays, boards, and surplus each sheet orders. It then recommends the
sheet that orders the least array area, excluding A3 whenever a smaller sheet
fits the board. Ties favor the smaller sheet. `--format json` writes the same
report as JSON.

`board-array create --mouse-bite` uses routed slots and perforated tabs instead
of V-scores; `--mouse-bite-placement` only analyzes placement and writes JSON.
The [experimental tab geometry and physical-acceptance limits](../pcb-ir/src/geom/mouse_bite/README.md)
apply even when manufacturing files are generated.

`fab-panel create` packs assembly panels into a fabrication panel. Repeat an
input path to request multiple copies:

```bash
pcb ipc2581 fab-panel create \
  --panel-size 16x18 \
  --output fabrication-panel.xml \
  assembly-a.xml assembly-a.xml assembly-b.xml
```

`--emit-usable-area` omits process margins from the output profile and rebases
the packing area to the origin, retaining stock size and margins in metadata.
For example, an 18 by 24 inch panel with `--edge-margin 25.4` emits a 16 by
22 inch profile.

Inputs must have identical physical stackups, units, and IPC revisions, and
each must have a panel layout root with nonempty profile bounds. The first input
provides the stackup and canonical physical layer definitions. Packing uses
profile bounding boxes; it does not validate rounded-rectangle shape or winding,
nor does accepting a profile establish manufacturing qualification.

The generated panel applies the fabrication projection by default. Gerber
manufacturing export separates profile geometry by purpose:

- `Fab_Panel_Outline.gm1` contains the emitted panel outline: the stock panel by
  default, or its usable packing area with `--emit-usable-area`.
- `Assembly_Panel_Outlines.gm1` contains nominal assembly-panel outlines.
- `Board_Cutouts.gm1` contains assembly-panel and board profile cutouts plus
  generated routing reliefs.

The outline files do not apply cutter compensation. Drill and route features
remain in their XNC files.

The command supports up to 32 assembly panels and fails without writing an
output when it cannot find a layout.

## In-memory library and WebAssembly

Disable the default `cli` feature for `wasm32-unknown-unknown`. This excludes
terminal output, network availability lookups, native Zstandard, and file
command wrappers; native CLI behavior remains enabled by default.

Import/export and DFM use the same implementations in both environments.
`manufacturing::build_manufacturing_package` reuses an imported design; the
resulting package exposes individual files and `to_zip()` for an in-memory
archive. `geometry::render::layer_artwork` returns a `LayerView` containing
artwork for the shared SVG/PNG renderers.

`assembly::build_report` builds the assembly report from the same imported design.

For browser and Node.js bindings, see [`pcb-ipc-wasm`](../pcb-ipc-wasm/README.md).

```bash
cargo check -p pcb-ipc2581-tools --no-default-features --target wasm32-unknown-unknown
```

Run native tests with `cargo test -p pcb-ipc2581-tools`.
