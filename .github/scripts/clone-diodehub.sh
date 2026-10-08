#!/usr/bin/env bash
# Usage: clone-diodehub.sh <owner/b/repo>[@<rev>]...
# Clones into workspaces/<repo> as the DIODE_CLIENT_ID service account.
set -euo pipefail

api=https://api.diode.computer
access=$(curl -fsS "$api/api/auth/token" -u "$DIODE_CLIENT_ID:$DIODE_CLIENT_SECRET" \
  -d grant_type=client_credentials | jq -r .access_token)
echo "::add-mask::$access"

for spec in "$@"; do
  path=${spec%@*}
  dir=workspaces/${path##*/}
  token=$(curl -fsS "$api/api/git/credentials" -H "Authorization: Bearer $access" \
    -H 'Content-Type: application/json' \
    -d "{\"host\":\"code.diode.computer\",\"path\":\"$path\"}" | jq -r .credential.token)
  echo "::add-mask::$token"
  git=(git -c "http.extraHeader=Authorization: Bearer $token")
  "${git[@]}" clone --depth 1 "https://code.diode.computer/$path" "$dir"
  if [[ $spec == *@* ]]; then
    "${git[@]}" -C "$dir" fetch --depth 1 origin "${spec#*@}"
    git -C "$dir" checkout --detach FETCH_HEAD
  fi
done
