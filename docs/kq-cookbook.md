# `pcb kq` Cookbook

`pcb kq` queries KiCad-style S-expressions directly.

It does not convert input to JSON. It reads a query program, matches directly on the parsed sexpr tree, and prints sexpr results. "Metadata views" and "electrical views" are just queries, not built-ins.

## Command Shape

```bash
pcb kq <QUERY> [FILE]
```

- `<QUERY>` is a `kq` query program.
- If `[FILE]` is omitted, input is read from stdin.
- Output is always one dense sexpr per line.

Examples:

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (emit $match))'
```

```bash
pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (property $name $value _ ...))
  (emit (kv (key $name) (value $value))))' \
  /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym
```

## Mental Model

`kq` is a staged tree query language:

1. Parse the input file into a sexpr tree.
2. Start with one current node: the root.
3. Each `select` walks the current nodes' subtrees and keeps matches.
4. Captures flow forward from one stage to the next.
5. `sort-by` reorders the final match states.
6. `emit` prints either the match, a capture, or a constructed sexpr.

The important consequence is that later `select` stages refine earlier ones.

## Query Grammar

Current grammar:

```lisp
(query
  (select <pattern>)
  ...
  (sort-by $capture)
  (emit <template>))
```

Rules:

- The top-level form must be exactly one `(query ...)`.
- A query must contain at least one `(select ...)`.
- A query may contain at most one `(sort-by $capture)`.
- A query must contain exactly one `(emit ...)`.
- `(select ...)` takes exactly one pattern.
- `(sort-by ...)` takes exactly one capture.
- `(emit ...)` takes exactly one template.

Anything else is rejected.

## Pattern Language

Patterns match directly against parsed `Sexpr` nodes.

### Literals

Any ordinary atom or list is a literal match.

```lisp
symbol
"ADUM4160"
42
3.3
(property "Reference" "U")
```

- symbol literals match symbol atoms
- string literals match string atoms
- integers match integers
- floats match floats
- list literals match recursively

### Wildcard

`_` matches any single node.

```lisp
(symbol "ADUM4160" _ ...)
```

### Captures

`$name` captures one node.

```lisp
(property $name $value _ ...)
```

- captures bind whole nodes, not pre-converted strings
- captures survive into later `select` stages
- reusing a capture name enforces equality with the earlier binding

Example:

```lisp
(query
  (select (symbol $sym_name _ ...))
  (select (property "Value" $sym_name _ ...))
  (emit $match))
```

### Repetition

`...` repeats the immediately previous pattern zero or more times.

```lisp
(property $name $value _ ...)
```

This means:

- match `property`
- match one node into `$name`
- match one node into `$value`
- match zero or more additional nodes

Rules:

- `...` is only valid inside list patterns
- `...` may not appear first in a list
- `...` applies only to the immediately preceding pattern
- matching is greedy, but backtracks if needed for later items

### List Patterns

A list pattern matches a list node positionally.

```lisp
(pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...)
```

This is how the common pin projection works:

- ignore electrical type
- ignore graphical style
- skip arbitrary intervening fields
- capture the pin name
- capture the pin number

## Evaluation Semantics

Each `select` stage:

- walks every current node's subtree in preorder
- tests the pattern against every visited node
- keeps only matching nodes
- carries captures forward
- updates `$match` to the node matched by that stage

Example:

```lisp
(query
  (select (symbol "ADUM4160" _ ...))
  (select (property $name $value _ ...))
  (emit $match))
```

This means:

1. Find the `ADUM4160` symbol.
2. Within that symbol, find all matching property forms.
3. Emit those property forms.

## Emit Templates

`(emit <template>)` controls what gets printed for each final match state.

Supported template forms:

- `$capture`
- `match`
- `$match`
- any literal/template list recursively containing captures

Examples:

```lisp
(emit $match)
(emit $name)
(emit (pin_info (name $name) (number $number)))
```

Template behavior:

- captures are substituted recursively
- unmatched literal structure stays literal
- output is always printed as dense sexpr

## Sorting

`sort-by` sorts the final match states before `emit`.

```lisp
(query
  (select (symbol "ADUM4160" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_info (name $name) (number $number))))
```

Current behavior:

- only one `sort-by` is supported
- sorting is stable
- integers sort numerically
- floats sort numerically
- strings and symbols use natural ordering
- list captures fall back to dense sexpr text ordering

Natural ordering means `"1"`, `"2"`, `"10"` sort as `1, 2, 10`.

Use `sort-by` instead of shell `sort`. Shell `sort` only sees lines, not structured records.

## Core Recipes

### Emit One Symbol

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (emit $match))'
```

This is the basic "pull one symbol out of a `.kicad_sym` file" query. The result is the full symbol sexpr.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Amplifier_Operational.kicad_sym \
| pcb kq '(query
  (select (symbol "LM358" _ ...))
  (emit $match))'
```

### List Direct Properties

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (property $name $value _ ...))
  (emit $match))'
```

## Metadata And Electrical "Views"

These are not built-ins. They are just ordinary queries.

### Minimal Metadata View

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (property $name $value _ ...))
  (emit (kv (key $name) (value $value))))'
```

### Minimal Electrical Signature

This intentionally keeps only pin names and numbers.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_info (name $name) (number $number))))'
```

### Rich Electrical Signature

This includes pin name, number, electrical type, and graphical style.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (pin $electrical $graphic _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_sig (name $name) (number $number) (electrical $electrical) (graphic $graphic))))'
```

### Same Electrical Signature On A Larger MCU

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/MCU_ST_STM32F4.kicad_sym \
| pcb kq '(query
  (select (symbol "STM32F401CCFx" _ ...))
  (select (pin _ _ _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_info (name $name) (number $number))))'
```

## Art And Drawing Recipes

KiCad symbols often store art inside nested unit/style sub-symbols. `ADUM4160` is a good example:

- `ADUM4160_1_1` contains the pins
- `ADUM4160_0_0` contains text art
- `ADUM4160_0_1` contains the symbol body and line art

So "pin placement plus symbol art" is often two related queries against different nested symbols, not one built-in view.

### Inspect Nested Unit Symbols

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol $unit_name _ ...))
  (sort-by $unit_name)
  (emit $unit_name))'
```

### Extract Text Labels From Art

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol "ADUM4160_0_0" _ ...))
  (select (text $value _ ...))
  (emit (text_label $value)))'
```

### Extract Pin Placement And Orientation

This projects pin placement from the pin-bearing nested symbol.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol "ADUM4160_1_1" _ ...))
  (select (pin $electrical $graphic (at $x $y $rotation) _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_placement (name $name) (number $number) (electrical $electrical) (graphic $graphic) (at $x $y $rotation))))'
```

### Extract Rectangle Geometry

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol "ADUM4160_0_1" _ ...))
  (select (rectangle (start $x1 $y1) (end $x2 $y2) _ ...))
  (emit (box (start $x1 $y1) (end $x2 $y2))))'
```

### Extract Polyline Segments

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol "ADUM4160_0_1" _ ...))
  (select (polyline (pts (xy $x1 $y1) (xy $x2 $y2) _ ...) _ ...))
  (emit (segment (from $x1 $y1) (to $x2 $y2))))'
```

### Emit Raw Draw Nodes

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (symbol "ADUM4160_0_1" _ ...))
  (select (polyline _ ...))
  (emit $match))'
```

The same pattern works for other draw node types such as `rectangle`, `circle`, `arc`, or `text` if those nodes exist in the file.

## Multi-Unit Symbols

`pcb kq` handles multi-unit symbols well, but it handles them structurally rather than semantically.

That means:

- it sees each nested KiCad unit/style symbol as just another `(symbol ...)` node
- it does not know that `74LS00_1_1` and `74LS00_1_2` are alternate graphics for the same logical gate
- if you query pins from the top-level symbol, you may get duplicates from multiple nested variants

In practice, the right workflow is:

1. list the nested unit symbols
2. choose the exact nested symbol you want
3. query pins or art inside that nested symbol

`74LS00` is a good real example because it contains:

- gate units like `74LS00_1_1`, `74LS00_2_1`, `74LS00_3_1`, `74LS00_4_1`
- alternate graphic variants like `74LS00_1_2`
- a power unit like `74LS00_5_0`

### List Nested Unit Symbols

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/74xx.kicad_sym \
| pcb kq '(query
  (select (symbol "74LS00" _ ...))
  (select (symbol $unit_name _ ...))
  (sort-by $unit_name)
  (emit $unit_name))'
```

Typical output includes:

```lisp
"74LS00_1_1"
"74LS00_1_2"
"74LS00_2_1"
"74LS00_2_2"
"74LS00_5_0"
```

### Query One Exact Logic Unit

This avoids duplicate matches from alternate body styles.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/74xx.kicad_sym \
| pcb kq '(query
  (select (symbol "74LS00" _ ...))
  (select (symbol "74LS00_1_1" _ ...))
  (select (pin $electrical $graphic _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_sig (name $name) (number $number) (electrical $electrical) (graphic $graphic))))'
```

This returns only the pins for that one gate body:

```lisp
(pin_sig (name "~") (number "1") (electrical input) (graphic line))
(pin_sig (name "~") (number "2") (electrical input) (graphic line))
(pin_sig (name "~") (number "3") (electrical output) (graphic inverted))
```

### Query The Power Unit

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/74xx.kicad_sym \
| pcb kq '(query
  (select (symbol "74LS00" _ ...))
  (select (symbol "74LS00_5_0" _ ...))
  (select (pin $electrical $graphic _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_sig (name $name) (number $number) (electrical $electrical) (graphic $graphic))))'
```

This returns:

```lisp
(pin_sig (name "GND") (number "7") (electrical power_in) (graphic line))
(pin_sig (name "VCC") (number "14") (electrical power_in) (graphic line))
```

### Top-Level Pin Queries Can Produce Duplicates

This is sometimes useful, but for multi-unit parts it often mixes multiple nested units and alternate symbol styles.

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/74xx.kicad_sym \
| pcb kq '(query
  (select (symbol "74LS00" _ ...))
  (select (pin $electrical $graphic _ ... (name $name _ ...) _ ... (number $number _ ...) _ ...))
  (sort-by $number)
  (emit (pin_sig (name $name) (number $number) (electrical $electrical) (graphic $graphic))))'
```

For example, pin `1` appears more than once because `74LS00_1_1` and `74LS00_1_2` both contribute a version of that gate.

## Useful Patterns

### Match An Exact Property

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (property "Reference" "U" _ ...))
  (emit $match))'
```

### Emit Only Property Names

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (property $name $value _ ...))
  (emit $name))'
```

### Emit Raw Pin Nodes

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (pin _ ...))
  (emit $match))'
```

### Extract All Nested `name` Forms

```bash
cat /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Interface_USB.kicad_sym \
| pcb kq '(query
  (select (symbol "ADUM4160" _ ...))
  (select (name $value _ ...))
  (emit $value))'
```

## What `pcb kq` Can Do Today

- match literal atoms and lists
- match any node with `_`
- capture nodes with `$name`
- reuse captures to enforce equality
- match variable-length tails with `...`
- refine matches with multiple `select` stages
- sort final matches with `sort-by $capture`
- emit matched nodes
- emit captures
- construct new sexprs from captures
- query metadata-like projections as ordinary queries
- query electrical signatures as ordinary queries
- inspect art and drawing nodes as ordinary queries

## What `pcb kq` Cannot Do Yet

- no `--query-file`
- no boolean operators
- no regex matching
- no explicit parent or ancestor predicates
- no positional indexing
- no multiple sort keys
- no built-in semantic views
- no JSON output mode
- no in-place rewrites

## Caveats

### Stdin Is Input Data

This is wrong:

```bash
cat file.kicad_sym | pcb kq file.kicad_sym
```

This is right:

```bash
cat file.kicad_sym | pcb kq '(query ...)'
```

And this is also right:

```bash
pcb kq '(query ...)' file.kicad_sym
```

### Comments Are Not Query-Visible

Queries operate on parsed sexpr nodes, not comment tokens.
