# pcb

`pcb` is a command-line tool for circuit board projects written in Zener.
Zener is a Starlark-based language for describing PCB schematics; `pcb` builds
those designs, manages dependencies, and generates KiCad layout files.

[Documentation](https://docs.pcb.new) | [Language reference](https://docs.pcb.new/pages/spec)

## Installation

Install the `pcb` launcher on macOS, Linux, or WSL2:

```bash
curl -fsSL https://raw.githubusercontent.com/diodeinc/pcb/main/install.sh | bash
```

For native Windows, run this command in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/diodeinc/pcb/main/install.ps1 | iex"
```

The launcher downloads the toolchain selected by each project. KiCad 10.x is
required for layouts, but not for building Zener designs. Native Windows support
is experimental; use WSL2 if needed.

## Quick start

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

Build the design and generate a KiCad layout:

```bash
pcb build blinky.zen
pcb layout blinky.zen
```

See [Getting Started](https://docs.pcb.new/pages/quickstart) for board setup,
toolchains, and CI authentication, and [Packages](https://docs.pcb.new/pages/packages)
for repository structure and dependencies. Run `pcb help` for commands.

## Developing from source

```bash
git clone https://github.com/diodeinc/pcb.git
cd pcb
./install.sh --local
```

Requires Rust. See [AGENTS.md](AGENTS.md) for test commands and repository conventions.

## License

Diode-authored code and docs are licensed under the MIT License except where
otherwise noted. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Acknowledgments

- Made possible by the excellent [KiCad](https://kicad.org/) PCB design suite.
- Built on [starlark-rust](https://github.com/facebookexperimental/starlark-rust) by Meta.
- Inspired by [atopile](https://github.com/atopile/atopile),
  [tscircuit](https://github.com/tscircuit/tscircuit), and others.
