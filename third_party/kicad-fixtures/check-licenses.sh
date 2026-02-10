#!/bin/sh
# Verify every vendored KiCad fixture includes a license file and that
# its SPDX identifier matches the checked-in LICENSES.spdx manifest.
#
# Requires: askalono (cargo install askalono-cli)
set -eu

dir="$(cd "$(dirname "$0")" && pwd)"
manifest="$dir/LICENSES.spdx"
status=0

if ! command -v askalono >/dev/null 2>&1; then
  echo "ERROR: askalono not found. Install with: cargo install askalono-cli"
  exit 1
fi

detected=$(mktemp)
trap 'rm -f "$detected"' EXIT

for project in "$dir"/*/; do
  [ -d "$project" ] || continue
  name=$(basename "$project")

  license_file=""
  for f in "$project"LICENSE* "$project"COPYING*; do
    [ -f "$f" ] && license_file="$f" && break
  done

  if [ -z "$license_file" ]; then
    echo "ERROR: missing license file in $name/"
    status=1
    continue
  fi

  spdx=$(askalono identify "$license_file" 2>/dev/null | head -1 | sed 's/^License: //; s/ (.*//')
  if [ -z "$spdx" ]; then
    echo "ERROR: could not detect license for $name/"
    status=1
    continue
  fi

  echo "$name $spdx" >> "$detected"
  echo "  $name: $spdx"
done

if [ "${1:-}" = "--generate" ]; then
  sort "$detected" > "$manifest"
  echo ""
  echo "Wrote $manifest"
  exit 0
fi

if [ ! -f "$manifest" ]; then
  echo ""
  echo "No LICENSES.spdx manifest found. Generate with:"
  echo "  $0 --generate"
  exit 1
fi

echo ""
while read -r name expected; do
  [ -z "$name" ] && continue
  actual=$(grep "^$name " "$detected" | awk '{print $2}')
  if [ -z "$actual" ]; then
    echo "ERROR: $name/ listed in manifest but not found"
    status=1
  elif [ "$actual" != "$expected" ]; then
    echo "ERROR: $name/ license mismatch: expected $expected, detected $actual"
    status=1
  fi
done < "$manifest"

while read -r name _; do
  if ! grep -q "^$name " "$manifest"; then
    echo "ERROR: $name/ not listed in LICENSES.spdx manifest"
    status=1
  fi
done < "$detected"

if [ $status -eq 0 ]; then
  echo "All fixtures have valid licenses matching manifest."
fi

exit $status
