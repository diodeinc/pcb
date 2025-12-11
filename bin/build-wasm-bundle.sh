#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

rm -rf target/wasm-bundle

wasm-pack build \
  --target web \
  --release \
  --out-dir ../../target/wasm-bundle \
  --out-name pcb_zen_wasm \
  crates/pcb-zen-wasm

# wasm-pack doesn't support overriding package name
cd target/wasm-bundle
jq '.name = "@diodeinc/pcb-zen-wasm"' package.json > package.json.tmp && mv package.json.tmp package.json
rm -f .gitignore

# Create tarball for cargo-dist extra-artifacts
cd ..
tar -czf wasm-bundle.tar.gz -C wasm-bundle .
