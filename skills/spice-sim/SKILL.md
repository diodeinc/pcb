---
name: spice-sim
description: Add or run an ngspice-backed Zener testbench with `pcb sim`.
---

# Spice Simulation

Use a focused testbench such as `<package>/testbench/test_<scenario>.zen` for one behavior: startup, enable, a protection threshold, or current limit.
Reuse an existing bench when it covers the request.
Run it with `pcb sim -v <file>.zen`; a separate dummy simulation is only useful when diagnosing setup problems.

`pcb sim` runs the `ngspice` executable in batch mode in the testbench's directory and stops it after 5 seconds.
Without `-v`, a successful run prints only that it passed.

## Model and testbench

Every component that is not DNP needs a SPICE model.
The stdlib generics, such as resistors, capacitors, inductors, ferrite beads, crystals, diodes, LEDs, and TVS diodes, have their own.
A leaf component needs `spice_model=SpiceModel(...)`.
Obtain a vendor model or, when appropriate, use a behavioral model with explicit limitations.
Match `nets` to the subcircuit's declared terminal order, and pass every `PARAMS:` entry in `args`, because defaults are not applied:

```zen
Component(
    name="MyPart",
    symbol=Symbol(library="MyPart.kicad_sym"),
    footprint=File("MyPart.kicad_mod"),
    part=Part(mpn="MYPART-1", manufacturer="Vendor"),
    pins={"VIN": VIN, "VOUT": VOUT, "GND": GND},
    spice_model=SpiceModel(
        "MyPart.lib",
        "MyPart_SUBCKT",
        nets=[VIN, VOUT, GND],
        args={},
    ),
)
```

The model text goes into the netlist, so a `.include` in the model file resolves from the testbench's directory.
The `.subckt` line must list all its terminals on one line; terminals on `+` continuation lines are not counted.
For a PSpice model, put `set ngbehavior=psa` in a `.spiceinit` file beside the testbench; for an LTspice model, `set ngbehavior=ltpsa`.
Without it, PSpice expressions such as `VALUE={LIMIT(...)}` give wrong results without an error.
Verilog-A and encrypted models do not run.

Instantiate the module under test and needed loads/passives in Zener.
Put sources, waveforms, analysis, and plot commands in the raw ngspice `setup` string of a top-level `Simulation`:

```zen
load("@stdlib/properties.zen", "Simulation")

Simulation(
    name="SIM",
    setup="""
V_IN VIN GND DC 12
.control
  tran 10u 10m
  meas tran vout_final FIND v(VOUT) AT=10m
  set hcopydevtype = svg
  hardcopy output/startup.svg v(VIN) v(VOUT)
.endc
""",
)
```

Adapt the source and analysis to the behavior; for example, `PULSE(...)` for enable timing or `PWL(...)` for a changing input.
Operating point, DC sweep, AC, transient, and noise analyses all run.

## Results

`pcb sim` reports success whenever ngspice exits normally, including when a `meas` fails, a plot is not written, or an expression divides by zero.
Read the `-v` output: take values from `meas` or `echo` lines, because `print` tables lose their column separators.
`hardcopy` and `wrdata` write only into an existing directory, so create `testbench/output/` first.
Convert an SVG plot to PNG with `rsvg-convert` before viewing it, and inspect only the signals needed to establish the result.

A switching converter at nanosecond steps simulates only about 1 to 3 ms within the 5-second limit.
For a longer run, write the netlist with `pcb sim <file>.zen -o <file>.cir`, then run `ngspice -b <file>.cir` in the testbench's directory with a longer timeout.

Distinguish a model or simulator failure from an electrical finding, and state what the model and analysis actually verify.
