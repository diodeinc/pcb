---
name: reference-design
description: Create high-quality reference designs for electronic components in Zener. Use when building a typical application circuit, reusable subcircuit module, or reference design for an IC — including studying the datasheet, structuring the design, choosing passives, writing the README, and iterating to a working build.
---

# Reference Design

Workflow for creating production-quality reference designs in Zener. A reference design wraps an IC with its recommended application circuit — decoupling, biasing, pull-ups, crystals, etc. — as a reusable module.

## Workflow

1. **Get the IC definition available to the design.** Prefer validated registry modules/components when they fit; import a web component into the workspace only when you need a new local component package.
2. **Study the datasheet** — extract the information listed in [Datasheet Checklist](#datasheet-checklist).
3. **Study an existing reference design** — read a validated example before writing. See [Studying Existing Designs](#studying-existing-designs).
4. **Create the design** — scaffold the directory, write the `.zen` file section by section, build after every major section. See [Design Structure](#design-structure).
5. **Write the README** — document interfaces, usage, and design notes. See [README Template](#readme-template).
6. **Validate** — `pcb build reference/<NAME>` must pass. Then `pcb fmt reference/<NAME>`.

## Datasheet Checklist

Extract these from the scanned datasheet markdown before writing any code:

- Typical application schematic / reference circuit
- Power supply pins and voltage requirements
- Decoupling capacitor values and placement (per power pin)
- Bias resistor values and connections
- Crystal / oscillator requirements (frequency, load capacitance)
- Digital interface pin mapping (match to stdlib interfaces like `Spi`, `I2c`, `Uart`, `Rgmii`, etc.)
- Strap / bootstrap configuration pins and their default states
- Reset circuit requirements (pull-up value, RC filter)
- Analog / differential pair connections and termination
- Exposed pad / thermal pad connections

## Studying Existing Designs

Before writing a new reference design, read a validated example from the workspace or registry to learn patterns. Find candidates with:

```bash
pcb search -m registry:modules "<similar function>" -f json
```

Then inspect the cached source with the relevant package docs or read the `.zen` file directly from `~/.pcb/cache/`.

Key patterns to observe:
- How IOs are structured (which nets are exposed vs internal)
- How optional features are handled (optional io, `dnp=`-driven parts, computed values)
- How passives are sized and grouped
- How the README documents the design

If an existing example conflicts with `idiomatic-zener`, follow `idiomatic-zener` for new work. In particular, do not use conditional instantiation for components.

## Design Structure

### Directory layout and naming

Use the MPN prefix plus `x` suffix. Example: `DP83867ISRGZR` → `DP83867x`.

```
reference/<PREFIX>x/
├── <PREFIX>x.zen       # Main design file
├── pcb.toml            # Empty or with non-auto dependencies
└── README.md           # Usage guide
```

Scaffold with `pcb new package reference/<PREFIX>x`.

### .zen file structure

Organize the file in this order:

1. **Docstring** — component name, brief description, key specs
2. **Loads** — stdlib interfaces and units needed beyond prelude
3. **IOs** — external interface of the module (power, ground, data interfaces, optional pins)
4. **Configs** — user-configurable parameters (output voltage, pull-up enable, etc.)
5. **Internal nets** — nets that don't leave the module, prefixed with `_`
6. **Component and generic imports** — the main IC and passives
7. **Main IC instantiation** — connect all pins
8. **Supporting circuitry** — decoupling, bias, pull-ups, crystals, reset, etc. grouped by function
9. **Optional feature sections** — organize circuitry controlled by config or optional io, but keep components instantiated and use `dnp=` rather than conditional instantiation

Reference-design-specific conventions:
- Internal nets use `_` prefix: `_RBIAS = Net("RBIAS")`
- Group decoupling caps near the IC instantiation, one per power pin as the datasheet recommends
- Use a `passives_size` variable when all passives share a package size (e.g. `"0402"`)
- Add brief comments referencing datasheet section/table for non-obvious values

### Build iteratively

Build after every major section — don't write the entire file and then build. This catches errors early:

```bash
pcb build reference/<PREFIX>x
```

Common errors:
- Wrong interface field names — inspect the relevant package API before wiring
- Missing loads — add the interface or unit import
- Path errors — `pcb build` takes the directory, not the `.zen` file

Format when done: `pcb fmt reference/<PREFIX>x`.

## Common Passive Patterns

| Purpose | Typical Value | Notes |
|---------|---------------|-------|
| Decoupling (digital) | 1µF per power pin | Place closest to pin |
| Decoupling (analog) | 100nF C0G + 10µF | C0G for low ESR |
| Decoupling (bulk) | 10µF–47µF | Near power input, X5R/X7R |
| I2C pull-ups | 2.2kΩ–4.7kΩ | To VDD, value depends on bus speed and capacitance |
| MDIO pull-up | 2.2kΩ | To VDDIO |
| SPI pull-up (CS) | 10kΩ | Keep CS deasserted at reset |
| Reset RC filter | 10kΩ pull-up + 100nF | ~1ms time constant |
| Bias resistor | Per datasheet (1% tolerance) | Always use exact datasheet value |
| LED current limit | 330Ω | ~10mA at 3.3V, adjust for target current |
| Crystal load caps | Per crystal spec | C0G dielectric, value from crystal datasheet formula |

When in doubt, follow the datasheet's recommended values exactly. Don't optimize passives without reason.

## README Template

```markdown
# <NAME> Reference Design

Brief description of the IC and what this reference design provides.

## Features
- **IC**: MPN (package)
  - Key electrical specs (voltage range, current, frequency, etc.)
- **Interfaces**: What buses/connections are exposed
- **Protection**: ESD, overcurrent, thermal features if relevant

## Interfaces

| Name | Type | Description |
|------|------|-------------|
| VIN | Power | Input supply (range) |
| VOUT | Power | Regulated output |
| GND | Ground | Common ground |
| SPI | Spi | Control interface |

## Usage

\```python
MyRef = Module("github.com/diodeinc/registry/reference/<NAME>/<NAME>.zen")

MyRef(
    name="U1",
    VIN=vin_3v3,
    VOUT=vout_1v8,
    GND=gnd,
    SPI=spi_bus,
)
\```

## Design Notes

Document key design decisions, tradeoffs, and anything non-obvious:
- Why specific passive values were chosen
- Strap pin configurations and what they select
- Thermal considerations
- Layout-sensitive connections

## References
- [Datasheet](url)
```
