# KiCad import pipeline

This module converts a KiCad project or standalone schematic into a Zener board repository.

```bash
pcb import <design.kicad_sch|project.kicad_pro> <output-directory>
```

## Pipeline

`flow.rs` runs these phases in order:

1. `discover` resolves the input schematic hierarchy and optional project files.
2. `validate` runs ERC and, for project imports, DRC and schematic-layout parity checks.
3. `extract` converts schematic data and optional layout data into the import IR.
4. `hierarchy` maps KiCad sheets to Zener modules.
5. `semantic` classifies components and passive-promotion candidates.
6. `materialize` copies project layout files and writes validation diagnostics.
7. `generate` writes board, module, component, and schematic-position sources.
8. `generated_validate` builds the generated board and verifies its physical pins and net partitions.
9. `report` writes the extraction report.

KiCad CLI validation and netlist export run on temporary source copies.
Import resolves the generated Zener workspace offline during final validation.

## Output behavior

A new output directory receives a Git repository, README, manifest, and
standard-library setup. Import refuses an existing board repository unless
`--force` is supplied. That flag removes generated board, module, component,
report, and archive files before regeneration, and replaces project layout output.

For a board named `<board>`, a standalone schematic import produces:

```text
<output-directory>/
├── .git/
├── .gitignore
├── README.md
├── pcb.toml
├── <board>.zen
├── modules/<SheetName>/<SheetName>.zen
├── components/.../*.zen
├── components/.../*.kicad_sym
├── components/.../*.kicad_mod
├── .kicad.import.extraction.json
└── .kicad.validation.diagnostics.json
```

A standalone import does not create a KiCad project, PCB, or source archive.
The generated board declares the standard `layout` path so a later `pcb layout` command can create layout files.

A project import also creates:

```text
<output-directory>/
├── <board>.kicad.archive.zip
└── layout/
    ├── <selected-project>.kicad_pro
    ├── <selected-board>.kicad_pcb
    └── <selected-project>.kicad_dru   # when present
```

## Standalone footprint resolution

A standalone `.kicad_sch` does not contain board-embedded footprint geometry.
Import resolves each referenced footprint in this order:

1. the sibling project `fp-lib-table`;
2. the global KiCad `fp-lib-table` under `KICAD_CONFIG_HOME` or the platform KiCad configuration directory;
3. the bundled KiCad standard-library subset;
4. the cached `kicad-footprints` package matching the schematic's KiCad major version.

An enabled project table entry wins when a project and global table use the same library nickname,
including when the project library does not contain the referenced footprint.
A disabled project entry falls through to the global table, matching KiCad behavior.
Import copies project, global, and cached footprint geometry into the generated component package.
Bundled standard-library footprints remain library references.
For resolved geometry, the footprint must contain every physical pin defined by the embedded symbol.

If a footprint cannot be resolved, import preserves its KiCad footprint ID and continues the structural conversion.
The warning prints a capped list of unresolved footprint IDs.
The generated board preserves connectivity but is not layout-ready until the missing geometry is supplied.

Referenced project-local sheets, symbols, and footprints must remain under the schematic directory.
Import rejects project-local paths and symlinks that escape that directory.
A global library is external by definition, but a resolved footprint must remain inside the directory declared for that library.

## Physical-pin mapping

Generated components map each KiCad physical pin number to a distinct Zener logical signal through `pin_defs`.
Displayed KiCad pin names are labels and do not define electrical identity.
Pins with duplicate displayed names remain distinct unless the source netlist connects them to the same net.
Separate no-connect pins remain separate open terminals.
Mechanical and documentation footprints with no numbered pads remain pinless components.

`pcb-component-gen` renders each physical-pin plan as `Component(...)`.

## Generated-board validation

The offline build suppresses `bom.unspecified` and `bom.underspecified` locally;
sourcing completion is not part of structural import.

Validation compares the complete generated physical-pin set and net partitions with the KiCad source.
It rejects missing pins, unexpected pins, lost source endpoints, and shorts that are absent from the source schematic.
A source pin with no connection may remain isolated on its own generated net.
It must not share that net with another endpoint.

## Cross-file identity

Import joins schematic, netlist, and layout records by `KiCadUuidPathKey`:
the instance sheet UUID path (`sheetpath.tstamps`) and symbol UUID.
Reference designators are unsuitable because they can change or collide across sheets.

## Footprint de-instancing

Project imports extract standalone footprints from the board, without requiring
the original `.kicad_mod` libraries.
`pcb-sexpr::board::transform_board_instance_footprint_to_standalone` removes
instance placement, path, UUID, property, and net data. It preserves local geometry,
converts back-side geometry and layers, and makes embedded zones and pad angles local.

## Schematic positions

Generated `.zen` files store symbol placement in trailing `pcb:sch` comments:

```text
# pcb:sch <id> x=<value> y=<value> rot=<value> [mirror=<x|y>]
```

The importer converts KiCad coordinates and transformations into the schematic editor coordinate system.
Passive promotion aligns transformed visual bounds when the replacement standard-library symbol has different geometry.
The implementation is under `generate/schematic_placement.rs`, `generate/schematic_comments.rs`, and `generate/schematic_types.rs`.

## Verification

Run the focused importer tests with:

```bash
cargo test -p pcbc import
```
