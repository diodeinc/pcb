---
name: pcb
description: Work with PCB designs in the Zener hardware description language. Use when writing or editing .zen files, building/testing schematics, generating KiCad layouts, or searching for components.
---

# PCB Skill

## MCP Tools (available when skill loads)

| Tool | Use |
|------|-----|
| `search_registry` | Find modules/components (try FIRST) |
| `search_component` | Search Diode database (fallback) |
| `add_component` | Download component to workspace |
| `run_layout` | Sync schematic to KiCad |

## CLI Commands

```bash
pcb build [PATHS...]     # Build and validate
pcb test [PATHS...]      # Run TestBench tests
pcb layout [PATHS...]    # Generate layout, open KiCad
pcb fmt [PATHS...]       # Format .zen files
pcb bom <FILE>           # Generate BOM
pcb open [PATHS...]      # Open existing layout
pcb update               # Update dependencies
pcb fork <URL>           # Fork dependency for local dev
pcb unfork <URL>         # Remove fork
pcb doc [PATH]           # View embedded documentation
```

## Key Flags

| Flag | Effect |
|------|--------|
| `-D warnings` | Treat warnings as errors |
| `-S <kind>` | Suppress diagnostics by kind |
| `--locked` | CI mode: fail if lockfile changes |
| `--offline` | Use only cached/vendored deps |
| `--check` | Check only, don't modify (fmt/layout) |

## Before Writing .zen Code

Use `pcb doc` to read the embedded Zener language documentation.

```bash
pcb doc                    # List all documentation pages
pcb doc spec               # View full language specification
pcb doc spec/net           # View specific section (fuzzy matched)
pcb doc spec --list        # List sections in a page
```

Key pages:
- `spec` - Language specification (types, builtins)
- `packages` - Dependency management and versioning
