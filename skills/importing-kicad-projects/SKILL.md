---
name: importing-kicad-projects
description: Imports KiCad projects and schematics into the current repository. Use when asked to import or convert an existing KiCad design.
---

# Importing KiCad Projects

## Import

Preserve the original source and import into the current repository root:

```sh
pcb import /path/to/design.kicad_pro .
```

Use `.kicad_pro` for a complete project, or the root `.kicad_sch` for a schematic-only request. Keep child sheets and local libraries together; do not silently drop the PCB or reconstruct the design. In an archive, skip `__MACOSX/`, `<project>-backups/` and `.history/`: they hold metadata and older copies of the project.

Inspect existing content before using `--force`. Use it for an untouched starter board or an already-authorized replacement; otherwise ask before overwriting user work. Checkpoint affected files first: a failed import can leave partial output. Without a source `.kicad_pcb`, import keeps the board's existing layout project and rejects one with a different name, so remove an untouched starter's layout project first.

`--skip-drc` skips the import's DRC and schematic/PCB parity checks, which can take most of a large board's import; run DRC afterwards with `pcb layout --check <board>.zen`.

## Verify and report

```sh
pcb build <board>.zen -S bom.unspecified -S bom.underspecified
```

Read the import diagnostics and, on success, `.kicad.import.extraction.json`. Inspect schematic and PCB previews for import losses. Source ERC/DRC and schematic/PCB parity findings are advisory; do not bypass generated connectivity mismatches or present partial output as a completed import.

Report what imported, what is missing, and any blockers. Missing sourcing data does not block structural verification; complete BOM sourcing or manufacturing checks when requested. Import success does not establish manufacturing readiness.
