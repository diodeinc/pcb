# pcb-zen-wasm

`pcb-zen-wasm` is an internal build-and-test target for the Zener evaluator's
WebAssembly compatibility. It is not published to npm or crates.io.

Buildkite builds the WASM bindings and evaluates a locally published fixture
with Node.js, checking module/stdlib resolution and JavaScript serialization.
This reuses the native compiler built by the workspace tests and needs no
private boards or service-account credentials.

Results are ordinary JavaScript values (`number`, object, array, `null`).
Values outside `Number.MAX_SAFE_INTEGER` follow JavaScript Number precision.

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
