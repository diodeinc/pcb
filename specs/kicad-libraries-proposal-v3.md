# KiCad-Style Libraries Decoupling (V3)

## Status

- Draft
- Last updated: 2026-02-25

## Problem

The toolchain is currently overfit to one hardcoded KiCad library bundle (`kicad-symbols`, `kicad-footprints`, `kicad-packages3D`) and special-case behavior tied to those specific repos.

We want to decouple this into a generic model for **kicad-style libraries**:

- symbols live in one repo/tree
- footprints live in another repo/tree
- models live in a third repo/tree
- symbols reference footprints via `<fp_lib>:<fp_name>`
- footprints reference models via text vars (for example `${KICAD9_3DMODEL_DIR}`)

## Core principles

1. Dependencies remain dependencies.
- `[dependencies]` represents what `.zen` directly references.
- No special dependency semantics for kicad-style repos.

2. Kicad library config is a resolution hint, not a dependency list.
- `[[workspace.kicad_library]]` describes symbol/footprint/model relationships.
- Sensible defaults are provided (KiCad 9 official libraries) so workspaces work out of the box.
- Auto-deps uses kicad_library config to resolve `@kicad-*` aliases to concrete dependencies.

3. Remove hardcoded KiCad coupling.
- No hardcoded alias behavior for `@kicad-*`.
- No hardcoded toolchain pin field like `[workspace].kicad9-libraries-version`.

4. Keep existing global resolver invariants.
- Dependencies without `pcb.toml` are not included in `pcb.sum`.
- Vendoring continues to apply only to deps with `pcb.toml`.

## Configuration model

`kicad_library` is workspace-root only and lives under workspace config as an array-of-tables:

```toml
[workspace]
pcb-version = "0.3"

[[workspace.kicad_library]]
version = "9.0.3"
symbols = "gitlab.com/kicad/libraries/kicad-symbols"
footprints = "gitlab.com/kicad/libraries/kicad-footprints"
models = { KICAD9_3DMODEL_DIR = "gitlab.com/kicad/libraries/kicad-packages3D" }
```

Multiple entries are allowed. Defaults are provided for KiCad 9 official libraries.

## Version matching model

`workspace.kicad_library[].version` is a concrete semver version (e.g. `"9.0.3"`).

Matching rule:
- The major component is used for selector matching: a dependency version `9.0.3` matches an entry with `version = "9.0.3"` because both have major `9`.
- Resolution uses normal dependency/MVS-selected versions.
- `kicad_library` entries are selected by major version compatibility, not exact patch match.

## Dependency semantics

Example dependencies remain normal:

```toml
[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
```

Alias behavior:
- No kicad-specific alias special casing.
- Existing generic dependency auto-aliasing applies (last path segment).
- So `@kicad-symbols` / `@kicad-footprints` work naturally when those deps exist.

Auto-deps behavior:
- Same as all other dependencies.
- No kicad-specific auto-dep path, no peeking into symbol files to add footprint deps.

## Kicad-style resolution behavior

`[[workspace.kicad_library]]` tells the toolchain how to interpret declared deps:

1. Symbol lookup
- `Symbol(library="@.../*.kicad_sym", ...)` resolves through declared symbol dependency as normal.

2. Footprint linkage from symbol `<fp_lib>:<fp_name>`
- Toolchain uses matching `workspace.kicad_library` entry to map symbol-space to footprint-space.
- If linkage target is not represented by the matched `kicad_library` config, this is a hard error.

3. Model linkage
- `models` map provides text-variable name -> model repo relationship.
- This drives project/layout model path resolution behavior.

## Error policy

Hard errors:
- Symbol/footprint linkage cannot be resolved through matched `workspace.kicad_library` entry.
- Ambiguous or incompatible `workspace.kicad_library` match for selected dependency versions.
- Missing workspace-root config when kicad-style linkage is required.

Auto-deps integration:
- `@kicad-*` aliases are resolved to concrete dependencies via `[[workspace.kicad_library]]` config.
- No implicit dependency insertion from linkage traversal beyond what auto-deps provides.

## Lockfile and vendoring

1. Lockfile (`pcb.sum`)
- Unchanged invariant: dependencies without `pcb.toml` are not tracked.
- Therefore kicad-style repos are not lockfile entries.

2. Vendoring / publish / wasm
- Unchanged invariant: vendoring applies only to dependencies with `pcb.toml`.
- Kicad-style repos (and any non-`pcb.toml` deps) are not vendored.

## Removal from current model

V3 removes the old hardcoded path:

- remove `[workspace].kicad9-libraries-version`
- remove hardcoded toolchain-managed kicad repo constants/flows as product behavior
- replace with generic `[[workspace.kicad_library]]`-driven interpretation

No compatibility shim is planned.

## Phased implementation plan

### Phase 1: Config + validation

Scope:
- Add `[[workspace.kicad_library]]` schema.
- Enforce workspace-root-only placement.
- Add version parsing/validation (concrete semver, major used for matching).
- Remove `kicad9-libraries-version` field and related validation.

Checks:
- Automated tests for parsing/validation/root-only behavior.
- Manual: load representative manifests with single and multiple entries.

### Phase 2: Resolver decoupling

Scope:
- Remove hardcoded kicad alias/repo coupling in resolution logic.
- Route symbol->footprint->model interpretation through matched `workspace.kicad_library` entries.
- Keep dependency/MVS behavior unchanged.

Checks:
- Automated tests for successful linkage and expected hard-error paths.
- Manual: run `pcb build`/`pcb layout` on a workspace using kicad-style config.

### Phase 3: Auto-deps + lockfile invariants

Scope:
- Ensure auto-deps stays generic (no kicad-specific special casing).
- Ensure lockfile behavior remains generic and excludes non-`pcb.toml` deps.

Checks:
- Automated tests for auto-deps behavior and lockfile output.
- Manual: run repeated builds and verify stable `pcb.sum` semantics.

### Phase 4: Publish/docs cleanup

Scope:
- Ensure publish/vendor behavior follows generic rules (no kicad-specific branch).
- Update docs/spec references from hardcoded kicad model to generic kicad-style model.
- Delete dead code from old hardcoded implementation.

Checks:
- Automated release/publish tests.
- Manual end-to-end: build/layout/publish on a kicad-style workspace.

## Success criteria

- Kicad-style behavior is driven by `[[workspace.kicad_library]]`, not hardcoded repo names.
- `.zen` dependency semantics remain normal and explicit.
- No kicad-specific special-case paths in auto-deps/lockfile/vendoring.
- Significant code deletion from old hardcoded KiCad flows.

## Implementation notes

### Active test repos

- `/Users/akhilles/src/diode/stdlib`
- `/Users/akhilles/src/diode/registry`
- `/Users/akhilles/src/dioderobot/demo`

`demo` is the primary board-level validation target. During rollout, run:

- `pcb build ...`
- `pcb layout ... --no-open`

### Progress log

- 2026-02-25: Added `[[workspace.kicad_library]]` config schema + validation plumbing.
- 2026-02-25: Removed hardcoded `KICAD_ALIASES` special-casing paths.
- 2026-02-25: Added selector-aware kicad-style resolver matching and treated non-`pcb.toml` kicad-style repos as leaf deps (no manifest placeholder path).
- 2026-02-25: Replaced hardcoded symbol `<lib>:<fp>` fallback mapping in component footprint inference with selector-matched `[[workspace.kicad_library]]` symbol->footprints mapping.
- 2026-02-25: Deleted obsolete `libfp_requires_resolved_footprints_root` snapshot test tied to old hardcoded fallback behavior.
- 2026-02-25: Replaced hardcoded `.kicad_pro` `KICAD9_3DMODEL_DIR` patch input with generic model text-variable mapping derived from `[[workspace.kicad_library]].models` and resolved package versions.
- 2026-02-25: Kept kicad-style repos out of closure/lockfile entirely (no cache-hash/lockfile entries for non-`pcb.toml` deps).
- 2026-02-25: Added workspace-root resolution fallback for alias/dependency matching during eval so dependency modules can resolve workspace-level declared deps.
- 2026-02-25: Removed obsolete integration snapshots relying on implicit KiCad alias behavior and updated netlist fixtures to explicit kicad-library/dependency declarations.
- 2026-02-25: Cleaned up/deduplicated resolver wave fetch path (`validate_kicad_selector_match` + `Option<PackageManifest>`) to avoid duplicate selector checks.
- 2026-02-25: Updated docs (`spec`, `packages`, tutorial wording) to remove `kicad9-libraries-version` and old “toolchain-managed KiCad dependency” guidance.
