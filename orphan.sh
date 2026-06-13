#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-zed}"

reachable=$(
  cargo tree -p "$ROOT" --prefix none \
    | sed 's/ v[0-9].*//' \
    | sort -u
)

workspace=$(
  cargo metadata --no-deps --format-version=1 \
    | jq -r '.packages[].name' \
    | sort -u
)

comm -23 <(echo "$workspace") <(echo "$reachable")
