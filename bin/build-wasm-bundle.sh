#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

case "${1:-zen}" in
  zen) crate=pcb-zen-wasm; bundle_dir=target/wasm-bundle ;;
  ipc) crate=pcb-ipc-wasm; bundle_dir=target/ipc-wasm-bundle ;;
  *) echo "Usage: $0 [zen|ipc]" >&2; exit 2 ;;
esac

rm -rf "$bundle_dir"

wasm-pack build \
  --target web \
  --release \
  --scope diodeinc \
  --out-dir "../../$bundle_dir" \
  --out-name "${crate//-/_}" \
  "crates/$crate"

rm -f "$bundle_dir/.gitignore"
