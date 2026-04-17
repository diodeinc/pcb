---
name: idiomatic-zener
description: Style rules and idioms for writing high-quality Zener HDL code. Use when writing, reviewing, or refactoring `.zen` files — modules, reference designs, or board files. Covers DNP patterns, typed configs, voltage checks, component naming, computation style, and common gotchas.
---

# Idiomatic Zener

Prescriptive style rules for `.zen` code. These apply to all modules, reference designs, and board files.

## No Conditional Instantiation

Never use `if` to create or remove components. Always instantiate every component. Use `dnp=` to control whether it is populated. This applies to modules, boards, and reference designs, including optional feature blocks.

```zen
# BAD
if add_decoupling:
    Capacitor(name="C_VDD", value="100nF", package="0402", P1=VCC, P2=GND)

# GOOD
Capacitor(name="C_VDD", value="100nF", package="0402", P1=VCC, P2=GND)
```

## DNP via Record Pattern

For optional subcircuits controlled by a config, use a record to pair values with DNP state. A zero/disabled config value means `dnp=True` with placeholder values.

```zen
Passive = record(value=typing.Any, dnp=bool)

input_filter = config(Frequency, default="0Hz", optional=True,
    help="Input lowpass cutoff. 0Hz disables the filter.")

def input_rc(f):
    dnp = f <= Frequency("0Hz")
    r = e96(Resistance("100ohm") if not dnp else Resistance("0ohm"))
    c = e24(1 / (2 * PI * r * f) if not dnp else Capacitance("100pF"))
    return Passive(value=r, dnp=False), Passive(value=c, dnp=dnp)

input_r, input_c = input_rc(input_filter)
Resistor(name="R_IN", value=input_r.value, dnp=input_r.dnp, package="0402", P1=A, P2=B)
Capacitor(name="C_IN", value=input_c.value, dnp=input_c.dnp, package="0402", P1=B, P2=GND)
```

## Minimize Component Count

Fewer parts = simpler BOM, easier assembly, lower cost.

**Prefer value-switching over duplicate components.** When a config selects between discrete options, use a single component with a computed value — don't instantiate multiple components with opposing DNP conditions.

```zen
# BAD: two resistors, one always DNP
Resistor(name="R_STRAP_HI", value="10kohm", P1=STRAP, P2=VCC, dnp=mode != "HIGH")
Resistor(name="R_STRAP_LO", value="100kohm", P1=STRAP, P2=VCC, dnp=mode != "LOW")

# GOOD: one resistor, value changes with config
_strap_value = { Mode("HIGH"): "10kohm", Mode("LOW"): "100kohm", Mode("FLOAT"): "10kohm" }[mode]
Resistor(name="R_STRAP", value=_strap_value, P1=STRAP, P2=VCC, dnp=mode == Mode("FLOAT"))
```

**Leverage internal pull-ups/pull-downs.** Many ICs have internal bias on strap pins. If the default state uses the internal pull, don't add an external resistor — just DNP the single resistor for that case.

## Typed Unit Configs

Use physical types from `@stdlib/units.zen` for configs. Expose one meaningful parameter (e.g. cutoff frequency), not raw R/C values. Use `enum()` only for discrete design choices.

```zen
# BAD
config("filter_r", str, default="10ohms")

# GOOD
input_filter = config(Frequency, default="0Hz", optional=True,
    help="Input lowpass cutoff. 0Hz disables the filter.")
```

## Computation in Named Functions

Put calculations in named functions with datasheet references. Snap to E-series with `e96()` / `e24()`.

```zen
def load_r(v_out, v_sense):
    """Datasheet §8.1.1 / Eq 4: V_OUT = V_SENSE × gm × R_L"""
    GM = Current("200uA") / Voltage("1V")
    return e96(v_out / (v_sense * GM))
```

## Voltage on Power IOs

Every `Power` io declares its voltage range via the template.

```zen
VCC = io(Power(voltage="2.7V to 36V"))
```

## Help Strings

Use `help=` when it adds integrator-visible meaning that is not already obvious from the name, type, or default. Omit it when it would just restate those fields.

```zen
VDD = io(Power(voltage="3.0V to 5.5V"))
GND = io(Ground)
EN = io(Net, help="High to enable the regulator")
input_filter = config(Frequency, default="0Hz", optional=True,
    help="Input lowpass cutoff. 0Hz disables the filter.")
```

## No `.NET` Accessor

Use Power/Ground ios directly as pin connections. Never use `.NET`.

```zen
# BAD
Capacitor(name="C_VDD", value="100nF", P1=VCC.NET, P2=GND.NET)

# GOOD
Capacitor(name="C_VDD", value="100nF", P1=VCC, P2=GND)
```

## Naming

Beyond the standard conventions (UPPERCASE io, lowercase config):

| Element | Convention | Example |
|---------|-----------|---------|
| Internal nets | `_` prefix | `_VREF`, `_XI`, `_RBIAS` |
| Component names | Uppercase functional prefix | `R_LOAD`, `C_VDD`, `U_LDO` |
| Differential pairs | `_P` / `_N` suffixes | `IN_P`, `IN_N` (not `_PLUS` / `_MINUS`) |

## Opinionated Defaults

Don't expose configs for implementation details integrators shouldn't tune: decoupling cap values, passive package sizes, test point style.

Do expose configs for things integrators legitimately need to change: filter cutoffs, output voltage, gain settings, enable/disable optional subcircuits.

## Checklist

1. No `if` guards on instantiation — always instantiate, use `dnp=`
2. No `.NET` accessor — use ios directly
3. No `str` configs for physical values — use typed units
4. Calculations in named functions with `e96()` / `e24()`
5. Voltage range on all `Power` ios via template
6. Add `help=` only when it clarifies non-obvious integrator-facing meaning
7. Diff pairs use `_P` / `_N`, not `_PLUS` / `_MINUS`
8. Internal nets prefixed with `_`
9. Minimize component count — value-switch, leverage internal bias
10. When renaming components or nets, keep existing `# pcb:sch` comments in sync
11. Omit `no_connect` pins from `Component()` `pins`
