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

See the [latest release](https://github.com/diodeinc/pcb/releases/latest) for installation instructions.

### From Source

```bash
# Clone the repository
git clone https://github.com/diodeinc/pcb.git
cd pcb

# Install using the provided script
./install.sh
```

### Requirements

- [KiCad 9.x](https://kicad.org/) (for generating and editing layouts)

## Quick Start

### 1. Create Your First Design

Create a file called `blinky.zen`:

[embed-readme]:# (examples/blinky.zen python)
```python
# ```pcb
# [workspace]
# pcb-version = "0.3"
#
# [dependencies]
# "github.com/diodeinc/stdlib" = "0.4.4"
# ```

# Load standard library
load("@stdlib/board_config.zen", "Board")
load("@stdlib/interfaces.zen", "Power", "Ground")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

# Define power nets
vcc = Power("VCC")
gnd = Ground("GND")
led_anode = Net("LED_ANODE")

# Create components
Resistor(
    name = "R1",
    value = "1kohm",
    package = "0402",
    P1 = vcc,
    P2 = led_anode
)

Led(
    name = "D1",
    package = "0402",
    color = "red",
    A = led_anode,
    K = gnd
)

Board(
    name = "blinky",
    layers = 4,
    layout_path = "layout/blinky"
)
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

A typical Zener workspace:

```
project/
├── pcb.toml              # Workspace configuration
├── pcb.sum               # Dependency lock file
├── boards/               # Board designs
│   └── MyBoard/
│       ├── MyBoard.zen   # Board schematic
│       ├── pcb.toml      # Board metadata & dependencies
│       └── layout/       # KiCad layout files
├── modules/              # Reusable circuit modules
│   ├── pcb.toml
│   └── PowerSupply/
│       └── PowerSupply.zen
├── components/           # Custom component definitions
│   └── Manufacturer/
│       └── MPN/
│           ├── MPN.zen
│           └── pcb.toml
└── vendor/               # Vendored dependencies
```

**Workspace `pcb.toml`:**
```toml
[workspace]
pcb-version = "0.3"
members = ["boards/*", "modules/*", "components/**"]

[dependencies]
"github.com/diodeinc/stdlib" = "0.4"
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
