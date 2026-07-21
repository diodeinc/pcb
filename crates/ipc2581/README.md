# ipc2581

`ipc2581` parses IPC-2581 XML into typed Rust data. It covers document metadata,
content dictionaries, ECAD steps, layers, stackups, components, and BOM data.

The parser interns repeated strings. Resolve an interned value through the
parsed document:

```rust
use ipc2581::Ipc2581;

let document = Ipc2581::parse_file("design.xml")?;
for layer in &document.ecad().unwrap().cad_data.layers {
    println!("{}", document.resolve(layer.name));
}
```

Call `validate_file` to validate a document against the vendored IPC-2581C XML
schema before parsing it.

```bash
cargo test -p ipc2581
```
