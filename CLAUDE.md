# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read docs/pages/spec.mdx to understand the Zener language specification.

## Commands

### Build and Development
- `cargo build` - Build the workspace
- `cargo test` - Run all tests
- `cargo run -- build [files]` - Build and validate Zener (.zen) PCB designs
- `cargo run -- fmt [files]` - Format .zen files using ruff formatter
- `cargo run -- layout [files]` - Generate KiCad PCB layout files from .zen designs

### Testing
- `cargo test --package <crate-name>` - Run tests for specific crate
- `cargo test --package ipc-2581` - Run IPC-2581 parser tests (includes 54 official test cases)
- `cargo test --package ipc-2581 --release` - Run IPC-2581 tests with optimizations (recommended for full test suite)
- `cargo insta test` - Run snapshot tests (uses insta crate)

Never run `cargo insta accept`. Let me review and approve any changes.

## Architecture

This is a Rust workspace implementing the Zener language - a Starlark-based PCB design language. The project is organized into specialized crates:

### Core Language & Runtime
- **`pcb-zen-core`** - Core language implementation including:
  - Component, Net, Interface, Module types
  - Configuration system with `config()` and `io()` functions
  - Type system and validation
  - Graph representations for circuit connectivity
- **`pcb-zen`** - Main runtime with LSP server, DAP (debug adapter), and Starlark execution
- **`pcb-zen-wasm`** - WebAssembly bindings for browser execution

### Main CLI Tool
- **`pcb`** - Primary CLI tool providing build, layout, open, fmt, test, lsp commands

### Schematic & Layout Generation
- **`pcb-sch`** - Schematic representation and netlist generation
- **`pcb-layout`** - PCB layout generation from schematics
- **`pcb-kicad`** - KiCad file format parsing and generation
- **`pcb-sexpr`** - S-expression parser for KiCad files

### Editor Integration & Language Services
- **`pcb-starlark-lsp`** - Language Server Protocol implementation for .zen files

### File Format Support
- **`ipc-2581`** - IPC-2581 XML parser for PCB manufacturing data exchange
  - Reference documentation in `crates/ipc-2581/reference/IPC-2581C.md` contains the full specification
  - Tested against 54 official IPC-2581 Rev C test cases

### Utilities & Support
- **`pcb-ui`** - Terminal UI components (spinners, progress bars)
- **`pcb-command-runner`** - External command execution utilities
- **`pcb-eda`** - EDA tool integration for symbols/footprints
- **`pcb-fmt`** - Code formatter for .zen files using ruff
- **`pcb-sim`** - Simulation capabilities
- **`pcb-test-utils`** - Testing utilities shared across crates

### Key Language Features
The Zener language extends Starlark with PCB-specific constructs:
- **Components** - Physical parts with pins, footprints, properties
- **Nets** - Electrical connections between pins
- **Interfaces** - Reusable connection patterns (e.g., Power, SPI, I2C)
- **Modules** - Hierarchical design units with `config()` parameters and `io()` interfaces
- **Layout** - PCB layout generation directives

### File Structure
- `.zen` files contain Zener language code
- `@stdlib/` prefix for standard library modules
- Module loading uses relative paths or `@stdlib/` namespace
- Generated layouts output to `layout/` directories by default

### Important Implementation Details
- Uses custom fork of `starlark-rust` with PCB-specific extensions
- Language server provides real-time diagnostics and completions
- Supports eager evaluation for interactive feedback
- Integration with KiCad 9.x for layout editing and manipulation
- Specification documentation in `docs/spec.md` must be kept in sync with implementation changes

## Rules

### No f-strings

Zen is based on Starlark, not Python. f-strings are not supported.

### Specification Documentation Rule

When making changes to the Zener language that affect:

- Language syntax
- Built-in functions
- Core types (Net, Component, Symbol, Interface, Module)
- Load resolution mechanisms
- Module system behavior
- Type system features
- Default behaviors or aliases

You MUST update `docs/spec.md` to reflect these changes. The specification should always be in sync with the implementation.

Guidelines:

1. Add new features to the appropriate section
2. Update existing documentation if behavior changes
3. Include clear examples in Starlark syntax
4. Document both the feature and its parameters/options
