---
name: spice-sim
description: Add a simulation test bench to a zener file.
---

# Spice Simulation

Add a small ngspice-backed testbench to a `.zen` design.

## Workflow

1. Confirm the target is simulation-capable by runnnig a dummy sim. `pcb sim <path/to/file.zen> --setup "* empty setup check"`

2. If the SPICE model is missing, add it.
Find a vendor model, download it, or create a simple behavioral model if needed. Wire it through the leaf component with `spice_model=SpiceModel(...)` before writing the testbench.

3. Create a focused testbench file.
Use a generic package-local path such as `<package>/testbench/test_<scenario>.zen`.

4. Keep the structure simple:
- top docstring
- imports
- nets/interfaces
- module-under-test instantiation
- minimal external load or pull-ups
- one `Simulation(...)` block

5. Put sources and analysis inside `Simulation.setup`.
Use raw ngspice for:
- `DC`
- `PULSE(...)`
- `PWL(...)`
- `.control`
- `tran`
- `hardcopy`

6. Write the plot to `testbench/output/<scenario>.svg`.

## Simulation In Zener

`Simulation` is a Zener property loaded from `@stdlib/properties.zen` and attached as a normal top-level object:

```python
load("@stdlib/properties.zen", "Simulation")

Simulation(
    name="SIM",
    setup="""
* raw ngspice goes here
.control
  tran 10u 10m
.endc
""",
)
```

The `setup` string is passed through as ngspice input. Put voltage sources, waveform definitions, analysis commands, and plot/export commands there.

## Pattern

```python
"""<Part> <scenario> simulation test."""

load("@stdlib/interfaces.zen", "Ground", "Power")
load("@stdlib/properties.zen", "Simulation")

Target = Module("../Target.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")

VIN = Power(voltage="12V")
VOUT = Power()
GND = Ground()

Target(
    name="UUT",
    VIN=VIN,
    VOUT=VOUT,
    GND=GND,
)

Resistor(
    name="R_LOAD",
    value="10ohm",
    package="0603",
    P1=VOUT,
    P2=GND,
)

Simulation(
    name="SIM",
    setup="""
* <Part> <scenario>

V_IN VIN GND DC 12

.control
  tran 10u 10m

  set hcopydevtype = svg
  hardcopy output/<scenario>.svg v(VIN) v(VOUT) title "<Part> <scenario>" xlabel "Time" ylabel "Voltage"
.endc

""",
)
```

## Component Pattern

If the leaf component does not already expose a SPICE model, add one like this:

```python
VIN = io(Power())
VOUT = io(Power())
GND = io(Ground())

Component(
    name="MyPart",
    symbol=Symbol(library="MyPart.kicad_sym"),
    pins={"VIN": VIN, "VOUT": VOUT, "GND": GND},
    spice_model=SpiceModel(
        "MyPart.lib",
        "MyPart_SUBCKT",
        nets=[VIN, VOUT, GND],
        args={},
    ),
)
```

## Example Shapes

Load switch enable test:
```spice
V_IN VIN GND DC 5.3
V_ON ON GND PULSE(0 0.9V 1ms 10us 10us 3ms 5ms)
```

Protection threshold sweep:
```spice
V_IN VIN GND PWL(0 12 5m 12 5.1m 22 10m 22 10.1m 12 15m 12 15.1m 2 20m 2)
```

## Notes

- Prefer one behavior per file: startup, enable/disable, OVLO/UVLO, current limit.
- Keep passives in Zener and keep sources in `setup`.
- Plot only the signals that prove the behavior.
- If a SPICE model is missing, obtain or create it first, then add the testbench.
