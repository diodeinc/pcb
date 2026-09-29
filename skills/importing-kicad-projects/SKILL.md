---
name: importing-kicad-projects
description: Imports KiCad projects and schematics into the current repository. Use when asked to import or convert an existing KiCad design.
---

# Importing KiCad Projects

## Import

Run from the current repository root and import into `.` so the user can see the generated files. Preserve the source and existing work; do not create a separate repository or import outside the checkout.

```sh
pcb import /path/to/design.kicad_pro .
```

Use `.kicad_pro` for the complete project, including PCB, rules, and source archive. For schematic-only import, pass the root `.kicad_sch`; this creates a minimal persistent KiCad project but no PCB or archive. Keep child sheets and local libraries together. Do not silently fall back to schematic-only import.

Ask before using `--force`: it replaces generated content. Standalone reimport preserves a matching PCB's geometry, routing, stackup, and project configuration while updating identity/net bindings.

## Inspect

Read `.kicad.import.extraction.json` and `.kicad.validation.diagnostics.json` for generated paths and findings.

Import preserves native hierarchy, symbols, wiring, no-connects, and graphics while binding component and net identities. Do not redesign or reconstruct the schematic.

Source ERC/DRC and schematic/PCB parity findings are advisory; generated pin/connectivity mismatches block import. Connectivity follows the schematic, without repairing existing PCB routing. Resolve missing footprints before calling the board layout-ready.

The shared engine interprets buses. Reused managed sheets, cross-sheet multi-unit components, and per-unit display overrides remain unsupported; report these rather than silently changing the design.

## Verify

From the current repository root:

```sh
pcb build <board>.zen
pcb apply schematic --no-open <board>.zen
pcb apply schematic --no-open <board>.zen
```

Complete missing BOM data or use `-S bom.unspecified -S bom.underspecified` for structural checks. Never suppress electrical diagnostics.

The first apply may normalize document structure. Inspect its diff; the second must report `schematic unchanged` with no file churn. Inspect rendered output for lost symbols, no-connects, placement/routing, or changed connectivity.

For an existing PCB, `pcb apply layout --check --no-open <board>.zen` checks layout and DRC without modifying it. Full-project `pcb apply --check --no-open <board>.zen` **writes before checking**; checkpoint files before using it. `--no-open` is not a dry run.

Report artifacts, checks, unresolved findings, and blockers. Successful import alone does not establish manufacturing readiness.
