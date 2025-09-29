# Metadata Examples

This directory contains examples demonstrating the `metadata()` feature in Zener, which provides typed, persistent storage that works across module boundaries.

## Overview

The `metadata()` function creates typed containers that can accumulate values over time and persist across module boundaries. This is useful for:

- Component tracking and analysis
- Design logging and debugging  
- Cross-module data sharing
- Power budget analysis
- Interface registry management

## Key Features

- **Type Safety**: Containers validate that pushed values match the declared type
- **Persistence**: Data survives across module boundaries and function calls
- **Accumulation**: Values are stored chronologically using `push()` 
- **Retrieval**: Access latest value with `get()` or all values with `list()`

## Supported Types

- Basic types: `str`, `int`, `float`, `bool`, `list`, `dict`
- Physical values: `Voltage`, `Current`, `Resistance`, etc.
- Not supported: enums, records, custom types

## Examples

### SimpleUsage.zen

Demonstrates basic metadata container functionality:
- Creating typed containers for different data types
- Type validation when pushing values
- Retrieving data with `get()` and `list()` methods
- Working with physical units and basic types

Run with: `pcb build examples/metadata/SimpleUsage.zen`

### ComponentTracker.zen  

Shows metadata usage with interfaces and component tracking:
- Logging component instantiations and properties
- Tracking interface creation and usage
- Design analysis using accumulated metadata
- Integration with power/ground interfaces and GPIO

Run with: `pcb build examples/metadata/ComponentTracker.zen`

### CrossModuleDemo.zen + SubModule.zen

Demonstrates cross-module metadata sharing:
- Shared metadata containers between modules
- Data accumulation across module boundaries
- Global system analysis using metadata from multiple modules
- Power budget tracking and requirements checking
- Module execution tracing

Run the main example: `pcb build examples/metadata/CrossModuleDemo.zen`

This will automatically load `SubModule.zen` which contributes to the shared containers.

## Common Patterns

### Creating Containers
```zen
# Typed containers
voltage_log = metadata(Voltage)
messages = metadata(str)
readings = metadata(float)
config_data = metadata(dict)
```

### Adding Data
```zen
voltage_log.push(Voltage("3.3V"))
messages.push("System initialized")
readings.push(3.14159)
config_data.push({"mode": "debug", "enabled": True})
```

### Retrieving Data
```zen
latest = voltage_log.get()        # Most recent value or None
all_data = voltage_log.list()     # All values in chronological order
count = len(voltage_log.list())   # Number of entries
```

### Cross-Module Sharing
```zen
# In multiple modules, use the same container name:
shared_log = metadata(str)

# Data pushed in any module is accessible from all modules
shared_log.push("Module A initialized")  # In ModuleA.zen
shared_log.push("Module B loaded")       # In ModuleB.zen

# Later, anywhere:
all_messages = shared_log.list()  # Contains both messages
```

## Error Handling

Metadata containers enforce type safety:

```zen
voltage_log = metadata(Voltage)
voltage_log.push(Voltage("5V"))    # ✅ Valid
voltage_log.push("not a voltage") # ❌ Error: type mismatch
voltage_log.push(3.3)             # ❌ Error: type mismatch
```

## Use Cases

1. **Design Analysis**: Track components and analyze circuit topology
2. **Power Budget**: Accumulate power consumption across modules
3. **Interface Registry**: Track all interfaces created in a design
4. **Debug Logging**: Trace module execution and parameter values
5. **Requirements Checking**: Validate design against specifications
6. **Component Inventory**: Track all parts used in the design
