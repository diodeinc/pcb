# Changelog

<!--
All notable changes to this project will be documented in this file.
The format is based on Keep a Changelog (https://keepachangelog.com/en/1.1.0/),
and this project adheres to Semantic Versioning (https://semver.org/spec/v2.0.0.html).
-->

## [Unreleased]

### Added

- Added MCP tool `resolve_datasheet` to convert `datasheet_url`, `pdf_path`, or `kicad_sym_path` (+ optional `symbol_name`) into cached local `datasheet.md` and `images/`.

## [0.3.44] - 2026-02-20

### Added

- `Component()` now infers missing `footprint` from symbol `Footprint` (`<stem>` or KiCad `<lib>:<fp>`), reducing duplicated footprint data over `.kicad_sym`.

### Changed

- MCP external tool discovery now prefers `mcp --code-mode=false` (raw tools) and falls back to `mcp` only when needed, avoiding nested code-mode wrappers for compatible `pcb-*` backends.

### Fixed

- Reduced `layout.sync` false positives in publish/check flows by normalizing `.kicad_pro` newline writes and ignoring trailing whitespace-only drift when comparing synced layout files.
- Simplified dependency fetch/index concurrency paths and reuse a shared cache index during resolve/fetch phases to reduce open-file pressure on macOS.
- Auto-deps is now conservative and online-only: it adds remote deps only after successful materialization, skips imports already covered by existing `dependencies`/`assets`, and no longer infers missing deps from `pcb.sum`.
- Branch-based dependencies now require commit pinning for reproducibility: online resolve/update pins `branch` deps to `rev`, while `--locked`/`--offline` reject branch-only declarations.
- Fixed dotted pin-name handling by resolving port owners with longest-prefix component matching in netlist/layout/publish flows.

## [0.3.43] - 2026-02-18

### Added

- `pcb import` now imports KiCad design rules (including solder-mask/zone defaults), copies sibling `.kicad_dru`, and prints the generated board `.zen` path.
- `pcb fmt` now formats KiCad S-expression files when given explicit file paths.

### Changed

- Bump stdlib to 0.5.9
- `pcb layout --check` now runs layout sync against a shadow copy.
- Removed `--sync-board-config`; board config sync is now always enabled for layout sync (CLI, MCP `run_layout`, and `pcb_layout::process_layout`).
- Stackup/layers patching in `pcb layout` now uses structural S-expression mutation + canonical KiCad-style formatting, with unconditional patch/write.
- `pcb layout` stackup sync now also patches `general (thickness ...)` from computed stackup thickness.
- Removed MCP resource `zener-docs` (https://docs.pcb.new/llms.txt) from `pcb mcp`, with Zener docs now embedded in `pcb doc`.
- Move board-config/title-block patching to Rust; simplify Python sync; only update `.kicad_pro` netclass patterns when assignments exist.
- `pcb search` now formats generated component `.kicad_sym` and `.kicad_mod` files with the KiCad S-expression formatter.
- `pcb search` now rewrites imported symbol `property "Footprint"` to the local `lib:footprint` form (`<stem>:<stem>`), matching fp-lib-table resolution during layout sync.
- `pcb search` now fails fast unless imported `.kicad_sym` contains exactly one symbol.

### Fixed

- Standardized KiCad unnamed-pin handling: empty/placeholder names now fall back to pin numbers in both import and runtime symbol loading, fixing `Unknown pin name` errors for imported components.
- KiCad symbol variant parsing now selects one style per unit using named-pin coverage (tie: lowest style index), avoiding pin-name overrides while supporting `_N_0` symbols.

## [0.3.42] - 2026-02-13

### Changed

- `config()` physical-value coercion now accepts numeric scalars (`int`/`float`) in addition to strings, matching constructor behavior.
- `config()` now enforces required module inputs: `optional=False` emits an error diagnostic even when `default` is set; omitted `optional` infers from `default`.
- Bump stdlib to 0.5.8

### Fixed

- Fix `package://` resolution for workspace and versioned dependencies, preventing absolute path leakage from `File()`.

## [0.3.41] - 2026-02-12

### Fixed

- Harden `pcb import` passive value parsing (e.g. `1 uF`, `2,2uF`, `1uF/16V`, `10 kΩ`, `R10`) so generic R/C auto-promotion is applied consistently.

## [0.3.40] - 2026-02-12

### Added

- `load()` and `Module()` with relative paths can now cross package boundaries within a workspace, resolved through the dependency system.

### Fixed

- `pcb publish` now works when run from a board directory with a relative `.zen` path (e.g., `pcb publish DM0002.zen`).

### Changed

- Resolve `Path()` and `File()` to stable relative paths for machine-independent build artifacts. 
- Bump stdlib to 0.5.7
- `pcb import` now scaffolds a full workspace (git init, README, .gitignore) when the output directory is new, matching `pcb new --workspace`.

## [0.3.39] - 2026-02-11

### Fixed

- Layout discovery now uses only `.kicad_pro` files, ignoring extra `.kicad_pcb` files in the layout directory.

## [0.3.38] - 2026-02-11

### Added

- `pcb release` now includes `drc.json` in the release archive containing the full KiCad DRC report.
- `pcb import <project.kicad_pro> <output_dir>` to generate a Zener board from a KiCad project.

### Changed

- KiCad layout discovery no longer assumes `layout.kicad_pcb`; it now discovers a single top-level `.kicad_pro` (preferred) or `.kicad_pcb` in the layout directory and errors on ambiguity.

## [0.3.37] - 2026-02-09

### Added

- Reference designator assignment now opportunistically honors unambiguous hierarchical path hints (e.g. `foo.R22.part`).
- `pcb:sch` comments now support optional `mirror=x|y`, and netlist `instances.*.symbol_positions.*` now serializes `mirror` when set.

### Fixed

- Improved LSP file change syncing to prevent spurious diagnostics.

## [0.3.36] - 2026-02-06

### Fixed

- On layout sync, detach all items from removed KiCad groups before deleting the group to avoid `SaveBoard` crashes from stale group handles.

## [0.3.35] - 2026-02-06

### Added

- `pcb new --board` now generates `README.md` and `CHANGELOG.md` files from templates
- `pcb new --package` now generates `README.md` and `CHANGELOG.md` files from templates

### Changed

- Bump stdlib to 0.5.6
- `pcb update <path>` limits updates to a single workspace package when `<path>` points to that package directory.
- Reference designator auto-assignment now uses natural sorting of hierarchical instance names (e.g., `R2` before `R10`).
- Drop support for v1 workspaces

### Fixed

- Stabilize auto-named single-port `NotConnected()` net names (e.g., `NC_R1_P2`) to reduce layout implicit renames.
- Layout sync: explode single-pin multi-pad `NotConnected` nets into per-pad `unconnected-(...)` nets.
- Accept KiCad copper role `jumper` when importing stackups.
- IPC-2581 rev B: parse `FunctionMode` `level` attribute as numeric.
- Restoring missing KiCad groups no longer triggers fragment placement that can move existing footprints.

## [0.3.34] - 2026-02-03

### Added

- `pcb preview <path/to/board.zen>` to generate a preview link for a release.

### Changed

- Board release gerber exports now use Gerber X2 format.
- Board release drill exports now generate separate PTH/NPTH Excellon files and both PDF + GerberX2 drill maps.

### Fixed

- Restore `NotConnected` compatibility: keep normal connectivity (no per-pad net exploding), warn when it connects multiple pins, and only mark pads `no_connect` for single-pin cases.

## [0.3.33] - 2026-02-03

### Changed

- `PhysicalValue` now formats symmetric tolerances as `10k 5%` (instead of `min–max (nominal nom.)`).

## [0.3.32] - 2026-02-02

### Changed

- Unify physical value and range types (e.g. VoltageRange is just an alias to Voltage)
- Deduplicate pin names when generating component .zen file

## [0.3.31] - 2026-02-01

### Added

- `config()` now auto-converts strings to PhysicalValue/PhysicalRange types (e.g., `voltage = "3.3V"`)

### Fixed

- `io()` default values now correctly apply net type promotion (e.g., `default=NotConnected()` promotes to the expected net type)

## [0.3.30] - 2026-02-01

### Added

- Warning for unnamed nets that fall back to auto-assigned `N{id}` names
- NotConnected nets now preserve their type in schematics and can be passed to any net type parameter
- Layout sync now handles NotConnected pads correctly

### Changed

- Bump stdlib to 0.5.4

## [0.3.29] - 2026-01-26

### Changed

- `pcb publish` no longer fails on warnings in non-interactive mode (CI)

### Fixed

- `pcb publish` now correctly handles workspaces with nested packages

## [0.3.28] - 2026-01-26

### Added

- `pcb publish <path/to/board.zen>` to publish a board release

### Removed

- `pcb tag` and `pcb release` are no longer supported. Use `pcb publish <path/to/board.zen>` instead.

### Changed

- Bump stdlib to 0.5.3

## [0.3.27] - 2026-01-23

### Added

- Post-sync detection of stale `moved()` paths that weren't renamed

### Changed

- Bump stdlib to 0.5.2
- Deterministic diagnostic ordering during parallel module evaluation
- `moved()` directives are now skipped if the target path already exists in the layout
- `moved()` now requires at least one path to be a direct child (depth 1)
- `pcb publish` now uses single confirmation prompt instead of two
- `pcb release` now works for boards without a layout directory
- `pcb layout` now auto-detects implicit net renames and patches zones/vias before sync

### Removed

- Remove `board_config.json` generation from `pcb release`

### Fixed

- Validate that member packages do not have `[workspace]` sections during workspace discovery
- `pcb new --board` and `pcb new --package` no longer generate `[workspace]` sections in pcb.toml
- `pcb update` now correctly respects interactive selection for breaking changes
- `pcb release` now correctly identifies the board package when workspace root has dependencies
- `copy_dir_all` now skips hidden files/directories to prevent copying `.pcb/`, `.git/`, etc.

## [0.3.26] - 2026-01-20

### Changed

- Bump stdlib to 0.5.1
- Standardize CLI: `build`/`test`/`fmt` take optional `[PATH]`, `layout`/`bom`/`sim`/`open`/`route`/`release` require `<FILE>`

## [0.3.25] - 2026-01-19

### Added

- Add `pcb mcp eval` to execute JavaScript with MCP tools (also exposed as `execute_tools` MCP tool)
- Add `pcb run add-skill` to install the pcb skill into any git repository
- Add V2 dependency resolution support to `pcb sim` (adds `--offline` and `--locked` flags)
- Add `pcb search --mode` to specify starting mode (`registry:modules`, `registry:components`, `web:components`)
- Add availability, pricing, offers to `pcb search`, `pcb bom -f json`, MCP tools

### Changed

- `pcb layout` now displays sync diagnostics (orphaned zones/vias, moved path warnings) even without `--check`
- Zones/vias referencing deleted nets are now unassigned instead of heuristically reassigned; use `moved()` for intentional net renames
- Change `pcb search --json` to `pcb search -f json` for consistency with other commands
- Rename `pcb search` TUI modes: `registry` → `registry:modules`/`registry:components`, `new` → `web:components`

### Removed

- Remove `--add` flag from `pcb search`
- Remove unused `pcb bom --rules` flag

### Fixed

- Fix `pcb layout` group splitting regression where running layout twice would cause module groups to split into two (one with footprints, one with tracks/zones)
- Fix race condition when populating dependency cache

## [0.3.24] - 2026-01-14

### Added

- `pcb release` now generates a canonical `netlist.json` in the release staging directory

## [0.3.23] - 2026-01-13

### Added

- Add `pcb doc --changelog` to view embedded release notes
- Add `pcb doc --package <url>` for viewing docs of a Zener package
- Add `pcb doc --package <pkg> --list` to list .zen files in a package as a tree
- Add subpath filtering for `pcb doc --package` (e.g., `@stdlib/generics` filters to generics/)

### Changed

- Bump stdlib to 0.4.10
- MCP `search_registry` tool now returns workspace-relative cache paths when run inside a workspace

### Removed

- Remove stdlib hijacking from evaluator. The toolchain now relies on the pinned stdlib version instead of replacing types at runtime.

### Fixed

- Fix repeated gitignore parsing when walking multiple directories

## [0.3.22] - 2026-01-13

### Added

- Add `pcb new --workspace <name> --repo <url>` to create a new workspace
- Add `pcb new --board <name>` to create a new board in an existing workspace
- Add `pcb new --package <path>` to create a new package (e.g., `modules/my_module`)
- Add `pcb new --component` to search and add a new component via the TUI
- Add `pcb doc` command for viewing embedded Zener documentation with fuzzy search
- Add HTML export to `pcb ipc2581` command
- Add surface finish detection and color swatches to `pcb ipc2581 info` and HTML export
- Include IPC-2581 HTML export as release artifact at `manufacturing/ipc2581.html`

### Changed

- Refactor layout sync to use a groups registry (virtual DOM pattern) as source of truth instead of querying KiCad directly

### Removed

- Remove `get_zener_docs` MCP tool (use `pcb doc` CLI command instead)
- Remove `pcb search --legacy` flag and the old interactive API search. Use the default TUI-based registry search instead.
- Remove `pcb clean` command. To recover from cache issues, manually delete files in `~/.pcb`.
- Remove `fab_drawing.html` from release artifacts (replaced by IPC-2581 HTML export)
- Remove `docs/` directory from release staging output

### Fixed

- Fix `pcb layout` crash due to stale SWIG wrappers after removing empty groups
- Fix intermittent "No such file or directory" errors during package fetch caused by race conditions between concurrent `pcb` processes

## [0.3.21] - 2026-01-10

### Added

- Add v2 dependency resolution support to `pcb test`

### Changed

- Use source layout directly in release instead of separate copy
- Change extra footprint sync diagnostic from error to warning
- Show module path and FPID in layout sync diagnostics (extra_footprint, missing_footprint, fpid_mismatch)
- `pcb layout` now auto-replaces footprints when FPID changes (preserving position and nets)
- Speed up workspace discovery by pruning unrelated directories

### Fixed

- Fix `pcb layout --check` only reporting first extra footprint instead of all
- Fix inconsistent handling of invalid pcb.toml files between `pcb build` and `pcb publish`
- Fix fp-lib-table in release staging to use vendored paths instead of .pcb/cache
- Create `<workspace>/.pcb/cache` symlink pointing to `~/.pcb/cache` for stable paths

## [0.3.20] - 2026-01-09

### Added

- Add `pcb route` command for auto-routing using DeepPCB
- Detect footprint sync issues (FPID mismatch, missing/extra components) during layout

### Changed

- Skip version prompt for unpublished packages in `pcb publish` (always 0.1.0)
- Error on path dependencies that point to workspace members
- Error on pcb.toml parse failures

### Fixed

- Fix asset resolution to check vendor directory before cache
- Fix inconsistent vendoring with folder assets and subfiles
- Fix TUI package details not loading after fresh index download

## [0.3.19] - 2026-01-06

### Added

- Add `pcb fork` subcommands (`add`, `remove`, `upstream`) for local package forking
- Add TUI mode to `pcb search` for browsing registry packages

### Changed

- Bump stdlib to 0.4.9

## [0.3.18] - 2026-01-01

### Added

- Support `schematic="embed"/"collapse"` as a top-level kwarg
- Add `dirty` status to `pcb info -f json` output
- Warn on duplicate module name

### Changed

- Error on invalid type passed to `io()`
- Format the auto-generated component .zen files

[Unreleased]: https://github.com/diodeinc/pcb/compare/v0.3.44...HEAD
[0.3.44]: https://github.com/diodeinc/pcb/compare/v0.3.43...v0.3.44
[0.3.43]: https://github.com/diodeinc/pcb/compare/v0.3.42...v0.3.43
[0.3.42]: https://github.com/diodeinc/pcb/compare/v0.3.41...v0.3.42
[0.3.41]: https://github.com/diodeinc/pcb/compare/v0.3.40...v0.3.41
[0.3.40]: https://github.com/diodeinc/pcb/compare/v0.3.39...v0.3.40
[0.3.39]: https://github.com/diodeinc/pcb/compare/v0.3.38...v0.3.39
[0.3.38]: https://github.com/diodeinc/pcb/compare/v0.3.37...v0.3.38
[0.3.37]: https://github.com/diodeinc/pcb/compare/v0.3.36...v0.3.37
[0.3.36]: https://github.com/diodeinc/pcb/compare/v0.3.35...v0.3.36
[0.3.35]: https://github.com/diodeinc/pcb/compare/v0.3.34...v0.3.35
[0.3.34]: https://github.com/diodeinc/pcb/compare/v0.3.33...v0.3.34
[0.3.33]: https://github.com/diodeinc/pcb/compare/v0.3.32...v0.3.33
[0.3.32]: https://github.com/diodeinc/pcb/compare/v0.3.31...v0.3.32
[0.3.31]: https://github.com/diodeinc/pcb/compare/v0.3.30...v0.3.31
[0.3.30]: https://github.com/diodeinc/pcb/compare/v0.3.29...v0.3.30
[0.3.29]: https://github.com/diodeinc/pcb/compare/v0.3.28...v0.3.29
[0.3.28]: https://github.com/diodeinc/pcb/compare/v0.3.27...v0.3.28
[0.3.27]: https://github.com/diodeinc/pcb/compare/v0.3.26...v0.3.27
[0.3.26]: https://github.com/diodeinc/pcb/compare/v0.3.25...v0.3.26
[0.3.25]: https://github.com/diodeinc/pcb/compare/v0.3.24...v0.3.25
[0.3.24]: https://github.com/diodeinc/pcb/compare/v0.3.23...v0.3.24
[0.3.23]: https://github.com/diodeinc/pcb/compare/v0.3.22...v0.3.23
[0.3.22]: https://github.com/diodeinc/pcb/compare/v0.3.21...v0.3.22
[0.3.21]: https://github.com/diodeinc/pcb/compare/v0.3.20...v0.3.21
[0.3.20]: https://github.com/diodeinc/pcb/compare/v0.3.19...v0.3.20
[0.3.19]: https://github.com/diodeinc/pcb/compare/v0.3.18...v0.3.19
[0.3.18]: https://github.com/diodeinc/pcb/compare/v0.3.17...v0.3.18
