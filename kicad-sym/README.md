# kicad-sym

Python helper for KiCad symbol S-expressions.

The library is intentionally low-level:

- forms are plain Python `list` objects
- quoted strings are plain Python `str`
- bare atoms are `kicad_sym.Sym`
- integers and floats stay native

The core workflow is:

1. read a `.kicad_sym` file
2. modify the tree in Python
3. write the whole file back out

There is no patch DSL and no heavy semantic object model.

## Install

```bash
uv add kicad-sym
```

One-off script without editing your project dependencies:

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

print(ks.dumps(ks.library(ks.symbol("Demo"))))
PY
```

## Basic Usage

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Device.kicad_sym")
symbol = ks.get_symbol(lib, "R")

print(ks.symbol_name(symbol))
print(ks.nested_symbol_names(symbol))
print(ks.properties(symbol).get("Description"))
print(symbol)
PY
```

Parsed and built forms print as KiCad-style S-expressions, so `print(node)` and
REPL inspection are useful for quick debugging of intermediate states.

## Read A Multi-Symbol Library

Use `symbol_names()` to inspect the top-level symbols, then `get_symbol()` to
pick the one you want.

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Device.kicad_sym")

print(ks.symbol_names(lib)[:5])

resistor = ks.get_symbol(lib, "R")
capacitor = ks.get_symbol(lib, "C")

print(ks.symbol_name(resistor), ks.properties(resistor).get("Description"))
print(ks.symbol_name(capacitor), ks.properties(capacitor).get("Description"))
PY
```

For editing, the simplest workflow is still to target one top-level symbol at a
time, modify it in memory, and write the whole library back out.

## Query-Like Views In Python

The main `pcb kq` use cases translate directly into ordinary Python traversals.
The library handles tree access; Python handles filtering, sorting, and shaping
the result.

### Metadata View

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym")
sym = ks.get_symbol(lib, "ADUM4160")

print(ks.properties(sym))
PY
```

### Electrical Signature

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

def pin_view(pin):
    return {
        "name": ks.child(pin, "name")[1],
        "number": ks.child(pin, "number")[1],
        "electrical": str(pin[1]),
        "graphic": str(pin[2]),
    }

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym")
sym = ks.get_symbol(lib, "ADUM4160")

rows = [pin_view(pin) for pin in ks.pins(sym)]
rows.sort(key=lambda row: row["number"])
print(rows)
PY
```

### Exact Unit In A Multi-Unit Symbol

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

def pin_view(pin):
    return {
        "name": ks.child(pin, "name")[1],
        "number": ks.child(pin, "number")[1],
        "electrical": str(pin[1]),
        "graphic": str(pin[2]),
    }

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/74xx.kicad_sym")
sym = ks.get_symbol(lib, "74LS00")
gate = ks.get_nested_symbol(sym, "74LS00_1_1")
power = ks.get_nested_symbol(sym, "74LS00_5_0")

print(ks.nested_symbol_names(sym)[:6])
print([pin_view(pin) for pin in ks.pins(gate)])
print([pin_view(pin) for pin in ks.pins(power)])
PY
```

### Art And Geometry View

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym")
sym = ks.get_symbol(lib, "ADUM4160")

text_art = ks.get_nested_symbol(sym, "ADUM4160_0_0")
body_art = ks.get_nested_symbol(sym, "ADUM4160_0_1")
pin_unit = ks.get_nested_symbol(sym, "ADUM4160_1_1")

labels = [node[1] for node in ks.walk(text_art, "text")]
rect = next(ks.walk(body_art, "rectangle"))
start = tuple(ks.child(rect, "start")[1:3])
end = tuple(ks.child(rect, "end")[1:3])
placements = [
    {
        "name": ks.child(pin, "name")[1],
        "number": ks.child(pin, "number")[1],
        "at": tuple(ks.child(pin, "at")[1:4]),
    }
    for pin in ks.pins(pin_unit)
]

print(labels)
print({"start": start, "end": end})
print(placements[:4])
PY
```

## Explore Artwork In An Existing Symbol

Artwork-heavy symbols are easiest to explore by:

1. loading the top-level symbol
2. listing its nested unit/style symbols
3. picking the art-bearing nested symbol
4. walking only the draw nodes you care about

`ADUM4160` is a good example because its body art and pin placement live in
different nested symbols.

```bash
uv run --with kicad-sym --no-project python - <<'PY'
from pathlib import Path
import kicad_sym as ks

DRAW = {"polyline", "rectangle", "circle", "arc", "bezier", "text", "text_box"}
root = Path("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols")

lib = ks.load(root / "Interface_USB.kicad_sym")
sym = ks.get_symbol(lib, "ADUM4160")

print(ks.nested_symbol_names(sym))

body_art = ks.get_nested_symbol(sym, "ADUM4160_0_1")
draw_nodes = [node for node in ks.walk(body_art) if ks.head(node) in DRAW]

print("draw nodes:", len(draw_nodes))
for node in draw_nodes[:3]:
    print()
    print(node)
PY
```

Because forms print as KiCad-style S-expressions, `print(node)` is usually
enough to inspect intermediate artwork state while you are iterating.

## Write A Multi-Symbol Library

Build or load the top-level symbols you want, keep them in one `library(...)`
root, then save the entire file.

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.library(
    ks.symbol(
        "R_Custom",
        ks.property("Reference", "R"),
        ks.property("Value", "R_Custom"),
    ),
    ks.symbol(
        "C_Custom",
        ks.property("Reference", "C"),
        ks.property("Value", "C_Custom"),
    ),
)

resistor = ks.get_symbol(lib, "R_Custom")
ks.set_property(resistor, "Description", "Example resistor in a multi-symbol library")

lib.append(
    ks.symbol(
        "TP_Custom",
        ks.property("Reference", "TP"),
        ks.property("Value", "TP_Custom"),
    )
)

path = "/tmp/custom-passives.kicad_sym"
ks.save(path, lib)
print(path)
print(ks.symbol_names(lib))
PY
```

## Modify Artwork In An Existing Symbol

The usual pattern is:

1. clone the symbol you want to modify
2. rename the top-level symbol and any nested units you touched
3. grab the nested art symbol
4. replace or append draw nodes directly

```bash
uv run --with kicad-sym --no-project python - <<'PY'
from pathlib import Path
import kicad_sym as ks

root = Path("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols")
lib = ks.load(root / "Interface_USB.kicad_sym")
sym = ks.clone(ks.get_symbol(lib, "ADUM4160"))

sym[1] = "ADUM4160_Debug"
for nested in ks.nested_symbols(sym):
    if isinstance(nested[1], str) and nested[1].startswith("ADUM4160_"):
        nested[1] = nested[1].replace("ADUM4160", "ADUM4160_Debug", 1)

body_art = ks.get_nested_symbol(sym, "ADUM4160_Debug_0_1")

def replace_child(node, kind, new_child):
    for i, child in enumerate(node[1:], start=1):
        if isinstance(child, list) and ks.head(child) == kind:
            node[i] = new_child
            return
    node.append(new_child)

rect = next(ks.walk(body_art, "rectangle"))
replace_child(rect, "stroke", ks.stroke(0.508, stroke_type="default"))
replace_child(rect, "fill", ks.fill("background"))

body_art.append(
    ks.text(
        "DBG",
        at=(0, -9.5, 0),
        effects=ks.effects(font=ks.font(1.0, 1.0)),
    )
)
body_art.append(
    ks.circle(
        (0, 0),
        2.0,
        stroke=ks.stroke(0.254, stroke_type="default"),
        fill=ks.fill("none"),
    )
)

out = ks.library(sym)
path = Path("/tmp/ADUM4160_Debug.kicad_sym")
ks.save(path, out)
print(path)
PY
```

## Update A Property In Place

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

lib = ks.load("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Device.kicad_sym")
symbol = ks.clone(ks.get_symbol(lib, "R"))

ks.set_property(symbol, "Datasheet", "https://example.com/resistor")
ks.set_property(symbol, "Description", "Resistor with updated metadata")
ks.set_property(symbol, "Manufacturer", "Example Corp", hidden=True)

out = ks.library(symbol)
path = "/tmp/R_customized.kicad_sym"
ks.save(path, out)
print(path)
PY
```

## Build A Minimal Single-Symbol Library

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

sym = ks.symbol(
    "TestPoint_Pad",
    ks.form("pin_numbers", ks.form("hide", ks.yesno(True))),
    ks.form("pin_names", ks.form("offset", 0)),
    ks.form("exclude_from_sim", ks.yesno(False)),
    ks.form("in_bom", ks.yesno(True)),
    ks.form("on_board", ks.yesno(True)),
    ks.property("Reference", "TP", at=(0, 2.54, 0)),
    ks.property("Value", "TestPoint_Pad", at=(0, -2.54, 0)),
    ks.property("Footprint", "", at=(0, 0, 0), hidden=True),
    ks.property("Datasheet", "", at=(0, 0, 0), hidden=True),
    ks.property("Description", "One-pin test point", at=(0, 0, 0), hidden=True),
    ks.unit_symbol(
        "TestPoint_Pad",
        1,
        1,
        ks.circle((0, 0), 1.27),
        ks.pin("1", "TP", at=(-5.08, 0, 0)),
    ),
)

lib = ks.library(sym)
path = "/tmp/TestPoint_Pad.kicad_sym"
ks.save(path, lib)
print(path)
PY
```

## Build Artwork From Scratch

The helpers cover the common draw forms directly: `rectangle`, `polyline`,
`circle`, and `text`. For forms that do not have a dedicated helper yet, such as
`arc`, just drop down to raw `form(...)`.

```bash
uv run --with kicad-sym --no-project python - <<'PY'
from pathlib import Path
import kicad_sym as ks

badge = ks.symbol(
    "Artwork_Debug_Badge",
    ks.property("Reference", "SYM", hidden=True),
    ks.property("Value", "Artwork_Debug_Badge", hidden=True),
    ks.property("Footprint", "", hidden=True),
    ks.property("Datasheet", "", hidden=True),
    ks.property("Description", "Artwork example", hidden=True),
    ks.unit_symbol(
        "Artwork_Debug_Badge",
        0,
        1,
        ks.circle(
            (0, 0),
            10,
            stroke=ks.stroke(0.4, stroke_type="default"),
            fill=ks.fill("none"),
        ),
        ks.circle(
            (0, 0),
            7,
            stroke=ks.stroke(0.25, stroke_type="default"),
            fill=ks.fill("background"),
        ),
        ks.form(
            "arc",
            ks.form("start", -6, 0),
            ks.form("mid", 0, 6),
            ks.form("end", 6, 0),
            ks.stroke(0.35, stroke_type="default"),
            ks.fill("none"),
        ),
        ks.polyline(
            [(-2.5, 4), (0.5, 0.5), (-1, 0.5), (2.5, -4), (0, -0.5), (1.2, -0.5)],
            stroke=ks.stroke(0.35, stroke_type="default"),
            fill=ks.fill("none"),
        ),
        ks.text(
            "DBG",
            at=(0, -8.6, 0),
            effects=ks.effects(font=ks.font(0.9, 0.9)),
        ),
    ),
)

out = ks.library(badge)
path = Path("/tmp/Artwork_Debug_Badge.kicad_sym")
ks.save(path, out)
print(path)
PY
```

## Build A Multi-Unit Symbol

Nested KiCad unit/style symbols are direct child `(symbol ...)` nodes under the
top-level symbol, so multi-unit symbols fit the library naturally.

```bash
uv run --with kicad-sym --no-project python - <<'PY'
import kicad_sym as ks

dual = ks.symbol(
    "Demo_Dual_Opamp",
    ks.form("pin_names", ks.form("offset", 0.508)),
    ks.form("exclude_from_sim", ks.yesno(False)),
    ks.form("in_bom", ks.yesno(True)),
    ks.form("on_board", ks.yesno(True)),
    ks.property("Reference", "U", at=(0, 6.35, 0)),
    ks.property("Value", "Demo_Dual_Opamp", at=(0, -6.35, 0)),
    ks.property("Footprint", "", at=(0, 0, 0), hidden=True),
    ks.property("Datasheet", "", at=(0, 0, 0), hidden=True),
    ks.property("Description", "Example multi-unit symbol", at=(0, 0, 0), hidden=True),
    ks.unit_symbol(
        "Demo_Dual_Opamp",
        1,
        1,
        ks.polyline([(-5.08, -3.81), (5.08, 0), (-5.08, 3.81), (-5.08, -3.81)]),
        ks.pin("1", "-", electrical="input", at=(-7.62, 2.54, 0)),
        ks.pin("2", "+", electrical="input", at=(-7.62, -2.54, 0)),
        ks.pin("3", "~", electrical="output", at=(7.62, 0, 180)),
    ),
    ks.unit_symbol(
        "Demo_Dual_Opamp",
        2,
        1,
        ks.polyline([(-5.08, -3.81), (5.08, 0), (-5.08, 3.81), (-5.08, -3.81)]),
        ks.pin("5", "+", electrical="input", at=(-7.62, -2.54, 0)),
        ks.pin("6", "-", electrical="input", at=(-7.62, 2.54, 0)),
        ks.pin("7", "~", electrical="output", at=(7.62, 0, 180)),
    ),
    ks.unit_symbol(
        "Demo_Dual_Opamp",
        3,
        0,
        ks.pin("4", "V-", electrical="power_in", at=(0, -7.62, 90)),
        ks.pin("8", "V+", electrical="power_in", at=(0, 7.62, 270)),
    ),
)

lib = ks.library(dual)
path = "/tmp/Demo_Dual_Opamp.kicad_sym"
ks.save(path, lib)
print(path)
print(ks.nested_symbol_names(dual))
PY
```

## KiCad Symbol Shape

A `.kicad_sym` file is an S-expression tree rooted at `(kicad_symbol_lib ...)`.

```lisp
(kicad_symbol_lib
  (symbol "NAME"
    SYMBOL_METADATA...
    GRAPHICS...
    PINS...
    (symbol "NAME_0_1" ...)
    (symbol "NAME_1_1" ...)
  )
  (symbol "ANOTHER_NAME" ...)
)
```

In practice:

- top-level `symbol` nodes are the library entries
- nested `symbol` nodes are units or styles inside a parent symbol
- metadata is usually `property`, `in_bom`, `on_board`, `pin_names`, `pin_numbers`, and sometimes `extends`
- drawing is built from forms like `rectangle`, `polyline`, `circle`, `arc`, and `text`
- many real KiCad symbols put graphics and pins in different nested symbols

`kicad-sym` keeps that structure visible instead of wrapping it in a heavier object model.
