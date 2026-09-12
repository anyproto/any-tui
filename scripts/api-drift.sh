#!/usr/bin/env bash
# Compare the any server's OpenAPI spec against the copy pinned in
# api/openapi.json, so API drift shows up as a diff instead of a runtime 400.
#
#   scripts/api-drift.sh                      # diff live daemon (ANY_API or :7001)
#   scripts/api-drift.sh http://127.0.0.1:7140/v1
#   scripts/api-drift.sh --file ~/any/any/internal/server/docs/swagger.json
#   scripts/api-drift.sh --update [<api-url>] # re-pin from the live daemon
#
# The served spec is the source of truth: the server stamps
# `additionalProperties: false` on the request schemas it binds strictly, and
# the swag-generated file in the any repo can't carry that. --file therefore
# strips the stamp from the pin before diffing, so only real drift remains.
# Exit status: 0 = identical, 1 = drift, 2 = usage/fetch error.
set -euo pipefail
cd "$(dirname "$0")/.."
PIN=api/openapi.json
mode=live; update=0; src=
while [ $# -gt 0 ]; do
  case "$1" in
    --file) mode=file; src=$2; shift 2 ;;
    --update) update=1; shift ;;
    -h|--help) sed -n 2,15p "$0"; exit 0 ;;
    *) src=$1; shift ;;
  esac
done
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
# Drop what varies per daemon rather than per API: the bind address the spec
# was served from, and the swag-2 `schemes` slot the file form never has.
norm() { jq -S 'del(.servers, .schemes, .host)' "$1"; }
if [ $mode = live ]; then
  url=${src:-${ANY_API:-http://127.0.0.1:7001/v1}}
  curl -sf "${url%/}/openapi.json" | jq -S . > "$tmp/new.json" \
    || { echo "cannot fetch ${url%/}/openapi.json — is the daemon running?" >&2; exit 2; }
  if [ $update = 1 ]; then
    cp "$tmp/new.json" "$PIN"
    ver=$(curl -sf "${url%/}/health" | jq -r .version)
    {
      echo "# The any server build whose GET /v1/openapi.json is pinned in openapi.json."
      echo "# Re-pin with: scripts/api-drift.sh --update [<api-url>]"
      echo "version: $ver"
      echo "any-repo-commit: $(git -C "${ANY_REPO:-$HOME/any/any}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
      echo "pinned-at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > api/OPENAPI_PIN
    echo "pinned $ver"; exit 0
  fi
  norm "$PIN" > "$tmp/old.json"; norm "$tmp/new.json" > "$tmp/new2.json"; mv "$tmp/new2.json" "$tmp/new.json"
else
  [ -f "$src" ] || { echo "no such file: $src" >&2; exit 2; }
  norm "$src" > "$tmp/new.json"
  norm "$PIN" | jq -S 'walk(if type == "object" and .additionalProperties == false then del(.additionalProperties) else . end)' > "$tmp/old.json"
fi
if diff -u --label pinned --label current "$tmp/old.json" "$tmp/new.json"; then
  echo "no drift ($(sed -n 's/^version: //p' api/OPENAPI_PIN))"
else
  echo; echo "^ API drift vs pin ($(sed -n 's/^version: //p' api/OPENAPI_PIN))"; exit 1
fi
