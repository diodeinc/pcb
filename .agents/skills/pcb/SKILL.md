---
name: pcb
description: Work with PCB designs in the Zener hardware description language. Use when writing or editing `.zen` files, building schematics, searching for components or Zener packages, reading datasheets with `pcb scan`, or reading/updating KiCad `.kicad_sym` symbol metadata.
---

# PCB

Zener is a Starlark-based HDL for PCB design. `.zen` files define components, nets, interfaces, and modules. Key concepts:
- **Net**: electrical connection between pins
- **Component**: physical part with symbol, footprint, pins
- **Interface**: reusable grouped connection pattern (e.g., `Spi`, `I2c`, `Uart`)
- **Module**: hierarchical subcircuit, instantiated with `Module("path.zen")`

**IMPORTANT: Before writing or modifying .zen code, run `pcb doc spec` to read the language specification.** The spec covers syntax, built-in functions, type system, and common patterns. For specific topics: `pcb doc spec --list` shows all sections, `pcb doc spec/<section>` reads one section.

**Prefer stdlib generics** (`@stdlib/generics/`) over specific components when possible. Generics like `Resistor`, `Capacitor`, `Led` are parameterized by value/package and resolved to real parts at build time.

**Common imports**: `load("@stdlib/interfaces.zen", "Power", "Ground", "Spi", "I2c", ...)` for standard net types and interfaces.

## Datasheets

Use `pcb scan` when a task depends on a datasheet or technical PDF.

- Input: local `.pdf` path or `http(s)` URL
- Command: `pcb scan <input>`
- Output: stdout is the resolved markdown path
- Next step: read the markdown file, not the raw PDF
- Images are linked from the markdown

Examples:

```bash
pcb scan ./TPS54331.pdf
pcb scan https://www.ti.com/lit/gpn/tca9554
```

## CLI Commands

```bash
# scaffolding
pcb new workspace <NAME> --repo <URL>  # Create new workspace with git init
pcb new board <NAME>                   # Add board to existing workspace (boards/<NAME>/)
pcb new package <PATH>                 # Create package at path (modules, etc.)
```

```bash
pcb build [PATH]         # Build and validate (default: cwd)
pcb fmt [PATH]           # Format .zen files (default: cwd)
pcb bom <FILE> -f json   # Generate BOM as JSON with availability data
pcb fork add <URL>       # Fork dependency for local dev
pcb fork remove <URL>    # Remove fork
```

## Component Search & Add

```bash
pcb search -m registry:modules buck -f json
pcb search -m registry:components usb-c -f json
pcb search -m web:components STM32F103C8T6 -f json
pcb new component --component-id <ID> --part-number <MPN> --manufacturer <MFR>
```

- `pcb search -m registry:modules ... -f json`: Search registry modules and reference designs. Includes descriptions, package URLs, dependencies/dependents, and availability data for component-backed results.
- `pcb search -m registry:components ... -f json`: Search registry components. Includes MPN/manufacturer metadata, availability, supplier metadata, and dependency/dependent context.
- `pcb search -m web:components ... -f json`: Search Diode's web component database. Includes `component_id`, pricing/stock/offers, datasheets, model availability, and source.
- `pcb new component --component-id ...`: Download a web component into the current workspace. `--part-number` and `--manufacturer` are optional fallbacks.

## MCP Tools

| Tool | Use |
|------|-----|
| `read_kicad_symbol_metadata` | Read structured KiCad symbol metadata (`primary` typed properties + `custom_properties`) from a `.kicad_sym` symbol. Supports `resolve_extends` and optional raw property map output. |
| `write_kicad_symbol_metadata` | Strict full-write of symbol metadata. Input becomes the full metadata state (unset fields/properties are removed). Supports `dry_run` for previewing changes. |
| `merge_kicad_symbol_metadata` | RFC 7396 JSON Merge Patch update for metadata. Use for incremental edits (object keys set/replace, `null` deletes, arrays replace whole). Supports `dry_run`. |

Metadata tool notes:
- These metadata tools are intended for `pcb mcp eval` scripted/structured metadata edits.
- Prefer `read_kicad_symbol_metadata` first, then choose either strict `write_kicad_symbol_metadata` or incremental `merge_kicad_symbol_metadata`.
- Canonical KiCad mapping lives under `metadata.primary`:
  - `Reference` <-> `primary.reference`
  - `Value` <-> `primary.value`
  - `Footprint` <-> `primary.footprint`
  - `Datasheet` <-> `primary.datasheet`
  - `Description` <-> `primary.description`
  - `ki_keywords` <-> `primary.keywords` (array in JSON, space-separated string in `.kicad_sym`)
  - `ki_fp_filters` <-> `primary.footprint_filters` (array in JSON, space-separated string in `.kicad_sym`)
- `custom_properties` is only for non-canonical properties. Do not put canonical keys there.
- Legacy note: older symbols may use `ki_description`. Reads normalize it to `primary.description` when canonical `Description` is absent; writes emit canonical `Description`.
- Common gotcha:
  - Wrong: `metadata_patch: {custom_properties: {ki_keywords: "powerline transceiver CAN"}}`
  - Right: `metadata_patch: {primary: {keywords: ["powerline", "transceiver", "CAN"]}}`

## Documentation

```bash
pcb doc spec               # Full language specification
pcb doc spec --list        # List all spec sections
pcb doc spec/<section>     # Read specific section (e.g., spec/io, spec/module)
pcb doc packages           # Dependency management docs
pcb doc docs_bringup       # Guide on writing bringup docs in markdown 
pcb doc docs_changelog     # Guide on writing a changelog after every change
```

Package docs (stdlib, registry packages):

```bash
pcb doc --package @stdlib                                           # Standard library docs
pcb doc --package @stdlib --list                                    # List files as tree
pcb doc --package @stdlib/generics                                  # Filter to subdirectory
pcb doc --package github.com/diodeinc/registry/module/<xyz>@0.1.0   # Remote package
```

## Part Sourcing & BOM Matching

Generic components are matched to "house parts" (pre-qualified, good availability). Warnings like `No house cap found for ...` or `No house resistor found for ...` mean no house part matches the spec—adjust the spec or specify `mpn` + `manufacturer` to use a specific part.

`pcb bom <FILE> -f json` outputs sourcing data with `availability_tier` (`"plenty"` | `"limited"` | `"insufficient"`) and distributor `offers` by region.
