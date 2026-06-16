# pcb-zen-wasm

WebAssembly bindings for the Zen PCB design language, intended for use in the
browser via [wasm-pack](https://rustwasm.github.io/wasm-pack/).

The npm package is built and published from [`bin/build-wasm-bundle.sh`](../../bin/build-wasm-bundle.sh).

To smoke-test the generated WASM bundle against a `pcb publish` release zip:

```sh
node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs \
  --build-wasm \
  --stdlib path/to/stdlib.tar.zst \
  --bundle path/to/release.zip
```
