#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

expect_closed() {
  local bead_id="$1"
  local canonical_id="$2"
  local status
  local reason

  status="$(br show "$bead_id" --json | jq -r '.[0].status')"
  reason="$(br show "$bead_id" --json | jq -r '.[0].close_reason // ""')"

  if [ "$status" != "closed" ]; then
    printf 'expected %s to be closed, got %s\n' "$bead_id" "$status" >&2
    return 1
  fi

  if ! grep -Fq "$canonical_id" <<<"$reason"; then
    printf 'expected %s close reason to cite %s, got: %s\n' \
      "$bead_id" "$canonical_id" "$reason" >&2
    return 1
  fi
}

expect_readme_reference() {
  local canonical_id="$1"

  if ! grep -Fq "\`$canonical_id\`" README.md; then
    printf 'expected README.md to reference `%s`' "$canonical_id" >&2
    printf '\n' >&2
    return 1
  fi
}

expect_closed bd-2gill.1 bd-3usjw.10
expect_closed bd-2gill.2 bd-3usjw.13
expect_closed bd-2gill.3 bd-3usjw.9

expect_readme_reference bd-3usjw.10
expect_readme_reference bd-3usjw.13
expect_readme_reference bd-3usjw.9
