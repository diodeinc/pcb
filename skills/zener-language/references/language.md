# Zener Language Reference

Base language:

- Assume normal Starlark semantics for expressions, functions, loops, comprehensions, dicts, lists, and `load()`.
- The rules below are the Zener-specific layer that matters most.

Modules:

- A `.zen` file is either a normal Starlark module loaded with `load()` or an instantiable schematic module loaded with `Module()`.
- `load("./foo.zen", "helper")` imports Starlark symbols into scope.
- `Foo = Module("./Foo.zen")` or `Foo = Module("github.com/org/repo/path/Foo.zen")` loads an instantiable subcircuit.
- `./` paths are relative to the current file and resolve within the same package. Cross-package `load()` and `Module()` require the full package URL.
- Instantiation always includes `name=...` and then any `io()` / `config()` inputs.
- Useful extra instantiation parameters include `properties`, `dnp`, and `schematic`.

Nets and interfaces:

- `Net(name=None, voltage=None, impedance=None)` is the base connection type.
- `Power`, `Ground`, and `NotConnected` are specialized net types; more specialized net types live in stdlib.
- Across `io()` boundaries: `NotConnected` can promote to any net type; specialized nets can demote to plain `Net`; plain `Net` does not auto-promote to specialized types.
- Use explicit casts like `Power(net, voltage=...)` or `Net(power_net)` when needed.
- `interface(...)` defines grouped reusable connectivity. Fields can be nets, nested interfaces, or typed `field(...)` values for non-net parameters.
- Interfaces are the preferred way to model buses and grouped signals.

Components and sourcing:

- `Component(...)` is the primitive physical-part constructor.
- Required fields are effectively `name`, `symbol`, and `pins`.
- Prefer `part=Part(mpn=..., manufacturer=...)` for sourcing over legacy scalar `mpn` and `manufacturer`.
- `Symbol(library, name=None)` points at a `.kicad_sym`; `name` is required for multi-symbol libraries.
- If a package manifest defines `parts`, `Component()` may inherit default sourcing from that manifest.
- Omit `no_connect` pins from `pins`; `Component()` wires `NotConnected()` automatically.

`io()`:

- Preferred form: `NAME = io(template, ...)` where `template` is a net/interface type or instance (e.g. `Power(voltage="3.3V")`).
- Name is inferred from the assignment target or struct field: `VDD = io(Power())` infers `"VDD"`.
- Use UPPERCASE names by convention.
- `optional=True` means omitted inputs get auto-generated nets or interfaces.

`config()`:

- Preferred form: `name = config(typ, default=..., ...)`. Name is inferred from the assignment target.
- Use lowercase names by convention.
- `typ` can be primitive types, enums, records, or physical value constructors like `Voltage` or `Resistance`.
- `allowed=` constrains accepted values to a discrete set.
- Strings auto-convert when possible: `"10k"` can become `Resistance("10k")`; `"0603"` can become an enum value.

Utilities:

- `Board(name, layout_path, layers=..., config=..., outer_copper_weight=...)` defines board-level defaults and layout path.
- In normal board authoring, define `Board(...)` once near the top of the board file and usually set `name`, `layers`, and `layout_path`.
- `Layout(name, path, hints=None, modifiers=None, bom_profile=...)` associates reusable layout metadata.
- `File(path)` resolves an existing file relative to the current package.
- `Path(path, allow_not_exist=False)` resolves a possibly non-existent path and supports package paths.
- `check(condition, message)`, `warn(message)`, and `error(message)` are the main validation and diagnostic primitives.

Tool-managed metadata:

- Trailing `# pcb:sch ...` position comments are tool-managed schematic placement metadata.
- Never hand-edit positions or add new placement comments. When renaming a component or net, update the corresponding names inside existing `# pcb:sch` comments to maintain consistency.

More detail:

- For language semantics, read `~/.pcb/docs/spec.md`.
