# pcb-zen-wasm

WebAssembly bindings for the Zen PCB design language.

## Testing with WASI

Build the WASI binary:

```bash
cargo build -p pcb-zen-wasm --bin pcb-zen-wasi --target wasm32-wasip2 --release
```

Run with wasmtime:

```bash
cat src.zip | wasmtime run target/wasm32-wasip2/release/pcb-zen-wasi.wasm
```
