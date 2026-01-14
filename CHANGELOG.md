# Changelog

<!--
All notable changes to this project will be documented in this file.
The format is based on Keep a Changelog (https://keepachangelog.com/en/1.1.0/),
and this project adheres to Semantic Versioning (https://semver.org/spec/v2.0.0.html).
-->

## [Unreleased]

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

[Unreleased]: https://github.com/diodeinc/pcb/compare/v0.3.24...HEAD
[0.3.24]: https://github.com/diodeinc/pcb/compare/v0.3.23...v0.3.24
[0.3.23]: https://github.com/diodeinc/pcb/compare/v0.3.22...v0.3.23
[0.3.22]: https://github.com/diodeinc/pcb/compare/v0.3.21...v0.3.22
[0.3.21]: https://github.com/diodeinc/pcb/compare/v0.3.20...v0.3.21
[0.3.20]: https://github.com/diodeinc/pcb/compare/v0.3.19...v0.3.20
[0.3.19]: https://github.com/diodeinc/pcb/compare/v0.3.18...v0.3.19
[0.3.18]: https://github.com/diodeinc/pcb/compare/v0.3.17...v0.3.18
