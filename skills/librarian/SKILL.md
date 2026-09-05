---
name: librarian
description: Create or repair reusable component and module packages in a PCB registry.
---

# Librarian

Create sourceable, evidence-backed packages that board designers can search
and instantiate. Work in the current registry; dispatch another librarian
only when the user asks. Use the artifact skills as needed: `kicad-symbol`,
`kicad-footprint`, `zener-language`, and `module-layout`.

## Package scope

Search before creating a package:

```bash
pcb search -m registry:modules <query> -f json
pcb search -m registry:components <query> -f json
```

Inspect and extend a suitable existing package, including published results.
Group a family when it shares footprint, pinout, feature set, and fundamental
schematic topology. Use separate packages when those contracts differ. For a
new package, establish its final directory and `pcb.toml` before authoring so
its evolving content is discoverable.

Keep simple parts such as connectors and passives primitive unless user
requirements or manufacturer guidance establish an application circuit. An
unspecified request for a reference design does not justify inventing board
wiring, protection, passive values, or a layout. Include datasheet-required or
strongly recommended support circuitry when warranted by the requested package;
respect a primitive-only request.

Curate the relevant symbols, footprints, and pin maps before building circuitry
on them. Do not invent datasheet facts, limits, or sourcing claims. Focused
repairs need only the relevant curation stages.

## Acquire artifacts

Use `pcb component` for authenticated catalog and EDA access; it resolves the
configured API and credentials. Use command-specific help for current flags.

```text
pcb component search QUERY [--backends cse,lcsc,ncti | --backends none] -f json
  -> [{mpn, manufacturer, cse, lcsc, ncti, digikey, offers}, ...]

pcb component download --mpn MPN --manufacturer MANUFACTURER \
  <one provider option> -f json
  -> {mpn, manufacturer, <provider>: {<provider-id>, symbol_url, footprint_url, step_url}}
```

| Provider | Search reference | Download option |
| --- | --- | --- |
| CSE | `.cse.part_ref` | `--cse-part-ref` |
| LCSC | `.lcsc.part_number` | `--lcsc-part-number` |
| NCTI | `.ncti.component_id` | `--ncti-component-id` |

Search returns an array merged by canonical manufacturer and MPN. Select an
exact match and pass its reference unchanged. Provider records may be null;
otherwise they contain availability flags, package metadata, and
`datasheet_url`. Omit `--backends` for all EDA providers plus DigiKey metadata;
`--backends none` requests DigiKey catalog data only. `offers` holds sourcing
information.

Download one provider's coherent asset set. Only that provider is returned,
and `footprint_url` or `step_url` may be null. Fetch available signed URLs to
task-local files with `curl -fL`; the URLs expire. If an asset remains
unavailable after a reasonable attempt, use another trusted provider or the
authoring path below and report the gap.

Use the selected provider's verified `datasheet_url`, falling back to
`digikey.datasheet_url` from the same search row. Keep the source URL in
metadata and inspect it with `datasheet-reader`; do not check downloaded PDFs
into the package.

## Curate symbols, footprints, and models

- Use `kicad-symbol` for pin signatures, ERC types, units, inheritance, and
  rendering. Functional silicon variants need distinct symbols; ordering-only
  variants such as tape/reel, temperature grade, or RoHS suffixes do not.
- Use `kicad-footprint` to validate the exact package geometry. An MPN match or
  resemblance to a reference footprint does not establish correctness.
  Requested creation or repair includes replacing an incorrect candidate or
  generating one from authoritative evidence; ask only if required geometry
  remains unresolved. Review-only work produces findings.
- For an accepted footprint, set `Datasheet` to the authoritative geometry
  source and `Description` to its package and evidence basis. Include a real
  `.kicad_mod`; record whether it is vendor-derived, stock-derived, generated,
  or intentionally adjusted.
- For generated footprints, retain the final generator YAML beside the
  footprint with the same stem (`Foo.kicad_mod` and `Foo.yaml`). Preserve the
  provenance and generator version required by `kicad-footprint`. Downloaded
  and stock footprints do not need invented generator inputs.
- Embed a verified STEP for the exact package, or make its absence and impact
  explicit. A known-wrong model is not an acceptable substitute. Scratch 3D
  geometry requires explicit user confirmation; after approval, label it as
  generated, cite its basis, inspect it, and disclose its limits.

Upgrade imported KiCad files before editing. Preserve one verified model
transform before embedding because `pcb embed-step` rewrites every model
reference:

```bash
kicad-cli sym upgrade <symbol.kicad_sym>
kicad-cli fp upgrade <package-directory>
pcb embed-step <footprint.kicad_mod> <model.step>
```

Inspect the embedded result; do not commit the standalone STEP.

## Public API and reference circuits

Component packages belong under `components/<Manufacturer>/<NAME>/` with
`pcb.toml`, `.zen` entrypoints, curated symbol/footprint files, and `README.md`.
Use `modules/` for higher-level functional blocks; do not add to the legacy
`reference/` or `connectors/` trees.

Each top-level `.zen` beside `pcb.toml` is a public entrypoint. Represent one
coherent primitive or application circuit per entrypoint; instantiate
`Component()` and its support circuitry directly instead of adding a thin
primitive-only wrapper. Additional application circuits can use separate
entrypoints. Follow `zener-language` for stable topology and layout declarations.

Expose application-level IO and configs. Keep implementation nodes internal
unless access is necessary. Include only reusable support circuitry: decoupling,
feedback, compensation, bootstrap, bias, reset, straps, or a supported
application topology. Leave board-specific, underspecified, already-provided,
or unverified circuitry to the integrator. A `Layout()` is useful when the
reference circuit has reusable physical relationships worth capturing.

Choose decoupling from the datasheet, effective capacitance, ESL, placement,
inrush, and regulator stability. Prefer one compact low-ESL MLCC per supply
pin when valid; do not add a 100 nF/bulk pair by habit. A larger capacitor in
the same package may suffice, but account for DC bias and resonance.

Put usage examples and concise integration notes in `README.md`. Put design
evidence in the `.zen` docstring: application mode, exact package/family,
operating envelope, IO/config assumptions, included versus integrator-owned
circuitry, physical constraints, and the datasheet equations or tables behind
important choices. If consumers must change code, layout, or assumptions to
adopt an update, document the migration and mark it breaking.

## Families and sourcing

Derive package names from the functional MPN pattern, dropping ordering-only
suffixes and using lowercase `x` for meaningful family differences. Excessive
masking indicates an overbroad family. A selector can use a compact table of
MPNs, symbols, limits, and properties filtered by `config()`.

Use the first match as `part=` and put remaining drop-in equivalents of that
selection in `properties={"alternatives": ...}`. Alternatives must be freely
swappable without changing electrical behavior, footprint, or fit; mutually
exclusive functional or mechanical configurations are not alternatives.

For otherwise equivalent MPNs, prefer automated-assembly packaging: tape/reel
or cut tape, then tray, tube, and bulk. Prefer manufacturer pickup aids for
non-flat parts; flag an unavoidable pickup limitation.

Use `preferred-parts` when selecting generics, without weakening requirements.
Express requirements through stdlib generic parameters; set `mpn` or
`manufacturer` only when those parameters cannot represent the design. Use
`pcb bom <entrypoint>.zen -f json` when checking changed part selection or
sourceability.

Complete the relevant artifact checks and Zener validation. Report verified
results, public API or layout effects, sourcing compromises, and any remaining
evidence or model gaps. A focused repair does not require re-curating an
unchanged package.
