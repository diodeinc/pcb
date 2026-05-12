---
name: librarian
description: Use before creating or modifying reusable PCB registry content, including component packages, symbols, footprints, STEP models, datasheets, or reusable Zener modules. Also use when working inside a registry component package or changing KiCad/Zener files that define reusable parts. Covers search-before-authoring, datasheet-backed cleanup, package structure, sourcing checks, and validation.
---

# Librarian

Create sourceable, evidence-backed registry packages that board designers can search, trust, and instantiate.

Use this for new registry components, package fixes, family selectors, symbol/footprint cleanup, datasheet-backed reference circuitry, and requests produced by registry search during board work.

If you are already operating inside a registry checkout or remote librarian session, continue authoring in that registry unless the user explicitly asks you to dispatch another librarian. Remote librarian dispatch is for agents working outside the registry, such as board or spec work that discovers missing reusable content.

## Intake And Scope

Start by clarifying the deliverable:

- primitive component package only
- component package with built-in required support circuitry
- separate `.zen` reference-design modules for one or more application circuits
- connector/module wrapper around existing components
- family selector covering multiple orderable MPNs

Search existing registry modules and components first:

```bash
pcb search -m registry:modules <query> -f json
pcb search -m registry:components <query> -f json
```

Patch or extend an existing package when it is the right home. Create a new package only when no suitable package exists or the physical package/footprint, pinout, or fundamental schematic topology differs.

## Family-First, Symbol-First

When a request names one MPN, first look for the related part family before authoring. A good component package usually covers all parts with the same physical package/footprint, pinout, feature set, and fundamental schematic topology.

Curate symbols before writing reference-design `.zen`. The symbol library defines the functional variants and primitive interface that the `.zen` package will select and wire; an untrusted symbol makes the rest of the package untrustworthy.

A typical flow:

1. Understand the request and intended deliverable.
2. Find the related part group/family.
3. Fetch or import ECAD artifacts.
4. Clean the symbols against the datasheet.
5. Represent each functional variant symbol without duplicating order-code variants.
6. Clean the footprint and embedded STEP against the datasheet.
7. Write the reference circuitry or selector logic.

Treat this as the default direction, not a rigid script. Focused patches may only touch one stage, but do not write reusable circuitry on top of untrusted symbols.

Functional variants need symbols; order-code variants do not. For example, fixed-output LDO voltages get separate symbols because the selected silicon changes electrical behavior, but tape/reel, temperature grade, RoHS, and packing suffixes do not.

Use `kicad-symbol` for symbol-file structure, editing, `extends`, rendering, and signature rules. The librarian-level rule is to curate the family symbols before `.zen` work and to cover functional variants without duplicating order-code variants.

## Artifact Acquisition

Use web component search only for registry authoring, not ordinary board consumption.

```bash
pcb search -m web:components <MPN> -f json
```

`web:components` results include `component_id`, source, model availability, datasheets, and availability. Prefer results with ECAD artifacts and STEP when they match the datasheet and the intended physical package.

Import web artifacts into a workspace with:

```bash
pcb new component --component-id <ID> --part-number <MPN> --manufacturer <MFR>
```

Use `pcb new component <DIR>` when importing a downloaded local ECAD directory.

Fetched artifacts are starting points, not authority. Check every symbol, pin, footprint, datasheet link, sourcing field, and 3D model against the manufacturer datasheet. If a symbol, footprint, or pin map is unsafe, fix it before writing reusable circuitry.

If a footprint and STEP model were fetched separately, embed the STEP into the footprint:

```bash
pcb embed-step <footprint.kicad_mod> <model.step>
```

`pcb search` plus `pcb new component` often handles this, but separately fetched models should always be embedded in the `.kicad_mod`.

## Package Shape

New reusable registry content belongs in a component package path:

```text
components/<Manufacturer>/<NAME>/
├── <NAME>.zen
├── <NAME>.kicad_sym
├── <NAME or footprint>.kicad_mod
├── pcb.toml
├── docs/
│   └── <datasheet>.pdf
└── README.md
```

Include checked-in datasheet PDFs under `docs/`. Include a real `.kicad_mod`; note whether it is datasheet-exact, KiCad-stock-derived, vendor-derived, or intentionally adjusted.

The README is for realistic usage examples and concise integration notes only. Put rationale and design evidence in the `.zen` docstring.

Legacy `reference/` packages are examples, not placement for new reusable circuitry.

## Reference Circuit Quality

A good reference design is one coherent schematic circuit around the curated symbols. It exposes application-level IO, keeps implementation-detail nodes internal unless access is necessary, and exposes only integrator-tunable choices.

Start from the primitive component: symbols, footprint, pins, and sourcing. Add surrounding schematic circuitry only when it is part of the reusable way to use the IC: required decoupling, compensation, feedback, bootstrap, bias, reset, straps, or a datasheet-recommended application circuit with clear defaults.

For decoupling, do not cargo-cult 100 nF or 100 nF + bulk pairs. Prefer one compact low-ESL MLCC, often 1 uF 0402, at each power pin when valid; check inrush and regulator stability.

Keep the package primitive if the surrounding circuit is board-specific, underspecified, already handled by another package, or blocked by untrusted symbol/footprint/pin data.

If one IC has fundamentally different schematic topologies for different modes, keep them in the same Zener package but use separate `.zen` reference-design modules.

The `.zen` docstring is the design document. It should explain:

- circuit/application mode
- exact IC/physical package or family and selector behavior
- operating envelope, interfaces, configs, and assumptions
- included support circuitry vs integrator-owned circuitry
- evidence for important choices, footprint notes, and sourceability compromises

Capture the facts that drive the circuit:

- typical application or recommended topology
- rails, limits, sequencing, and required passives
- straps, reset/enable, bias, compensation, timing, and mode selects
- equations and datasheet-recommended example points
- oscillator/crystal requirements and sensitive nets
- physical-package caveats that affect the public API

Do not guess ambiguous passives, straps, sequencing, oscillators, or topology choices. Find evidence or ask.

## Family Scope And Naming

One component Zener package may cover a part family when the parts share the same physical package/footprint, pinout, feature set, and fundamental schematic topology. Values or selected silicon may vary by config. Fixed-output LDO trims in the same physical package/pinout are a good grouping.

Use separate Zener packages when physical package/footprint, pinout, or fundamental schematic topology differs. Electrical grouping requires judgment: if the same schematic shape still applies, grouping is usually fine; if you are masking most of the MPN or combining unrelated feature sets, split the package.

Name the Zener package from the functional MPN pattern, not the full orderable SKU:

- derive the name from the MPNs being covered
- drop ordering-only suffixes such as temperature, tape/reel, and RoHS markings
- use lowercase `x` to mask patterned MPN differences inside a family
- if the name needs too many `x`s, the family is probably too broad

Examples: `DP83867ISRGZR` -> `DP83867`; `TPS3430WDRCR` -> `TPS3430WDRC`; `SN74LXC1T45DRYR` / `SN74AXC1T45QDRYRQ1` -> `SN74x1T45-DRY`.

For selectable families, use a compact table/list of variants with MPN, symbol, limits, and properties. Filter by `config()` values, use the first match as `part=`, and put remaining drop-in matches in `properties={"alternatives": ...}`.

## Sourceability And Style

Prefer strong registry exemplars: `TPS709-Q1`, `TPSM336xx-Q1`, `TCPP01-M12`, `SN74x1T45-DRY`, `SSM3KxxxCT`, `W25QxxUX`, `Wago/2060-4xx_998-404`, `FTSH-105-01-L-DV-K-A-P-TR`.

Prefer house-matchable generic choices when technically valid. If rounding, clamping, or substituting values, document why. If a generic cannot reasonably match, ask whether to use an explicit part or suppress the warning with justification. Use `pcb doc --package @stdlib --list` to locate and inspect `bom/match_generics.zen` when generic matching matters.

Use comments for evidence and judgment only: datasheet section/table/equation references, rounded or clamped values, or stuffing strategy. Avoid comments that restate code. Do not add decorative banner/divider comments such as `====` or `----`.

## Verification

Build after each major block:

```bash
pcb build -Wstyle components/<Manufacturer>/<NAME>
```

Use BOM output when build warnings are not enough to understand sourcing:

```bash
pcb bom components/<Manufacturer>/<NAME>/<NAME>.zen -f json
```

Format before finishing:

```bash
pcb fmt components/<Manufacturer>/<NAME>
```

Before stopping, make sure existing registry packages were checked, important choices are evidence-backed, artifacts match the datasheet, sourceability compromises are documented, and `pcb build -Wstyle` warnings were reviewed.
