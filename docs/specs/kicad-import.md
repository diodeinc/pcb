# KiCad → Zener Import (Spec / Plan)

**Status:** Draft  
**Last updated:** 2026-02-02  
**Owner:** `pcb import` (CLI)

## Summary

Implement a KiCad → Zener “adoption” flow that takes an existing KiCad project and produces a Zener board package that:

- Preserves the existing KiCad **layout** (footprints + copper) as the initial source of truth.
- Generates enough Zener code (components + nets + board config) such that running `pcb layout` will **sync to the imported layout** with minimal diff.
- Establishes stable **sync hooks** so subsequent `pcb layout` runs update the KiCad layout deterministically (metadata changes OK; large geometry churn is not).

This spec is intentionally high-level; the detailed implementation past the current milestone is expected to evolve.

## Goals

- Import a KiCad project into an **existing** Zener workspace.
- Deterministically select “main” KiCad artifacts (project, schematic, board).
- Validate inputs and surface diagnostics (ERC/DRC/parity) early.
- Materialize an initial Zener board package and copy KiCad layout artifacts into the board’s `layout_path`.
- Best-effort extract and model KiCad **stackup** using stdlib `BoardConfig(stackup=...)` (skip if unsupported/exotic).
- Generate a “flat” Zener design that models:
  - All placed components (footprint + symbol references)
  - Electrical connectivity (nets/pins)
- Patch the imported KiCad PCB file so `pcb layout` can match existing footprints to generated Zener entities and avoid delete+readd churn.

## Non-goals (initially)

- Perfect semantic preservation of all KiCad project settings (UI prefs, per-user settings, etc.).
- Multi-board / multi-project import in one invocation.
- A fully faithful translation of hierarchical sheet structure into Zener module hierarchy (first version can be flat).
- Zero diff on first `pcb layout` run (metadata diffs are acceptable; geometry should be stable).

## CLI / UX

Command:

`pcb import [WORKSPACE_PATH] --kicad-project <PATH>`

Notes:

- `WORKSPACE_PATH` is optional (defaults to CWD) but must resolve to an **existing pcb workspace**.
- `--kicad-project` is required and may be a directory or a `.kicad_pro` file.

Expected behavior:

1. Discovery phase (scan for relevant KiCad files).
2. Validation phase (ERC/DRC/parity; prompt on errors).
3. Materialize phase (create board directory and copy selected layout artifacts).
4. Future phases (generate Zener code, patch layout for sync hooks, verify).

Notes:

- Generated `.zen` files should always be formatted before writing (use the built-in `pcb fmt` behavior).

## Output Layout (Target Workspace)

For an imported board named `<board>`:

- `boards/<board>/`
  - `pcb.toml`
  - `<board>.zen`
  - `layout/<board>/layout.kicad_pro`
  - `layout/<board>/layout.kicad_pcb`
  - `.kicad.validation.diagnostics.json` (captured validation diagnostics)

Future additions may include:

- Generated component libraries (e.g. `boards/<board>/components/…` or `components/kicad/<board>/…`)
- Import metadata (e.g. `.kicad.import.json` capturing source hashes, mapping summary, etc.)

## Phases and Milestones

### M0 — Discover + Validate + Materialize (Implemented)

**Discovery**

- Recursively scan the KiCad project root for relevant artifacts (`.kicad_pro`, `.kicad_sch`, `.kicad_pcb`, etc.).
- Require a single project file (`.kicad_pro`) for directory-based imports (to avoid ambiguous source-of-truth).
- Infer board name from the `.kicad_pro` stem.

**Validation**

- Ensure required artifacts exist and are selectable unambiguously:
  - `.kicad_pro`, `.kicad_sch`, `.kicad_pcb`
- Run KiCad validations via `kicad-cli`:
  - ERC on the schematic
  - DRC on the PCB
  - DRC parity (`--schematic-parity`) to detect schematic/layout mismatch
- Render diagnostics to stderr for user visibility.
- If parity fails: hard error (import must not proceed).
- If ERC/DRC contain errors: prompt the user to continue (default “No”).

**Materialize**

- Require the destination to be an existing Zener workspace.
- Create the Zener board package like `pcb new --board` (board dir, `pcb.toml`, `<board>.zen`).
- Copy:
  - selected `.kicad_pro` → `layout/<board>/layout.kicad_pro`
  - selected `.kicad_pcb` → `layout/<board>/layout.kicad_pcb`
- Best-effort parse stackup from the imported `layout.kicad_pcb` and update `<board>.zen` with:
  - `layers = <N>` (2/4/6/8/10) and
  - `config = BoardConfig(stackup = Stackup(...))`
  - If parsing fails or is unsupported/exotic: leave default board config.
- Persist validation diagnostics to:
  - `boards/<board>/.kicad.validation.diagnostics.json`

Implementation references:

- CLI + discovery/validation/materialize: `crates/pcb/src/import.rs`
- Board scaffold helper: `crates/pcb/src/new.rs` (`scaffold_board`)
- KiCad runners: `crates/pcb-kicad/src/lib.rs`, `crates/pcb-kicad/src/drc.rs`, `crates/pcb-kicad/src/erc.rs`
- Zener codegen + formatting helpers: `crates/pcb/src/codegen/`

### M1 — Extract KiCad Design Data (Planned)

Goal: create an intermediate representation (IR) of the KiCad project that can drive codegen and sync patching.

Inputs:

- Schematic (`.kicad_sch`) as the authoritative source for connectivity.
- PCB (`.kicad_pcb`) as the authoritative source for placement, copper, and existing FPIDs.

Key outputs (IR):

- `parts[]`: instances with (reference, value, footprint FPID, symbol ref, fields/properties)
- `nets[]`: net name + connected pins (ref/pin mapping)
- `footprints[]`: existing PCB footprints keyed by (reference + FPID + UUID/path info as available)
- `mapping`: cross-links between schematic symbols and PCB footprints (reference designator, UUIDs, etc.)

Open questions:

- Whether to source connectivity via:
  - parsing `.kicad_sch` directly, or
  - `kicad-cli sch export netlist`/JSON (if available/consistent), or
  - KiCad Python via `eeschema` APIs.

### M2 — Generate Zener Components (Planned)

Goal: generate Zener component instances that correspond 1:1 to KiCad components.

Strategy (high level):

- For each KiCad component instance, emit a Zener `Component(...)` instance with:
  - `name` matching the reference designator (flat path)
  - `symbol = Symbol(...)` referencing an imported/known `.kicad_sym`
  - `footprint = <FPID>` matching the KiCad footprint identifier from the PCB file
  - `properties/fields` as needed (value, manufacturer fields, DNP flags, etc.)

Library sourcing options:

- Prefer referencing existing KiCad libraries via stable aliases when possible.
- Vendor/copy any project-local symbol/footprint libraries into the Zener workspace for reproducibility.

### M3 — Generate Nets + Pin Plumbing (Planned)

Goal: generate Zener `Net(...)` objects and connect component pins.

Strategy (high level):

- Create `Net` objects (named where appropriate).
- For each component pin connection in the KiCad netlist, map it into the Zener `pins = { ... }` structure.

Notes:

- First version should be “flat”: no attempt to preserve KiCad sheet hierarchy in Zener module hierarchy.
- Power nets, no-connects, and implicit global labels need explicit handling to avoid accidental merges/splits.

### M4 — Patch Imported Layout for Sync Hooks (Planned)

Goal: modify the imported `layout.kicad_pcb` so Zener layout sync can match existing footprints instead of recreating them.

Known mechanism (from current layout sync implementation):

- Footprint identity in the sync layer is derived from:
  - a hidden custom footprint field named **`Path`**, and
  - the footprint **FPID** (library + footprint name)
- The sync layer also sets the footprint’s KiCad internal `KIID_PATH` based on a stable UUID derived from the Zener entity path.

Import-time patching should:

- Assign a deterministic Zener entity path for every footprint (flat = reference designator).
- Write that entity path into the footprint’s custom **`Path`** field.
- Ensure KiCad internal path/UUID fields align with the generated entity path (to match sync expectations).

Acceptance criteria:

- First `pcb layout` run should not delete and recreate all footprints.
- Copper geometry should not churn (tracks/zones should remain effectively identical).

### M5 — Verification + “Minimal Diff” Contract (Planned)

Goal: define and enforce what “minimal diff” means for adoption.

Suggested verification steps:

- Run `pcb layout --check` (or equivalent dry-run) after import.
- Compute a diff report between imported `layout.kicad_pcb` and post-sync `layout.kicad_pcb`.
- Gate on:
  - No footprint deletions/re-additions unless truly necessary.
  - No large coordinate/orientation shifts.
  - No track/zone deletion churn.
  - Allowable metadata changes (ordering, formatting, text variables, hidden fields).

## Operational Considerations

- **No overwrite by default:** importing into an existing board dir/layout dir should be a hard error until an explicit `--force` story exists.
- **Determinism:** output should be stable across repeated imports of the same source (modulo timestamps and KiCad version strings).
- **Captured provenance:** keep validation diagnostics and (eventually) import metadata to support debugging and re-import workflows.

## Current Status Snapshot

Implemented today (M0):

- `pcb import` command with discovery, selection, validation, prompting, and materialization.
- Copies KiCad `.kicad_pro` + `.kicad_pcb` into the deterministic Zener layout directory.
- Writes validation diagnostics JSON into the board package.
- Best-effort stackup extraction from KiCad PCB into stdlib `BoardConfig(stackup=...)`.
- Basic Zener codegen infra with “format before write” behavior.

Not implemented yet (M1+):

- Extract netlist/parts IR from the KiCad project.
- Generate Zener components + nets from the IR.
- Patch the imported `layout.kicad_pcb` with sync hook fields/paths so `pcb layout` adopts it cleanly.
- Verification tooling for the “minimal diff” contract.
