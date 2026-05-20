#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures}"
mkdir -p "$ROOT"

while IFS= read -r fm_dir; do
    target="$ROOT/$(basename "$fm_dir")"
    mkdir -p "$target"
    EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh"
    EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/assert.sh"
done < <(find "$SCRIPT_DIR" -mindepth 1 -maxdepth 1 -type d -name 'fm-*' | sort)
