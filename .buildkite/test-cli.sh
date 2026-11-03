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
