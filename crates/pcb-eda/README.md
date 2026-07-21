# pcb-eda

`pcb-eda` parses KiCad symbol libraries into the EDA data model used by the PCB
toolchain. It preserves symbols, pins, internal connectivity, sourcing metadata,
and the original S-expression when available.

`Symbol` reads a single-symbol `.kicad_sym` file. `SymbolLibrary` reads a
multi-symbol file or split `.kicad_symdir` directory. Unsupported file types,
invalid S-expressions, and missing files return errors.

The crate does not download EDA assets or generate Zener source.

```bash
cargo test -p pcb-eda
```
