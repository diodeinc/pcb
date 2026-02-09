# KiCad → Zener Import (Spec / Plan)

**Status:** In progress  
**Last updated:** 2026-02-06  
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
- A fully faithful 1:1 translation of KiCad hierarchy into Zener module structure (import generates a sheet/module tree, but naming and boundaries may evolve).
- Zero diff on first `pcb layout` run (metadata diffs are acceptable; geometry should be stable).

## Known Caveats / Expectations

- **Netclass assignments may break** (renames + regenerated netlists can disrupt KiCad netclass membership).
- **Reference designators are not stable** and may be reassigned (this is currently accepted to keep the importer simpler).
- **Stackup may change** (best-effort extraction; unsupported/exotic stackups fall back to defaults).
- **Design rules / constraints may change** (KiCad board setup, constraints, and rule tables are not fully preserved yet).
- **3D models likely won’t be preserved** (model file references are not currently extracted/rewritten during import).

## CLI / UX

Command:

`pcb import <PATH_TO_PROJECT.kicad_pro> <OUTPUT_DIR>`

Notes:

- `PATH_TO_PROJECT.kicad_pro` is required and is treated as the KiCad project source-of-truth.
- `OUTPUT_DIR` is required; if it does not contain a V2 pcb workspace (`pcb.toml` with `[workspace]`),
  import will create a minimal workspace there.
- `--force` skips interactive confirmations and continues even if ERC/DRC errors are present.

Expected behavior:

1. Resolve paths (output workspace root + KiCad project root from `.kicad_pro`).
2. Discovery + selection (scan project for KiCad files, select `.kicad_sch/.kicad_pcb` by `.kicad_pro` stem).
3. Validation (ERC/DRC/parity; prompt on errors unless `--force`).
4. Extraction (build an in-memory IR from netlist + schematic + layout).
5. Materialize (clean-create board dir, copy selected layout artifacts, write validation diagnostics JSON).
6. Materialize also writes a portable archive of the source KiCad project (`.kicad.archive.zip`) for archival/repro.
7. Generation (components + nets + board `.zen`, pre-patch layout for sync hooks, write extraction report JSON).
8. (Future) Verification (define and gate “minimal diff” contract).

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
  - `components/<manufacturer>/<part_name>/` (preferred when KiCad provides `Manufacturer`)
    - `<part_name>.kicad_sym`
    - `<footprint_name>.kicad_mod`
    - `<part_name>.zen`
  - `components/<part_name>/` (fallback when `Manufacturer` is missing)
    - `<part_name>.kicad_sym`
    - `<footprint_name>.kicad_mod`
    - `<part_name>.zen`
  - `.kicad.validation.diagnostics.json` (captured validation diagnostics)
  - `.kicad.import.extraction.json` (captured extraction report; does not include raw symbol/footprint S-expressions)

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
  - Tolerate `layout.parity.duplicate_footprints` when the duplicated footprints are unannotated (`REF**`).
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
  - If stackup parsing fails or is unsupported/exotic: fall back to extracting copper layer count from the PCB’s `(layers ...)` section; error if neither is available.
- Persist validation diagnostics to:
  - `boards/<board>/.kicad.validation.diagnostics.json`
- Persist import extraction report to:
  - `boards/<board>/.kicad.import.extraction.json`

Implementation references:

- Phase orchestrator: `crates/pcb/src/import/mod.rs`
- Phase modules + shared types: `crates/pcb/src/import/`
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

#### Footprint De-Instancing (Required)

Import cannot assume the original `.kicad_mod` library footprint files are available at import time.
Therefore, footprint generation must treat the footprint S-expression embedded in `layout.kicad_pcb`
as the source of truth, but **normalize it back** into a canonical standalone footprint suitable for
writing to a `.kicad_mod` file.

Problem:

- A `(footprint ...)` inside `.kicad_pcb` is a *board instance*, not a library definition.
- KiCad serializes some transforms into children (notably pad angles and flipped geometry).
- Footprint-embedded keepout `zone` polygons in `.kicad_pcb` are serialized in **absolute board
  coordinates**. In `.kicad_mod` they are in **footprint-local** coordinates.

This means a naive "strip instance-only fields" transform will mangle geometry:

- Pads get the footprint placement rotation baked into per-pad `(at ... ANGLE)` and end up rotated
  in the generated `.kicad_mod`.
- Embedded footprint zones detach because their points remain absolute.

##### Canonical Model

We define a board footprint instance pose:

- translation `t = (tx, ty)` and rotation `theta` (degrees) from the footprint `(at tx ty theta)`.
- `is_back` derived from the footprint root `(layer "B.*")`.

We also define a mirror across the X axis (flip Y):

- `M(x, y) = (x, -y)`

and a rotation matrix `R(theta)`.

KiCad board-instance coordinate conventions we normalize from:

1. Most footprint-local points (pads, `fp_*` graphics, custom pad primitives):

- `.kicad_pcb` stores them in the footprint's *local* coordinates, but mirrored when on the back
  side:

  - `p_file = p_local` (front)
  - `p_file = M(p_local)` (back)

2. Footprint-embedded `zone` polygon points:

- `.kicad_pcb` stores `xy` points in **board coordinates**, with the instance pose applied:

  - `p_file = t + R(theta) * p_local` (front)
  - `p_file = t + R(theta) * M(p_local)` (back)

  Therefore the inverse mapping to recover footprint-local points is:

  - `p_local = R(-theta) * (p_file - t)` (front)
  - `p_local = M( R(-theta) * (p_file - t) )` (back)

3. Pad angles (the optional third field in pad `(at x y ANGLE)`):

- In `.kicad_pcb` pad angles are serialized as a *board-absolute* orientation that already accounts
  for footprint rotation and flipping.

  We normalize back to `.kicad_mod` pad-local angles with:

  - `a_local = a_file - theta` (front)
  - `a_local = theta - a_file` (back)

4. Footprint text angles (`fp_text ... (at x y ANGLE) ...`):

- When flipped to the back, KiCad typically also uses `justify mirror` and rotates text by 180.
  To normalize back to a front-side library footprint we use:

  - `a_local = a_file - theta` (front)
  - `a_local = theta + 180 - a_file` (back)

5. Layers and mirroring:

- When normalizing a back-side board instance to a front-side library footprint:
  - Swap all `layer`/`layers` strings `B.* <-> F.*`.
  - Remove `mirror` from `(justify ...)` (because we are converting to canonical front-side).

##### Implementation Outline

The footprint de-instancing transform should be implemented in a single place (in the S-expression
layer), and used by import codegen:

- Input: the raw board-instance `(footprint ...)` S-expression substring (as extracted by span).
- Output: a standalone `.kicad_mod` `(footprint ...)` S-expression with:
  - instance-only nodes removed: root `at`, `path`, `sheetname`, `sheetfile`, `locked`, root `uuid`,
    per-pad `net` and per-pad `uuid`.
  - geometry normalized using the canonical model above.

Recommended structure:

- Parse footprint root.
- Extract pose (`t`, `theta`, `is_back`).
- Recursively transform nodes with a small context:
  - `coord_space = Local | ZoneAbs`
  - `angle_semantics = None | PadAbs | TextAbs`
- Rewrite nodes by tag:
  - `pad`: strip instance fields and set `angle_semantics = PadAbs` for its subtree.
  - `fp_text`: set `angle_semantics = TextAbs` for its subtree.
  - `zone`: set `coord_space = ZoneAbs` for its subtree.
  - `at`: rewrite y (back) and rewrite angles based on `angle_semantics`.
  - `xy`: rewrite based on `coord_space`.
  - `layer`/`layers`: swap F/B when `is_back`.
  - `justify`: drop `mirror` when `is_back`.

##### Regression Tests

Add `pcb-sexpr` unit tests that exercise the transform without requiring external KiCad files:

- Front-side pad angle: footprint `(at ... 90)`, pad `(at ... 180)` -> emits pad local `(at ... 90)`.
- Back-side flip: footprint layer `B.Cu`, pad y-sign and angle normalization matches expected.
- Zone points: a zone point equal to `t + R(theta) * M(p_local)` is transformed back to `p_local`.
- The per-part key is derived from schematic/layout data with best-effort heuristics:
  - `MPN` (preferred) + footprint FPID + lib_id + value (fallbacks)
  - This is deterministic and collision-safe (suffixes may be applied).

How we handle “a component referenced multiple times”:

- Multiple KiCad instances (different UUID path keys / refdes) can map to the same per-part key.
- Import does not duplicate EDA artifacts in that case; it writes one component package and reuses it.

**Module loads in the board (implemented):**

- The imported board `.zen` `load()`s each generated per-part component module via:
  - `<SCREAMING_SNAKE> = Module("components/<manufacturer>/<part_name>/<part_name>.zen")` (preferred when KiCad provides `Manufacturer`)
  - `<SCREAMING_SNAKE> = Module("components/<part_name>/<part_name>.zen")` (fallback when `Manufacturer` is missing)
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
  - KiCad `"unconnected-(...)"` nets that connect to exactly one logical port are rendered as `NotConnected()` at the callsite (no `Net(...)` declaration is emitted for them).
  - Other `"unconnected-(...)"` nets (if they ever occur) are treated as real nets and preserved.
- If an IO’s pin numbers connect to **multiple different** real KiCad nets, import chooses a deterministic net for now (and logs a debug message). This should be revisited once we decide how to represent “same IO name, different nets” in Zener.
- Instance flags:
  - Propagate KiCad schematic instance flags into the generated module invocation:
    - `dnp = True` when KiCad marks the symbol instance as DNP
    - `skip_bom` / `skip_pos` when KiCad marks the instance as not-in-BOM / not-on-board
  - These are SOURCE-authoritative in the layout lens, and are used to carry KiCad population/BOM/POS intent through the Zener-generated netlist.
  - To keep the board `.zen` concise, import may omit `skip_bom` / `skip_pos` kwargs on instances when they match the per-part module defaults.
- Determinism:
  - Emit instances sorted by refdes.
  - Emit IO args in a stable order (sorted by IO name).
  - Filesystem paths are allocated deterministically and avoid case-insensitive collisions by suffixing directory names with `_2`, `_3`, etc. when needed (casing is preserved otherwise).

Notes:

- Import generates a schematic sheet/module tree (non-root sheets) and instantiates it from the root board file.
- Power nets, no-connects, and implicit global labels need explicit handling to avoid accidental merges/splits.

### M4 — Patch Imported Layout for Sync Hooks (Implemented)

Goal: modify the imported `layout.kicad_pcb` so Zener layout sync can match existing footprints instead of recreating them.

Known mechanism (from current layout sync implementation):

- Footprint identity in the sync layer is derived from:
  - a hidden custom footprint field named **`Path`**, and
  - the footprint **FPID** (library + footprint name)
- The sync layer also sets the footprint’s KiCad internal `KIID_PATH` based on a stable UUID derived from the Zener entity path.

Import-time patching (implemented) does:

- For each imported footprint that is keyed and referenced by the netlist (i.e. managed by Zener import):
  - Assign a deterministic Zener entity path: `<REFDES>.<PART_NAME>` (flat board).
  - Ensure the footprint has a hidden `(property "Path" "<REFDES>.<PART_NAME>")` entry.
  - Ensure the footprint’s KiCad internal `(path "...")` is set to:
    - `"/<uuid>/<uuid>"` where `uuid = uuid5(NAMESPACE_URL, "<REFDES>.<PART_NAME>")`
    - This matches the sync lens expectations and avoids `layout.sync.unmanaged_footprint`.
- Rename net names in the imported layout to match the sanitized Zener net names used in `<board>.zen`:
  - Patch KiCad `(net ...)` declarations and `(zone (net_name ...))` strings using the existing net rename patcher.
  - Net **variables** use fully-sanitized SCREAMING_SNAKE identifiers.
  - Net **names** in `Net("...")` are kept close to KiCad and only minimally sanitized (e.g. `.` → `_`).

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

### M6 — Detect Power/Ground Nets (Planned)

Goal: classify imported KiCad nets as `Power` / `Ground` (or fall back to `Net`) so the generated
Zener board + module `.zen` files use stdlib `Power(...)` / `Ground(...)` instead of plain
`Net(...)` where we have **high confidence**.

#### Canonical model (initial)

- `NetKind = Net | Power | Ground`
- Default: `Net`
- Only emit `Power` / `Ground` when classification is derived from **explicit schematic intent**.

#### Option B (preferred): schematic-declared rails, netlist connectivity

We use the netlist as the source of truth for **connectivity**, but do not parse wires/junctions
ourselves.

Key observation:

- A KiCad schematic `symbol` instance marked `(power)` implicitly connects to a **global net**
  named by its `Value` property (e.g. `"+1V8"`, `"GND"`).
- The KiCad netlist export includes the resulting net connectivity under that same net name, but
  typically does **not** include the power symbol itself as a component/node.

Therefore:

1. Extract all `(power)` symbol instances from all `.kicad_sch` files and record their declared net
   name from `property "Value"`.
2. Determine whether each `(power)` symbol represents `Ground` vs `Power` from the *symbol identity*
   (e.g. `lib_id`/symbol name), not from arbitrary net-name heuristics.
3. Join to the netlist by exact net name match:
   - if `declared_value == netlist_net.name`, classify that net as `Power` or `Ground`.
4. Use the netlist for all wiring; `NetKind` only changes how we *declare* the net in Zener.

This yields a simple, robust pipeline:

- no wire graph parsing
- no “guessing” from net names alone
- connectivity remains entirely netlist-driven

#### Confidence / conflict rules (high confidence only)

- Classify a net as `Ground` only when we see at least one `(power)` symbol whose symbol identity
  clearly indicates ground (e.g. symbol name contains `GND`/`GROUND`/`EARTH` or matches a curated
  allowlist of ground symbol ids).
- Classify a net as `Power` only when we see at least one `(power)` symbol that is not classified
  as `Ground`.
- If the same net name is declared by both `Power` and `Ground` symbols (conflict), fall back to
  `Net` and record a debug reason.
- If a `(power)` symbol is missing a `Value` property, ignore it (and record a debug reason).

These rules intentionally err on the side of under-classifying rather than producing incorrect
typed nets.

#### Output / codegen behavior

- Board `.zen` and module `.zen` net declarations:
  - `VCC = Power("VCC")` for `NetKind::Power`
  - `GND = Ground("GND")` for `NetKind::Ground`
  - `FOO = Net("FOO")` for `NetKind::Net`
- Net IO typing (where we use `io("NAME", ...)`):
  - Use `Power`/`Ground` as the type when `NetKind` is known.
- This applies in both:
  - the root board file
  - generated schematic sheet modules

#### Phases to complete M6

1. **Extraction**
   - Parse all `.kicad_sch` files already discovered for the import.
   - Collect a list of `power_symbol_decls` with:
     - `sheetpath` (for debugging)
     - `lib_id` (symbol identity)
     - `value` (declared global net name)
   - Persist these declarations into the import extraction report for inspection.
2. **Semantic analysis**
   - Compute `BTreeMap<KiCadNetName, NetKind>` using:
     - netlist net names (join key)
     - `power_symbol_decls` (intent)
   - Persist per-net `NetKind` plus debug reasons (e.g. “declared by power symbol X”, “conflict”).
3. **Codegen**
   - When generating net declarations in board/modules, select `Power(...)` / `Ground(...)` when
     `NetKind` indicates it.
   - Ensure required stdlib loads are present (e.g. `load("@stdlib/interfaces.zen", "Power", "Ground")`).
4. **Validation / evaluation**
   - Add a debug print / JSON report summary showing:
     - number of nets classified as `Power`/`Ground`
     - any conflicts / ignored declarations
   - Verify on a few real designs that typed nets match the user’s expectations.

## Incremental Execution Plan (Hierarchical Sheet → Modules)

This is a staged refactor of the “flat board file” generation into a hierarchical module tree that mirrors the KiCad schematic sheet structure.

1. **Extract + persist schematic sheet tree (IR)**
   - Extract sheet-instance UUID chains and build a sheet tree keyed by `sheetpath.tstamps`.
   - Extract and persist subschematic instance names (`Sheetname`) and referenced schematic file paths (`Sheetfile`).
   - Write this structure into the import extraction report for debugging.

2. **Build a hierarchy plan (no codegen changes yet)**
   - Derive per-net “owner” sheet via LCA (lowest common ancestor) of connected ports’ sheet paths.
   - For each sheet/module, classify nets into:
     - `nets_defined_here`: owner == this sheet
     - `nets_io_here`: net is used in subtree, owner is an ancestor (net crosses boundary)
   - This makes internal-vs-external net decisions deterministic and simple.

3. **Generate leaf modules first**
   - Generate modules for sheets with components and no child sheets with components.
   - Root board file may remain flat initially but loads + instantiates leaf modules to validate wiring rules.

4. **Generate full module tree (bottom-up) (Implemented)**
   - Generate all non-root sheet modules in postorder (leaves → root-children).
   - Make the root board `.zen` act as the root schematic module, instantiating only its direct child sheet modules and wiring nets.

## Operational Considerations

- **Clean import:** if the destination board directory already exists, `pcb import` performs a clean import by deleting and recreating it.
- **Determinism:** output should be stable across repeated imports of the same source (modulo timestamps and KiCad version strings).
- **Captured provenance:** keep validation diagnostics and (eventually) import metadata to support debugging and re-import workflows.

## Current Status Snapshot

Implemented (phased importer):

- `pcb import` is structured as explicit top-level phases (paths → discover → validate → extract → materialize → generate → report).
- Copies KiCad `.kicad_pro` + `.kicad_pcb` into the deterministic Zener layout directory.
- Writes validation diagnostics JSON into the board package.
- Best-effort stackup extraction from KiCad PCB into stdlib `BoardConfig(stackup=...)`.
- Extracts netlist components + net connectivity from KiCad netlist export (keyed by KiCad UUID path).
- Extracts schematic symbol instance metadata (including multi-unit + mirror axis) and embedded
  `lib_symbols`.
- Emits KiCad mirror state into `# pcb:sch` comments as `mirror=x|y` when present on a placed
  symbol instance. KiCad’s `(mirror x|y)` names the axis being mirrored across, and the
  netlist/viewer `mirror=x|y` semantics follow the same convention.
- Extracts a schematic sheet-instance tree (sheet UUID paths + subschematic names + referenced `.kicad_sch` files) and persists it in the extraction report.
- Derives a hierarchy plan from net connectivity:
  - net owner sheet = LCA of connected ports’ sheet paths
  - per-sheet sets for `nets_defined_here` and `nets_io_here` (boundary nets)
- Generates a full schematic sheet module tree under `boards/<board>/modules/<module>/<module>.zen` (non-root sheets only) and instantiates it from the root board file (root instantiates direct child sheet modules; modules instantiate their children).
  - Module directory names are derived from the KiCad `Sheetname` (sanitized for filesystem usage) and are only suffixed when needed to avoid collisions.
- Extracts keyed PCB footprint data (including pads + exact footprint S-expression slice) and joins it to netlist components.
- Persists a selective import extraction report (no raw symbol/footprint S-exprs) to `boards/<board>/.kicad.import.extraction.json`.

Implemented (M2 scaffolding):

- Generates per-part component packages under `boards/<board>/components/`:
  - writes `.kicad_sym` + transformed standalone `.kicad_mod` + auto-generated `<part>.zen`
- Board `.zen` declares nets and loads the generated component modules.

Implemented (M3):

- Board `.zen` instantiates per-refdes components (module invocations) and wires IOs to nets using netlist connectivity.
- Pins with no connectivity should not occur in a KiCad netlist export; if it does, treat it as an import bug.

Implemented (M4):

- Pre-patches the imported `layout.kicad_pcb` so `pcb layout` can adopt it without footprint churn:
  - ensures a hidden `Path` property per managed footprint and a deterministic KiCad internal `(path ...)` value derived from it
  - renames KiCad net names in the layout to the sanitized Zener net names used by the generated board file

Not implemented yet (next):

- Verification tooling for the “minimal diff” contract.
- Power/ground net classification and codegen (`Power(...)` / `Ground(...)`).

## Schematic Placement Mapping (KiCad → `# pcb:sch`) (Implemented)

Goal: emit `# pcb:sch ...` comments such that the editor’s schematic renderer places symbols as
close as possible (visually) to KiCad’s schematic placement for the same schematic.

This section documents the **theoretical mapping** between KiCad’s persisted schematic placement
and the editor’s persisted `# pcb:sch` placement. The intent is that most of the importer can work
in “KiCad semantics”, and then apply a single well-defined conversion right before writing
`# pcb:sch` comments.

### Notation

- All distances are in **mm** unless otherwise stated.
- KiCad schematic sheet coordinates are **Y-down**.
- Symbol-local coordinates (from KiCad `lib_symbols` / `.kicad_sym`) are **Y-up**.
- `R(θ)` is a CCW rotation matrix about the origin.
- `M_X` mirrors across the X axis (flips Y); `M_Y` mirrors across the Y axis (flips X).

We will use **column-vector notation**:

```
p' = A · p + t
```

### KiCad: persisted → rendered

For a placed symbol instance, KiCad persists:

- `t_k = (x_k, y_k)` in sheet coordinates (mm, Y-down)
- `θ_k` (degrees)
- optional `(mirror a)` where `a ∈ {x, y}` names the axis being mirrored across

For transformation math, it is convenient to work in a Y-up world coordinate system. Convert the
persisted translation to world Y-up via:

```
T_k = ( X_k, Y_k ) = ( x_k, -y_k )
```

KiCad’s forward transform order is:

1. rotate
2. mirror

So the rendered world position of a symbol-local point `q` (symbol-local, Y-up) is:

```
p_k = T_k + M(a_k) · ( R(θ_k) · q )
```

This is “rotate then mirror about the symbol origin, then translate to `t_k`”.

### Editor: persisted `# pcb:sch` → rendered

The editor persists `# pcb:sch`:

- `x_p, y_p` in **0.1mm units** in a stored coordinate system (**Y-down**)
- `rot_p` (degrees, **clockwise-positive**)
- optional `mirror_p ∈ {X, Y}` using the same axis semantics as KiCad:
  - `X` mirrors across X axis (flips Y)
  - `Y` mirrors across Y axis (flips X)

For computation, it is convenient to convert into mm (still Y-down):

```
x_s = 0.1 · x_p
y_s = 0.1 · y_p
```

On load, the editor converts stored `(x_s, y_s)` into a symbol-origin world translation using a
**constant per-symbol offset** derived from the symbol’s **untransformed** local bbox.

Let the symbol-local bbox be:

```
B_local = [min_x, max_x] × [min_y, max_y]
```

Define the “origin offset”:

```
o = (-min_x, max_y)
```

Then the editor reconstructs the symbol-origin translation in world coordinates (mm, Y-up) as:

```
T_e = ( X_e, Y_e ) = ( x_s + o_x,  -y_s - o_y )
θ_e = -rot_p
mirror_e = mirror_p
```

Runtime rendering then applies (about the symbol origin):

```
p_e = T_e + M(mirror_e) · ( R(θ_e) · q )
```

This matches KiCad’s rotate-then-mirror order, but the editor’s persistence anchor is **not**
the origin; it’s a stored anchor converted to the origin using `o`.

### Required mapping for visual parity

#### Non-promoted (same symbol geometry)

To make the editor render the same symbol origin as KiCad, we require (in world Y-up):

```
T_e.x = x_k
T_e.y = -y_k
```

Substituting `T_e = (x_s + o_x, -y_s - o_y)` and `o = (-min_x, max_y)` yields the conversion from
KiCad’s persisted origin to the editor’s stored anchor:

```
x_s = x_k + min_x
y_s = y_k - max_y
```

Rotation is handled by matching the editor’s load step `θ_e = -rot_p` and the desire that
`θ_e` matches KiCad’s `θ_k` (in the editor’s world CCW convention):

```
rot_p = -θ_k
```

Mirror uses the same axis semantics in both systems (axis being mirrored across), so:

```
mirror_p = mirror_k
```

Finally, serialize `(x_s, y_s)` back into the on-disk `# pcb:sch` units:

```
x_p = 10 · x_s
y_p = 10 · y_s
```

#### Promoted passives (symbol substitution)

When we *substitute* the KiCad symbol with a different editor-rendered symbol family (e.g.
promoting `customCapacitors0402:*` into stdlib `Capacitor` rendered as `Device:C`), we cannot
simultaneously preserve:

- the KiCad instance **origin** `(x_k, y_k)` (because the origin’s meaning is symbol-definition
  dependent), and
- the **visual placement** (what the user sees on the sheet).

For these promotions, the intent is visual parity. We therefore align the **visual AABB top-left**
of the substituted symbol to the KiCad symbol’s visual AABB top-left.

Let:

- `B_s` be the source symbol’s local bbox (from `lib_symbols`)
- `B_t` be the target symbol’s local bbox (e.g. `Device:C`)
- `L_s = M(m_k) · R(θ_k)` be the source linear transform (rotate then mirror, Y-up)
- `L_t = M(m_p) · R(θ_e)` be the target linear transform in the editor world (Y-up)
- `TL(AABB(...))` extract the “top-left” of an axis-aligned bbox in Y-up, i.e. `(min_x, max_y)`

Compute the source top-left in world Y-up:

```
TL_s = TL( AABB( T_k + L_s · corners(B_s) ) )
```

Compute the target top-left **relative to the origin**:

```
TL_t_rel = TL( AABB( L_t · corners(B_t) ) )
```

Then choose the substituted symbol origin `T_e` such that:

```
TL( AABB( T_e + L_t · corners(B_t) ) ) = TL_s
⇒ T_e = TL_s - TL_t_rel
```

Finally convert that target origin back into the editor’s stored anchor using the editor’s
untransformed-bbox offset (same as the non-promoted mapping, but using `B_t` and the *computed*
target origin).

### Critical detail: which bbox to use

The conversion needs both source and target geometry when symbol substitution occurs:

- For normal (non-promoted) components: use the bounds of the extracted `lib_symbols` entry for
  the instance’s `lib_id` and unit.
- For **promoted passives** (KiCad symbol replaced with stdlib `Resistor`/`Capacitor`):
  - compute the desired substituted **origin** by aligning the **source** vs **target** transformed
    bbox top-left (visual AABB), then
  - convert that origin into the editor’s stored anchor using the **target** bbox `(min_x, max_y)`.

In addition, the editor expands symbol bounds by a small constant margin; import-time bbox
extraction should match that expansion to avoid systematic translation drift.

### Implementation guidance: make transforms first-class

While the `# pcb:sch` schema stores separate `x/y/rot/mirror` fields, the underlying placement is
an affine transform. To make the math explicit and avoid sign/order mistakes, prefer an internal
representation such as:

- `Vec2` for translations
- `Mat2` (or `Linear2`) for 2×2 linear transforms
- `Affine2 { A: Mat2, t: Vec2 }` for full placement transforms

Even if you ultimately emit `rot_p` + `mirror_p`, representing the linear part as `A = M · R`
(rotate then mirror, about origin) makes composition, inversion, and parity checks easy and
testable.

## Passive Promotion (Resistors/Capacitors) (Implemented)

Goal: opportunistically replace certain imported KiCad components with stdlib generic passives
(`@stdlib/generics/Resistor.zen`, `@stdlib/generics/Capacitor.zen`) instead of generating a
per-part component package.

This is intentionally **conservative**:

1. **Semantic analysis (detection)** classifies which KiCad instances are “safe” resistor/capacitor
   candidates based on multiple independent signals.
2. **Codegen substitution** replaces those instances with stdlib generics and avoids generating
   per-part component packages for them.

### Architectural placement

Add a semantic-analysis phase between extraction and codegen:

`discover → validate → extract → hierarchy → semantic → materialize → generate → report`

The semantic phase produces a deterministic, serializable analysis object that is persisted into the
import extraction report (for iteration/debugging).

### Detection scope (initial)

Only classify **2-pad** passives on the PCB:

- Candidate resistor: exactly 2 pads + strong evidence of “resistor”
- Candidate capacitor: exactly 2 pads + strong evidence of “capacitor”
- Everything else: unknown/other (no promotion)

We intentionally ignore arrays/networks, feedthrough parts, jumpers, etc. for the first cut.

In addition, for stdlib substitution we require:

- A recognized package size: `01005` / `0201` / `0402` / `0603` / `0805` / `1206` / `1210`
- A confidently parsed value (resistance/capacitance)
- If package or value is ambiguous/missing, we do **not** attempt promotion.
- `skip_bom` is supported and will be emitted when needed.
- `skip_pos` is intentionally **not** emitted for promoted passives (even if present in the source),
  because the stdlib generics do not currently expose a `skip_pos` config knob.

### Detection signals (robust, not overfit)

We derive independent signals from the extracted KiCad artifacts:

- **Refdes prefix** (weak-to-medium): `R…` → resistor, `C…` → capacitor.
- **Schematic lib_id** (strong): if the symbol library/name clearly encodes `R`/`C` or contains
  “resistor”/“capacitor” (case-insensitive).
  - Examples: `Device:R`, `Device:C`, `customResistors0402:R_10k_0402`,
    `customCapacitors0402:C_100n_0402`.
- **Footprint FPID** (strong): if the footprint library/name clearly encodes resistor/capacitor.
  - Examples: `Resistor_SMD:R_0402_1005Metric`, `Capacitor_SMD:C_0402_1005Metric`,
    `custom-footprints:R_0402_1005Metric`, `custom-footprints:C_0402_1005Metric`.
- **Value hint** (strong): if the KiCad value/property clearly encodes a resistance/capacitance
  (including common project naming schemes like `R_10k_0402` / `C_100n_0402`).

Classification uses a scoring model with contradiction handling:

- Require pad_count == 2
- Aggregate scores for resistor vs capacitor from the signals above
- Classify only when the top score exceeds a threshold and the score margin is clear
- Attach a confidence level (`high`/`medium`/`low`) plus an evidence list for debugging

We also opportunistically extract passive attributes for later codegen:

- `package` (`01005`..`1210` only; larger sizes are not captured)
- `parsed_value` (normalized for `Resistance("...")` / `Capacitance("...")`)
- Optional sourcing hints when present: `manufacturer`, `mpn`
- Optional properties when present: `tolerance`, `voltage`, `dielectric`, `power`
  - Note: stdlib `Resistor.zen` / `Capacitor.zen` currently accept `mpn` + `manufacturer`, and
    `Capacitor.zen` also accepts `voltage` + `dielectric`. We do **not** currently emit
    `tolerance` / `power` because the stdlib generics reject unknown kwargs.

### Codegen behavior (current)

For each **high confidence** promoted passive instance:

- Do **not** generate a `boards/<board>/components/...` package.
- Load the stdlib module in the relevant `.zen` file:
  - `Resistor = Module("@stdlib/generics/Resistor.zen")`
  - `Capacitor = Module("@stdlib/generics/Capacitor.zen")`
- Instantiate it and wire `P1`/`P2` using netlist connectivity, passing the extracted config args:
  - Always: `value`, `package`
  - Optional: `mpn`, `manufacturer`
  - Capacitors only (optional): `voltage`, `dielectric`
- For layout sync hooks, the imported KiCad footprint `Path` property uses the stdlib component name:
  - Resistors: `<refdes>.R`
  - Capacitors: `<refdes>.C`

### Output

Persist per-component classification keyed by `KiCadUuidPathKey` into the extraction report:

- `kind`: `resistor` | `capacitor` | null
- `confidence`: `high` | `medium` | `low` | null
- `signals`: list of evidence strings (e.g. `refdes_prefix:R`, `footprint_name:R_0402...`)
- `pad_count`

This is used for evaluation on real projects before any codegen substitution is attempted.
