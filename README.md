# pcb

`pcb` is a command-line tool for circuit board projects written in Zener.
Zener is a Starlark-based language for describing PCB schematics; `pcb` builds
those designs, manages dependencies, and generates KiCad layout files.

[Documentation](https://docs.pcb.new) | [Language reference](https://docs.pcb.new/pages/spec)

## Installation

Install the `pcb` shim:

```bash
curl -fsSL https://raw.githubusercontent.com/diodeinc/pcb/main/install.sh | bash
```

On Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/diodeinc/pcb/main/install.ps1 | iex"
```

The shim downloads and runs the `pcbc` toolchain requested by each project.
The Unix installer writes `pcb` to `$HOME/.local/bin` by default. The Windows
installer writes `pcb.exe` to `%USERPROFILE%\.pcb\bin` by default. Set
`PCB_INSTALL_DIR` to choose a different directory.

Requirements:

- [KiCad 10.x](https://kicad.org/) for generating and editing layouts.

Windows support is experimental. For the most stable experience, use WSL2,
macOS, or Linux.

### Developing from source

```bash
git clone https://github.com/diodeinc/pcb.git
cd pcb
cargo build -p pcb -p pcbc
./install.sh --local
```

## Quick Start

Create `blinky.zen`:

[embed-readme]:# (examples/blinky.zen python)
```python
# ```pcb
# [workspace]
# pcb-version = "0.4"
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

Build the design:

```bash
pcb build blinky.zen
```

Generate a KiCad layout:

```bash
pcb layout blinky.zen
```

## Project Structure

Zener projects use one of two repository shapes.

### Board repository

A board repository contains one board plus any local modules and components it
owns:

```text
MyBoard/
├── pcb.toml              # Workspace and board manifest
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

Board repository `pcb.toml`:

```toml
[workspace]
repository = "github.com/myorg/MyBoard"
pcb-version = "0.4"

[board]
name = "MyBoard"
path = "MyBoard.zen"
description = "Replace with concise board description."
```

### Registry repository

A registry repository contains reusable packages and no board:

```text
registry/
├── pcb.toml              # Workspace manifest
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

Registry `pcb.toml`:

```toml
[workspace]
repository = "github.com/myorg/registry"
pcb-version = "0.4"
```

## Common Commands

```bash
pcb new board <NAME> <REPO_URL>              # Create a board repository
pcb build [PATHS...]                         # Build and validate designs
pcb sync                                     # Reconcile imports and dependency manifests
pcb layout <FILE>                            # Generate layout files
pcb import <KICAD_PRO> <OUTPUT_DIR>          # Import a KiCad project
```

Run `pcb help` or `pcb help <command>` for the full command reference.

## License

Diode-authored code and docs are licensed under the MIT License except where
otherwise noted. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Acknowledgments

- Made possible by the excellent [KiCad](https://kicad.org/) PCB design suite.
- Built on [starlark-rust](https://github.com/facebookexperimental/starlark-rust) by Meta.
- Inspired by [atopile](https://github.com/atopile/atopile),
  [tscircuit](https://github.com/tscircuit/tscircuit), and others.
