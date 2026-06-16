# pcb-docgen

Generates stdlib docs (`stdlib.mdx`) from `.zen` files.

## Notes

- Input stdlib path is typically the workspace-local, toolchain-versioned stdlib root.
- Output is deterministic and used by `pcb doc`.

## CLI

```bash
cargo run -p pcb-docgen
cargo run -p pcb-docgen -- <stdlib_path> <docs_dir> <pcb_cli_path>
```

## Library

```rust
use pcb_docgen::generate_stdlib_mdx;
use std::path::Path;

let result = generate_stdlib_mdx(
    Path::new("../lib/std"),
    Path::new("docs/pages"),
    Path::new("target/debug/pcb"),
)?;
```
