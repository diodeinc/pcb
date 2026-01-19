---
name: pcb
description: Work with PCB designs in the Zener hardware description language. Use when writing or editing .zen files, building schematics, searching for components, or searching for Zener packages.
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
pcb mcp eval -f script.js                              # Run from file
```

Use `pcb mcp eval` to chain multiple tool calls. Tools available via `tools.name({...})`, metadata at `tools._meta`.

## MCP Tools

| Tool | Use |
|------|-----|
| `search_registry` | Find modules/components in Zener registry (try FIRST). Returns pricing and availability data. |
| `search_component` | Search Diode online database (fallback). Returns pricing and availability data. |
| `add_component` | Download component to workspace |

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
