#!/usr/bin/env bash
# Compare HEAD against its merge base with BASE, using the DiodeHub benchmark workspaces.
# Usage: bash .buildkite/build-perf.sh BASE OWNER/b/WORKSPACE[@REV]...
set -euo pipefail

if (( $# < 2 )); then
  echo "Usage: $0 BASE OWNER/b/WORKSPACE[@REV]..." >&2
  exit 2
fi

base=$(git merge-base HEAD "$1")
shift
repo=$(git rev-parse --show-toplevel)
# Let Cargo create and mark its own directory; the shared cache may lack CACHEDIR.TAG.
target=$(realpath -m "${CARGO_TARGET_DIR:-$repo/target}/build-perf-target")
results=$repo/target/build-perf
scratch=$(mktemp -d)
trap 'git -C "$repo" worktree remove --force "$scratch/base" >/dev/null 2>&1 || true; rm -rf "$scratch"' EXIT

git -C "$repo" worktree add --detach "$scratch/base" "$base"
rm -rf "$results"
mkdir -p "$results"

# The step registers the client secret with the redactor; register the tokens minted from it too.
redact() {
  if [[ ${BUILDKITE:-false} == true ]]; then
    printf '%s' "$1" | buildkite-agent redactor add
  fi
}

echo "--- :git: Fetch benchmark workspaces"
api=https://api.diode.computer
access=$(curl -fsS "$api/api/auth/token" -u "$DIODE_CLIENT_ID:$DIODE_CLIENT_SECRET" \
  -d grant_type=client_credentials | jq -er .access_token)
redact "$access"
for spec in "$@"; do
  path=${spec%@*}
  dir=$scratch/workspaces/${path##*/}
  token=$(curl -fsS "$api/api/git/credentials" -H "Authorization: Bearer $access" \
    -H 'Content-Type: application/json' \
    -d "{\"host\":\"code.diode.computer\",\"path\":\"$path\"}" | jq -er .credential.token)
  redact "$token"
  git=(git -c "http.extraHeader=Authorization: Bearer $token")
  "${git[@]}" clone --depth 1 "https://code.diode.computer/$path" "$dir"
  if [[ $spec == *@* ]]; then
    "${git[@]}" -C "$dir" fetch --depth 1 origin "${spec#*@}"
    git -C "$dir" checkout --detach FETCH_HEAD
  fi
done
unset access token git DIODE_CLIENT_ID DIODE_CLIENT_SECRET

for revision in base head; do
  echo "--- :rust: Build $revision compiler"
  source=$repo
  if [[ $revision == base ]]; then
    source=$scratch/base
  fi
  toolchain=$scratch/$revision-toolchain
  if [[ $revision == head ]]; then
    # Path-crate fingerprints can be reused across worktrees, even with different sources.
    # Rebuild workspace crates for head while retaining third-party dependencies.
    cargo clean --release --workspace \
      --manifest-path "$source/Cargo.toml" --target-dir "$target"
  fi
  cargo build --locked --release -p pcbc \
    --manifest-path "$source/Cargo.toml" --target-dir "$target"
  mkdir -p "$toolchain/lib"
  install -m 755 "$target/release/pcbc" "$toolchain/pcbc"
  cp -R "$source/lib/std" "$toolchain/lib/std"
done

# Both revisions run on the same machine, with their own compiler, stdlib and data.
for revision in base head; do
  for workspace in "$scratch/workspaces"/*/; do
    name=$(basename "$workspace")
    echo "--- :stopwatch: $revision $name"
    XDG_DATA_HOME="$scratch/$revision-data" python3 "$repo/bin/pcb-build-perf" \
      --pcbc "$scratch/$revision-toolchain/pcbc" \
      --workspace "$workspace" --suppress bom \
      --output "$results/$name-$revision.json" > /dev/null
  done
done

python3 "$repo/bin/pcb-build-perf-report" "$results" > "$results/report.md"
