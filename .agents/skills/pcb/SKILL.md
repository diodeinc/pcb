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

Use `pcb doc spec` for language reference. Use `pcb doc --package @stdlib` to explore stdlib.

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
pcb build [PATHS...]     # Build and validate
pcb fmt [PATHS...]       # Format .zen files
pcb bom <FILE>           # Generate BOM
pcb fork add <URL>       # Fork dependency for local dev
pcb fork remove <URL>    # Remove fork
pcb doc [PATH]           # View language documentation
pcb doc [PATH] --list    # List all sections in the [PATH] page
pcb doc --package <PKG>  # View docs for a Zener package
```

## MCP Tools

| Tool | Use |
|------|-----|
| `search_registry` | Find modules/components (try FIRST) |
| `search_component` | Search Diode database (fallback) |
| `add_component` | Download component to workspace |

## Before Writing .zen Code

Use `pcb doc` to read documentation.

### Language reference

```bash
pcb doc spec --list        # List all sections in the spec page
pcb doc spec/io            # View specific section
pcb doc spec               # View full language specification
```

Key pages: `spec` (language spec), `packages` (dependency management)

### Package docs (stdlib, registry)

View docs for any Zener package to understand its modules, functions, and types:

```bash
pcb doc --package @stdlib                                           # Standard library
pcb doc --package ../path/to/local/package                          # Local package
pcb doc --package github.com/diodeinc/registry/module/<xyx>@0.1.0   # Remote package with version
```

The output includes a  `<!-- source: /path/to/checkout -->` comment with local filesystem path, so you can read the actual .zen files for implementation details.
