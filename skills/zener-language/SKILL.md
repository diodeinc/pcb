---
name: zener-language
description: Canonical Zener HDL semantics and workflow. Use before reading or modifying `.zen` files. Covers module loading and instantiation, `io()`/`config()` API design, nets/interfaces/power domains, components and sourcing, `pcb.toml` manifests, stdlib/package discovery with `pcb doc`, physical units, generics, checks, DNP patterns, naming, and validation.
---

# Zener Language

Canonical Zener HDL semantics and authoring guidance.

## Workflow

1. Use `pcb doc --package @stdlib` or `pcb doc --package <package>` to find the public API and source root (`<!-- source: ... -->`); add `--list` for the file tree. Read source from that root for exact behavior.
2. Preserve `# pcb:sch ...` comments; see "Schematic Position Comments" below.
3. After adding, removing, or changing package `Module()` / `load()` imports, run `pcb sync` from the relevant workspace or package, then run `pcb build <path>` to validate. `pcb sync` is the dependency reconciliation step; `pcb build` is the validation step.
4. For recent Zener, stdlib, and `pcb` CLI changes, check the pcb changelog entries for the installed version and nearby previous releases: <https://github.com/diodeinc/pcb/blob/main/CHANGELOG.md>

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

Components and sourcing:

- `Component(...)` is the primitive physical-part constructor. Required fields are effectively `name`, `symbol`, and `pins`.
- The symbol is the source of truth for footprint, part metadata, and datasheet metadata. Make the symbol properties correct; do not repeat `footprint=`, `part=`, or `datasheet=` in `Component()` when they are already provided by the symbol.
- Prefer `part=Part(mpn=..., manufacturer=...)` over legacy scalar `mpn` and `manufacturer` when part metadata is not already in the symbol.
- `Symbol(library, name=None)` points at a `.kicad_sym`; `name` is required for multi-symbol libraries.
- Omit `no_connect` pins from `pins`; `Component()` wires `NotConnected()` automatically.

`io()`:

- Preferred form: flat top-level `NAME = io(template, ...)` where `template` is a net/interface type or instance, e.g. `Power(voltage="3.3V")`.
- Do not introduce `Pins = struct(...)` wrappers for component pins; that older style is deprecated. Existing packages may still use it, but new and touched `.zen` should expose pins as top-level `io()`s.
- Name is inferred from the assignment target. `optional=True` means omitted inputs get auto-generated nets or interfaces.

`config()`:

- Preferred form: `name = config(typ, default=..., ...)`; name is inferred from the assignment target.
- `typ` can be primitive types, enums, records, or physical values such as `Voltage`, `Current`, or `Resistance`.
- Use physical types from `@stdlib/units.zen` for every physical-value config, even when only a few choices are valid. Constrain discrete choices with `allowed=[...]`; strings auto-convert, e.g. `config(Current, default="3A", allowed=["1A", "2A", "3A"])`.
- Use `enum()` only for non-physical design choices such as operating mode, protocol variant, polarity, or enablement strategy.

Public compatibility:

- For reusable packages, compatibility means existing consumers can update without changing their Zener, layout, or integration assumptions.
- Breaking changes include public interface changes (`io()`, `config()`, entrypoints, module call shape), substantial layout/physical integration changes, or behavior changes that require consumer action. Collapsing loose ios into one interface is breaking even if the netlist still builds.
- `pcb build` passing only validates the current package; it does not prove existing consumers remain compatible. When making a breaking change, document the migration and mark the commit as breaking.

Utilities:

- `Layout(name, path)` associates reusable layout metadata to a module.
- `check(condition, message)`, `warn(message)`, and `error(message)` are the validation and diagnostic primitives.

## Authoring Idioms

### Power, Interfaces, And Checks

- Keep rails explicit with prelude `Power(voltage=...)` and `Ground`; each public `Power` `io()` declares its voltage range unless the local API intentionally keeps it generic.
- Use `@stdlib/interfaces.zen` interfaces for buses and grouped signals that are not in the prelude; prefer public bus interfaces such as `I2c`, `Spi`, `Qspi`, `Uart`, `Usb2`, or `DiffPair` over separate loose top-level nets when the grouped signal semantics are clear.
- Use typed values and validation primitives (`check(...)`, `warn(...)`, `error(...)`, `@stdlib/checks.zen`) for electrical constraints instead of comments when possible.
- Connect `Power` and `Ground` ios directly to pins and passives.

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

VFB_REF = Voltage("0.8V")
R_FB_TOP_VAL = Resistance("100kohm")

def fb_bottom(vout):
    """Datasheet Table 1: R2 = R1 × VFB / (VOUT − VFB)"""
    return e96(R_FB_TOP_VAL * VFB_REF / (vout - VFB_REF))

VCC = Power()
FB = Net()
MSYNC = Net()

# Same feedback divider instances and nets for every output voltage; only value changes.
Resistor(name="R_FB_TOP", value=R_FB_TOP_VAL.with_tolerance("1%"), package="0402", P1=VOUT, P2=FB)
Resistor(name="R_FB_BOT", value=fb_bottom(voltage_out).with_tolerance("1%"), package="0402", P1=FB, P2=GND)

# Same strap options and nets for every mode; only population changes.
Resistor(name="R_MSYNC_GND", value="0ohm", package="0402", P1=MSYNC, P2=GND, dnp=mode != Mode("PFM"))
Resistor(name="R_MSYNC_VCC", value="0ohm", package="0402", P1=MSYNC, P2=VCC, dnp=mode != Mode("PWM"))
```

### Style

- Prefer concise one-line `io()` and `config()` declarations when readable.
- Avoid overly verbose `help=` text. Use `help=` only when it adds integrator-visible meaning that is not already obvious from the name, type, or default.
- Omit comments and help text that merely restate the code.
- Do not use decorative section-divider comments such as `# ===== Config =====`, `# ----- IOs -----`, or multi-line banner blocks. They add no value.
- These comment-cleanup rules never apply to `# pcb:sch` lines; see "Schematic Position Comments".

### Naming

| Element | Convention | Example |
|---|---|---|
| `io()` names | UPPERCASE | `VDD`, `GND`, `I2C` |
| `config()` names | lowercase | `input_filter`, `output_voltage` |
| Components | Uppercase functional prefix | `R_LOAD`, `C_VDD`, `U_LDO` |
| Differential pairs | `_P` / `_N` suffixes | `IN_P`, `IN_N` |

## Schematic Position Comments (`# pcb:sch`)

Schematic placement is stored in `# pcb:sch <ID> x=... y=... rot=...` comments at the end of a `.zen` file. Treat existing records as persisted layout state, not as ordinary comments.

- Preserve existing placement records through textual Zener edits. Add new code above the block.
- When renaming a component or net, update the matching names inside its records. When deleting a component, remove only its own records.
- Do not add records for new components, edit coordinates by hand, or otherwise change placement unless the user explicitly asks for schematic layout changes. Schematic editor/MCP layout operations may update these records as part of layout persistence. Unpositioned new items may be displayed with auto-placement, but that does not make existing placement records disposable.

## Packages And Manifests

Imports and dependencies:

- `@stdlib/...` is implicit and toolchain-managed; do not declare it in `[dependencies]`.
- Package imports in `.zen` use full package URLs without versions.
- Do not manually edit `pcb.toml` to add or remove package dependencies. Add or remove the `Module()` / `load()` import in `.zen`, then run `pcb sync`.
- `pcb sync` updates package manifests: `[dependencies]` for direct package imports and `[dependencies.indirect]` for the resolved transitive dependency state.
- Let `pcb sync` maintain `pcb.toml`, especially `[dependencies.indirect]`. Commit `pcb.toml` files after `pcb sync` changes them.

Updating dependency versions:

- Run dependency update commands from the package directory.
- `pcb list -m -u` is read-only. It shows direct remote dependencies, the latest compatible update in brackets, and the latest breaking update as `[breaking: ...]`.
- `pcb add -u` updates all direct remote dependencies to the latest stable compatible version; `pcb add -u <url>` updates one.
- For a specific or breaking version, check versions with `pcb list -m -versions <url>`, then run `pcb add <url>@<version>`. Do not edit `pcb.toml`.
- Do not use `pcb update`; it is for legacy dependency manifests.

`pcb.toml` per repository/package type:

- Board repository root: `[workspace]` metadata, `[board]` with `name`, `path`, and `description`, and board `[dependencies]`.
- Registry repository root: `[workspace]` metadata and top-level `components/**` / `modules/*` members; no `[board]`.
- Reusable packages (modules, components): `[dependencies]` and optional default `parts`.

## Stdlib

Prelude symbols available in `.zen` files without `load()`: `Net`, `Power`, `Ground`, `NotConnected`, `Board`, `Layout`, `Part`. Local definitions can shadow them.

`@stdlib/board_config.zen`:

- `Board` is a prelude helper backed by `@stdlib/board_config.zen`. For standard boards, prefer the `layers=` helper instead of manually writing stackups and design rules:

  ```zen
  Board(name="MainBoard", layout_path="layout/MainBoard", layers=4)
  ```

- `layers` selects default stackup, netclasses, constraints, and predefined sizes for common 2/4/6/8/10-layer boards.
- `outer_copper_weight`, `copper_finish`, `solder_mask_color`, `track_widths`, and `via_dimensions` customize those defaults. Extra track widths and vias are appended, deduplicated, and sorted.
- Use explicit `BoardConfig`, `Stackup`, `DesignRules`, `NetClass`, and related records only when the standard defaults are insufficient; if both `layers` and `config` are provided, `config` is merged over the layers-derived defaults.

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

- Prefer generics for common parts: `Resistor`, `Capacitor`, `Inductor`, `FerriteBead`, `Led`, `Rectifier`, `Zener`, `Tvs`, `Crystal`, `TestPoint`, `PinHeader`, `NetTie`, `SolderJumper`, `MountingHole`, `Fiducial`, `Version`.
- `Diode` is deprecated; use `Rectifier` (standard/Schottky), `Zener` (breakdown/reference), or `Tvs` (transient suppressor).
