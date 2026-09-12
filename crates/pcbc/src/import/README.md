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
5. `semantic` classifies power and ground nets from native power symbols.
6. `materialize` copies the original schematic hierarchy, optional project layout, and diagnostics.
7. `generate` writes board, module, and component sources using the original embedded symbols.
8. `generated_validate` builds the board, verifies physical pins and net partitions, then binds and checks the persistent schematic through `pcb-kicad-sch`.
9. `report` writes the extraction report.

KiCad CLI validation and netlist export run on temporary source copies.
Import resolves the generated Zener workspace offline during final validation.
Source ERC, DRC, and schematic/PCB parity findings are reported, not import blockers.
Connectivity follows the schematic even when the PCB is out of sync; import retains
the existing PCB placement and routing rather than repairing it. The diagnostics JSON
retains the individual parity findings, and the extraction report records their count.
Generated-model pin and connectivity validation still blocks incorrect conversions.

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
├── layout/<board>.kicad_pro
├── layout/<board>.kicad_sch       # and its original child sheets
├── .kicad.import.extraction.json
└── .kicad.validation.diagnostics.json
```

A standalone import creates a minimal KiCad project for its persistent schematic,
but no PCB or source archive. It keeps an existing layout and project configuration
on forced reimport. The board enables `schematic = True` at the standard `layout`
path so Quiche and `pcb apply schematic` use the copied document.

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
Native no-connect markers become `NotConnected()`; floating unmarked pins remain singleton nets.
One marker covers all stacked pins, including hidden pins, at the same symbol anchor on the same page.
Each physical pad remains a distinct generated terminal. Wires, labels, or touching unrelated symbols
prevent classifying an anchor as an intentional open.
Mechanical and documentation footprints with no numbered pads remain pinless components.

`pcb-component-gen` renders each physical-pin plan as `Component(...)`.

## Generated-board validation

The offline build suppresses `bom.unspecified` and `bom.underspecified` locally;
sourcing completion is not part of structural import.

Validation compares the complete generated physical-pin set and net partitions with the KiCad source.
It rejects missing pins, unexpected pins, lost source endpoints, and shorts that are absent from the source schematic.
Only geometry-proven intentional-open groups are compared as individual open terminals rather than
shared nets; KiCad exports stacked marked pads as geometric groups. The raw extraction report and
native schematic retain those groups unchanged. All other partitions must match exactly.
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

## Persistent schematic identity

`schematic.rs` patches the copied native document without reconstructing it:

- Each managed symbol receives its evaluated component `Path` and the shared
  `SymbolSlotKey` UUID for that path and unit. Multi-unit symbols share a Path.
- Hidden `pcb:net` fields bind labels and power symbols to Zener net names without
  changing displayed text. Hidden power-input pins use `pcb:net:<pin-name>`.
  The shared connectivity engine still merges using native KiCad names and wiring;
  logical bindings cannot connect disconnected geometry or conceal a physical short.
- Generated component properties retain displayed `Value`, `Footprint`, and
  `schematic_description`, including empty strings, separately from resolved
  footprint geometry and the inferred BOM description. Applying does not replace
  displayed values with library IDs or populate visible empty descriptions.
- Wires, graphics, sheet relationships, native no-connects, and symbol geometry
  remain in the original schematic. Import no longer classifies passive-promotion
  candidates, substitutes symbols, or writes legacy `pcb:sch` comments.
- Embedded definitions resolve through each symbol's `lib_name` override in its
  source sheet, while `lib_id` remains its library identity. Distinct cached
  definitions generate distinct parts even when their library IDs match.

The shared engine must be able to inspect the imported connectivity. Unsupported
constructs (including buses and managed components on reused sheet files) fail
explicitly rather than silently reconstructing a different circuit. KiCad 9 and
10 documents use the same parser/normalization as the persistent editor.

## Verification

Run the focused importer tests with:

```bash
cargo test -p pcbc import
```
