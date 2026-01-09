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
| `get_zener_docs` | Get language spec URLs |

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

Call `get_zener_docs` to get the language spec URL. Zener has unique syntax for modules, components, and nets that differs from Python/Starlark.
