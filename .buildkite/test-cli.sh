#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo="$PWD"
binaries="$(realpath "${CARGO_TARGET_DIR:-target}/debug")"
host="$(rustc -vV | sed -n 's/^host: //p')"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# Clone before isolating HOME so Git can use the agent's managed GitHub credentials.
git clone --depth 1 https://github.com/diodeinc/kicad-test-fixtures.git "$scratch/fixtures"
mapfile -d '' -t projects < <(find "$scratch/fixtures" -name '*.kicad_pro' -print0)
test "${#projects[@]}" -gt 0

# Exercise the installed shim/toolchain boundary, not binaries running in the checkout.
# nextest already built both binaries; the release installer is still tested on macOS.
export HOME="$scratch/home"
export XDG_DATA_HOME="$scratch/data"
export XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache"
export PATH="$scratch/bin:$PATH"
export DIODE_API_AUTH=none
export RUST_LOG=warn
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

# Publish once, then evaluate that same archive through the actual JS/WASM bindings.
git -C "$scratch/workspace" init -q
git -C "$scratch/workspace" add .
git -C "$scratch/workspace" -c user.name=CI -c user.email=ci@example.invalid commit -qm Fixture
board="$scratch/workspace/boards/WorkspaceTestBoard.zen"
pcb +local bom "$board"
pcb +local layout "$board" --no-open
pcb +local publish "$board" -S layout.drc.invalid_outline
bundles=("$scratch/workspace"/.pcb/releases/*.zip)
test "${#bundles[@]}" -eq 1
test -s "${bundles[0]}"
tar -C "$toolchain/lib/std" -cf - . | zstd -q -3 -c > "$scratch/stdlib.tar.zst"
node "$repo/crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs" \
  --bundle "${bundles[0]}" \
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

for project in "${projects[@]}"; do
  echo "--- :kicad: Import, build and layout $(basename "$project")"
  out="$scratch/imports/$(basename "${project%.kicad_pro}")"
  pcb +local import "$project" "$out"
  for board_zen in "$out"/*.zen; do
    pcb +local build "$board_zen" -S bom
    pcb +local layout "$board_zen" --no-open -S bom
  done
done
echo "KiCad E2E: ${#projects[@]} projects imported, built and laid out"
