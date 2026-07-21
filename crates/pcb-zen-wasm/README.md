# pcb-zen-wasm

`pcb-zen-wasm` exposes the Zener evaluator to browser applications through
WebAssembly.

[`bin/build-wasm-bundle.sh`](../../bin/build-wasm-bundle.sh) builds and publishes
the npm package with `wasm-pack`.

To test a generated bundle against a `pcb publish` release archive, run:

```sh
node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs \
  --build-wasm \
  --stdlib path/to/stdlib.tar.zst \
  --bundle path/to/release.zip
```
