# stdlib-docgen

Automated documentation generator for the Zener standard library (`@stdlib`).

## Overview

This crate parses `.zen` files from the stdlib directory and generates a single `stdlib.mdx` 
documentation file that is used by the `pcb doc` command.

**Important**: The stdlib version is re-exported from `pcb-zen-core::STDLIB_VERSION`, so it 
stays automatically in sync when the stdlib version is bumped.

## How it works

1. **File Discovery**: Walks the cached stdlib at `~/.pcb/cache/github.com/diodeinc/stdlib/<version>`, 
   excluding `test/` and `kicad/` directories
2. **Classification**: Distinguishes between library files (functions/types) and module files (instantiable components)
3. **Library Parsing**: Extracts docstrings, functions, types, and constants using regex-based parsing
4. **Module Signatures**: Runs `pcb build <file.zen> --netlist` to get the `__signature` JSON
5. **MDX Generation**: Produces deterministic `docs/pages/stdlib.mdx`

## Usage

### CLI

```bash
# Generate stdlib docs using cached stdlib (default)
cargo run -p stdlib-docgen

# Generate with custom paths (for development/testing)
cargo run -p stdlib-docgen -- <stdlib_path> <docs_dir> <pcb_cli_path>
```

The tool will use the stdlib from `~/.pcb/cache/github.com/diodeinc/stdlib/<version>` by default.
If the cache is empty, run `pcb build` on any project first to populate it.

### As a library

```rust
use stdlib_docgen::generate_stdlib_mdx;
use std::path::Path;

let result = generate_stdlib_mdx(
    Path::new("../stdlib"),
    Path::new("docs/pages"),
    Path::new("target/debug/pcb"),
)?;

println!("Generated {} libraries and {} modules", 
    result.library_count, result.module_count);
```

## Docstring Conventions

### File-level docstrings

Put a triple-quoted docstring at the top of the file (after `load` statements):

```python
"""Short summary of the module.

Longer description with examples if needed.
"""

load("@stdlib/units.zen", "Voltage")
```

### Function docstrings

Put a triple-quoted docstring as the first statement in the function body:

```python
def my_function(arg1, arg2):
    """Short summary of what the function does.

    Args:
        arg1: Description of arg1
        arg2: Description of arg2

    Returns:
        Description of return value
    """
    ...
```

### Module files

Module files are detected by the presence of `config()` or `io()` declarations.
Their signatures are automatically extracted from `pcb build --netlist` output.

Add a file-level docstring to describe the component's purpose:

```python
"""
Pin Header Connector Component

A configurable pin header component that supports:
- Single and dual row configurations
- Multiple pitch options
- Various mount types

Example usage:
    PinHeader(name="J1", pins=4, P1=vcc, P2=gnd)
"""

package = config("package", Package, default=Package("0603"))
P1 = io("P1", Net)
```

## Integration with build.rs

To regenerate stdlib docs during `cargo build`, add to `pcb-docs/build.rs`:

```rust
use stdlib_docgen::generate_stdlib_mdx;

// In main():
stdlib_docgen::generate_stdlib_mdx(
    &stdlib_root,
    &docs_dir,
    &pcb_cli,
)?;
```
