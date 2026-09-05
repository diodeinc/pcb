---
name: spice-sim
description: Add or run an ngspice-backed Zener testbench with `pcb sim`.
---

# Spice Simulation

Use a focused testbench such as `<package>/testbench/test_<scenario>.zen` for
one behavior: startup, enable, a protection threshold, or current limit. Reuse
an existing bench when it covers the request. Run it with `pcb sim <file>.zen`;
a separate dummy simulation is only useful when diagnosing setup problems.

## Model and testbench

The leaf component needs `spice_model=SpiceModel(...)`. Obtain a vendor model
or, when appropriate, use a behavioral model with explicit limitations. Match
`nets` to the subcircuit's declared terminal order:

```zen
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

Instantiate the module under test and needed loads/passives in Zener. Put
sources, waveforms, analysis, and plot commands in the raw ngspice `setup`
string of a top-level `Simulation`:

```zen
load("@stdlib/properties.zen", "Simulation")

Simulation(
    name="SIM",
    setup="""
V_IN VIN GND DC 12
.control
  tran 10u 10m
  set hcopydevtype = svg
  hardcopy output/startup.svg v(VIN) v(VOUT)
.endc
""",
)
```

Adapt the source and analysis to the behavior; for example, `PULSE(...)` for
enable timing or `PWL(...)` for a changing input. Write plots under
`testbench/output/` and inspect only the signals needed to establish the result.
Distinguish a model or simulator failure from an electrical finding, and state
what the model and analysis actually verify.
