# pcb-zen-wasm

`pcb-zen-wasm` exposes the Zener evaluator to browser applications through
WebAssembly.

[`bin/build-wasm-bundle.sh`](../../bin/build-wasm-bundle.sh) builds a local browser
bundle with `wasm-pack` in `target/wasm-bundle`. It does not publish anything.
Pass `ipc` to the same script to build the [IPC bindings](../pcb-ipc-wasm/README.md).

To test a generated bundle against a `pcb publish` release archive, run:

```sh
node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs \
  --build-wasm \
  --stdlib path/to/stdlib.tar.zst \
  --bundle path/to/release.zip
```
