# ipc2581

A Rust parser for IPC-2581 that converts XML into typed Rust data structures.

## Usage

```rust
use ipc2581::Ipc2581;

let doc = Ipc2581::parse_file("design.xml")?;

// Access parsed data
println!("Revision: {}", doc.revision());

// Resolve interned strings
if let Some(ecad) = doc.ecad() {
    for layer in &ecad.cad_data.layers {
        let name = doc.resolve(layer.name);
        println!("Layer: {} ({:?})", name, layer.layer_function);
    }
    
    for step in &ecad.cad_data.steps {
        for component in &step.components {
            let refdes = doc.resolve(component.ref_des);
            println!("Component: {}", refdes);
        }
    }
}
```

## What's Parsed

- Content (FunctionMode, Dictionaries)
- ECAD (CadHeader, CadData: Steps, Layers, Stackup)
- BOM (Items, Assembly)
- Metadata (LogisticHeader, HistoryRecord)
