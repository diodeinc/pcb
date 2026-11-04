#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo="$PWD"
binaries="$(realpath "${CARGO_TARGET_DIR:-target}/debug")"
host="$(rustc -vV | sed -n 's/^host: //p')"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# Exercise the installed shim/toolchain boundary, not binaries running in the checkout.
# nextest already built both binaries; the release installer is still tested on macOS.
export HOME="$scratch/home"
export XDG_DATA_HOME="$scratch/data"
export XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache"
export PATH="$scratch/bin:$PATH"
export DIODE_API_AUTH=none
toolchain="$XDG_DATA_HOME/pcb/toolchains/local/$host"
mkdir -p "$HOME" "$scratch/bin" "$toolchain/lib"
install -m 755 "$binaries/pcb" "$scratch/bin/pcb"
install -m 755 "$binaries/pcbc" "$toolchain/pcbc"
cp -R "$repo/lib/std" "$toolchain/lib/std"
cp -R "$repo/examples" "$scratch/examples"
cp -R "$repo/test-workspaces/with-pcb-toml" "$scratch/workspace"

cd "$scratch"
pcb +local --help > /dev/null
pcb +local build examples
pcb +local build "$toolchain/lib/std" -S bom

# Scaffolding is not covered by the workspace's CLI integration tests.
pcb +local new board TB0001 github.com/test/TB0001
cd TB0001
pcb +local new package modules/mod_a
test -s TB0001.zen
test -s modules/mod_a/mod_a.zen
pcb +local build .

# Test the actual JS/WASM bindings against a real release archive, without private fixtures.
git -C "$scratch/workspace" init -q
git -C "$scratch/workspace" add .
git -C "$scratch/workspace" -c user.name=CI -c user.email=ci@example.invalid commit -qm Fixture
tar -C "$toolchain/lib/std" -cf - . | zstd -q -3 -c > "$scratch/stdlib.tar.zst"
node "$repo/crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs" \
  --publish "$scratch/workspace/boards/WorkspaceTestBoard.zen" \
  --pcbc "$scratch/bin/pcb" --pcbc-arg +local \
  --stdlib "$scratch/stdlib.tar.zst" \
  --wasm-bundle "$repo/target/wasm-bundle" > "$scratch/wasm-netlist.json"

# A successful but empty/partial evaluation must not pass the smoke check.
node --input-type=module - "$scratch/wasm-netlist.json" <<'JS'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const schematic = JSON.parse(readFileSync(process.argv[2], 'utf8'))
const components = Object.values(schematic.instances).filter(i => i.kind === 'Component')
assert.equal(components.length, 5)
assert.deepEqual(Object.keys(schematic.nets).sort(), [
  'FILTERED_VCC', 'GND', 'PART_DEMO_NET', 'STATUS1.LED_ANODE', 'VCC_3V3',
])
console.log('WASM round trip: 5 components and 5 nets; JS serialization checks passed')
JS
