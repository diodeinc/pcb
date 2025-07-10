# `pcb`: CLI for circuit boards

> PCB tooling by [Diode Computers, Inc.](https://diode.computer/)

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org/)

`pcb` is a command-line utility for building PCBs. It uses the
[Zener](https://github.com/diodeinc/pcb/blob/main/docs/spec.md) language to describe
PCB schematics and provides automations on top of KiCad to build PCBs fast.

> [!WARNING]
> We're still in the early days of getting this out into the world; expect breaking changes
> and better documentation in the next few days.

## Table of Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
- [Command Reference](#command-reference)
- [Examples](#examples)
- [Architecture](#architecture)
- [License](#license)

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/diodeinc/pcb.git
cd pcb

# Install using the provided script
./install.sh
```

> [!NOTE]
> Package manager installation coming soon.

### Requirements

- [KiCad 9.x](https://kicad.org/) (for generating and editing layouts)

## Quick Start

### 1. Create Your First Design

Create a file called `blinky.star`:

```python
# Load standard library
load("@github/diodeinc/stdlib:main/properties.star", "Layout")

Resistor = Module("@github/diodeinc/stdlib:main/generics/Resistor.star")
Led = Module("@github/diodeinc/stdlib:main/generics/Led.star")

# Define power nets
vcc = Net("VCC")
gnd = Net("GND")
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

Layout("layout", "layout/")
```

### 2. Build Your Design

```bash
# Compile the design and check for errors
pcb build blinky.star

# Output:
# ✓ blinky.star (2 components)
```

### 3. Generate Layout

```bash
# Generate PCB layout files
pcb layout blinky.star

# Output:
# ✓ blinky.star (layout/blinky.kicad_pcb)
```

### 4. Open in KiCad

```bash
# Open the generated layout
pcb open blinky.star
```

## Core Concepts

### Components

Components are the building blocks of your design. They define physical parts with pins, footprints, and properties:

```python
Component(
    name = "U1",
    type = "microcontroller",
    footprint = "QFP-48",
    pin_defs = {
        "VDD": "1",
        "GND": "2",
        "PA0": "3",
        # ... more pins
    },
    pins = {
        "VDD": vcc_3v3,
        "GND": gnd,
        "PA0": led_control,
    }
)
```

### Nets

Nets represent electrical connections between component pins:

```python
# Create named nets
power_5v = Net("5V")
ground = Net("GND")
data_bus = Net("SPI_MOSI")

# Nets are type-safe and tracked across the design
```

### Interfaces

Interfaces define reusable connection patterns:

```python
# Define a power interface
PowerInterface = interface(
    vcc = Net,
    gnd = Net,
)

# Define an SPI interface
SPIInterface = interface(
    clk = Net,
    mosi = Net,
    miso = Net,
    cs = Net,
)
```

### Modules

Modules enable hierarchical design and reusability. A module is a `.star` file that defines configuration parameters and IO interfaces:

```python
# power_supply.star
# Configuration parameters
input_voltage = config("input_voltage", float, default = 12.0)
output_voltage = config("output_voltage", float, default = 3.3)

# IO interfaces
input = io("input", Power)
output = io("output", Power)

# Module implementation
Regulator(
    name = "REG1",
    vin = input.vcc,
    vout = output.vcc,
    gnd = input.gnd,
    # ... component configuration
)

# main.star
PowerSupply = Module("power_supply.star")

PowerSupply(
    name = "PSU1",
    input_voltage = 9.0,
    output_voltage = 5.0,
    input = system_power_in,
    output = regulated_power,
)
```

#### Module Configuration with `config()`

The `config()` function defines configuration parameters at the module level:

```python
# sensor_module.star
# Required configuration
i2c_address = config("i2c_address", int)

# Optional configuration with defaults
sample_rate = config("sample_rate", int, default = 100)
gain = config("gain", float, default = 1.0)

# Configuration with type conversion
threshold = config("threshold", float, convert = lambda x: float(x))

# Enum configuration
Package = enum("QFN", "TQFP", "BGA")
package_type = config("package", Package, convert = Package)
```

#### Module IO with `io()`

The `io()` function defines input/output interfaces at the module level:

```python
# uart_bridge.star
# Define IO interfaces
uart_in = io("uart_in", UARTInterface)
uart_out = io("uart_out", UARTInterface)
power = io("power", PowerInterface)

# Simple net IO
enable = io("enable", Net)

# Module implementation uses the IO
Bridge(
    name = "U1",
    rx_in = uart_in.rx,
    tx_in = uart_in.tx,
    rx_out = uart_out.rx,
    tx_out = uart_out.tx,
    vcc = power.vcc,
    gnd = power.gnd,
    en = enable,
)
```

## Command Reference

### `pcb build`

Build and validate PCB designs from `.star` files.

```bash
pcb build [PATHS...]

Arguments:
  [PATHS...]     One or more .star files or directories containing .star files
                 When omitted, all .star files in the current directory are built
                 Directories are scanned non-recursively

Examples:
  pcb build                    # Build all .star files in current directory
  pcb build board.star         # Build specific file
  pcb build designs/           # Build all .star files in designs/ directory (non-recursive)
  pcb build a.star b.star      # Build multiple specific files
```

The build command:

- Validates your Starlark code
- Reports any errors or warnings with detailed diagnostics
- Shows component count for successful builds
- Exits with error code if any file fails to build

### `pcb layout`

Generate PCB layout files from `.star` designs.

```bash
pcb layout [OPTIONS] [PATHS...]

Options:
  -s, --select      Always prompt to choose a layout even when only one exists
      --no-open     Skip opening the layout file after generation
  -h, --help        Show help information

Arguments:
  [PATHS...]        One or more .star files to process for layout generation
                    When omitted, all .star files in the current directory are processed

Examples:
  pcb layout                   # Generate layouts for all .star files
  pcb layout board.star        # Generate layout for specific file
  pcb layout --no-open         # Generate without opening in KiCad
  pcb layout -s                # Force layout selection prompt
```

The layout command:

- First builds the .star file (same as `pcb build`)
- Generates KiCad PCB layout files if a `Layout()` directive exists
- Shows warnings for files without layout directives
- Opens the generated layout in KiCad by default (unless `--no-open`)
- Prompts for selection when multiple layouts exist (or with `-s`)

### `pcb open`

Open existing PCB layout files in KiCad.

```bash
pcb open [PATHS...]

Arguments:
  [PATHS...]     One or more .star files to find and open layouts for
                 When omitted, searches current directory for .kicad_pcb files

Examples:
  pcb open                     # Open layout files in current directory
  pcb open board.star          # Open layout associated with board.star
  pcb open *.star              # Open layouts for all .star files
```

The open command:

- Builds .star files to find their associated layout paths
- Falls back to searching for .kicad_pcb files if no .star files specified
- Prompts for selection when multiple layouts are found
- Opens the selected layout in your system's default PCB editor (typically KiCad)

### `pcb fmt`

Format `.zen` and `.star` files using the bundled buildifier formatter.

```bash
pcb fmt [OPTIONS] [PATHS...]

Options:
      --check     Check if files are formatted correctly without modifying them
      --diff      Show diffs instead of writing files
  -h, --help      Show help information

Arguments:
  [PATHS...]      One or more .zen/.star files or directories containing such files
                  When omitted, all .zen/.star files in the current directory are formatted

Examples:
  pcb fmt                      # Format all .zen/.star files in current directory
  pcb fmt design.star          # Format specific file
  pcb fmt src/                 # Format all files in src/ directory
  pcb fmt --check              # Check formatting without making changes
  pcb fmt --diff main.star     # Show what would change
```

The fmt command:

- Uses the bundled buildifier formatter (no external dependencies)
- Formats files according to Bazel/Starlark style conventions
- Supports checking mode (`--check`) for CI/CD pipelines
- Shows diffs (`--diff`) without modifying files
- Returns non-zero exit code if files need formatting in check mode

### `pcb lsp`

Start the Language Server Protocol server for editor integration.

```bash
pcb lsp
```

The LSP command:

- Starts the LSP server for Starlark PCB files
- Provides intelligent code completion, diagnostics, and go-to-definition
- Typically launched automatically by your editor's LSP client
- Supports eager evaluation for real-time feedback

## Project Structure

A typical Zener project structure:

```
my-pcb-project/
├── main.star              # Main board definition
├── components/            # Reusable components
│   ├── mcu.star
│   ├── power.star
│   └── connectors.star
├── modules/               # Reusable modules
│   ├── usb_interface.star
│   └── power_supply.star
├── libs/                  # External libraries
│   └── stdlib.star
├── eda/                   # KiCad symbols and footprints
│   ├── symbols/
│   └── footprints/
└── layout/                # Generated layouts
    └── main.kicad_pcb
```

## Architecture

Zener is built as a modular Rust workspace with specialized crates:

### Core Language & Runtime

- **`pcb-zen`** - Main Starlark runtime with PCB-specific extensions, LSP server, and DAP support
- **`pcb-zen-core`** - Core language features including components, modules, nets, interfaces, and the type system
- **`pcb-zen-wasm`** - WebAssembly bindings for running Starlark PCB designs in the browser

### Schematic & Layout

- **`pcb-sch`** - Schematic representation, netlist structures, and KiCad export formats
- **`pcb-layout`** - PCB layout generation from schematics and KiCad file creation
- **`pcb-kicad`** - KiCad file format parsing and generation utilities

### Language Server & Editor Support

- **`pcb-starlark-lsp`** - Language Server Protocol implementation for Starlark with PCB extensions
- **`pcb`** - Main CLI tool providing build, layout, open, and lsp commands

### EDA Integration & Utilities

- **`pcb-eda`** - EDA tool integration for processing symbols and footprints from various sources
- **`pcb-sexpr`** - S-expression parser for KiCad file formats
- **`pcb-ui`** - Terminal UI components including spinners, progress bars, and styled output
- **`pcb-command-runner`** - Utility for running external commands with proper output capture

## Examples

### Simple LED Circuit

```python
load("@github/diodeinc/stdlib:main/properties.star", "Layout")

Resistor = Module("@github/diodeinc/stdlib:main/generics/Resistor.star")
LED = Module("@github/diodeinc/stdlib:main/generics/LED.star")
Capacitor = Module("@github/diodeinc/stdlib:main/generics/Capacitor.star")

vcc = Net("VCC")
gnd = Net("GND")
led = Net("LED")

# Power supply filtering
Capacitor(
    name = "C1",
    value = "100nF",
    package = "0402",
    P1 = vcc,
    P2 = gnd
)

# Current limiting resistor
Resistor(
    name = "R1",
    value = "330ohm",
    package = "0402",
    P1 = vcc,
    P2 = led
)

# Status LED
LED(
    name = "D1",
    color = "red",
    package = "0402",
    A = led,
    K = gnd
)

Layout("layout", "layout/")
```

### Module with Configuration

```python
# voltage_regulator.star
input_voltage = config("input_voltage", float)
output_voltage = config("output_voltage", float, default = 3.3)
max_current = config("max_current", float, default = 1.0)

input = io("input", PowerInterface)
output = io("output", PowerInterface)
enable = io("enable", Net)

# Create the regulator component
Component(
    name = "REG",
    type = "voltage_regulator",
    footprint = "SOT-23-5",
    pin_defs = {
        "VIN": "1",
        "GND": "2",
        "EN": "3",
        "VOUT": "4",
        "FB": "5",
    },
    pins = {
        "VIN": input.vcc,
        "GND": input.gnd,
        "EN": enable,
        "VOUT": output.vcc,
        "FB": Net("FEEDBACK"),
    },
    properties = {
        "input_voltage": input_voltage,
        "output_voltage": output_voltage,
    }
)

# main.star
load("@github/diodeinc/stdlib:main/interfaces.star", "Power")
VoltageRegulator = Module("voltage_regulator.star")

# Define power rails
input_power = Power("VIN")
output_power = Power("3V3")

# Create voltage regulator
VoltageRegulator(
    name = "VREG1",
    input_voltage = 5.0,
    output_voltage = 3.3,
    max_current = 0.5,
    input = input_power,
    output = output_power,
    enable = Net("VREG_EN"),
)
```

### Complex System with Multiple Modules

```python
load("@github/diodeinc/stdlib:main/properties.star", "Layout")
load("@github/diodeinc/stdlib:main/interfaces.star", "Power", "SPI", "I2C")

# Load modules
MCU = Module("stm32f4.star")
Sensor = Module("bmi270.star")
Flash = Module("w25q128.star")

# Power distribution
system_power = Power("3V3")

# Communication buses
spi_bus = SPI("SPI1")
i2c_bus = I2C("I2C1")

# Microcontroller
MCU(
    name = "U1",
    power = system_power,
    spi1 = spi_bus,
    i2c1 = i2c_bus,
)

# IMU Sensor
Sensor(
    name = "U2",
    power = system_power,
    i2c = i2c_bus,
    i2c_address = 0x68,
    sample_rate = 400,  # 400Hz
)

# Flash Memory
Flash(
    name = "U3",
    power = system_power,
    spi = spi_bus,
    capacity = "128Mbit",
)

Layout("layout")
```

## License

Zener is licensed under the MIT License. See [LICENSE](LICENSE) for details.

### Third-Party Software

- **buildifier**: The `pcb fmt` command includes a bundled buildifier binary from the [bazelbuild/buildtools](https://github.com/bazelbuild/buildtools) project, which is licensed under the Apache License, Version 2.0. See [crates/pcb-buildifier/LICENSE](crates/pcb-buildifier/LICENSE) for the full license text.

## Acknowledgments

- Built on [starlark-rust](https://github.com/facebookexperimental/starlark-rust) by Meta.
- Inspired by [atopile](https://github.com/atopile/atopile), [tscircuit](https://github.com/tscircuit/tscircuit), and others.

---

<p align="center">
  Made in Brooklyn, NY, USA.
</p>
