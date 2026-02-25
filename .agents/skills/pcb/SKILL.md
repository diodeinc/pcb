---
name: pcb
description: Work with PCB designs in the Zener hardware description language. Use when writing or editing `.zen` files, building schematics, searching for components or Zener packages, or reading/updating KiCad `.kicad_sym` symbol metadata. Also use this skill when you need to resolve datasheets into local markdown and images for analysis.
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

## Datasheet Workflow (Important)

When datasheet content is needed, **call `resolve_datasheet` first**.

Use it because it:
- Produces model-readable local `datasheet.md` + `images/` files.
- Reuses cache for faster repeated analysis.
- Avoids brittle website-specific PDF fetch behavior.

Usage:
- `pdf_path` when you already have a local PDF.
- `datasheet_url` when you only have a URL.
- `kicad_sym_path` when starting from a KiCad symbol file.
- Add `symbol_name` when the `.kicad_sym` contains multiple symbols.

Avoid ad-hoc `curl`/`wget` or raw web reads unless `resolve_datasheet` fails.

Examples:

```bash
pcb mcp eval 'tools.resolve_datasheet({datasheet_url: "https://www.ti.com/lit/gpn/tca9554"})'
pcb mcp eval 'tools.resolve_datasheet({pdf_path: "../registry/components/TCA9554DBQR/TCA9554DBQR.pdf"})'
pcb mcp eval 'tools.resolve_datasheet({kicad_sym_path: "../registry/components/TCA9554DBQR/TCA9554DBQR.kicad_sym"})'
pcb mcp eval 'tools.resolve_datasheet({kicad_sym_path: "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Analog_ADC.kicad_sym", symbol_name: "AD574A"})'
```

## CLI Commands

```bash
# scaffolding
pcb new --workspace <NAME>   # Create new workspace with git init
pcb new --board <NAME>       # Add board to existing workspace (boards/<NAME>/)
pcb new --package <PATH>     # Create package at path (modules, etc.)
pcb new --component          # Interactive TUI (use search_component + add_component MCP tools instead)
```

```bash
pcb build [PATH]         # Build and validate (default: cwd)
pcb fmt [PATH]           # Format .zen files (default: cwd)
pcb bom <FILE> -f json   # Generate BOM as JSON with availability data
pcb fork add <URL>       # Fork dependency for local dev
pcb fork remove <URL>    # Remove fork
```

```bash
# JavaScript scripting with MCP tools
pcb mcp eval 'tools.search_registry({query: "buck"})'  # Search registry
pcb mcp eval 'tools.resolve_datasheet({pdf_path: "./part.pdf"})'  # Extract datasheet to markdown + images
pcb mcp eval -f script.js                              # Run from file
```

Use `pcb mcp eval` to chain multiple tool calls. Tools available via `tools.name({...})`, metadata at `tools._meta`.

## MCP Tools

| Tool | Use |
|------|-----|
| `search_registry` | Find modules/components in Zener registry (try FIRST). Returns pricing and availability data. |
| `search_component` | Search Diode online database (fallback). Returns pricing and availability data. |
| `add_component` | Download component to workspace |
| `resolve_datasheet` | Resolve datasheet input (`datasheet_url`, `pdf_path`, or `kicad_sym_path` + optional `symbol_name`) into cached local `datasheet.md` + `images/` paths |
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
