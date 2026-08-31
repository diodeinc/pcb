# pcb-ipc-wasm

In-memory IPC-2581 import, export, and DFM for browsers, Web Workers, and Node.
No filesystem, network service, native libraries, or shared-memory threads.

## Build and test

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --locked
./bin/build-wasm-bundle.sh ipc
node crates/pcb-ipc-wasm/scripts/smoke.mjs
npx --yes --package typescript@5.9.3 tsc --noEmit --strict --module nodenext --moduleResolution nodenext --target es2022 --lib es2022,dom crates/pcb-ipc-wasm/scripts/typecheck.ts
```

This uses the same browser-bundle builder as [pcb-zen-wasm](../pcb-zen-wasm/README.md),
writing to `target/ipc-wasm-bundle`. It does not publish anything. The smoke test
requires Node 24+ and checks all formats, compressed input, DFM/waivers, malformed
input, and a worker.

## Use

```ts
import init, { IpcDocument } from "./target/ipc-wasm-bundle/pcb_ipc_wasm.js";

await init();
const pcb = IpcDocument.fromBytes(new Uint8Array(await file.arrayBuffer()), {
  name: file.name,
  validate: true, // Optional IPC-2581C XSD validation.
});
try {
  const [gerbers] = pcb.export({ format: "gerber", zip: true });
  const report = pcb.checkDfm({ pdk: "standard" });
  // gerbers.data is Uint8Array; report.verdict is "pass" or "fail".
} finally {
  pcb.free();
}
```

Use `new IpcDocument(xml, options?)` for strings. `fromBytes` detects UTF-8 XML
or Zstandard from the bytes, including concatenated/skippable frames. Names
are report labels; no path is opened. Geometry is imported and cached on demand.
All methods are synchronous; use a Web Worker for large exports or DFM. Results
are owned copies and remain valid after `free()`.

| API | Result |
| --- | --- |
| `validate()` | Validate the source against the bundled IPC-2581C XSD. |
| `info()` | The native `ipc info --format json` summary. |
| `layers()` | Source layer names accepted by SVG/PNG export. |
| `export(options)` | `{ name, mediaType, data: Uint8Array }[]`. |
| `checkDfm(options?)` | The full native DFM report, including evidence and waivers. |
| `builtinPdks()` | Bundled PDK names and TOML sources. |

Export formats: `ipc2581`, `gerber` (Gerber X2 and XNC, optionally zipped),
`svg`, `png`, `dxf`, `bom` (JSON), `cpl` (CSV), `ict` (CSV), and `html`.
SVG/PNG require `layer`; rendering uses the native sizing and profile defaults.
IPC export returns original XML unless `mode` selects a native projection.
`layoutTarget` defaults to `board-array`; `board` selects the canonical board.
CPL keeps the native single-board semantics. The generated package includes
TypeScript declarations (TS 5.7+); unknown options and enum values throw errors.
Raw IR snapshots, editing, and panel generation are deliberately outside this API.

Custom PDKs and waivers are supplied as text:

```ts
const report = pcb.checkDfm({
  pdk: { name: "fab.toml", source: pdkToml },
  waivers: { name: "waivers.toml", source: waiverToml },
  generatedAt: "2026-08-30T12:00:00Z", // Optional; controls UTC waiver expiry.
});
```

All native PDK checks are available. Violations return a `fail` verdict; invalid
inputs or unsupported geometry throw JavaScript `Error`s. Reports preserve the
native schema, vector scene, exact PDK source, and input/PDK/waiver hashes,
including compressed input bytes. See the [DFM contract](../pcb-ipc2581-tools/docs/dfm.md).

## Portable Rust crates

```sh
cargo check --target wasm32-unknown-unknown \
  -p ipc2581 -p pcb-ir -p gerberx2 -p pcb-ipc2581-tools --no-default-features
```

The full `pcb-ir` geometry/import/dialect stack remains portable, including
KiCad data/writing, rendering, DFM geometry, and copper balancing. Disable the
tools crate's default `cli` feature to exclude filesystem commands, terminal UI,
network BOM enrichment, and native dependencies. Native DFM/balancing keep
parallel execution; WASM executes the same algorithms serially.

Custom PDKs are limited to 1 MiB. Zstandard decoding is bounded to 256 MiB; the importer limits layout
depth to 64 and expanded instances to 100,000. These checks prevent simple input
amplification, but do not guarantee a fixed runtime or memory budget.
