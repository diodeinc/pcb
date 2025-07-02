# Zener Language Specification

## Overview

Zener is a domain-specific language built on top of
[Starlark](https://github.com/bazelbuild/starlark/blob/master/spec.md) for
describing PCB schematics. It provides primitives for defining components, nets,
interfaces, and modules in a type-safe, composable manner.

This specification describes the language extensions and primitives added on top
of Starlark. For the base Starlark language features, please refer to the
[Starlark specification](https://github.com/bazelbuild/starlark/blob/master/spec.md) and the
[starlark-rust types extension](https://github.com/facebook/starlark-rust/blob/main/docs/types.md).

## Table of Contents

1. [Evaluation Model](#evaluation-model)
2. [Core Types](#core-types)
3. [Built-in Functions](#built-in-functions)
4. [Module System](#module-system)
5. [Type System](#type-system)

## Evaluation Model

### Files as Modules

Each `.zen` file is a Starlark module. It can be used in two ways:

1. Its exported symbols can be `load()`ed into other modules. For example,
   `load("./MyFile.zen", "MyFunction", "MyType")` will load the `MyFunction`
   and `MyType` symbols from the `MyFile.zen` module.
2. It can be loaded as a schematic module using the `Module()` helper. For
   example, `MyFile = Module("./MyFile.zen")` will import `MyFile.zen` as a
   schematic module, which you can instantiate like so:
   ```starlark
   MyFile(
       name = "MyFile",
       ...
   )
   ```

### Load Resolution

The `load()` and `Module()` statements support multiple resolution strategies:

```starlark
# Local file (relative to current file)
load("./utils.star", "helper")

# Package reference
load("@stdlib:1.2.3/math.star", "calculate")

# GitHub repository
load("@github/user/repo:branch/path.star", "function")
```

## Core Types

### Net

A `Net` represents an electrical connection between component pins.

```starlark
# Create a net with optional name
net1 = Net()
net2 = Net("VCC")
```

**Type**: `Net`  
**Constructor**: `Net(name="")`

- `name` (optional): String identifier for the net

### Component

Components represent physical electronic parts with pins and properties.

```starlark
Component(
    name = "U1",                   # Required: instance name
    footprint = "SOIC-8",          # Required: PCB footprint
    pin_defs = {                   # Required: pin name to number mapping
        "VCC": "1",
        "GND": "4",
        "OUT": "8"
    },
    pins = {                       # Required: pin connections
        "VCC": vcc_net,
        "GND": gnd_net,
        "OUT": output_net
    },
    prefix = "U",                  # Optional: reference designator prefix (default: "U")
    symbol = "path/to/symbol",     # Optional: schematic symbol path
    mpn = "LM358",                 # Optional: manufacturer part number
    type = "op-amp",               # Optional: component type
    properties = {                 # Optional: additional properties
        "voltage": "5V"
    },
    ...
)
```

**Type**: `Component`  
**Constructor**: `Component(**kwargs)`

### Interface

Interfaces define reusable connection patterns as a collection of nets or sub-interfaces.

```starlark
# Define an interface type
PowerInterface = interface(
    vcc = Net,
    gnd = Net
)

# Create an interface instance
power = PowerInterface("MAIN")  # Creates MAIN_VCC and MAIN_GND nets

# Interfaces can contain other interfaces
SystemInterface = interface(
    power = PowerInterface,
    data = Net,
    clock = Net
)

# Access interface members
system = SystemInterface()
resistor = Component(
    name = "R1",
    footprint = "0402",
    pin_defs = {"1": "1", "2": "2"},
    pins = {
        "1": system.power.vcc,
        "2": system.power.gnd
    }
)
```

**Type**: `interface`  
**Constructor**: `interface(**fields)`

- Fields can be `Net` types, `Net` instances (templates), or other interface types/instances

### Module

Modules represent hierarchical subcircuits that can be instantiated multiple times.

```starlark
# Load a module from a file
SubCircuit = Module("./subcircuit.star")

# Instantiate the module
SubCircuit(
    name = "power_supply",
    # ... pass inputs defined by io() and config() in the module
)
```

**Type**: `Module`  
**Constructor**: `Module(path)`

## Built-in Functions

### io(name, type, default=None, optional=False)

Declares a net or interface input for a module.

```starlark
# Required net input
vcc = io("vcc", Net)

# Optional interface input with default
PowerInterface = interface(vcc = Net, gnd = Net)
power = io("power", PowerInterface, optional=True)

# With explicit default
data = io("data", Net, default=Net("DATA"))
```

**Parameters:**

- `name`: String identifier for the input
- `type`: Expected type (`Net` or interface type)
- `default`: Default value if not provided by parent
- `optional`: If True, returns None when not provided (unless default is specified)

### config(name, type, default=None, convert=None, optional=False)

Declares a configuration value input for a module.

```starlark
# String configuration
prefix = config("prefix", str, default="U")

# Integer with conversion
baudrate = config("baudrate", int, convert=int)

# Enum configuration
Direction = enum("NORTH", "SOUTH", "EAST", "WEST")
heading = config("heading", Direction)

# Optional configuration
debug = config("debug", bool, optional=True)
```

**Parameters:**

- `name`: String identifier for the input
- `type`: Expected type (str, int, float, bool, enum, or record type)
- `default`: Default value if not provided
- `convert`: Optional conversion function
- `optional`: If True, returns None when not provided (unless default is specified)

### load_component(symbol_path, footprint=None)

Loads a component factory from an EDA symbol file.

```starlark
# Load from KiCad symbol file
OpAmp = load_component("./symbols/LM358.kicad_sym")

# Override footprint
OpAmp = load_component("./symbols/LM358.kicad_sym", footprint="SOIC-8")

# Instantiate
OpAmp(
    name = "U1",
    pins = { ... }
)
```

### File(path)

Resolves a file or directory path using the load resolver.

```starlark
# Get absolute path to a file
config_path = File("./config.json")

# Works with load resolver syntax
stdlib_path = File("@stdlib/components")
```

### error(msg)

Raises a runtime error with the given message.

```starlark
if not condition:
    error("Condition failed")
```

### check(condition, msg)

Checks a condition and raises an error if false.

```starlark
check(voltage > 0, "Voltage must be positive")
check(len(pins) == 8, "Expected 8 pins")
```

### add_property(name, value)

Adds a property to the current module instance.

```starlark
add_property("layout_group", "power_supply")
add_property("critical", True)
```

## Module System

### Module Definition

A module is defined by a `.star` file that declares its inputs and creates components:

```starlark
# voltage_divider.star

# Declare inputs
vin = io("vin", Net)
vout = io("vout", Net)
gnd = io("gnd", Net)

r1_value = config("r1", str, default="10k")
r2_value = config("r2", str, default="10k")

# Create components
Component(
    name = "R1",
    type = "resistor",
    footprint = "0402",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": vin, "2": vout},
    properties = {"value": r1_value}
)

Component(
    name = "R2",
    type = "resistor",
    footprint = "0402",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": vout, "2": gnd},
    properties = {"value": r2_value}
)
```

### Module Instantiation

```starlark
# Load the module
VDivider = Module("./voltage_divider.star")

# Create instances
VDivider(
    name = "divider1",
    vin = Net("INPUT"),
    vout = Net("OUTPUT"),
    gnd = Net("GND"),
    r1 = "100k",
    r2 = "47k"
)
```
