# IPC-2581 Parser

Rust library for parsing IPC-2581 XML files.

## Usage

```rust
use bumpalo::Bump;
use ipc_2581::Ipc2581;

let arena = Bump::new();
let doc = Ipc2581::parse_file(&arena, "design.xml")?;

println!("Revision: {}", doc.revision());
println!("Mode: {:?}", doc.content().function_mode.mode);
println!("Layers: {}", doc.content().layer_refs.len());

for entry in &doc.content().dictionary_color.entries {
    let id = doc.resolve(entry.id);
    println!("Color {}: RGB({}, {}, {})", id,
        entry.color.r, entry.color.g, entry.color.b);
}
```

## Features

- Arena allocation with bumpalo
- String interning with PHF map
- Zero-copy XML parsing with roxmltree
- MD5 checksum validation
- Data-oriented design

## Implemented

- Content section (function mode, refs, dictionaries)
- DictionaryColor, DictionaryLineDesc, DictionaryStandard
- Geometric primitives: Circle, RectCenter, RectRound, Oval, Contour
- Polygon parsing (PolyBegin, PolyStepSegment, PolyStepCurve)
- LogisticHeader, HistoryRecord
- All 8 function modes

## TODO

- Remaining primitives (Diamond, Donut, Thermal, Hexagon, Octagon, etc.)
- DictionaryUser, BOM, Ecad, AVL sections
- Write support

## Testing

Tested against 54 official IPC-2581 Rev C test cases from ipc2581.com.

```bash
cargo test --release
```
