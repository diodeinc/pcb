---
name: zener-language
description: Use before reading or changing `.zen` files for Zener semantics, package APIs, dependency workflow, stable topology, and validation.
---

# Zener Language

Zener is Starlark plus PCB-specific modules, typed electrical connections, physical parts, sourcing, layout, and validation.

## Discover Before Authoring

Use `pcb doc --package @stdlib` or `pcb doc --package <package>` to inspect public APIs and their source roots. Read the installed implementation when exact behavior matters.

For toolchain behavior that may have changed, inspect the installed version and nearby entries in the [`pcb` changelog](https://github.com/diodeinc/pcb/blob/main/CHANGELOG.md) instead of relying on old examples.

## Modules and Components

A `.zen` file can be:

- a normal Starlark module imported with `load()`; or
- an instantiable schematic module imported with `Module()`.

Relative paths stay within the current package. Cross-package imports use full package URLs:

```zen
load("./helpers.zen", "helper")
LocalBlock = Module("./LocalBlock.zen")
RemoteBlock = Module("github.com/org/repo/modules/RemoteBlock.zen")
```

An instantiation passes `name=...`, its public `io()` and `config()` inputs, and optional properties such as `dnp`, `properties`, or `schematic`.

`Component()` creates a physical part from `name`, `symbol`, and `pins`. The selected symbol is the authority for its footprint, part identity, datasheet, and pins. Keep those properties correct in `.kicad_sym` rather than repeating them in Zener. Omit true `no_connect` pins; they are wired to `NotConnected()` automatically.

Use `Symbol(library, name=...)` for multi-symbol libraries. Use `Part(mpn=..., manufacturer=...)` only when the symbol does not already provide part identity.

`Layout(name, path)` associates reusable layout metadata with a module.

## IO and Config

Define public electrical connections as flat top-level assignments:

```zen
VDD = io(Power(voltage="3.0V to 5.5V"))
GND = io(Ground)
EN = io(Net, optional=True)
```

Do not introduce legacy `Pins = struct(...)` wrappers in new or touched APIs.

With `optional=True`, an omitted IO receives an automatically generated net or interface.

`Net` is the base connection type. `Power`, `Ground`, `NotConnected`, and stdlib interfaces add constraints and semantics. Across an `io()` boundary, `NotConnected` can promote to any net type, a specialized net can demote to `Net`, and a plain `Net` does not automatically gain specialized semantics. Adapt intentionally with casts such as `Power(net, voltage=...)` or `Net(power_net)`.

Use stdlib interfaces such as `DiffPair`, `I2c`, `I3c`, `Spi`, `Qspi`, `Uart`, `Usart`, `Swd`, `Jtag`, `Usb2`, and `Usb3` when the grouped protocol semantics are meaningful.

Use `UartPair()` and `UsartPair()` when a point-to-point link should cross-connect the two endpoints.

Declare a voltage range on every public `Power` IO unless the API is intentionally generic.

Define non-electrical choices with typed `config()`:

```zen
output_voltage = config(Voltage, default="3.3V", allowed=["3.3V", "5V"])
```

Load physical value types from `@stdlib/units.zen`: `Voltage`, `Current`, `Resistance`, `Capacitance`, `Inductance`, `Impedance`, `Frequency`, `Temperature`, `Time`, and `Power`. String defaults and allowed values auto-convert to the declared physical type. Use enums for non-physical modes and strategies. Expose application-level choices rather than internal passive values or implementation details.

Physical constructors accept point values, engineering notation, ranges, and tolerances. Arithmetic tracks units. Equality between two physical values is strict; use `.matches(...)` for coercive comparison with a string or scalar.

Useful physical-value operations include `.with_tolerance(...)`, `.with_value(...)`, `.abs()`, `.diff(...)`, `.within(...)`, and `.matches(...)`.

Enum defaults use the selected string value:

```zen
Mode = enum("PULLUP", "PULLDOWN")
mode = config(Mode, default="PULLUP")
```

## Stable Topology

Configs may change values and `dnp=` state, but should not add, remove, or reconnect schematic instances. Stable instance and net identity preserves layout and reviewability.

- Compute a selected value on one component when the nets do not change.
- Instantiate every mutually exclusive strap or option and DNP the inactive alternatives.
- Do not use conditional instantiation to change topology.
- Use an IC's internal pull-up or pull-down for its default mode when appropriate; add external bias components with `dnp=` only for populated alternatives.

Put non-trivial electrical calculations in named functions. Cite the relevant datasheet equation or table and snap calculated values with the appropriate stdlib E-series helper.

The available E-series helpers in `@stdlib/utils.zen` are `e3`, `e6`, `e12`, `e24`, `e48`, `e96`, and `e192`.

Use `check`, `warn`, `error`, and stdlib checks for enforceable electrical constraints rather than documenting them only in comments.

For reusable power-rail boundary checks, inspect and prefer `voltage_within(...)` from `@stdlib/checks.zen` instead of duplicating the constraint.

## Public Compatibility

Reusable-package compatibility includes the public `io()` and `config()` API, entrypoints, behavior, layout, and physical integration assumptions. A build of the current package does not prove existing consumers remain compatible.

If consumers must change to adopt an update, treat it as breaking. Document the migration and use a breaking commit.

## Schematic Position State

`# pcb:sch <ID> ...` comments persist schematic placement. Preserve them during textual edits and add new code above the block.

When renaming or deleting an item, update or remove only its corresponding records. Do not add records or edit coordinates by hand unless the user requested schematic layout work.

## Packages and Dependencies

The stdlib is toolchain-managed and does not belong in `[dependencies]`. For other packages, change the `load()` or `Module()` import in `.zen`, then run `pcb sync`. Let `pcb sync` maintain direct and indirect dependency state in `pcb.toml`.

Use `pcb list -m -u` to inspect compatible and breaking updates. Use `pcb add -u` for compatible updates and `pcb list -m -versions <url>` plus `pcb add <url>@<version>` for a specific or breaking version. Do not hand-edit resolved versions or use the legacy `pcb update` workflow.

Board roots contain workspace and board metadata. Registry roots contain reusable component and module members without a root board. Reusable packages contain their own direct dependencies and optional default parts.

## Style

Match the surrounding Zener code. Keep declarations concise, use comments for evidence or non-obvious judgment, and avoid decorative section banners or prose that restates the code. These cleanup rules never apply to `# pcb:sch` records.

Use established naming:

- public `io()` names: uppercase;
- `config()` names: lowercase;
- component instances: uppercase functional names; and
- differential signals: `_P` and `_N`.

Prefer stdlib generics for common passives, discretes, connectors, test points, and mechanical features. Inspect the current stdlib package rather than relying on a memorized inventory. Use `Rectifier`, `Zener`, or `Tvs` instead of the deprecated generic `Diode`.

For ordinary boards, prefer `Board(..., layers=<count>)`; standard defaults exist for 2, 4, 6, 8, and 10 layers. Customize them with `outer_copper_weight`, `copper_finish`, `solder_mask_color`, `track_widths`, and `via_dimensions`. Use explicit stackup and design-rule records only when those defaults are insufficient; an explicit `config` merges over the layers-derived defaults.

## Validation

After changing imports, run `pcb sync` from the relevant workspace or package. Run `pcb fmt` on changed Zener and `pcb build <path>` for the affected entrypoints. Use `pcb bom <entrypoint>.zen -f json` when sourceability or part selection is relevant.

Verification should cover the public API, electrical constraints, stable topology, dependencies, sourceability where applicable, and preservation of schematic position state.
