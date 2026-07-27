# KiCad import pipeline

This module implements both import forms:

```bash
pcb import <design.kicad_sch|project.kicad_pro> [output-directory]
```

The command converts a KiCad schematic or project into a Zener board repository.
When `output-directory` is omitted, import writes to `./<input-file-stem>`.

Import is a one-way migration tool. Zener is the source of truth afterwards, so
import writes as little as it can: the converted sources, and the `pcb.toml` that
makes the result buildable. It keeps no record of previous runs.

`flow.rs` coordinates the pipeline. `mod.rs` defines the module boundary.

## Offline

Import never reaches the network. `offline_eval_state` in `mod.rs` is the only
place the module resolves a Zener workspace and it always resolves offline, MPN
lookup reads only on-disk indexes, and a substitution candidate must already be in
`~/.pcb/cache` or the board's `vendor/`. An uncached candidate is skipped and the
component falls back to a generated Zener component, never an error.

## Pipeline

Import runs these phases in order:

1. `discover` resolves the input schematic hierarchy and optional project files.
2. `validate` runs ERC and, for project imports, DRC and schematic-layout parity checks.
3. `extract` converts schematic data and optional layout data into the import IR.
4. `hierarchy` maps KiCad sheets to Zener modules.
5. `semantic` classifies components, including passive-promotion candidates.
6. `registry_lookup` performs exact MPN candidate lookup against cached registry indexes.
7. `materialize` prepares the board paths and copies KiCad project and board files.
8. `generate` writes board, module, component, and schematic-position sources.
9. `reuse_validate` builds the written board and verifies its complete physical-pin set and net partitions.
10. `report` writes the extraction report used for diagnostics.

KiCad CLI validation and netlist export run on contained temporary copies, so
import does not change source-side preference files.

There is no staging tree and no commit phase. Import only ever *adds* files, so
`output::ImportWriter` writes straight into the destination and applies one rule
per path: a path this run already wrote may be rewritten, a path whose generated
bytes match what is already there is skipped silently, a differing existing path
is kept and reported, and `--force` replaces what this run generates. Every
content write is a temp-file-and-rename, so no file is ever left half-written.
A failure part-way therefore leaves some generated files and every pre-existing
file untouched; delete the output directory and re-run.

## Re-importing

Import into a directory that already holds output needs no flag, and by default
it cannot overwrite anything. A generated path whose content differs from what is
already there keeps the existing bytes, is named on stderr, and the run
succeeds. Genuinely new files are still added. Hand-authored sources, and fixes
an agent made to generated sources, therefore survive a re-import untouched.

A collision on any file inside a `components/<X>` or `modules/<X>` package keeps
the **whole** package. Pairing an authored `.zen` with a freshly generated symbol
beside it would produce an incoherent package, so the package is the unit.
Collisions outside a package keep the individual file.

`--force` overwrites what import generates on that run. It is bounded by what
import writes, not by a record of earlier runs, so it never reaches a file import
did not produce this time. Reach for it while iterating on a migration; it will
discard edits to any file import regenerates.

A symlinked destination is never followed: in the default mode it counts as
existing, and under `--force` the rename replaces the link itself rather than
writing through it. A symlink standing in for an output directory is refused
outright. Import refuses to write outside the directory it was given.

## Output

For a board named `<board>`, the output repository contains:

```text
<output-directory>/
├── pcb.toml                                       # only when absent
├── .gitignore                                     # only when absent
├── <board>.zen
├── modules/<SheetName>/<SheetName>.zen
├── components/.../*.zen
├── components/.../*.kicad_sym
├── <board>.kicad.archive.zip                      # only with --archive-sources
├── layout/<selected-project>.kicad_pro            # project imports only
└── layout/<selected-board>.kicad_pcb              # project imports only
```

Import writes `pcb.toml` and `.gitignore` only when they are absent, so the
result is buildable without overwriting a repository that already exists. It does
not run `git init`, write a `README.md`, or vendor the stdlib into the
destination; the validating build materializes `.pcb/stdlib` itself when it needs it.

The extraction report and the validation diagnostics are written under the output
repository's `.pcb/import/`, and import prints both paths on stderr:

```text
Wrote import extraction report to <output>/.pcb/import/.kicad.import.extraction.json
Wrote import validation diagnostics to <output>/.pcb/import/.kicad.validation.diagnostics.json
```

Both describe the conversion rather than the design. `.pcb/` is gitignored, so
they do not churn `git status`; they are written directly rather than through the
no-overwrite policy, so a re-import overwrites them instead of colliding with
them; and there is exactly one set per board, so nothing accumulates. They sit
next to the board they describe, so they are findable without reading stderr.

## Known limitations

These are deliberate for a first version, not oversights. Import is a one-way
migration tool and Zener is the source of truth afterwards, so each of them is
cheap to live with and expensive to fix properly.

Validating the result builds it, which materializes `.pcb/stdlib` in the output
directory — the same `.pcb/` any `pcb build` there produces, and gitignored for
the same reason. It is not part of the tracked output.

`--force` replaces what the current run generates; it never deletes a file an
earlier import produced that the current source no longer yields. Delete a
component in KiCad, re-import, and the old generated package stays. Import into a
fresh directory when you want the output to match the source exactly.

A collision inside a `components/<X>` or `modules/<X>` package reverts the whole
package, so a file this run generated inside it is discarded rather than merged.
Keeping the package coherent matters more than keeping the newest of each file.

On a case-insensitive filesystem, two paths differing only in case collapse onto
one. Import does not detect that, so a KiCad design with sheets or components
whose names differ only in case can silently lose one of them on macOS.

`pcb.toml` holds a single `[board]` table and import never rewrites a file you
own, so importing into a repository that already declares a different board
leaves the new one undeclared. Import warns and the board still builds by
explicit path.

Footprint resolution searches KiCad libraries, not Zener component packages. A
registry component ships its own `.kicad_mod` inside its package, and `pcb layout`
emits a reference under that component's own library nickname, so importing a
KiCad design that `pcb layout` generated leaves exactly those footprints
unresolved — everything drawn from a standard KiCad library resolves normally.
The warning reports this as a per-footprint gap rather than a missing library,
which is the correct reading.

`--archive-sources` additionally writes `<board>.kicad.archive.zip`, preserving
the referenced KiCad source files. It is off by default.

The generated layout retains the original KiCad project and board filenames. In
the default mode nothing existing is rewritten or deleted, layout files included.
`pcb.toml` and files import does not generate are never rewritten on any path.
`--force` replaces the generated output it produces on that run, which includes
`layout/`.

A standalone `.kicad_sch` import omits the `layout/` directory and KiCad project
and board files. It generates a four-layer `Board` declaration with
`layout_path = "layout"`. A later `pcb layout` command can create layout files
after every component has concrete footprint geometry. Standalone footprints
resolve in this order:

1. the sibling project `fp-lib-table`, which wins on a library-nickname conflict for
   enabled rows (a KiCad `(disabled)` project row is skipped, matching KiCad);
2. the global KiCad `fp-lib-table` under `KICAD_CONFIG_HOME` or the platform KiCad
   configuration directory, including its versioned subdirectories;
3. the bundled KiCad stdlib subset;
4. the cached KiCad library matching the schematic's major version.

KiCad itself resolves a footprint through the project table *or* the global one,
and registering a third-party library globally is the normal KiCad workflow, so a
design with an empty project table still resolves. Reading global state costs the
output nothing: import copies each resolved footprint into the generated
component package, so the machine's KiCad configuration is baked in at import
time rather than becoming an ongoing dependency of the generated board.

Import copies only referenced project, global, or cached footprint files into
generated component packages; it does not copy a complete KiCad library. Global
libraries live outside the project directory, so their files are not archived with
the project sources.
Bundled stdlib footprint IDs remain library references. If footprint geometry
cannot be resolved, import records the original KiCad ID, when present, in the
generated component and as `__imported_unresolved_footprint`. The package remains
buildable, but it is not layout-ready until the footprint is replaced.

Generated components preserve connectivity by mapping each KiCad physical pin
number to a distinct Zener logical signal through `pin_defs`. Displayed pin names
are labels, not electrical identities: pins with the same displayed name remain
separate unless the KiCad netlist places them on the same net. Separate no-connect
pins also remain separate open terminals. Connectivity comes from the KiCad
netlist's `(reference designator, physical pin number)` endpoints; schematic
coordinates affect placement comments only. Mechanical and documentation
footprints with no numbered pads are retained as pinless components.

This physical-pin generation applies to `pcb import`. The shared component
renderer used by component search and registry APIs retains its existing
signal-name-based interface.

Import treats sourcing as enrichment rather than a prerequisite for structural
conversion. When KiCad provides both an explicit MPN and manufacturer, generated
components receive `Part(...)`. When KiCad provides only an explicit MPN, import
retains it and can fill the manufacturer if all cached exact-MPN registry records
that name one agree. This metadata enrichment does not require registry-module
compatibility. A populated generated fallback without complete sourcing carries
an importer provenance marker; `pcb build` reports `bom.imported_incomplete` as
a warning rather than fabricating a part or excluding the component from the
BOM. A later agent or author can replace the fallback or add `Part(...)`.

Referenced sheets and project-local symbol and footprint assets must remain
under the schematic directory; external paths and symlinks are rejected. A
library the global `fp-lib-table` registers is external by definition, so that
containment check does not apply to it; a footprint still cannot escape the
library directory the table entry names.

If footprints do not resolve, the warning names the failing library nicknames
with their footprint and component counts, lists the four places import looked,
and states whether every referenced footprint failed (usually a library that is
not registered where import looks) or only some did (a per-footprint gap).

## Exact MPN candidate lookup

Import reads explicit MPN properties such as `MPN`,
`Manufacturer_Part_Number`, `MP`, and `SnapEDA_PN` from schematic instances and
their embedded library symbols. Instance overrides take precedence. It
normalizes each distinct MPN to uppercase alphanumeric characters and queries the indexed
`symbols.mpn_normalized` field. The lookup uses only registry indexes already on
disk and never fetches registry metadata, downloads an index, or adds a package
dependency.

For an exact MPN candidate whose package is already present under `vendor/` or
`~/.pcb/cache`, import evaluates each published entrypoint without downloading
anything. Automatic substitution requires exactly one compatible entrypoint,
exactly one physical component, no required config without a default, a matching
`Part` MPN, agreeing manufacturers, an identical footprint name, concrete
`.kicad_mod` geometry with the
same numbered-pad set, and an IO-to-pad mapping that covers the source physical
pins. Every physical pin grouped behind
one candidate IO must also have identical KiCad connectivity on every imported
instance. Import copies the selected package into an importer-owned local path,
then builds the complete board and verifies all source `(reference
designator, physical pin number)` net partitions. Candidate rejection is never an
import failure: zero compatible candidates and two or more compatible candidates
both fall back to a generated Zener component that preserves the KiCad physical
pins and connectivity. Ambiguity is reported, because which entrypoints are
cached locally varies per machine and is not something the source design states.

An exact MPN does not identify a part on its own, because manufacturers reuse
part numbers. Substitution therefore also requires manufacturer agreement: an
explicit source manufacturer must match the candidate's `Part` manufacturer, and
when the source names none, the cached exact-MPN records must name exactly one
manufacturer that the candidate does not contradict. An MPN indexed under
several manufacturers does not prove identity.

Import builds the written board and checks its complete numbered-pad set and
physical-pin partitions. A `.zen` that already existed is kept and then validated
in place under the same complete-board checks, so an authored or agent-fixed
component is verified against the KiCad source rather than trusted. An
incompatible interface, footprint, or electrical mapping fails the import.

A built physical pin that the source netlist leaves unconnected must stay alone
on its own net. Sharing a net with any other endpoint is a short that the source
design does not describe, so it fails and names the endpoints involved. This
applies to two unconnected pins tied to each other as well.

The extraction report records matching registry candidates under
`semantic.registry_mpn_lookup`, selected local entrypoints under
`generated.registry_reused_entrypoints`, and each physical component's sourcing
outcome under `generated.sourcing_by_refdes`. Outcomes distinguish source parts,
registry metadata enrichment, registry-module reuse, parametric sourcing,
intentional BOM exclusion, and incomplete sourcing. If a cached index is
unavailable or unreadable, import continues without registry candidates and
records the lookup status or error. The component `Value` field is not treated as an explicit MPN.

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

For project imports, import cannot assume that the original `.kicad_mod`
libraries are present. It therefore converts each embedded `(footprint ...)`
instance in the board into a standalone footprint file.

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
