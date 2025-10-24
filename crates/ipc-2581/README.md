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
- CLI tool with validation and integrity checks

## Implemented

### Core Parsing
- Content section (function mode, refs, dictionaries)
- DictionaryColor, DictionaryLineDesc, DictionaryStandard
- Geometric primitives: Circle, RectCenter, RectRound, Oval, Contour
- Polygon parsing (PolyBegin, PolyStepSegment, PolyStepCurve)
- LogisticHeader, HistoryRecord
- All 8 function modes

### ECAD Data
- Layers with 17 layer functions (CONDUCTOR, PLANE, DRILL, etc.)
- Components, Packages, PadStack definitions
- LogicalNets with pin connectivity
- LayerFeatures: Holes, Pads, Traces
- Detailed Stackup with layer materials, thickness, dielectric constants
- Board profile and dimensions

### BOM Data
- Bill of Materials with electrical/mechanical categorization
- Component quantities and reference designators

## CLI Tool

Validate and analyze IPC-2581 files:

```bash
# Build the CLI
cargo build --release --bin ipc2581

# Check a single file
./target/release/ipc2581 check design.xml

# Check multiple files
./target/release/ipc2581 check file1.xml file2.xml file3.xml

# Verbose output with detailed stackup info
./target/release/ipc2581 check design.xml --verbose
```

The CLI performs comprehensive data integrity checks:
- Component package and layer references
- Pin connectivity validation (all pins reference existing components)
- Drill counts and plating status
- Board dimensions and stackup
- BOM consistency

## Testing

Tested against 54 official IPC-2581 Rev C test cases from ipc2581.com.

```bash
cargo test --release
```
