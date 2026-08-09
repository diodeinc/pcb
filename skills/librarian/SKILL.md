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
- Do not manually create scratch footprints or synthetic 3D models without explicit user confirmation. “Add a footprint/model” means find, verify, and embed trusted artifacts first. When no provider or library footprint exists for the part, generate one from the manufacturer datasheet with the `kicad-footprint` skill; if the datasheet cannot establish the geometry, stop and ask.
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

Once search and family scoping show that a new package is the right home, establish its final directory and create `pcb.toml` before package-local authoring. `pcb.toml` declares the package boundary: it makes the work discoverable as a module and gives its evolving artifacts a stable home while curation continues.

## Family-First, Symbol-First

When a request names one MPN, first look for the related part family before authoring. A good component package usually covers all parts with the same physical package/footprint, pinout, feature set, and fundamental schematic topology.

Curate symbols before writing reference-design `.zen`. The symbol library defines the functional variants and primitive interface that the `.zen` package will select and wire.

A typical flow:

1. Understand the request and intended deliverable, including whether reference circuitry is warranted.
2. Find the related part group/family.
3. Establish the package directory and `pcb.toml`.
4. Fetch or import ECAD artifacts.
5. Clean the symbols against the datasheet.
6. Represent each functional variant symbol without duplicating order-code variants.
7. Validate the footprint candidate against the datasheet before curating it further.
8. Clean the footprint and embedded STEP against the datasheet.
9. Ensure the footprint has an embedded STEP: find and embed any referenced local model, otherwise download a matching model and embed it with `pcb embed-step`.
10. Write the primitive API, reference circuitry, or selector logic.

Treat this as the default direction, not a rigid script. Focused patches may only touch one stage.

Functional variants need symbols; order-code variants do not. For example, fixed-output LDO voltages get separate symbols because the selected silicon changes electrical behavior, but tape/reel, temperature grade, RoHS, and packing suffixes do not.

Use `kicad-symbol` for symbol-file structure, editing, `extends`, rendering, and signature rules. The librarian-level rule is to curate the family symbols before `.zen` work and to cover functional variants without duplicating order-code variants.

## Artifact Acquisition

Use `pcb component` for authenticated catalog and EDA access. The CLI resolves
the configured API server and authentication. Use JSON output when composing
commands with `jq`.

```text
pcb component search QUERY
  [--backends cse,lcsc,ncti | --backends none] [--limit 1..100] -f json
  -> [{mpn, manufacturer, cse, lcsc, ncti, digikey, offers}, ...]

pcb component download
  --mpn <MPN>
  --manufacturer <MANUFACTURER>
  exactly one of:
    --cse-part-ref <REF>
    --lcsc-part-number <PART_NUMBER>
    --ncti-component-id <ID>
  -f json
  -> {mpn, manufacturer, <provider>: {
       <provider-id>, symbol_url, footprint_url, step_url
     }}
```

Search always returns an array. `cse`, `lcsc`, and `ncti` are null or contain
the provider reference, description, category, package, `symbol`, `footprint`,
`step`, and `datasheet_url`. `digikey` contains catalog metadata, including
`datasheet_url`; `offers` contains sourcing data. Download returns only the
selected provider. Its `footprint_url` and `step_url` may be null.

| Provider | Search reference | Download option |
| --- | --- | --- |
| CSE | `.cse.part_ref` | `--cse-part-ref` |
| LCSC | `.lcsc.part_number` | `--lcsc-part-number` |
| NCTI | `.ncti.component_id` | `--ncti-component-id` |

Search merges catalog and EDA data by canonical manufacturer and MPN. Omit
`--backends` to search all EDA providers and enrich results with DigiKey data.
Use `--backends none` for DigiKey catalog data without EDA providers. Require
an exact MPN and manufacturer match before using a provider reference. Download
with exactly one provider reference so the symbol, footprint, and STEP remain
one coherent asset set. Download URLs are signed and temporary.

Prefer URL references over checked-in PDFs. Use the selected EDA provider's
verified `datasheet_url` in the curated symbol's `Datasheet` property. If that
provider has no usable datasheet URL, use `digikey.datasheet_url` from the same
search row. Keep the source URL in metadata and use `datasheet-reader` to
inspect it; never copy the PDF into `docs/`.

Run the acquisition stages below only after registry search confirms that no suitable package exists. A published package returned by registry search still counts as existing content; inspect and patch that package instead of creating a duplicate.

For a justified new package, keep acquisition inspectable: select one exact search row, request one provider's asset set, select the datasheet URL, then fetch the signed assets. Pass provider references exactly as returned. If a returned reference cannot be downloaded, report the API or provider failure; do not rewrite provider IDs or add client-side fallback logic.

### Search One Part

```bash
search_json="/tmp/tps54331dr-search.json"
part_json="/tmp/tps54331dr-part.json"

pcb component search TPS54331DR \
  --backends cse \
  --limit 10 \
  -f json > "$search_json"

jq -e '
  [.[] | select(
    .mpn == "TPS54331DR" and
    .manufacturer == "Texas Instruments"
  )] |
  if length == 1 then .[0] else error("expected one exact match") end
' "$search_json" > "$part_json"
```

### Discover A Family

```bash
for mpn in TPS70912QDRVRQ1 TPS70933QDRVRQ1 TPS70950QDRVRQ1; do
  pcb component search "$mpn" \
    --backends cse \
    --limit 10 \
    -f json |
    jq --arg mpn "$mpn" '
      .[] |
      select(
        .mpn == $mpn and
        .manufacturer == "Texas Instruments"
      )
    '
done
```

### Download One CSE Asset Set

```bash
part_json="/tmp/tps54331dr-part.json"
assets_json="/tmp/tps54331dr-assets.json"

jq -e '.cse.symbol and .cse.footprint and .cse.step' "$part_json" \
  > /dev/null

pcb component download \
  --mpn "$(jq -r '.mpn' "$part_json")" \
  --manufacturer "$(jq -r '.manufacturer' "$part_json")" \
  --cse-part-ref "$(jq -r '.cse.part_ref' "$part_json")" \
  -f json > "$assets_json"

jq -e '.cse.symbol_url and .cse.footprint_url and .cse.step_url' \
  "$assets_json" > /dev/null
```

### Select The Datasheet URL

```bash
part_json="/tmp/tps54331dr-part.json"
datasheet_url="$(
  jq -er '.cse.datasheet_url // .digikey.datasheet_url' "$part_json"
)"
printf '%s\n' "$datasheet_url"
```

### Fetch The Signed Assets

```bash
assets_json="/tmp/tps54331dr-assets.json"
out="components/Texas_Instruments/TPS54331DR"

mkdir -p "$(dirname "$out")"
mkdir "$out"
touch "$out/pcb.toml"
curl -fL "$(jq -er '.cse.symbol_url' "$assets_json")" \
  -o "$out/TPS54331DR.kicad_sym"
curl -fL "$(jq -er '.cse.footprint_url' "$assets_json")" \
  -o "$out/TPS54331DR.kicad_mod"
curl -fL "$(jq -er '.cse.step_url' "$assets_json")" \
  -o "$out/TPS54331DR.step"
```

Treat downloaded artifacts as inputs to curation, not finished registry content. Verify the symbol, pins, footprint, package, datasheet, sourcing fields, and model against manufacturer evidence. Set the symbol's `Datasheet` property, embed the verified STEP, author the package `.zen` and README, and complete the build, BOM, formatting, and sourceability checks below.

When no provider or library footprint exists for the part, generate one from the manufacturer datasheet with the `kicad-footprint` skill. Copy its workspace deliverables into the package directory: `candidate.kicad_mod` becomes `<footprint>.kicad_mod`, and `generator-input.yaml` becomes `<footprint>.yaml` beside it — same stem, same directory. The YAML carries the provenance and the generator version, and it is what makes the footprint editable at the source rather than by patching S-expressions.

### Footprint Validation

Validate every footprint candidate against the manufacturer datasheet before curating it. A provider offering it for this MPN is not evidence that it matches the part, and neither is resemblance to another footprint.

Resolve the part first — manufacturer, exact MPN including orderable suffix, package variant, and drawing code. A document is authoritative for a part only when its publisher is the part's manufacturer, or when a manufacturer document establishes a second source. A matching MPN string is not enough: commodity part numbers such as `FS8205A` and `DW01A` are used by several manufacturers, and a third-party datasheet cannot establish the geometry of a part it does not cover.

Prefer the manufacturer land pattern for the exact variant, then the package drawing, then an explicitly associated JEDEC or other standard, then a documented IPC-7351 derivation from cited package limits. Only an explicit land pattern supports calling a footprint datasheet-exact; a package drawing alone supports a verified IPC derivation.

Write down the required geometry before opening the candidate, then read its pads, holes, and slots and state the values you measured. Deriving the requirement from the artifact produces agreement rather than validation. KiCad's y axis points down, so a pad at negative y sits *above* the origin — check the sign before concluding anything about pin 1 or handedness.

The footprint either passes or it does not. It passes when every requirement the evidence establishes is satisfied; differences in silkscreen, courtyard, text, metadata, or the 3D model do not fail it.

On a pass, record the evidence on the footprint itself, in the two standard fields KiCad maintains and imported footprints leave empty. Set `Datasheet` to the document that established the geometry — usually the same part datasheet used for the symbol, since one document typically carries the pinout, the package drawing, and the land pattern, so use the same URL in both; point it at a separate document only when a separate one established the geometry, such as a standalone manufacturer package drawing or an associated JEDEC outline. Set `Description` to the package, the basis, and the fact that it passed — for example `SOT-23-5 (TI DBV0005A); validated against manufacturer land pattern`, or `16-lead LFCSP CP-16-27; IPC-7351 derivation from package drawing, validated`. Keep it to one line: this field appears in KiCad's footprint chooser, so it should read as a description that happens to carry the basis, not as a changelog.

On a failure, report what you checked and ask. Say whether the evidence contradicts the footprint or was insufficient to establish what the footprint must be, since that decides whether the next step is another source, a generated footprint, or better evidence. Do not substitute a generated footprint for one that failed validation without asking.

If a trusted STEP model is unavailable, report what you checked and ask before creating scratch geometry. After approved scratch work, label it as scratch/generated, cite the evidence used, render/verify it, and call out the risk in the README and completion report.

Upgrade imported KiCad files before editing. Keep one verified model transform before embedding because `pcb embed-step` rewrites every model reference.

```bash
kicad-cli sym upgrade <symbol.kicad_sym>
kicad-cli fp upgrade <package-directory>
rg -n '^\s*\(model ' <footprint.kicad_mod>
pcb embed-step <footprint.kicad_mod> <model.step>
```

Do not commit the standalone STEP after verifying the embedded model.

## Package Shape

New reusable registry content belongs in a component package path:

```text
components/<Manufacturer>/<NAME>/
├── <NAME>.zen                  # primitive package, or primary reference design
├── <reference-design>.zen       # optional additional reference designs
├── <NAME>.kicad_sym
├── <NAME or footprint>.kicad_mod
├── <NAME or footprint>.yaml     # generator input; same stem as the .kicad_mod; only for a generated footprint
├── pcb.toml
└── README.md
```

Include a real `.kicad_mod`. Note in its `Description` field whether it is datasheet-exact, KiCad-stock-derived, vendor-derived, or intentionally adjusted; footprint facts belong on the footprint.

A generated footprint also carries its `kicad-footprint` generator input beside it, renamed from the skill's workspace `generator-input.yaml` to share the footprint's stem (`Foo.kicad_mod` → `Foo.yaml` in the same package directory). Keep it: the geometry cannot be recovered from the footprint file, because several generator families and several sets of declared dimensions produce identical lands, and the file does not distinguish a declared value from a generator default. Without the input, the next change to that footprint means re-deriving it from the datasheet or editing copper by hand. Downloaded and stock footprints have no generator input and do not gain one.

Every top-level `.zen` file in a directory containing `pcb.toml` is a public package entrypoint. Each should therefore represent a coherent primitive component or reusable subschematic/reference design with a complete public API. A package may contain multiple entrypoints when one curated part/family has multiple useful datasheet-backed application circuits; avoid thin local wrappers that only re-export another `.zen`.

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
- evidence for important choices and sourceability compromises

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

For selectable families, use a compact table/list of variants with MPN, symbol, limits, and properties. Filter by `config()` values, use the first match as `part=`, and put remaining drop-in equivalents *of that same selection* in `properties={"alternatives": ...}`.

`alternatives` is for parts a sourcing system may freely swap in without changing behavior, footprint, or fit. Functional/mechanical variants that `config()` chooses between are mutually exclusive; true equivalents are second sources or order-code siblings (tape/reel, RoHS, temp grade) of the selected variant.

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
- accurate footprint against trusted package data, with its `Datasheet` and `Description` recording the validated evidence and basis
- embedded STEP model in the footprint
- component `.zen` with clean public `io()`s and appropriate interfaces
- reference circuitry in the component `.zen` when warranted
- `Layout()` included when the `.zen` contains reference circuitry
- clean `pcb build -Wstyle components/<Manufacturer>/<NAME>`
- sourceability reviewed with `pcb bom <entrypoint>.zen -f json`

This checklist is diagnostic, not permission to do sketchy work. If a trusted STEP, sourceable BOM, exact footprint evidence, or another item cannot be satisfied, do not fake it or invent data. Call out what is missing, what you checked, and the impact for the user.
