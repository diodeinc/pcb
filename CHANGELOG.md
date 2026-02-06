# Changelog

<!--
All notable changes to this project will be documented in this file.
The format is based on Keep a Changelog (https://keepachangelog.com/en/1.1.0/),
and this project adheres to Semantic Versioning (https://semver.org/spec/v2.0.0.html).
-->

## [Unreleased]

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

[Unreleased]: https://github.com/diodeinc/pcb/compare/v0.3.36...HEAD
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
