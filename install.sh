#!/usr/bin/env bash
set -euo pipefail

base_url="https://pcb.api.diode.computer/pcb"
install_dir="${PCB_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

command -v curl >/dev/null || { echo "missing required command: curl" >&2; exit 1; }

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

json="$(curl -fsSL "$base_url/pcb-latest.json")"
tag="$(printf '%s' "$json" | sed -n 's/.*"tag"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[ -n "$tag" ] || { echo "could not read latest pcb release" >&2; exit 1; }

artifact="pcb-$target"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$base_url/$tag/$artifact.sha256" -o "$tmp/pcb.sha256"
if command -v zstd >/dev/null \
  && curl -fsSL "$base_url/$tag/$artifact.zst" -o "$tmp/pcb.zst" 2>/dev/null; then
  zstd -q -d -f "$tmp/pcb.zst" -o "$tmp/pcb"
else
  curl -fsSL "$base_url/$tag/$artifact" -o "$tmp/pcb"
fi

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
