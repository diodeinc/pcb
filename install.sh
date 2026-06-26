#!/usr/bin/env bash
set -euo pipefail

base_url="https://pcb.api.diode.computer/pcb"
install_dir="${PCB_INSTALL_DIR:-$HOME/.local/bin}"
mode="release"

usage() {
  cat <<EOF
Usage: install.sh [--local]

Options:
  --local    Build pcb and pcbc from this checkout and install a local toolchain.
  -h, --help Show this help.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --local) mode="local" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
  esac
  shift
done

add_install_dir_to_path() {
  case ":$PATH:" in *":$install_dir:"*) return 0 ;; esac

  if [ -n "${GITHUB_PATH:-}" ]; then
    echo "$install_dir" >> "$GITHUB_PATH"
  fi

  env_script="$install_dir/env"
  cat > "$env_script" <<EOF
case ":\${PATH}:" in
  *:"$install_dir":*) ;;
  *) export PATH="$install_dir:\$PATH" ;;
esac
EOF

  source_line=". \"$env_script\""
  for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    [ -e "$rc" ] || [ "$rc" = "$HOME/.profile" ] || continue
    touch "$rc"
    grep -Fqx "$source_line" "$rc" || printf '\n%s\n' "$source_line" >> "$rc"
  done

  fish_dir="$HOME/.config/fish/conf.d"
  if [ -d "$HOME/.config/fish" ]; then
    mkdir -p "$fish_dir"
    printf 'fish_add_path "%s"\n' "$install_dir" > "$fish_dir/pcb.env.fish"
  fi

  echo "Added $install_dir to PATH. Restart your shell or run: $source_line"
}

install_local() {
  command -v cargo >/dev/null || { echo "missing required command: cargo" >&2; exit 1; }

  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if [ -f "$script_dir/Cargo.toml" ] && [ -d "$script_dir/crates/pcb" ] && [ -d "$script_dir/crates/pcbc" ]; then
    source_dir="$script_dir"
  elif [ -f "Cargo.toml" ] && [ -d "crates/pcb" ] && [ -d "crates/pcbc" ]; then
    source_dir="$(pwd)"
  else
    echo "could not find pcb checkout; run ./install.sh --local from the repository root" >&2
    exit 1
  fi

  target_dir="$source_dir/target"
  cargo build --release -p pcb -p pcbc -p rectify --manifest-path "$source_dir/Cargo.toml" --target-dir "$target_dir"

  local_target_dir="$data_dir/toolchains/local/$target"
  stdlib_dir="$local_target_dir/lib/std"

  [ -d "$source_dir/lib/std" ] || { echo "missing stdlib: $source_dir/lib/std" >&2; exit 1; }

  mkdir -p "$install_dir"
  install -m 755 "$target_dir/release/pcb" "$install_dir/pcb"

  mkdir -p "$local_target_dir/lib"
  install -m 755 "$target_dir/release/pcbc" "$local_target_dir/pcbc"
  install -m 755 "$target_dir/release/pcb-rectify" "$local_target_dir/pcb-rectify"
  rm -f "$install_dir/pcbc"
  rm -rf "$stdlib_dir"
  cp -R "$source_dir/lib/std" "$stdlib_dir"

  add_install_dir_to_path

  echo "Installed local pcb to $install_dir/pcb"
  echo "Installed local pcbc to $local_target_dir/pcbc"
  echo "Installed local pcb-rectify to $local_target_dir/pcb-rectify"
  echo "Installed local stdlib to $stdlib_dir"
}

platform="$(uname -s)-$(uname -m)"
download_targets=""
case "$platform" in
  Darwin-arm64) target="aarch64-apple-darwin"; download_targets="$target"; data_dir="$HOME/Library/Application Support/pcb" ;;
  Darwin-x86_64) target="x86_64-apple-darwin"; download_targets="$target"; data_dir="$HOME/Library/Application Support/pcb" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu"; download_targets="aarch64-unknown-linux-musl $target"; data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/pcb" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu"; download_targets="x86_64-unknown-linux-musl $target"; data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/pcb" ;;
  *) echo "unsupported platform: $platform" >&2; exit 1 ;;
esac

if [ "$mode" = "local" ]; then
  install_local
  exit 0
fi

command -v curl >/dev/null || { echo "missing required command: curl" >&2; exit 1; }

json="$(curl -fsSL "$base_url/pcb-latest.json")"
tag="$(printf '%s' "$json" | sed -n 's/.*"tag"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[ -n "$tag" ] || { echo "could not read latest pcb release" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

artifact=""
downloaded=""
for artifact_target in $download_targets; do
  rm -f "$tmp/pcb" "$tmp/pcb.zst" "$tmp/pcb.sha256"
  artifact="pcb-$artifact_target"
  if ! curl -fsSL "$base_url/$tag/$artifact.sha256" -o "$tmp/pcb.sha256" 2>/dev/null; then
    continue
  fi
  if command -v zstd >/dev/null \
    && curl -fsSL "$base_url/$tag/$artifact.zst" -o "$tmp/pcb.zst" 2>/dev/null; then
    zstd -q -d -f "$tmp/pcb.zst" -o "$tmp/pcb"
  elif ! curl -fsSL "$base_url/$tag/$artifact" -o "$tmp/pcb" 2>/dev/null; then
    continue
  fi
  downloaded="true"
  break
done
[ -n "$downloaded" ] || { echo "could not find pcb release artifact for $target" >&2; exit 1; }

expected="$(sed 's/[[:space:]].*//' "$tmp/pcb.sha256")"
if command -v shasum >/dev/null; then
  actual="$(shasum -a 256 "$tmp/pcb" | sed 's/[[:space:]].*//')"
elif command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$tmp/pcb" | sed 's/[[:space:]].*//')"
else
  echo "missing shasum or sha256sum" >&2
  exit 1
fi
[ "$actual" = "$expected" ] || { echo "checksum mismatch" >&2; exit 1; }

mkdir -p "$install_dir"
chmod +x "$tmp/pcb"
mv "$tmp/pcb" "$install_dir/pcb"

add_install_dir_to_path

echo "Installed pcb to $install_dir/pcb"
