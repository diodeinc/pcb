# KiCad → Zener Import (Spec / Plan)

**Status:** Draft  
**Last updated:** 2026-02-04  
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
- Establish a stable identifier strategy that can join data across KiCad schematic / netlist / PCB.
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
- `--force` skips interactive confirmations and continues even if ERC/DRC errors are present.

Expected behavior:

1. Discovery phase (scan for relevant KiCad files).
2. Validation phase (ERC/DRC/parity; prompt on errors).
3. Materialize phase (create board directory and copy selected layout artifacts).
4. Future phases (generate Zener code, patch layout for sync hooks, verify).

Notes:

- Generated `.zen` files should always be formatted before writing (use the built-in `pcb fmt` behavior).

## Identifier Strategy (KiCad UUID Path)

KiCad provides a stable, cross-artifact identifier for component instances that we can use as the *primary key* for import.

We key component instances by:

- `sheetpath.tstamps`: a `/.../`-delimited chain of sheet UUIDs (root sheet is `/`)
- `symbol_uuid`: the component’s symbol UUID

These appear in different places depending on the artifact:

- **Netlist** (`kicad-cli sch export netlist --format kicadsexpr`):
  - `sheetpath (tstamps "<sheet_uuid_chain>/")`
  - `tstamps "<symbol_uuid>"`
- **PCB** (`.kicad_pcb` footprints):
  - `path "/<sheet_uuid_chain>/<symbol_uuid>"`
- **Schematics** (`.kicad_sch`):
  - root schematic contains sheet objects with `uuid "<sheet_uuid>"`
  - subsheet symbol objects contain `uuid "<symbol_uuid>"`
  - subsheet symbol instances contain `instances.project.path "/<root_sch_uuid>/<sheet_uuid_chain>"` (note: includes root schematic UUID; PCB path does not)

Normalization:

- Treat missing/empty sheetpath as `/`.
- Ensure `sheetpath.tstamps` is normalized to start and end with `/`.
- Derive the PCB footprint path key as:
  - if `sheetpath == "/"`: `"/<symbol_uuid>"`
  - else: `"<sheetpath><symbol_uuid>"` (concatenation; `sheetpath` already ends with `/`)

This identifier strategy is preferred over reference designators. Refdes are useful labels but are not stable enough for import joins and sync.

## Output Layout (Target Workspace)

For an imported board named `<board>`:

- `boards/<board>/`
  - `pcb.toml`
  - `<board>.zen`
  - `layout/<board>/layout.kicad_pro`
  - `layout/<board>/layout.kicad_pcb`
  - `components/<part_name>/`
    - `<part_name>.kicad_sym`
    - `<footprint_name>.kicad_mod`
    - `<part_name>.zen`
  - `.kicad.validation.diagnostics.json` (captured validation diagnostics)

Future additions may include:

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
- If parity has blocking issues: hard error (import must not proceed).
  - Tolerate `layout.parity.extra_footprint` (layout has extra footprints not represented in the schematic).
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

### M1 — Extract KiCad Design Data (Implemented: Netlist + Schematic + Layout)

Goal: create an intermediate representation (IR) of the KiCad project that can drive codegen and sync patching.

Inputs:

- **Netlist** (`kicad-cli sch export netlist --format kicadsexpr`) as the authoritative source for:
  - component identities (stable join key)
  - net connectivity
- **Schematics** (`.kicad_sch`) as enrichment (embedded symbols + placed symbol instance metadata).
- **PCB** (`.kicad_pcb`) as enrichment (placement/FPID/pads) and the preserved layout source artifact.

Key outputs (IR / extraction JSON):

- `netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData>`
  - `netlist`: refdes/value/footprint + multi-unit `unit_pcb_paths`
  - `schematic`: per-unit placed symbol metadata keyed by unit `KiCadUuidPathKey` (lib_id/unit/flags/at/mirror/instance path/properties/pins)
  - `layout`: keyed footprint data (only footprints with `(path "...")`)
- `netlist_nets: BTreeMap<KiCadNetName, ImportNetData>`
  - Each net contains a set of `ports`, where a port is `(component: KiCadUuidPathKey, pin: KiCadPinNumber)`.

Notes:

- **Multi-unit support:** netlist components may contain multiple UUIDs (one per unit). The PCB footprint `(path ...)` uses a single unit UUID as the **anchor**. Import joins schematic + layout into the component using that anchor key, and retains all unit keys for future reconciliation.
- **Layout extraction is keyed-only:** footprints without `(path "...")` are ignored (unkeyed footprints are intentionally not tracked).
- **Captured footprint S-expression:** for each keyed footprint, import captures the exact `(footprint ...)` substring using the parser span (byte offsets) so we can later reason about or patch the footprint deterministically.

Open questions:

- Whether to source connectivity via:
  - parsing `.kicad_sch` directly, or
  - `kicad-cli sch export netlist`/JSON (if available/consistent), or
  - KiCad Python via `eeschema` APIs.
  - (Current approach: KiCad S-expression netlist is sufficient and stable for connectivity/identity.)

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

Current approach (import-time scaffolding; implemented):

- Import generates per-**part** (deduped) component packages under:
  - `boards/<board>/components/<part_name>/`
    - `<part_name>.kicad_sym`
    - `<footprint_name>.kicad_mod`
    - `<part_name>.zen` (auto-generated module from the EDA artifacts)
- The per-part key is derived from schematic/layout data with best-effort heuristics:
  - `MPN` (preferred) + footprint FPID + lib_id + value (fallbacks)
  - This is deterministic and collision-safe (suffixes may be applied).

How we handle “a component referenced multiple times”:

- Multiple KiCad instances (different UUID path keys / refdes) can map to the same per-part key.
- Import does not duplicate EDA artifacts in that case; it writes one component package and reuses it.

**Module loads in the board (implemented):**

- The imported board `.zen` `load()`s each generated per-part component module via:
  - `<SCREAMING_SNAKE> = Module("components/<part_name>/<part_name>.zen")`
- The module identifier is derived deterministically from the component directory name and is only prefixed with `_` when needed (e.g. starts with a digit).

### M3 — Instantiate Components + Wire Nets (Implemented)

Goal: in the imported board `.zen`, instantiate a Zener module for each **board instance** (refdes) and plumb nets into the component IOs using KiCad netlist connectivity.

High level strategy:

- Connectivity source-of-truth: `netlist_nets` from KiCad netlist export:
  - `KiCadNetName -> { (component: KiCadUuidPathKey, pin: KiCadPinNumber), ... }`
- For each imported (on-PCB) KiCad component instance (anchor `KiCadUuidPathKey`):
  - Find its per-part module (`<SCREAMING_SNAKE> = Module("components/...")`) from the part dedup mapping.
  - Emit a module invocation with `name="<REFDES>"` plus keyword args for each IO:
    - `MODULE_IDENT(name="U8", EN=USB_DEBUG_DP_USBC3_VBUS, GND=GND, ...)`
- Module IO signature source-of-truth: the generated per-part `.kicad_sym`:
  - Pin group (IO) name is derived from the symbol pin signal name using the same sanitization rules as component generation.
  - A pin group may include multiple pin numbers (e.g. multiple `GND` pins).
- Wiring rules:
  - Map IO → KiCad pin number(s) via the symbol definition.
  - Map `(anchor_uuid_path, pin_number)` → KiCad net name via the netlist.
  - Map KiCad net name → board net variable name via the import’s net declaration table.
  - If an IO has no connected net, allocate a deterministic `UNCONNECTED_<REFDES>_<IO>` net and use it for that IO.
  - KiCad `"unconnected-..."` nets are treated as unconnected (they do not count as a “real” connection).
  - If an IO’s pin numbers connect to **multiple different** real KiCad nets, import chooses a deterministic net for now (and logs a debug message). This should be revisited once we decide how to represent “same IO name, different nets” in Zener.
- Determinism:
  - Emit instances sorted by refdes.
  - Emit IO args in a stable order (sorted by IO name).

Notes:

- First version is “flat”: no attempt to preserve KiCad sheet hierarchy in Zener module hierarchy.
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

- **Clean import:** if the destination board directory already exists, `pcb import` performs a clean import by deleting and recreating it.
- **Determinism:** output should be stable across repeated imports of the same source (modulo timestamps and KiCad version strings).
- **Captured provenance:** keep validation diagnostics and (eventually) import metadata to support debugging and re-import workflows.

## Current Status Snapshot

Implemented (M0–M1):

- `pcb import` command with discovery, selection, validation, prompting, and materialization.
- Copies KiCad `.kicad_pro` + `.kicad_pcb` into the deterministic Zener layout directory.
- Writes validation diagnostics JSON into the board package.
- Best-effort stackup extraction from KiCad PCB into stdlib `BoardConfig(stackup=...)`.
- Extracts netlist components + net connectivity from KiCad netlist export (keyed by KiCad UUID path).
- Extracts schematic symbol instance metadata (including multi-unit) and embedded `lib_symbols`.
- Extracts keyed PCB footprint data (including pads + exact footprint S-expression slice) and joins it to netlist components.
- Basic Zener codegen infra with “format before write” behavior (used for board scaffold and stackup edits).

Implemented (M2 scaffolding):

- Generates per-part component packages under `boards/<board>/components/`:
  - writes `.kicad_sym` + transformed standalone `.kicad_mod` + auto-generated `<part>.zen`
- Board `.zen` declares nets and loads the generated component modules.

Implemented (M3):

- Board `.zen` instantiates per-refdes components (module invocations) and wires IOs to nets using netlist connectivity.
- Pins with no connectivity are wired to generated `UNCONNECTED_<REFDES>_<IO>` nets.

Not implemented yet (next):

- Patch the imported `layout.kicad_pcb` with sync hook fields/paths so `pcb layout` adopts it cleanly.
- Verification tooling for the “minimal diff” contract.
