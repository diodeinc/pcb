# `pcb`: CLI for circuit boards

> PCB tooling by [Diode Computers, Inc.](https://diode.computer/)

`pcb` is a command-line utility for building PCBs. It uses the Zener language to describe
PCB schematics and provides automations on top of KiCad to build PCBs fast.

**[Read the docs](https://docs.pcb.new)** | [Language Reference](https://docs.pcb.new/pages/spec)

> [!WARNING]
> **Windows support is experimental.** Some features may be limited or unstable. For the best
> experience, we recommend using WSL2 or macOS/Linux. If you encounter issues, please open an
> issue in our [issue tracker](https://github.com/diodeinc/pcb/issues).

## Table of Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [Project Structure](#project-structure)
- [Core Concepts](#core-concepts)
- [Command Reference](#command-reference)
- [Architecture](#architecture)
- [License](#license)

## Installation

### From Installer

Install the `pcb` shim, which will download and run the right `pcbc` toolchain for each project:

```bash
curl -fsSL https://raw.githubusercontent.com/diodeinc/pcb/main/install.sh | bash
```

On Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/diodeinc/pcb/main/install.ps1 | iex"
```

The Unix installer writes `pcb` to `$HOME/.local/bin` by default. The Windows installer writes
`pcb.exe` to `%USERPROFILE%\.pcb\bin` by default. Set `PCB_INSTALL_DIR` to choose a different
directory. The installers add that directory to your user `PATH` when needed.

### From Source

```bash
# Clone the repository
git clone https://github.com/diodeinc/pcb.git
cd pcb

# Build locally
cargo build -p pcb -p pcbc

# Install local release builds for development
./install.sh --local
```

### Requirements

- [KiCad 10.x](https://kicad.org/) (for generating and editing layouts)

## Quick Start

### 1. Create Your First Design

Create a file called `blinky.zen`:

[embed-readme]:# (examples/blinky.zen python)
```python
# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

VCC = Power()
GND = Ground()
LED_ANODE = Net()

Resistor(name="R1", value="1kohm", package="0402", P1=VCC, P2=LED_ANODE)
Led(name="D1", package="0402", color="red", A=LED_ANODE, K=GND)
Board(name="blinky", layers=4, layout_path="layout/blinky")
```

### 2. Build Your Design

```bash
# Compile the design and check for errors
pcb build blinky.zen

# Output:
# ✓ blinky.zen (2 components)
```

### 3. Generate Layout

```bash
# Generate PCB layout files
pcb layout blinky.zen

# Output:
# ✓ blinky.zen (layout/blinky.kicad_pcb)
```

## Project Structure

Zener projects use one of two repository shapes.

### Board repository

A board repository contains one board plus any local modules and components it owns:

```
MyBoard/
├── pcb.toml              # Workspace and board manifest
├── pcb.sum               # Dependency lock file
├── MyBoard.zen           # Board schematic
├── layout/               # KiCad layout files
├── modules/              # Reusable circuit modules
│   └── PowerSupply/
│       ├── PowerSupply.zen
│       └── pcb.toml
├── components/           # Custom component definitions
│   └── Manufacturer/
│       └── MPN/
│           ├── MPN.zen
│           └── pcb.toml
└── vendor/               # Vendored dependencies
```

Create one with:

```bash
pcb new board MyBoard https://github.com/myorg/MyBoard
```

**Board repository `pcb.toml`:**
```toml
[workspace]
repository = "github.com/myorg/MyBoard"
pcb-version = "0.3"

[board]
name = "MyBoard"
path = "MyBoard.zen"
description = "Replace with concise board description."
```

### Registry repository

A registry repository contains reusable packages and no board:

```
registry/
├── pcb.toml              # Workspace manifest
├── pcb.sum               # Dependency lock file
├── components/           # Component packages
│   └── TPS54331/
│       ├── TPS54331.zen
│       ├── TPS54331.kicad_sym
│       ├── TPS54331.kicad_mod
│       └── pcb.toml
└── modules/              # Reusable module packages
    └── UsbCSink/
        ├── UsbCSink.zen
        └── pcb.toml
```

**Registry `pcb.toml`:**
```toml
[workspace]
repository = "github.com/myorg/registry"
pcb-version = "0.3"
```

## Core Concepts

Zener extends [Starlark](https://github.com/bazelbuild/starlark/blob/master/spec.md) with PCB-specific primitives. See the [Language Reference](https://docs.pcb.new/pages/spec) for full details.

| Concept | Description |
|---------|-------------|
| **Net** | Electrical connection between pins (`Net("VCC")`, `Power("5V")`, `Ground()`) |
| **Component** | Physical part with symbol, footprint, and pin connections |
| **Interface** | Reusable connection patterns (e.g., SPI, I2C, USB) |
| **Module** | Hierarchical subcircuit loaded from a `.zen` file |
| **config()** | Declare configuration parameters for modules |
| **io()** | Declare net/interface inputs for modules |

## Command Reference

All commands accept `.zen` files or directories as arguments. When omitted, they operate on the current directory.

```bash
pcb build [PATHS...]              # Build and validate designs
pcb layout [PATHS...]             # Generate layout and open in KiCad
pcb open [PATHS...]               # Open existing layouts in KiCad
pcb fmt [PATHS...]                # Format .zen files
```

## Architecture

Rust workspace with specialized crates:

| Crate | Description |
|-------|-------------|
| `pcb` | Main CLI tool |
| `pcb-zen` | Starlark runtime, LSP server, DAP support |
| `pcb-zen-core` | Core language: components, modules, nets, interfaces |
| `pcb-zen-wasm` | WebAssembly bindings for browser execution |
| `pcb-layout` | PCB layout generation |
| `pcb-kicad` | KiCad file format parsing and generation |
| `pcb-ipc2581-tools` | IPC-2581 export for manufacturing |
| `pcb-starlark-lsp` | Language Server Protocol implementation |

## License

Zener is licensed under the MIT License. See [LICENSE](LICENSE) for details.

### Third-Party Software

- **ruff**: The `pcb fmt` command uses `ruff fmt` from the [astral-sh/ruff](https://github.com/astral-sh/ruff) project, which is licensed under the MIT. See [LICENSE](https://github.com/astral-sh/ruff/blob/main/LICENSE) for the full license text.

## Acknowledgments

- Built on [starlark-rust](https://github.com/facebookexperimental/starlark-rust) by Meta.
- Inspired by [atopile](https://github.com/atopile/atopile), [tscircuit](https://github.com/tscircuit/tscircuit), and others.

---

<p align="center">
  Made in Brooklyn, NY, USA.
</p>
