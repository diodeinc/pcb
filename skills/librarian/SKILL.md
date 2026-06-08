---
name: librarian
description: Use before creating or modifying reusable PCB registry content, including component packages, symbols, footprints, STEP models, datasheets, or reusable Zener modules. Also use when working inside a registry component package or changing KiCad/Zener files that define reusable parts. Covers search-before-authoring, datasheet-backed cleanup, package structure, sourcing checks, and validation.
---

# Librarian

Create sourceable, evidence-backed registry packages that board designers can search, trust, and instantiate.

Use this for new registry components, package fixes, family selectors, symbol/footprint cleanup, datasheet-backed reference circuitry, and requests produced by registry search during board work.

If you are already operating inside a registry checkout or remote librarian session, continue authoring in that registry unless the user explicitly asks you to dispatch another librarian. Remote librarian dispatch is for agents working outside the registry, such as board or spec work that discovers missing reusable content.

## Guardrails

- Do not invent datasheet facts, pin mappings, footprints, passive values, limits, sourceability, or application topology. Find evidence or ask.
- Do not build reusable circuitry on untrusted symbols, footprints, or pin maps. Curate artifacts against the manufacturer datasheet first.
- Do not manually create scratch footprints or synthetic 3D models without explicit user confirmation. “Add a footprint/model” means find, verify, and embed trusted artifacts first; if none are available or they conflict, stop and ask.
- Do not add new packages under `reference/`; that tree is deprecated. A component's reference design belongs inside the component package. Prefer one `.zen` file per reference design: instantiate `Component()` directly in that file with its support circuitry, not through a separate primitive-only local wrapper. Use `modules/` for higher-level reusable functional blocks and subsystem schematics that are not simply one component's reference design.
- Treat registry packages as public integration contracts. If existing consumers must change Zener, layout, or assumptions to adopt the new version, it is breaking; obvious examples include `io()`/`config()`/entrypoint changes and substantial layout changes. Note the migration and mark the commit accordingly.

## Intake And Scope

Start by clarifying the deliverable. A request to add a component package includes judging whether a datasheet-backed reference design is warranted; if it is, include it unless the user asks for a primitive-only package.

- primitive component package only
- component package with built-in required support circuitry
- component package with reusable reference-design circuitry
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

Curate symbols before writing reference-design `.zen`. The symbol library defines the functional variants and primitive interface that the `.zen` package will select and wire.

A typical flow:

1. Understand the request and intended deliverable, including whether reference circuitry is warranted.
2. Find the related part group/family.
3. Fetch or import ECAD artifacts.
4. Clean the symbols against the datasheet.
5. Represent each functional variant symbol without duplicating order-code variants.
6. Clean the footprint and embedded STEP against the datasheet.
7. Ensure the footprint has an embedded STEP: find and embed any referenced local model, otherwise download a matching model and embed it with `pcb embed-step`.
8. Write the primitive API, reference circuitry, or selector logic.

Treat this as the default direction, not a rigid script. Focused patches may only touch one stage.

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

Fetched artifacts are starting points, not authority. Check every symbol, pin, footprint, datasheet link, sourcing field, and 3D model against the manufacturer datasheet. Footprints need an embedded STEP; imported or KiCad-copied footprints often only reference external models, which you must locate or download and embed.

If trusted footprint or STEP artifacts are unavailable, report what you checked and ask before creating scratch geometry. After approved scratch work, label it as scratch/generated, cite the evidence used, render/verify it, and call out the risk in the README and completion report.

Embed real STEP models with `pcb embed-step`; do not hand-edit model blocks:

```bash
pcb embed-step <footprint.kicad_mod> <model.step>
```

## Package Shape

New reusable registry content belongs in a component package path:

```text
components/<Manufacturer>/<NAME>/
├── <NAME>.zen                  # primitive package, or primary reference design
├── <reference-design>.zen       # optional additional reference designs
├── <NAME>.kicad_sym
├── <NAME or footprint>.kicad_mod
├── pcb.toml
├── docs/
│   └── <datasheet>.pdf
└── README.md
```

Include checked-in datasheet PDFs under `docs/`. Include a real `.kicad_mod`; note whether it is datasheet-exact, KiCad-stock-derived, vendor-derived, or intentionally adjusted.

Each `.zen` entrypoint should be a complete public API for one primitive component or one reference design. A package may contain multiple `.zen` entrypoints when one curated part/family has multiple useful datasheet-backed application circuits; avoid thin local wrappers that only re-export another `.zen`.

The README is for realistic usage examples and concise integration notes only. Put rationale and design evidence in the `.zen` docstring.

## Reference Circuit Quality

A good reference design is one coherent schematic circuit around the curated symbols. It exposes application-level IO and keeps implementation-detail nodes internal unless access is necessary. Implement it as a single `.zen` entrypoint that instantiates `Component()` directly and includes the support circuitry in the same file.

Not every component needs a reference design. Add one when the datasheet defines required or strongly recommended application circuitry, typically for ICs/modules such as regulators, converters, chargers, transceivers, PHYs, sensors, clocks, MCUs, protection/controllers, and analog front ends.

Keep simple parts primitive unless there is a reusable circuit worth capturing: resistors, capacitors, inductors, ferrites, diodes, LEDs, MOSFETs/BJTs, simple protection parts, connectors, switches, crystals, and similar parts. A primitive `.zen` should still expose a clean public API with appropriate nets/interfaces and clear names.

When a reference design is warranted, start from the primitive facts: symbols, footprint, pins, sourcing, and datasheet guidance. Add surrounding schematic circuitry only when it is part of the reusable way to use the IC: required decoupling, compensation, feedback, bootstrap, bias, reset, straps, or a datasheet-recommended application circuit with clear defaults. Include a `Layout()` for reference circuitry to capture intended placement or physical relationships where useful, e.g. `Layout(name="TLV62568DBVR", path="layout/TLV62568DBVR")`.

For decoupling, do not cargo-cult 100 nF or 100 nF + bulk pairs. Prefer one compact low-ESL MLCC, often 1 uF 0402, at each power pin when valid; check inrush and regulator stability. Motivation: modern MLCCs provide much higher capacitance density than the historical parts that made 100 nF a useful default, and a larger capacitor in the same small package generally has lower impedance across the relevant range. Package and placement often matter more than folklore value-splitting: smaller packages and shorter power/ground loops reduce ESL, move self-resonance higher, and keep high-frequency currents local. Parallel 100 nF + bulk capacitors can waste BOM/placement area and may introduce undamped impedance peaks, especially when the farther capacitor's trace inductance dominates. Caveats still apply: account for DC-bias derating, total rail capacitance/inrush, and regulator stability or phase margin. See Graham Sutherland, [Proper decoupling practices, and why you should leave 100nF behind](https://codeinsecurity.wordpress.com/2025/01/25/proper-decoupling-practices-and-why-you-should-leave-100nf-behind/).

Keep the `.zen` primitive if the surrounding circuit is board-specific, underspecified, already handled by another package, or blocked by untrusted symbol/footprint/pin data.

If one IC has fundamentally different schematic topologies for different modes, keep them in the same component Zener package and select or expose the topology there when practical. Split only when the public API or schematic topology is too different to keep coherent.

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

Build after each major block. If package imports or dependencies changed, run `pcb sync` first:

```bash
pcb sync  # when imports/dependencies changed
pcb build -Wstyle components/<Manufacturer>/<NAME>
```

Review sourceability for relevant public `.zen` entrypoints, especially reference designs with passives or generics:

```bash
pcb bom components/<Manufacturer>/<NAME>/<entrypoint>.zen -f json
```

Format before finishing:

```bash
pcb fmt components/<Manufacturer>/<NAME>
```

Before finishing a component package, check the expected completion points:

- high-quality symbol, following `kicad-symbol`
- accurate footprint against trusted package data
- embedded STEP model in the footprint
- component `.zen` with clean public `io()`s and appropriate interfaces
- reference circuitry in the component `.zen` when warranted
- `Layout()` included when the `.zen` contains reference circuitry
- clean `pcb build -Wstyle components/<Manufacturer>/<NAME>`
- sourceability reviewed with `pcb bom <entrypoint>.zen -f json`

This checklist is diagnostic, not permission to do sketchy work. If a trusted STEP, sourceable BOM, exact footprint evidence, or another item cannot be satisfied, do not fake it or invent data. Call out what is missing, what you checked, and the impact for the user.
