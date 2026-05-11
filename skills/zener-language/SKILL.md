---
name: zener-language
description: Canonical Zener HDL guidance. Use for any non-trivial `.zen` work or whenever syntax, `Module()`, `io()`, `config()`, imports, `pcb.toml`, stdlib APIs, package APIs, source lookup, generics, DNP patterns, or validation are uncertain.
---

# Zener Language

Canonical Zener HDL semantics and authoring guidance. Start from nearby code; use `pcb doc` for package APIs and source locations before searching elsewhere.

## Workflow

1. Start from nearby workspace code. Prefer the local package's patterns before generic examples.
2. Check exact semantics here before editing unfamiliar syntax, manifests, imports, stdlib APIs, package interfaces, or authoring patterns.
3. Use `pcb doc` first for interface fields, generic parameters, stdlib/package APIs, file trees, and source roots.
4. Read source from the path reported by `pcb doc` when behavior matters. Do not search broad filesystem roots or random cache directories to find stdlib/package source.
5. Never invent syntax, stdlib modules, interfaces, fields, package APIs, footprints, or part names.
6. Preserve trailing `# pcb:sch ...` comments. Only update names inside an existing comment when you rename the matching component or net.

## Using `pcb doc`

`pcb doc` is the entry point for Zener package discovery. It works for `@stdlib`, local paths, installed packages, and registry/Git URL packages.

- `pcb doc --package @stdlib` or `pcb doc --package <package>` prints API docs and a `<!-- source: ... -->` root; read files under that root when docs are not enough. Pin URL packages with `@<version>`, e.g. `github.com/org/repo/path@v0.2.5`.
- Add `--list` to print the `.zen` file tree, e.g. `pcb doc --package @stdlib --list`, before opening files like `interfaces.zen`, `units.zen`, `generics/Resistor.zen`, or `bom/match_generics.zen`.
- Warnings do not necessarily make the command useless; partial output can still reveal the source root or file tree.

Use docs for signatures and public surfaces, then read source for exact behavior. For stdlib/package source lookup, `pcb doc --package ...` replaces ad hoc `find /`, `find /Users`, or broad cache searches.

## Language

Base language is normal Starlark — expressions, functions, loops, comprehensions, dicts, lists, `load()`. Below is the Zener-specific layer.

Modules:

- A `.zen` file is either a normal Starlark module loaded with `load()` or an instantiable schematic module loaded with `Module()`.
- `load("./foo.zen", "helper")` imports Starlark symbols. `Foo = Module("./Foo.zen")` or `Foo = Module("github.com/org/repo/path/Foo.zen")` loads a subcircuit.
- `./` paths are relative to the current file and resolve within the same package. Cross-package `load()` and `Module()` require the full package URL.
- Instantiation always passes `name=...` first, then any `io()` / `config()` inputs. Useful extras: `properties`, `dnp`, `schematic`.

Nets and interfaces:

- `Net(name=None, voltage=None, impedance=None)` is the base connection type.
- `Power`, `Ground`, and `NotConnected` are specialized net types; more specialized net types live in stdlib.
- Across `io()` boundaries: `NotConnected` can promote to any net type; specialized nets can demote to plain `Net`; plain `Net` does not auto-promote to specialized types. Use explicit casts like `Power(net, voltage=...)` or `Net(power_net)` when needed.
- For buses and grouped signals, use stdlib interfaces from `@stdlib/interfaces.zen` (e.g. `I2c`, `Spi`, `Uart`, `Usb2`) instead of loose net bundles.

Components and sourcing:

- `Component(...)` is the primitive physical-part constructor. Required fields are effectively `name`, `symbol`, and `pins`.
- The symbol is the source of truth for footprint and part metadata. Make the symbol properties correct; do not repeat `footprint=` or `part=` in `Component()` when they are already provided by the symbol.
- Prefer `part=Part(mpn=..., manufacturer=...)` over legacy scalar `mpn` and `manufacturer` when part metadata is not already in the symbol.
- `Symbol(library, name=None)` points at a `.kicad_sym`; `name` is required for multi-symbol libraries.
- Omit `no_connect` pins from `pins`; `Component()` wires `NotConnected()` automatically.

`io()`:

- Preferred form: `NAME = io(template, ...)` where `template` is a net/interface type or instance (e.g. `Power(voltage="3.3V")`).
- Name is inferred from the assignment target or struct field: `VDD = io(Power())` infers `"VDD"`. Use UPPERCASE names by convention.
- `optional=True` means omitted inputs get auto-generated nets or interfaces.

`config()`:

- Preferred form: `name = config(typ, default=..., ...)`. Name is inferred from the assignment target. Use lowercase names by convention.
- `typ` can be primitive types, enums, records, or physical value constructors like `Voltage` or `Resistance`.
- `allowed=` constrains accepted values to a discrete set.
- Strings auto-convert when possible: `"10k"` can become `Resistance("10k")`; `"0603"` can become an enum value.
- Use physical types from `@stdlib/units.zen` for physical-value configs. Use `enum()` only for discrete design choices.

Utilities:

- `Board(name, layout_path, layers=..., config=...)` defines board-level defaults. Define once near the top of a board file and usually set `name`, `layers`, and `layout_path`.
- `Layout(name, path)` associates reusable layout metadata to a module.
- `check(condition, message)`, `warn(message)`, and `error(message)` are the validation and diagnostic primitives.

## Authoring Idioms

### Power, Interfaces, And Checks

- Keep power domains explicit with `Power(voltage=...)`, `Ground`, typed bus interfaces, and `@stdlib/checks.zen` helpers such as `voltage_within(...)`.
- Every `Power` `io()` declares its voltage range in the template unless the existing local API intentionally keeps it generic.
- Connect `Power` and `Ground` ios directly to pins and passives.
- Use `help=` when it adds integrator-visible meaning that is not already obvious from the name, type, or default. Keep it concise; prefer one-line `io()` / `config()` declarations when readable. Omit help text that merely restates those fields.

```zen
VDD = io(Power(voltage="3.0V to 5.5V"))
GND = io(Ground)
EN = io(Net, help="High to enable the regulator")
```

### Configs And Computation

- Expose meaningful design choices, not incidental implementation details. Good configs include output voltage, gain, cutoff frequency, address, mode, or optional feature enablement. Avoid configs for fixed decoupling values, passive package sizes, and test-point style unless local code already makes them public API.
- Prefer one meaningful physical config over raw R/C/L strings. For example, expose a cutoff `Frequency` and compute snapped passives internally.
- Put non-trivial calculations in named functions with datasheet section or equation references when available. Snap results to E-series values with `e96()`, `e24()`, or the appropriate stdlib utility.

```zen
def load_r(v_out, v_sense):
    """Datasheet §8.1.1 / Eq 4: V_OUT = V_SENSE × gm × R_L"""
    GM = Current("200uA") / Voltage("1V")
    return e96(v_out / (v_sense * GM))
```

### DNP And Optional Circuitry

- Configs may change component values and `dnp=` state, but they should not change which instances or nets exist in the schematic.
- Never use conditional instantiation to add, remove, or reconnect circuitry. Always instantiate the relevant components and use `dnp=` for population state.
- When a config selects a value on the same two nets, prefer one component with a computed value.
- When a config selects between mutually exclusive net straps, instantiate each strap option and DNP the inactive ones so topology stays stable.
- Leverage an IC's internal pull-up or pull-down when the default mode uses it; use external bias components with `dnp=` only for populated alternatives.

```zen
load("@stdlib/units.zen", "Voltage", "Resistance")
load("@stdlib/utils.zen", "e96")

Resistor = Module("@stdlib/generics/Resistor.zen")

Mode = enum("PFM", "PWM")
mode = config(Mode, default="PFM")
voltage_out = config(Voltage, default="5V", allowed=["3.3V", "5V"])

VOUT = io(Power(voltage=voltage_out))
GND = io(Ground())

_VFB = Voltage("0.8V")
_R_FB_TOP_VAL = Resistance("100kohm")

def _fb_bottom(vout):
    """Datasheet Table 1: R2 = R1 × VFB / (VOUT − VFB)"""
    return e96(_R_FB_TOP_VAL * _VFB / (vout - _VFB))

VCC = Power()
FB = Net()
MSYNC = Net()

# Same feedback divider instances and nets for every output voltage; only value changes.
Resistor(name="R_FB_TOP", value=_R_FB_TOP_VAL.with_tolerance("1%"), package="0402", P1=VOUT, P2=FB)
Resistor(name="R_FB_BOT", value=_fb_bottom(voltage_out).with_tolerance("1%"), package="0402", P1=FB, P2=GND)

# Same strap options and nets for every mode; only population changes.
Resistor(name="R_MSYNC_GND", value="0ohm", package="0402", P1=MSYNC, P2=GND, dnp=mode != Mode("PFM"))
Resistor(name="R_MSYNC_VCC", value="0ohm", package="0402", P1=MSYNC, P2=VCC, dnp=mode != Mode("PWM"))
```

### Naming

| Element | Convention | Example |
|---|---|---|
| `io()` names | UPPERCASE | `VDD`, `GND`, `I2C` |
| `config()` names | lowercase | `input_filter`, `output_voltage` |
| Internal nets | `_` prefix | `_VREF`, `_XI`, `_RBIAS` |
| Components | Uppercase functional prefix | `R_LOAD`, `C_VDD`, `U_LDO` |
| Differential pairs | `_P` / `_N` suffixes | `IN_P`, `IN_N` |

## Packages And Manifests

Imports and dependencies:

- `@stdlib/...` is implicit and toolchain-managed; do not declare it in `[dependencies]`.

`pcb.toml` per package type:

- Workspace root: `[workspace]` metadata and members.
- Board packages: `[board]` and `[dependencies]`.
- Reusable packages (modules, components): `[dependencies]` and optional default `parts`.

## Stdlib

Prelude symbols available in `.zen` files without `load()`: `Net`, `Power`, `Ground`, `NotConnected`, `Board`, `Layout`, `Part`. Local definitions can shadow them.

`@stdlib/interfaces.zen`:

- Common interfaces: `DiffPair`, `I2c`, `I3c`, `Spi`, `Qspi`, `Uart`, `Usart`, `Swd`, `Jtag`, `Usb2`, `Usb3`, and others.
- `UartPair()` and `UsartPair()` generate cross-connected point-to-point links.

`@stdlib/units.zen`:

- Physical types: `Voltage`, `Current`, `Resistance`, `Capacitance`, `Inductance`, `Impedance`, `Frequency`, `Temperature`, `Time`, `Power`.
- Constructors accept point values and ranges:

  ```python
  Voltage("3.3V")             # point value
  Resistance("4k7")           # 4.7kΩ resistor notation
  Capacitance("100nF")
  Voltage("1.1–3.6V")          # range
  Voltage("11–26V (12V)")      # range with explicit nominal
  ```

- Arithmetic tracks units automatically: `Voltage("3.3V") * Current("0.5A")` → `1.65W`; `Voltage("5V") / Current("100mA")` → `50Ω`.
- Properties: `.value` (alias for `.nominal`), `.nominal`, `.min`, `.max`, `.tolerance`, `.unit`.
- Methods: `.with_tolerance(t)`, `.with_value(v)`, `.with_unit(u)`, `.abs()`, `.diff(other)`, `.within(other)`, `.matches(other)`.
- Operators: `+`, `-`, `*`, `/` (with unit tracking), `<`, `>`, `<=`, `>=`, `==` (strict equality against another `PhysicalValue`), unary `-`. Use `.matches(other)` for coercive comparisons against strings or scalars, e.g. `Voltage("5V").matches("5V")`.
- String formatting: point → `"3.3V"`; symmetric tolerance → `"10k 5%"`; range → `"11–26V (16V nom.)"`.

`@stdlib/checks.zen`:

- `voltage_within(...)` is the main reusable `io()`-boundary power-rail check.

`@stdlib/utils.zen`:

- `e3`, `e6`, `e12`, `e24`, `e48`, `e96`, `e192` snap physical values to standard E-series.

`@stdlib/generics/*`:

- Prefer generics for common parts: `Resistor`, `Capacitor`, `Inductor`, `FerriteBead`, `Led`, `Rectifier`, `Zener`, `Tvs`, `Crystal`, `Thermistor`, `TestPoint`, `PinHeader`, `TerminalBlock`, `NetTie`, `SolderJumper`, `MountingHole`, `Standoff`, `Fiducial`, `Version`.
- `Diode` is deprecated; use `Rectifier` (standard/Schottky), `Zener` (breakdown/reference), or `Tvs` (transient suppressor).
