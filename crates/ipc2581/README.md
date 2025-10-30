# ipc2581

**Pure IPC-2581 XML parser** - Zero dependencies on visualization or export.

## Overview

`ipc2581` is a maximally pure, spec-compliant parser for IPC-2581 PCB data interchange format. It converts IPC-2581 XML documents into typed Rust data structures with zero downstream processing dependencies.

## Features

- ✅ **Spec-compliant**: Pure IPC-2581 specification implementation
- ✅ **Zero vendor hacks**: No tool-specific workarounds
- ✅ **High performance**: String interning, zero-copy parsing, minimal allocations
- ✅ **Lightweight**: Only 7 dependencies (no graphics/rendering libs)
- ✅ **Type-safe**: Proper Rust enums and structs for all IPC-2581 constructs

## Installation

```toml
[dependencies]
ipc2581 = "0.3"
```

## Usage

```rust
use bumpalo::Bump;
use ipc2581::Ipc2581;

// Parse from file
let arena = Bump::new();
let doc = Ipc2581::parse_file(&arena, "design.xml")?;

// Access parsed data
println!("Revision: {}", doc.revision());

if let Some(ecad) = doc.ecad() {
    // Iterate layers
    for layer in &ecad.cad_data.layers {
        let name = doc.resolve(layer.name);
        println!("Layer: {} ({:?})", name, layer.layer_function);
    }
    
    // Access stackup
    for stackup in &ecad.cad_data.stackups {
        let name = doc.resolve(stackup.name);
        println!("Stackup: {}", name);
        
        for layer in &stackup.layers {
            if let Some(thickness) = layer.thickness {
                println!("  Thickness: {} mm", thickness);
            }
        }
    }
    
    // Access components
    for step in &ecad.cad_data.steps {
        for component in &step.components {
            let refdes = doc.resolve(component.ref_des);
            let package = doc.resolve(component.package_ref);
            println!("Component: {} ({})", refdes, package);
        }
    }
}

// Access BOM
if let Some(bom) = doc.bom() {
    for item in &bom.items {
        let part = doc.resolve(item.oem_design_number_ref);
        println!("BOM Item: {} (qty: {:?})", part, item.quantity);
    }
}
```

## Performance

- **String interning**: 121 pre-cached IPC-2581 tokens
- **Zero clones**: Efficient ownership transfer
- **Minimal allocations**: ~40KB savings per typical file
- **Fast parsing**: No tessellation or rendering during parse

## Architecture

```
ipc2581/
├── parse.rs        (2,100 lines) - Pure XML→Rust parser  
├── intern.rs       (188 lines)   - String interner
├── checksum.rs     (66 lines)    - MD5 validation
├── units.rs        (70 lines)    - Unit conversion helpers
└── types/          (1,100 lines) - Data structures
    ├── bom.rs        - Bill of Materials
    ├── content.rs    - Content section
    ├── dictionary.rs - Dictionaries  
    ├── ecad.rs       - ECAD types (main)
    ├── metadata.rs   - Headers
    ├── primitives.rs - Geometric shapes
    └── transform.rs  - Transforms
```

## Dependencies

Core dependencies only (6 crates):
- `roxmltree` - XML parsing
- `bumpalo` - Arena allocator
- `md-5` - Checksum validation
- `base64` - Checksum encoding
- `thiserror` - Error types
- `rustc-hash` - Fast hashing (FxHasher for interner)

## Visualization & Export

For rendering, visualization, and export functionality, see the companion crate:

```toml
[dependencies]
ipc2581-tools = "0.3"  # Includes ipc2581 + export modules
```

## Documentation

- [MAXIMAL_PURITY.md](MAXIMAL_PURITY.md) - Purity philosophy and metrics
- [PURITY_IMPROVEMENTS.md](PURITY_IMPROVEMENTS.md) - Detailed change log
- [IPC-2581 Specification](https://www.ipc.org/TOC/IPC-2581.pdf)

## License

MIT OR Apache-2.0
