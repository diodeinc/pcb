---
name: zener-language
description: Canonical Zener HDL semantics reference — language constructs, package rules, manifests, and stdlib APIs. Use when writing or editing non-trivial `.zen` code and you need exact semantics for `Module()`, `io()`, `config()`, imports, `pcb.toml`, `pcb.sum`, stdlib interfaces, units, properties, or generics. Pair with `idiomatic-zener` for style; this skill is the semantics companion.
---

# Zener Language

## Workflow

1. Start from nearby workspace code. Prefer the local package's patterns before generic examples.
2. Check exact semantics here before editing when the code touches unfamiliar syntax, manifests, imports, stdlib APIs, or package interfaces.
3. Use `pcb doc` to discover package APIs whenever you need interface field names, generic parameters, or any unfamiliar surface. Output includes the resolved source path so you can read the package source directly.
   - `pcb doc --package @stdlib` (or `@stdlib/generics`) for stdlib.
   - `pcb doc --package <package>` for any installed or registry package. Pin URL packages with `@<version>`, e.g. `pcb doc --package github.com/org/repo/path@v0.2.5`.
4. When in doubt about exact behavior or semantics, read the actual source `.zen` files — stdlib modules and package sources alike. `pcb doc` is type-level only; the source is the truth.
5. Never invent syntax, stdlib modules, interfaces, fields, or package APIs.

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
- Prefer `part=Part(mpn=..., manufacturer=...)` over legacy scalar `mpn` and `manufacturer`.
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

Utilities:

- `Board(name, layout_path, layers=..., config=...)` defines board-level defaults. Define once near the top of a board file and usually set `name`, `layers`, and `layout_path`.
- `Layout(name, path)` associates reusable layout metadata to a module.
- `check(condition, message)`, `warn(message)`, and `error(message)` are the validation and diagnostic primitives.

Tool-managed metadata:

- Trailing `# pcb:sch ...` comments are tool-managed schematic placement metadata. Leave them alone, never delete them, and never add new ones. The only allowed edit is updating names inside an existing comment when you rename the matching component or net.

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
