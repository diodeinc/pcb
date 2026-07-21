# pcb-docgen

`pcb-docgen` generates Markdown from Zener package sources for `pcb doc`. It
extracts file docstrings, module signatures, and exported library symbols.

The crate is a library and has no binary target. Use the CLI to generate package
documentation:

```bash
pcb doc --package @stdlib
pcb doc --package ./modules/PowerSupply
```

Generation requires a valid PCB workspace and resolved dependencies. Files that
cannot be evaluated produce warnings and are omitted from the output.

```bash
cargo test -p pcb-docgen
```
