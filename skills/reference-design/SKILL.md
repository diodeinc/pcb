---
name: reference-design
description: Grow a component package into a high-quality, sourceable reusable design in Zener. Use when translating a datasheet, application note, or eval design into circuitry that should live with the component package itself — including checking for existing reusable packages first, extracting evidence, choosing sourceable passives, documenting the design in the `.zen` docstring, and validating with `pcb build`.
---

# Component Package Design

Grow component packages beyond a generated wrapper when the part benefits from reusable surrounding circuitry. A good component package captures the support circuitry, default mode, key equations, layout-sensitive notes, and evidence needed to use the part confidently.

## Hard Rules

1. Reuse before create. Search existing registry/workspace packages first.
2. Do not create a separate `reference/` package. This work belongs in the relevant `components/...` package.
3. The `.kicad_sym` file is the source of truth for the primitive component interface and pin structure. Keep the `.zen` API aligned to it.
4. The `.zen` docstring is the canonical design document. The README is for usage examples only.
5. `pcb build` warnings matter. Review them, especially BOM/sourceability warnings such as `bom.match_generic`.
5. Do not guess ambiguous passives, straps, sequencing, or oscillator details. Get evidence or stop and ask.
6. Imitate only strong exemplars. Weak/generated packages are useful for pin lookup and starting structure, not authoring style.
7. Preserve `# pcb:sch ...` comments. They carry tool-managed schematic layout metadata. Do not delete them. If you rename a referenced component, module instance, or net, update the corresponding `# pcb:sch` names too.
8. The registry still contains legacy `reference/` packages. You may inspect them for electrical structure, public API shape, and documentation quality, but do not copy their directory placement for new work.
9. If an existing registry example conflicts with this skill, this skill wins.

## Reuse Before Create

1. Search registry modules and component packages with `component-search`.
2. Search registry components or import the IC if it is missing, then inspect close matches with `pcb doc --package ...` and by reading their source.
3. If an existing package is close, prefer using it or patching it in place.
4. Grow a package beyond its generated signature only when it adds real reusable design judgment.

Do not grow the package yet if:

- the datasheet does not clearly specify the required support circuitry
- the topology still depends on unresolved system-level choices
- the result would mostly duplicate an existing package with only minor edits
- the IC symbol/component quality is too poor to wire confidently

## Strong Vs Weak Exemplars

Strong exemplars usually have most of these traits:

- minimal, integrator-facing IO instead of exposing every raw IC pin
- typed configs for real design choices
- passive values traceable to datasheet tables, equations, or app-note guidance
- optional features modeled cleanly, with evidence notes for non-obvious choices
- sourceable generic choices or documented house-part compromises
- useful usage examples, and layout/testbench artifacts when complexity warrants them

Weak/generated exemplars usually show warning signs such as:

- mostly raw IC pins exposed directly
- little or no design rationale, or magic values with no evidence
- no attempt to capture the intended application circuit
- no sourceability thinking
- README content that is generic, marketing-like, or duplicated from the code

Use weak examples for package API lookup only.

## Quality Bar

A high-quality component package is electrically faithful, narrowly scoped, reasonably sourceable, and evidence-backed. Treat the `.zen` file as the design artifact, not just executable code.

## Evidence Extraction

Extract at least:

- the exact typical application circuit or recommended topology
- supply rails, limits, sequencing, and required external passives
- strap, mode-select, reset, enable, bias, compensation, and timing networks
- equations for programmable values and any datasheet-recommended example points
- oscillator or crystal requirements, sensitive analog/high-speed connections, and thermal/layout guidance
- any package-specific caveats that change how the design should be exposed

When the datasheet is ambiguous, look for app notes, eval schematics, or nearby validated registry designs before guessing.

## Define The Public API First

Package API rules:

- Expose the application-level interface, not the raw pinout.
- Keep layout-sensitive or implementation-detail nodes internal unless external access is genuinely required.
- Expose configs only for choices an integrator should reasonably tune; do not expose every passive.
- Prefer one narrow, coherent operating mode over a sprawling universal module.
- If two operating modes materially change topology, consider separate designs instead of config explosion.

## Scaffold And Implement

### Directory layout and naming

Name the component package from the functional MPN, not the full orderable SKU.

Use these rules:

- Start from the manufacturer part number.
- Keep functional variation in the name: electrical options and package/pinout differences stay.
- Replace non-functional variation with lowercase `x`: temperature grade, reel/tray packaging, RoHS/Pb-free, and other ordering-only suffixes.
- If the only wildcarded characters would be trailing non-functional suffixes, omit the trailing `x`.
- If there is only one functional variant, do not add an unnecessary `x`.
- If the part exists in multiple footprint or pinout options, make a separate component package for each one.
- If multiple manufacturers make footprint-compatible parts with different package suffixes, use the common base name plus a clear package suffix.

Examples:

- `DP83867ISRGZR` -> `DP83867`
- `TPS3430WDRCR` -> `TPS3430WDRC`
- compatible cross-vendor variants with different package suffixes -> `L78L05_TO92`

Use `<NAME>` for the resolved package name from the rules above, for example:

```text
components/<NAME>/
├── <NAME>.zen
├── <NAME>.kicad_sym
├── pcb.toml
└── README.md
```

Scaffold with `pcb new package components/<NAME>` when creating a fresh package. If the component already exists, evolve the existing `components/...` package instead of creating a sibling package.

### File structure

Organize the `.zen` file in this order:

1. Top-of-file docstring
2. `load()`s and helper definitions
3. `io()` and `config()` definitions
4. internal nets and imports
5. main IC instantiation and supporting circuitry grouped by function
6. layout / tool-managed metadata

Group support circuitry by electrical function: power, decoupling, feedback, straps, clocks, reset, interface conditioning, protection.

Keep the `# pcb:sch ...` block intact and in sync with renames. Treat the symbol file as canonical for pins and primitive interface naming.

### Docstring policy

Include:

- what circuit/application mode this module implements
- the exact IC/package or family it targets
- the intended operating envelope, interfaces, configs, and assumptions
- evidence notes for important choices and non-obvious layout guidance
- sourceability compromises such as house-part rounding when relevant

Keep this in the `.zen` file so the code and rationale stay together.

### Comment policy

Good comment targets:

- datasheet section/table/equation references
- justification for rounded/clamped values
- optional-feature stuffing strategy or layout-sensitive placement guidance

Avoid comments that merely restate the code.

## Sourceability And BOM Quality

Read `registry/.pcb/stdlib/bom/match_generics.zen` when sourceability choices matter. The stdlib matcher only covers a constrained house catalog, so generic values, packages, dielectric choices, and voltage ratings affect whether parts match.

Use these rules:

1. Treat `pcb build` warnings as review items, especially `bom.match_generic`.
2. Prefer generic values/package/voltage combinations that match house parts when that does not compromise the design.
3. If the datasheet value does not match house parts, a nearby house value is acceptable only when the change is technically defensible.
4. Whenever you round, clamp, or substitute to land on a house-matchable value, document the reason in the docstring or a nearby comment.
5. If a generic cannot reasonably match, do not silently force a workaround. Ask the user whether they want to specify an explicit part or suppress the warning with justification.

Typical fixes are choosing the nearest valid house value above a datasheet minimum, clamping computed values to supported parts, or adjusting package/voltage choices without violating the design. Use `pcb bom <path> -f json` when you need sourcing detail beyond the matcher.

## Build Iteratively

Build after every major block, not just at the end.

```bash
pcb build components/<NAME>
```

Typical problems:

- wrong interface field names or package wiring
- missing `load()`s, bad stdlib assumptions, or ambiguous optional-feature modeling
- unmatched generics or values that are plausible electrically but not sourceable

Format when done:

```bash
pcb fmt components/<NAME>
```

## README Policy

Use it for:

- realistic instantiation examples
- different application contexts when they materially change integration
- concise consume-the-module notes only

Do not put general feature lists, design notes, or long rationale sections in the README. That belongs in the `.zen` docstring.

Minimal README shape:

````markdown
# <NAME>

## Usage

```python
MyRef = Module("github.com/diodeinc/registry/components/<NAME>/<NAME>.zen")

MyRef(
    name="U1",
    VIN=vin,
    VOUT=vout,
    GND=gnd,
)
```

## Other Usage Examples

Add additional examples only when they show materially different integration patterns.
````

## Stop Conditions

Stop and ask or gather more evidence when:

- the datasheet is unclear about a required passive, strap, or topology choice
- the design depends on unresolved system-level requirements
- the imported component/symbol quality is too poor to wire safely
- `pcb build` warnings suggest unresolved correctness or sourceability issues
- the design is drifting into a generic breakout instead of a reference module

## Final Checklist

1. Existing registry/workspace packages were checked first.
2. The package implements one coherent reusable design around the part.
3. The symbol file remains the source of truth for the primitive component interface.
4. The docstring explains the design, evidence, and any sourceability compromises.
5. `pcb build` was run and warnings were reviewed.
6. `pcb fmt` was run, and the README contains usage examples only.
