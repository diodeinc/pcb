# Changelog

<!--
All notable changes to this project will be documented in this file.
The format is based on Keep a Changelog (https://keepachangelog.com/en/1.1.0/),
and this project adheres to Semantic Versioning (https://semver.org/spec/v2.0.0.html).
-->

## [Unreleased]

## [0.4.7] - 2026-07-10

### Added

- LSP position saves accept a `baseHash` and return a text edit; evaluation results carry a `contentHash`.

### Changed

- Physical quantity types now compose with `*` and `/` from builtin fundamentals, replacing `builtin.physical_value()`.
- Physical dimensions now use five SI bases while preserving existing electrical units.

## [0.4.6] - 2026-07-08

### Changed

- `pcb scan` now uses the datasheet cache API and skips re-uploading PDFs the backend already has.
- `pcb new component` resolves symbol datasheet URLs through the datasheet cache API and keeps the URL in the symbol instead of vendoring a local PDF copy.

## [0.4.5] - 2026-07-07

### Added

- Added `pcb gerber normalize` to re-emit a Gerber layer through the pcb-ir pipeline.
- Added `pcb ipc2581 dfm` to flag features and gaps narrower than a manufacturing minimum.

### Changed

- Gerber import preserves standard-aperture flashes.
- Board array profile Gerbers emit arcs instead of tessellated segments.

### Fixed

- `pcb sync` no longer downgrades workspace dependency pins when local git tags are stale.
- Gerber regions no longer connect holes with board-length cut-in slivers.
- `pcb open` now accepts sandbox file URIs in the new `/fs/read?path=...` form emitted by Diode Registry and uses the current streamed sandbox exec API.
- Reference designators derived from instance-name hints now honor 4-digit (1000-series) numbers such as `R1000`, `R1500`, and `LED1001` instead of silently auto-renumbering them.
- `pcb doc @stdlib/...` now works from inside any workspace.

## [0.4.4] - 2026-07-03

### Added

- Added `pcb sync --check` to fail CI when hydrated `pcb.toml` manifests or vendored package versions are out of sync. The check covers the whole workspace regardless of the current directory.
- Added `DIODE_API_AUTH=none` to send Diode API requests without attaching client auth, for proxy-injected authentication.

### Changed

- `pcb sync` now applies manifest and vendor updates only after the whole workspace resolves; a resolution failure no longer leaves partially hydrated manifests.
- `pcb sync` and `pcb vendor` no longer create an empty `vendor/` directory when no packages match the vendor patterns.

### Removed

- Removed `pcb sync --offline`; use `pcb build --offline` with a synced manifest for offline reproducibility.

## [0.4.3] - 2026-06-29

### Added

- IPC-2581 mutations now create a history record when the source file does not already have one, including board array creation.
- IPC-2581 board arrays now include panelization metadata.

### Fixed

- Fixed a `pcb build` performance regression in large workspaces with many packages.
- IPC-2581 to Gerber export now emits compound region holes as local Gerber cut-ins.

## [0.4.2] - 2026-06-26

### Added

- Added IPC-2581 board array creation, viewing, and export support to `pcb ipc`.
- Added `pcb rectify check` and `pcb rectify fix`, backed by a bundled `pcb-rectify` sidecar binary, to check and patch KiCad footprint 3D model `(rotate ...)`/`(offset ...)` transforms by matching tessellated STEP geometry against the footprint's pads and holes.

### Fixed

- `pcb` toolchain resolution now only considers fully-published releases. Releases are gated on a `pcb/index/<version>` completion marker written as the final step of publishing, so the shim never resolves or downloads a release whose artifacts are still uploading.
- `NotConnected` is now an open-net constructor, not a net type.

## [0.4.1] - 2026-06-22

### Fixed

- Net kind merging now promotes empty or `NotConnected` placeholders to observed concrete kinds.
- Stdlib TVS matching now includes the `SP3022-01ETG-NM` SOD-882 peak pulse power rating.
- `pcb build` now rejects relative imports into undeclared nested workspace packages and asks users to run `pcb sync`.

## [0.4.0] - 2026-06-17

### Fixed

- Net aliases used inside `io()` interface templates no longer unregister the original net name.
- Child net-symbol position overrides (`# pcb:sch <child>.<NET>.<idx>`) are no longer dropped when the net is renamed across the module boundary.
- Package content hashes ignore generated `pcb.sum` files.
- Read/evaluation paths now require hydrated manifests and never mutate dependency state.
- `pcb info` reports cache dependencies using stable `.pcb/cache` paths.
- Frozen eval root detection uses resolved package roots.
- Restored stdlib `Crystal` support for the `2012_2Pin` package with its bundled KiCad footprint.

### Changes

- Stdlib LEDs now use color-specific KiCad small filled symbols for single-color LEDs.
- Stdlib generic passives, diodes, crystals, ferrite beads, and test points now use KiCad small schematic symbols by default.
- Removed legacy v1/`pcb.sum` resolution, disabled `pcb update`, and dropped obsolete `--locked` read-command flags.
- `pcb migrate` removes deprecated `[workspace].members`; other commands reject it.
- `pcb sync` no longer writes stdlib-only KiCad dependency entries.
- `pcb migrate` now upgrades workspace `pcb-version` after successful latest-toolchain migrations.
- Added `[workspace.bom] strict = true` to require exact MPN matching when fetching BOM availability.
- `pcb publish` release metadata now records strict workspace BOM matching when enabled.
- Bundled selected KiCad library assets into stdlib, including embedded footprint STEP models; `@kicad-symbols` and `@kicad-footprints` now resolve to those bundled assets.
- Legacy `gitlab.com/kicad/libraries/*` manifest dependencies are ignored during resolution because stdlib carries the referenced KiCad assets.
- Updated stdlib generics to use KiCad 10.0.3 symbols and footprints, while keeping `Crystal()` compatible with KiCad 9 four-pin symbols.
- `pcb publish` now bundles only referenced KiCad split-symbol files instead of whole split-library directories.
- Added `pcb +local ...` to run the local toolchain installed by `install.sh --local`.
- `pcb +nightly ...` now caches nightly release metadata for 30 minutes.
- Removed legacy manifest and import support: `[module]`, `[packages]`, `[assets]`, and `[workspace].resolver` are rejected; legacy stdlib load paths are no longer accepted; `pcb migrate` no longer runs V1 codemods.
- Removed deprecated stdlib files and modules, including `config.zen`, `metadata.zen`, `pins.zen`, `kicad/*`, and the generic BJT, diode, MOSFET, standoff, and terminal-block modules.
- Removed deprecated stdlib API shims, including `Properties()`, `Schematics()`, `config_unit()`, `config_properties()`, `*Range` unit aliases, legacy `NetTie` inputs, and legacy resistor, capacitor, ferrite, and inductor inputs.
- Removed deprecated language shims, including `config(convert=...)`, bare `add_property(...)`, `NET=` net casts, module DNP properties, automatic component property key capitalization, legacy `Component(properties={...})` sourcing/DNP keys, and legacy net moved aliases.
- Regular nets now require explicit or assignment-inferred unique names; unnamed or duplicate regular nets fail evaluation.
- `NotConnected` nets are now source-unnamed; explicit names are ignored with a warning and downstream tools assign connection-derived names as needed.
- Added KiCad 10 jumper-pin support.

## [0.3.93] - 2026-06-12

### Changed

- Remote sandbox sync now uses octet-stream writes, refreshes auth during sessions, and keeps recoverable local KiCad session files when sync exits unexpectedly.

## [0.3.92] - 2026-06-09

### Changed

- `pcb publish` package mode now includes board manifests.

## [0.3.91] - 2026-06-08

### Changed

- Added a `No match (unknown part)` type to the `pcb bom` availability legend and summary.

## [0.3.90] - 2026-06-04

### Added

- Added ESR-aware stdlib crystal matching, 24 MHz house parts, and lower-ESR ECS 2520 options.

### Fixed

- `pcb publish` now rehydrates V2 package dependency manifests when publishing dependents of newly tagged workspace packages.

## [0.3.89] - 2026-06-03

### Added

- Render IPC-2581 artwork layers in stackup order, plus non-stackup layer SVGs, in the HTML export through the shared IR geometry pipeline.

### Fixed

- Reduced cache-index SQLite connection churn during `pcb sync` to avoid intermittent crashes.
- Fixed the Nix flake build and exposed both `pcb` and `pcbc`.

## [0.3.88] - 2026-06-02

### Added

- `pcb build` now accepts multiple explicit `.zen` file paths from the same workspace.
- Added `pcb list -m -u` and `pcb list -m -versions` for read-only V2 dependency update discovery.

### Fixed

- `pcb new board` and fresh `pcb import` output no longer create obsolete `pcb.sum` files.

## [0.3.87] - 2026-05-29

### Changed

- Batched IPC/Gerber polygon boolean operations for much faster real-board geometry processing.

### Fixed

- `pcb open` now ignores diagnostics when evaluation still produces usable output.
- `pcb build` now syncs workspace-vendored dependencies before evaluating hydrated MVS v2 projects.
- `pcb publish` V2 source bundles now validate offline without requiring KiCad assets in `.pcb/cache`.
- V2 workspaces now derive KiCad footprint library names from resolved stdlib dependencies.

## [0.3.86] - 2026-05-28

### Added

- Added a 48 V Vishay SMBJ DO-214AA unidirectional TVS house part.
- Added `install.sh --local` to build and install local `pcb` and `pcbc` binaries side-by-side for development.

### Changed

- `config()` now rejects Starlark `record()` types as module input types.
- Lowered the default board-config minimum silkscreen text height from 0.8 mm to 0.6 mm.
- Removed the hidden `pcb package` subcommand.
- `pcb update` now rejects hydrated V2 dependency manifests and points users to `pcb add -u`.
- Workspaces with `pcb-version = "0.4"` or newer now always use MVS v2 dependency resolution.
- `pcb migrate` now hydrates V2 dependency manifests and removes obsolete `pcb.sum` lockfiles.
- Workspace package discovery is now implicit; use `[workspace].exclude` to prune paths.
- Updated the embedded Starlark runtime integration to the current starlark-rust APIs.

### Fixed

- Removed stale KiCad board items that reference layers deleted by layout stackup sync.
- `pcb add -u` now works when the workspace root is also a package directory.
- Use hydrated MVS v2 dependency resolution consistently across CLI, LSP, docs, and WASM evaluation.

## [0.3.85] - 2026-05-21

### Added

- Added Würth Elektronik WE-PMFI 1210 power inductors to stdlib house BOM matching.


## [0.3.84] - 2026-05-21

### Added

- Added remote sandbox URI support for `pcb open` and `pcb layout`.
- Added `pcb open` support for local and remote `.kicad_pcb` files.
- Added a `pcbc` compiler/toolchain binary alongside `pcb` so releases can publish versioned toolchain artifacts.
- Added a rustup-style `pcb` shim crate that selects, installs, and executes the separate versioned `pcbc` toolchain crate from the workspace `pcb-version` lane or a `+<toolchain>` CLI override.
- Added `pcb +nightly ...` and scheduled `pcbc` nightly publishing from the head of `main`.

### Changed

- Treat empty legacy `Component()` sourcing values as missing.
- Error for BOM components without part data unless house matching supports them.

## [0.3.83] - 2026-05-19

### Changed

- Standalone `pcb` releases now use the Diode CDN for faster, less exciting downloads and update checks.

## [0.3.82] - 2026-05-19

### Added

- Added `pcb ipc2581 render` for processed IPC-2581 layer output to SVG, PNG, and terminal graphics, including board outline overlays.
- Added `pcb ipc2581 outline` to export the IPC-2581 board profile as a KiCad-importable DXF.
- Added `pcb ipc2581 gerber` to export IPC-2581 fabrication layers as Gerber X2 files through a canonical artwork pipeline.
- Added initial `gerberx2` crate and `pcb gerber render` SVG/PNG/terminal previews.
- Added `pcb build --diagnostics <path>` to write structured JSON diagnostics for all evaluated root files.
- Added `pcb layout -f json` and `pcb layout --no-sync` for machine-readable layout output and existing-layout discovery.

### Changed

- Added diagnostic kinds for more toolchain diagnostics, including duplicate child names.
- Deprecated legacy `properties=` and sourcing kwargs on `Component()`/`Module()`; use typed kwargs (`dnp`, `skip_bom`, `skip_pos`, `type`, `description`, `part=Part(...)`).
- Deprecated the bare `add_property(...)` global in favor of `builtin.add_property(...)`.
- `pcb new board <name> <repo-url>` now creates a board repository directly, replacing the separate `pcb new workspace` flow.
- `pcb import` now writes directly into a board repository root instead of creating `boards/<name>/` under a workspace.
- Board release uploads now derive the Diode workspace name from the first path segment of `[workspace].repository`, with `[workspace].name` available as an override.
- `pcb layout` now deletes stale copied synced footprints.
- Deprecated stdlib generic modules `generics/TerminalBlock.zen` and `generics/Standoff.zen`, and removed generic BOM matching for standoffs.
- Removed the hidden `pcb build --board-config` JSON output option.

### Fixed

- Fixed remote package imports from non-GitHub/non-GitLab hosts such as `code.diode.computer`.
- Fixed auto-dependency discovery warnings caused by non-import strings that only looked like remote URLs.
- Fixed stdlib LPDDR4 channel impedance defaults to use 40 Ω single-ended and 80 Ω differential routing targets.

## [0.3.81] - 2026-05-11

### Changed

- Explicitly loading prelude-provided stdlib identifiers now emits a warning encouraging use of the @stdlib prelude.
- `pcb self update` now prints release notes between the old and new versions.
- `pcb doc` is now only for package documentation.
- Removed embedded `docs/pages`, embedded changelog rendering, `pcb doc spec`, `pcb doc --install`, and `pcb doc --list`.

### Fixed

- `pcb publish` now tolerates layouts missing release text variables from older `pcb layout` runs by adding them during staging.

## [0.3.80] - 2026-05-11

### Added

- Added stdlib BOM matching for Murata 0201 2.2nF capacitors.
- Added an LPDDR4 channel interface to stdlib.
- Added scoped multi-registry search for registry-backed `pcb search`.
- Builds now validate file-backed KiCad footprint S-expressions and embedded model checksums before layout generation.
- `pcb info -f json` now includes the full transitive external dependency closure using package metadata aligned with workspace packages.
- Added stdlib BOM matching for 0603 and 0805 Würth Elektronik WE-PMI and WE-PMCI power inductors.
- Added stdlib BOM matching for Panasonic ERJ-1GJF and ERJ-1GNF 0201 resistors.

### Changed

- `pcb layout` now initializes release text variables in KiCad project and board files, using `d10d3c0` as the placeholder git hash.
- Embedded STEP writers now use KiCad's current MMH3 checksums instead of legacy SHA-256 checksums.
- `pcb embed-step` is now shown in CLI help.

### Fixed

- Improved `pcb publish` workspace resolution.
- `pcb publish` no longer loads and rewrites boards with KiCad Python just to update release text variables, avoiding headless KiCad failures.
- Fixed 4-digit resistor R-notation for generic BOM matching of sub-10Ω E96 values.
- MVS v2 layout generation now keeps cache-backed footprint library paths workspace-relative.

## [0.3.79] - 2026-05-08

### Added

- Added simpler version footprint and separate QR to stdlib.

### Fixed

- MVS v2 offline builds now load dependency manifests from `vendor/` before falling back to the package cache.

## [0.3.78] - 2026-05-07

### Fixed

- `pcb layout` now embeds footprint-relative 3D models.
- `pcb layout --check` and board publish now use a semantic managed-footprint/connectivity check instead of byte-comparing shadow-synced KiCad files.
- Schematic position parsing for hierarchical labels representing interface nets now works correctly.

## [0.3.77] - 2026-05-07

### Changed

- Capacitor voltage-rating warnings now compare explicit ratings against the connected net voltage difference instead of the inferred 1.5x rounded requirement.
- Hydrated MVS v2 builds now reuse cached loads across packages with identical package-local dependency maps.

## [0.3.76] - 2026-05-06

### Added

- Added SOD-882 TVS diodes with capacitance-based BOM matching.

### Changed

- Capacitors now warn when explicit voltage ratings are below the inferred 1.5x rounded net-voltage requirement.

### Fixed

- KiCad CLI discovery now checks `PATH` before platform fallbacks.
- `pcb search` now refreshes stale local registry and KiCad indexes before non-interactive searches.
- `pcb layout` no longer fails when KiCad groups contain generated tuning-pattern items.
- Capacitor auto voltage ratings now round the 1.5x net-voltage requirement up to common capacitor voltage tiers.

## [0.3.75] - 2026-05-02

### Added

- Added API authentication via AWS credentials.

## [0.3.74] - 2026-04-30

### Added

- Added experimental MVS v2 dependency resolution via `pcb sync` / `pcb add`.

## [0.3.73] - 2026-04-27

### Changed

- Generated component `.zen` files now declare pins as flat top-level `io(Net)` assignments instead of a `Pins = struct(...)` block.
- Component datasheets now prefer `Part` metadata before component-level datasheets, with KiCad symbol datasheets as the final fallback.

## [0.3.72] - 2026-04-27

### Added

- `pcb layout`, `pcb simulate`, and `pcb test` now support repeatable `--config KEY=VALUE` overrides.
- `pcb-version` now requires `major.minor`; auto-deps bumps older workspace minors forward and newer-required minors error out.
- `pcb info -f json` now includes package entrypoints and top-level KiCad symbol names.

### Changed

- Removed `pcb info --tree`; it was not reliable and will be added back later when the dependency-tree semantics are more robust.
- Removed the hidden `pcb mcp` command and deleted the `pcb-mcp` / `rquickjs` integration from the workspace.
- Board releases no longer generate GLB model files.
- ODB++ release exports now use precision 4.
- `pcb info` now computes package dirty status from git metadata more efficiently.

### Fixed

- `pcb doc --package <url>` now prefers matching local workspace members for bare package URLs.
- `pcb doc` now supports `changelog[@latest|@unreleased|@<version>]`.
- Suppressed `binding.rebind` warnings for repeated `_` discard targets in top-level assignments.
- Warn when a BOM-included non-generic component is missing part information.

## [0.3.71] - 2026-04-20

### Added

- Extended house Schottky and TVS BOM ladders with additional higher-voltage entries.

### Fixed

- Avoid collisions in generated footprint library names.

### Added

- `pcb fmt` now supports `--include=kicad-sym|all` to format `.kicad_sym` files during directory walks.

## [0.3.70] - 2026-04-17

### Fixed

- Unresolvable inherited KiCad symbol datasheet paths are now dropped silently instead of emitting build warnings.

## [0.3.69] - 2026-04-17

### Fixed

- Fixed `io` prelude handling for `generics/Rectifier.zen` and `generics/Zener.zen`.
- Generated component .zen files now omit KiCad `no_connect` pins from `io()` and `Component(..., pins=...)`.

## [0.3.68] - 2026-04-17

### Migration Guide

Prefer template-first `io(template)` over `io(type, default=...)`. `default=` for `io()` remains source-compatible for now, but it is deprecated and now emits a warning.

Before:

```python
VDD = io(Power, default=Power("VDD", voltage="3.3V"))
GND = io(Ground, default=Ground("GND"))
```

After:

```python
VDD = io(Power("VDD", voltage="3.3V"))
GND = io(Ground("GND"))
```

Example warning:

```text
Warning: io() parameter `default` is deprecated; prefer template-first `io(template)` instead
    ╭─[ /Users/akhilles/src/diode/registry/reference/TCA9517Ax/TCA9517Ax.zen:46:6 ]
 46 │EN = io("EN", Net, optional=True, default=Net(VCC_A))
    │                             ╰──────────────────────── io() parameter `default` is deprecated; prefer template-first `io(template)` instead
```

Omit explicit connections for `pin.no_connect` pins. If a pin is marked `no_connect`, leave it out of `pins` and `Component()` will wire `NotConnected()` automatically.

Before:

```python
NC = io("NC", Net)

Component(
    name="J1",
    ...,
    pins={"A": A, "B": B, "NC": NC},
)
```

After:

```python
Component(
    name="J1",
    ...,
    pins={"A": A, "B": B},
)
```

Example warning:

```text
Warning: Pin 'NC' on component '1-2199119-3' is marked no_connect but was explicitly connected to Net net 'NC'; omit it from `pins` and Component() will wire NotConnected() automatically
    ╭─[ /Users/akhilles/src/dioderobot/demo/components/TE_Connectivity/1M2199119M3/1M2199119M3.zen:38:8 ]
 38 │    NC=io("NC", Net),
    │             ╰─────── Pin 'NC' on component '1-2199119-3' is marked no_connect but was explicitly connected to Net net 'NC'; omit it from `pins` and Component() will wire NotConnected() automatically
```

Avoid rebinding the same top-level name in a module. If you need to derive a final wiring choice, bind it to a new name instead of overwriting the original `io()` or intermediate value.

Before:

```python
RT = io("RT", Net)

if rt_value == "GND":
    RT = GND
elif rt_value == "VCC":
    RT = VCC
```

After:

```python
RT = io("RT", Net)

if rt_value == "GND":
    rt_pin = GND
elif rt_value == "VCC":
    rt_pin = VCC
else:
    rt_pin = RT
```

Example warning:

```text
Warning: Rebinding 'CURR_FDBK1_OPAMP_MINUS' in the same scope
    ╭─[ /Users/akhilles/src/dioderobot/demo/boards/DM0001/src/ShuntSense.zen:43:1 ]
 43 │CURR_FDBK1_OPAMP_MINUS = GND
    │           ╰─────────── Rebinding 'CURR_FDBK1_OPAMP_MINUS' in the same scope
```

Use `Power()` or `Ground()` for `io()`s that feed power pins instead of plain `Net`.

Before:

```python
VDD = io("VDD", Net)
GND = io("GND", Net)
```

After:

```python
VDD = io(Power())
GND = io(Ground())
```

Example warning:

```text
Warning: Pin 'VDD' on component 'LIS3DH' is a power pin but is connected to plain Net 'VDD'; consider using Power() or Ground()
    ╭─[ /Users/akhilles/src/dioderobot/demo/components/STMicroelectronics/LIS3DH/LIS3DH.zen:16:9 ]
 16 │    VDD=io("VDD", Net),
    │               ╰─────── Pin 'VDD' on component 'LIS3DH' is a power pin but is connected to plain Net 'VDD'; consider using Power() or Ground()
```

Migrate deprecated `generics/Diode.zen` usage to the more specific diode generics:

- Use `generics/Rectifier.zen` for standard and Schottky diodes, including small-signal / signaling diodes.
- Use `generics/Zener.zen` for reverse-breakdown regulation and reference diodes.
- Use `generics/Tvs.zen` for transient-voltage suppressors.

Package mapping:

```text
SMA -> DO-214AC
SMB -> DO-214AA
SMC -> DO-214AB
SOD-123 / SOD-323 / SOD-523 stay the same
```

Rectifier / Schottky:

```python
Diode(package="SMA", variant="Schottky", v_r="40V", i_f="1A", v_f="500mV", A=A, K=K)
Rectifier(package="DO-214AC", technology="Schottky", reverse_voltage="40V", forward_current="1A", forward_voltage="500mV", A=A, K=K)
```

Zener:

```python
Diode(package="SOD-123", variant="Zener", v_r="5.1V", A=A, K=K)
Zener(package="SOD-123", zener_voltage="5.1V", A=A, K=K)
```

TVS:

```python
Tvs(package="DO-214AA", direction="Unidirectional", reverse_standoff_voltage="24V", reverse_clamping_voltage="38.9V", peak_pulse_power="3000W", A=GND, K=VIN)
```

### Added

- Added `generics/Rectifier.zen` and `generics/Zener.zen` with expanded package support and house-part BOM matching coverage.
- `pcb layout` and board publish now fail early when a board was last saved by a newer KiCad major version than the one installed locally.
- `pcb build` now accept repeatable `--config key=value` for setting `config()` parameters.
- Net type physical-value fields now coerce string and scalar inputs like `io()`/`config()`.
- Unnamed `Net()`/typed nets and generated interface child nets now infer names from assignment targets when possible.
- Add `io()` direction metadata plus `input()` / `output()` sugar.
- `config()`, `io()`, `input()`, and `output()` now allow omitting the explicit name when assigned to a top-level variable.
- `config()` now supports discrete `allowed=` sets for scalar and physical-value inputs.
- Preserve KiCad symbol pin metadata and add `Component()` pin/net compatibility warnings.
- Added style advice for redundant explicit names on `io()`, `config()`, nets, and interfaces.
- Add pass-based schematic ERC plumbing and net-site `pin.no_connect` diagnostics with inline suppression support.
- `io(template)` now infers placeholder types and enforces typed-net voltage compatibility.

### Changed

- Component generation no longer automatically scans datasheets.
- Component modifiers can now override `spice_model`.
- House Murata caps now use vendor MLCC models when available.
- `voltage_within()` now accepts nets with voltage metadata or direct `Voltage` values.
- `pcb new component` and `pcb search` component imports now place datasheet artifacts under each component's `docs/` subdirectory.
- Layout sync and KiCad netlist export now normalize file- and package-based footprints to library-aware FPIDs.
- `Component()` now infers `spice_model` from symbol `Sim.*` properties.
- `Simulation()` now accepts `bom_profile=`.
- Module-scoped variable rebinding is now a warning.
- Removed the 10uF 100V 1210 stdlib house capacitor from generic matching due to severe derating.
- `Net` and `Power` now expose unset `voltage` as `None`.
- Deprecated `NET=` net casts now warn; use positional forms like `Power(other_net)`.
- Remove legacy `pcb fork` subcommands and reserve `pcb fork` for future use.

### Fixed

- LSP diagnostics now publish to the `.zen` file that owns the root diagnostic span.

## [0.3.67] - 2026-04-10

### Added

- Added workspace-scoped Diode endpoint overrides via `[workspace].endpoint`, with auth tokens stored per resolved endpoint.

### Changed

- Loading deprecated stdlib physical-unit `*Range` aliases now emits a deprecation warning pointing to the corresponding base unit type.
- Deprecated the stdlib `pins.zen` and `metadata.zen` modules.
- Deprecated the `Schematics()` helper.
- Deprecated generic modules `generics/Bjt.zen`, `generics/Diode.zen`, `generics/Mosfet.zen`, and `generics/OperationalAmplifier.zen`.
- Deprecated non-standard packages in `generics/Inductor.zen`.
- Newly added KiCad symbol properties now default to `justify left top` and `hide yes`.
- `pcb scan` now resolves local PDFs through the shared datasheet materialization cache by default, and `--output` copies the materialized Markdown and images out of that cache.
- `pcb scan` now prints both `PDF:` and `Markdown:` output paths, with local PDF scans reporting the original input PDF path and URL scans reporting the materialized cached PDF path.
- `pcb search` now merges docs full-text results for registry packages and KiCad symbols, with consistent phrase handling across indices.

### Fixed

- `pcb build --offline` now reuses selected locked pseudo-versions for rev-pinned workspace deps.
- `PhysicalValue` is now hashable in Starlark, including after freezing.
- Layout sync no longer creates empty footprint `(embedded_files)` blocks that KiCad removes on save.
- Fixed an LSP memory leak during reparsing.
- Rev-pinned dependencies now override stale lockfile-seeded pseudo-versions during resolution.

## [0.3.66] - 2026-04-06

## [0.3.65] - 2026-04-03

### Added

- Added 10uF 100V 1210 house capacitor in stdlib

### Fixed

- `pcb layout` no longer crashes when a managed component path is numeric-only, such as `1053091102`.
- `pcb build` now warns and drops invalid inherited symbol datasheet paths instead of failing the build.

## [0.3.64] - 2026-04-02

### Added

- Added support for KiCad 10.

## [0.3.63] - 2026-03-30

### Changed

- KiCad symbol is now the source of truth for component metadata (footprint, datasheet, part); generated `.zen` files are minimal wrappers.
- `Component()` inherits `skip_bom` from the KiCad symbol `in_bom` flag (inverted) when not explicitly set.
- `pcb fork add` is now blocked and points users to `pcb sandbox`; `pcb fork remove` and `pcb fork upstream` remain for existing forks.

### Added

- Warn when module `io()`s are declared but never connected to any realized ports.

### Fixed

- Stdlib `Crystal` and `MountingHole` no longer expose unused variant-specific ports.
- Untagged `branch`/`rev` dependencies now use `0.1.1-0...` pseudo-versions so they outrank plain `0.1.0` deps.

## [0.3.62] - 2026-03-27

### Fixed

- `pcb doc --package <url>` now defaults remote packages to the latest tagged version and accepts an explicit `@latest` suffix.

## [0.3.61] - 2026-03-25

### Added

- `pcb new component` now prints a `Module("...")` usage hint when it can infer a qualified URL.

### Fixed

- Auto-deps now only upgrades synced workspace member dependency versions.
- Workspace-namespace dependencies now fail locally with a clear missing-member error instead of remote fetch fallback.

## [0.3.60] - 2026-03-25

### Added

- Board publish now blocks outside CI when any `pcb.toml` uses `[patch]` or `branch`/`rev` dependencies.

### Changed

- Show `Fetching <repo>` progress while populating shared bare repos under `~/.pcb/bare`.
- Vendored remote packages now copy only canonical package files instead of whole cache directories.

### Fixed

- Git HTTPS fallback probes now run non-interactively before falling back to SSH.
- Exclude `pcb.sum` in canonical package hash
- LSP now watches `**/pcb.toml` and `**/pcb.sum` for dependency and workspace updates.
- Auto-deps now sync workspace member versions against tags reachable from the current `HEAD`, avoiding version bumps from future history on historical checkouts.

## [0.3.59] - 2026-03-23

### Added

- `pcb search` now supports `kicad:components` in both the TUI and non-interactive search.

### Changed

- Dependency materialization now archives directly from `~/.pcb/bare` for tagged and pinned dependencies.

### Fixed

- Existing partial bare repos under `~/.pcb/bare` now transparently hydrate to full clones before serving pinned local commits.

## [0.3.58] - 2026-03-21

### Added

- `Usb2TypeC` interface adapter in stdlib to connet USB2 to TypeC
- `Dvp` interface for cameras with 8, 10, 12 and 16 bit width
- `SdRam` interface for 16 bit and 32 bit

### Changed

- `pcb publish` for packages now builds before planning publish waves and aborts if the validation build dirties the repository.

### Fixed

- `pcb update` now consistently scopes nested paths to the containing package.
- Branch and `rev` dependencies now resolve the pseudo-version matching the pinned commit, avoiding flaky `pcb.sum` entries.

## [0.3.57] - 2026-03-18

### Added

- `pcb publish` now supports inferred package bumps from conventional commits with `--bump=infer`.
- `pcb publish -y` now skips the final package publish confirmation prompt.
- Include version in dependency/dependent URLs in search results (e.g. `...@1.0`).

### Changed

- All `@stdlib/kicad/` modules (`PinHeader`, `PinSocket`, `MolexPicoBlade`, `SolderWire`, `TagConnect`) now emit deprecation warnings.
- `pcb publish` now fetches remote state before preflight checks and verifies local main is in sync with the remote.
- `pcb publish` for packages now requires the `CI` environment variable to be set (use `--force` or `CI=true` to bypass).
- Skill setup and instructions now live in `pcb ai` instead of the main `pcb` CLI and MCP server.

### Fixed

- `pcb publish` now generates shorter conventional commit titles for dependency bumps.
- `pcb publish` now rolls back local tags and commits if pushing to remote fails.

## [0.3.56] - 2026-03-14

### Fixed

- Reject same-package package URLs in `Module()` and require relative paths instead of adding self-dependencies.

## [0.3.55] - 2026-03-14

### Fixed

- Stop syncing `Alternatives` and `Matcher` component metadata into KiCad footprint fields during layout sync.
- `pcb bom` now keeps MOQ-expensive offers in BOM output but marks them hard to source in table summaries instead of dropping them.

## [0.3.54] - 2026-03-12

### Added

- Hidden `pcb kq` command to inspect KiCad symbol libraries as structured JSON views (`sym`, `metadata`, `electrical`, `raw`).
- `pcb new component --component-id <id>` now installs a web-searched component non-interactively, with optional `--part-number` fallback and `--manufacturer` override/fallback.
- `[[workspace.kicad_library]]` now supports `parts = "<url>"`, materializing a virtual parts manifest for KiCad symbol repos so `@kicad-symbols/...` symbols can inherit default parts too.

### Changed

- CLI and MCP component search JSON outputs are now aligned, with cleaner payloads and per-source caps for `web:components`.

### Fixed

- `pcb publish` now shows `patch`, `minor`, and `major` bumps for boards consistently, and `major` on `0.x` now produces `1.0.0`.

## [0.3.53] - 2026-03-11

### Added

- `pcb new workspace` now creates an empty `pcb.sum`, so `pcb build --locked --offline` works immediately.
- `pcb scan` now accepts `http(s)` datasheet URLs in addition to local PDF paths and prints the resolved markdown path.
- `Part` is now in the standard library prelude. Use `Part(mpn=..., manufacturer=...)` with `Component(part=...)` for manufacturer sourcing.
- `pcb doc --install` writes embedded documentation to `~/.pcb/docs`; runs automatically on first use and after `pcb self update`.
- `Component()` now inherits default `part`, `alternatives`, and part qualifications from manifest `parts` entries matched by stable `package://` symbol URIs.

### Changed

- `Part` is now the single source of truth for component sourcing. `mpn` and `manufacturer` on `Component()` still work but `part=` is preferred.
- `pcb new` now uses subcommands (`workspace`, `board`, `package`, `component`) instead of `--workspace/--board/--package/--component` flags.
- `pcb new component [DIR]` now imports local component directories; `pcb search --dir` support was removed.
- `Net` is now defined in `@stdlib/interfaces.zen` and available via the stdlib prelude instead of being a language builtin.
- Removed deprecated backward-compatibility shims: `builtin.physical_range()`, `builtin.Voltage`/`Current`/etc. attributes, `using()`.
- `pcb scan` removed `--model` and `--json`; local PDF and URL flows now both resolve to markdown output with default processing.

### Fixed

- `match_component()` now skips non-component children such as `ElectricalCheck` instead of failing on missing `mpn`.
- `pcb update` no longer proposes updates for KiCad asset libraries (symbols, footprints, 3D models) which publish breaking changes in patch releases.
- Auto-dependency detection now resolves relative path imports that cross workspace member boundaries (e.g. `load("../../modules/Lib/Lib.zen", ...)`).
- Layout sync now applies pad assignments to all same-number KiCad pad objects in a footprint.

## [0.3.52] - 2026-03-07

### Added

- `config()` now supports `checks` parameter for validation functions, matching `io()`.
- Stdlib prelude: `Power`, `Ground`, `NotConnected`, `Layout`, and `Board` are now implicitly available in `.zen` files without explicit `load()` statements.

### Changed

- `config()` parameter `convert` is now deprecated and emits a warning.
- Bundle the standard library with `pcb` and make it available automatically in each workspace.

### Fixed

- Preserve KiCad zone priorities during `layout.sync` and bias fragment zones so overlapping fills keep the intended precedence.

## [0.3.51] - 2026-03-04

### Changed

- Bump stdlib to 0.5.11

### Added

- Added typed part metadata with `builtin.Part(...)` and `Component(part=...)`, including JSON netlist serialization and `list[Part]` support for `properties["alternatives"]`.

### Fixed

- Apply MVS-selected KiCad asset versions in resolution/materialization and sibling promotion, preventing `@kicad-*` alias failures after patch updates.
- `pcb update` now ignores prerelease dependency versions when selecting updates.

## [0.3.50] - 2026-03-02

### Changed

- Bump stdlib to 0.5.10

### Fixed

- `pcb publish` now skips tracked paths from regular manifest dependencies and correctly copies tracked asset directories, fixing release staging failures on patched/forked module layouts.

## [0.3.49] - 2026-03-02

### Changed

- Replace `[assets]` with `[[workspace.kicad_library]]` for built-in resolution of KiCad symbol, footprint, and 3D model repositories.
- Embed referenced 3D models directly into `.kicad_pcb` layout files, eliminating the need for external 3D model files at layout time.

## [0.3.48] - 2026-02-28

### Added

- `pcb simulate`: run SPICE simulations directly via ngspice with inline `set_sim_setup()`, `--netlist`/`-o -` output, workspace discovery, and LSP diagnostics on save. Errors on components missing a `SpiceModel` to prevent incomplete netlists.
- Support net names in f-strings for SPICE simulation setup (e.g. `f"V1 {VIN} {GND} AC 1"`), including dot-notation for interface nets (`{POWER.vcc}`).

### Fixed

- Normalize net and component symbol source paths to `package://...`, so emitted schematic/netlist `symbol_path` values no longer leak absolute cache paths.

## [0.3.47] - 2026-02-27

### Fixed

- Ensure `File()` footprint paths resolve to `package://...` when dependency files are read from `~/.pcb/cache`.

## [0.3.46] - 2026-02-26

### Changed

- Use zstd level 17 (was 15) when embedding STEP 3D models into KiCad footprints.
- `Path(..., allow_not_exist=True)` no longer emits missing-path warnings.
- `pcb search` component add now prefers symbol datasheets, using fallback URL scan only when needed.
- `pcb search` add-component now rewrites symbol `Manufacturer_Part_Number`/`Manufacturer_Name` metadata before generating `.zen`.

### Fixed

- Normalize netlist `package_roots` cache paths to `<workspace>/.pcb/cache` for unvendored remote dependencies (including stdlib).
- MCP `resolve_datasheet` now avoids top-level JSON Schema combinators in `inputSchema`, fixing strict MCP clients (e.g., Claude Code) that reject `oneOf` at schema root.

## [0.3.45] - 2026-02-25

### Added

- Added `pcb doc` guides for bringup, changelog, and readme conventions.
- Added MCP tool `resolve_datasheet` to produce cached `datasheet.md` + `images/` from `datasheet_url`, `pdf_path`, or `kicad_sym_path`.
- Added LSP request `pcb/resolveDatasheet`, sharing the same resolve flow as the MCP tool.

### Fixed

- Standalone `.zen` files with inline manifests now map `Board(..., layout_path=...)` to `package://workspace/...`, avoiding absolute-path leakage.
- Stackup sync now emits dielectric `(type ...)` once per grouped layer (not per `addsublayer`), preventing spurious `layout.sync` drift.

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

[Unreleased]: https://github.com/diodeinc/pcb/compare/v0.4.7...HEAD
[0.4.7]: https://github.com/diodeinc/pcb/compare/v0.4.6...v0.4.7
[0.4.6]: https://github.com/diodeinc/pcb/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/diodeinc/pcb/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/diodeinc/pcb/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/diodeinc/pcb/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/diodeinc/pcb/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/diodeinc/pcb/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/diodeinc/pcb/compare/v0.3.93...v0.4.0
[0.3.93]: https://github.com/diodeinc/pcb/compare/v0.3.92...v0.3.93
[0.3.92]: https://github.com/diodeinc/pcb/compare/v0.3.91...v0.3.92
[0.3.91]: https://github.com/diodeinc/pcb/compare/v0.3.90...v0.3.91
[0.3.90]: https://github.com/diodeinc/pcb/compare/v0.3.89...v0.3.90
[0.3.89]: https://github.com/diodeinc/pcb/compare/v0.3.88...v0.3.89
[0.3.88]: https://github.com/diodeinc/pcb/compare/v0.3.87...v0.3.88
[0.3.87]: https://github.com/diodeinc/pcb/compare/v0.3.86...v0.3.87
[0.3.86]: https://github.com/diodeinc/pcb/compare/v0.3.85...v0.3.86
[0.3.85]: https://github.com/diodeinc/pcb/compare/v0.3.84...v0.3.85
[0.3.84]: https://github.com/diodeinc/pcb/compare/v0.3.83...v0.3.84
[0.3.83]: https://github.com/diodeinc/pcb/compare/v0.3.82...v0.3.83
[0.3.82]: https://github.com/diodeinc/pcb/compare/v0.3.81...v0.3.82
[0.3.81]: https://github.com/diodeinc/pcb/compare/v0.3.80...v0.3.81
[0.3.80]: https://github.com/diodeinc/pcb/compare/v0.3.79...v0.3.80
[0.3.79]: https://github.com/diodeinc/pcb/compare/v0.3.78...v0.3.79
[0.3.78]: https://github.com/diodeinc/pcb/compare/v0.3.77...v0.3.78
[0.3.77]: https://github.com/diodeinc/pcb/compare/v0.3.76...v0.3.77
[0.3.76]: https://github.com/diodeinc/pcb/compare/v0.3.75...v0.3.76
[0.3.75]: https://github.com/diodeinc/pcb/compare/v0.3.74...v0.3.75
[0.3.74]: https://github.com/diodeinc/pcb/compare/v0.3.73...v0.3.74
[0.3.73]: https://github.com/diodeinc/pcb/compare/v0.3.72...v0.3.73
[0.3.72]: https://github.com/diodeinc/pcb/compare/v0.3.71...v0.3.72
[0.3.71]: https://github.com/diodeinc/pcb/compare/v0.3.70...v0.3.71
[0.3.70]: https://github.com/diodeinc/pcb/compare/v0.3.69...v0.3.70
[0.3.69]: https://github.com/diodeinc/pcb/compare/v0.3.68...v0.3.69
[0.3.68]: https://github.com/diodeinc/pcb/compare/v0.3.67...v0.3.68
[0.3.67]: https://github.com/diodeinc/pcb/compare/v0.3.66...v0.3.67
[0.3.66]: https://github.com/diodeinc/pcb/compare/v0.3.65...v0.3.66
[0.3.65]: https://github.com/diodeinc/pcb/compare/v0.3.64...v0.3.65
[0.3.64]: https://github.com/diodeinc/pcb/compare/v0.3.63...v0.3.64
[0.3.63]: https://github.com/diodeinc/pcb/compare/v0.3.62...v0.3.63
[0.3.62]: https://github.com/diodeinc/pcb/compare/v0.3.61...v0.3.62
[0.3.61]: https://github.com/diodeinc/pcb/compare/v0.3.60...v0.3.61
[0.3.60]: https://github.com/diodeinc/pcb/compare/v0.3.59...v0.3.60
[0.3.59]: https://github.com/diodeinc/pcb/compare/v0.3.58...v0.3.59
[0.3.58]: https://github.com/diodeinc/pcb/compare/v0.3.57...v0.3.58
[0.3.57]: https://github.com/diodeinc/pcb/compare/v0.3.56...v0.3.57
[0.3.56]: https://github.com/diodeinc/pcb/compare/v0.3.55...v0.3.56
[0.3.55]: https://github.com/diodeinc/pcb/compare/v0.3.54...v0.3.55
[0.3.54]: https://github.com/diodeinc/pcb/compare/v0.3.53...v0.3.54
[0.3.53]: https://github.com/diodeinc/pcb/compare/v0.3.52...v0.3.53
[0.3.52]: https://github.com/diodeinc/pcb/compare/v0.3.51...v0.3.52
[0.3.51]: https://github.com/diodeinc/pcb/compare/v0.3.50...v0.3.51
[0.3.50]: https://github.com/diodeinc/pcb/compare/v0.3.49...v0.3.50
[0.3.49]: https://github.com/diodeinc/pcb/compare/v0.3.48...v0.3.49
[0.3.48]: https://github.com/diodeinc/pcb/compare/v0.3.47...v0.3.48
[0.3.47]: https://github.com/diodeinc/pcb/compare/v0.3.46...v0.3.47
[0.3.46]: https://github.com/diodeinc/pcb/compare/v0.3.45...v0.3.46
[0.3.45]: https://github.com/diodeinc/pcb/compare/v0.3.44...v0.3.45
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
