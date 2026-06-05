---
name: registry-search
description: Search the registry for prepared Zener modules, reference designs, and component symbols while designing a board, subsystem, or spec. Use for board-facing part/module selection with `pcb search -m registry:modules` and `pcb search -m registry:components`, API inspection, sourcing comparison, and deciding what to instantiate. If no suitable registry package exists, prepare a librarian request instead of importing or authoring components inline.
---

# Registry Search

Find prepared registry content before designing a circuit or choosing a new part by hand.

Use this when working on a board, subsystem, or spec and you need a reusable schematic module, reference design, primitive component package, concrete MPN, symbol, footprint, datasheet, or sourcing signal.

## Search Modes

- **`registry:modules`** — reusable Zener packages and `.zen` entrypoints. This is the primary search mode for schematic reuse.
- **`registry:components`** — registry symbols/parts with MPN, manufacturer, footprint, datasheet, availability, and `moduleUrl`. Use this for concrete part discovery and to find the package behind a symbol.

Do not create, import, or patch component packages in this workflow. If the registry does not contain a suitable result, or the closest result needs even a small package/API/circuit tweak, produce a librarian request.

## Search Workflow

Start with reusable schematic content:

```bash
pcb search -m registry:modules <query> -f json
```

Use functional queries when the board need is functional: `"usb c source"`, `"3.3v ldo"`, `"128mb spi flash"`, `"automotive high side switch"`.

Search symbols/parts when you need a concrete MPN, footprint, availability, or the package behind a primitive:

```bash
pcb search -m registry:components <query> -f json
```

Use MPN/manufacturer queries when the user named a part: `TPS70933`, `Texas Instruments TPSM336`, `USB4105`.

Then inspect the candidate API before instantiating it:

```bash
pcb doc --package <module-url>@<version>
```

If docs are incomplete or fail, use the source path or file tree from `pcb doc` to inspect the package source instead of guessing the IO/config interface.

## Choosing Results

Prefer the most reusable correct abstraction:

1. A higher-level module or reference design that already implements the needed schematic circuit.
2. A component package with included support circuitry when that is exactly the intended use.
3. A primitive component package when the board genuinely needs only the raw part.

Use `registry:components` results to compare physical package, pinout, MPN, stock, price, datasheet, and `moduleUrl`. Use `registry:modules` results to compare entrypoints, dependencies, dependents, and package descriptions.

Ask only when tradeoffs are real: package size, cost, stock, electrical margin, automotive/industrial grade, interface differences, or user-visible feature choices.

## Using Results In A Board

Instantiate the `.zen` entrypoint from the chosen `registry:modules` result directly in the consuming `.zen` file.

```python
PartModule = Module("github.com/diodeinc/registry/components/<Manufacturer>/<NAME>/<NAME>.zen")
```

Do not manually edit `pcb.toml` to add the dependency; `pcb sync` handles manifest updates.

Use `pcb doc --package` and its reported source path for exact IO and configs. Do not infer pin names from search snippets.

After adding the package to a board or module, verify the consuming design:

```bash
pcb sync
pcb build <board-or-module-path>
pcb bom <board-or-module-path> -f json
```

## Librarian Requests

When no suitable registry content exists, or a close match needs to be changed before it is safe to use, stop the registry search workflow. Do not author or patch reusable packages inline.

If you are inside a registry checkout or explicit registry-authoring task, use `librarian`. Otherwise use `librarian-dispatch`.
