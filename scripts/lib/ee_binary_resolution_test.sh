#!/usr/bin/env bash
# Unit checks for scripts/lib/ee_binary_resolution.sh.
#
# This test is intentionally shell-only: it stubs cargo and jq so it can prove
# target-directory resolution without starting a Cargo build.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"

assert_eq() {
    local actual="${1:?actual required}"
    local expected="${2:?expected required}"
    local label="${3:?label required}"

    if [ "$actual" != "$expected" ]; then
        printf 'FAIL %s\nexpected: %s\nactual:   %s\n' "$label" "$expected" "$actual" >&2
        exit 1
    fi
    printf 'ok %s\n' "$label"
}

SCRATCH_ROOT="${TMPDIR:-/tmp}/ee-binary-resolution-test.$$"
FAKE_BIN="$SCRATCH_ROOT/bin"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ge 1 ] && [ "$1" = "metadata" ]; then
    printf '{"target_directory":"/fixture/cargo-metadata-target"}\n'
    exit 0
fi
printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 2
SH

cat >"$FAKE_BIN/jq" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cat >/dev/null
printf '/fixture/cargo-metadata-target\n'
SH

chmod +x "$FAKE_BIN/cargo" "$FAKE_BIN/jq"

explicit_binary="$(
    EE_BINARY="/custom/bin/ee" \
        CARGO_TARGET_DIR="/external/target" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release
)"
assert_eq "$explicit_binary" "/custom/bin/ee" "explicit EE_BINARY wins"

cargo_target_binary="$(
    unset EE_BINARY
    CARGO_TARGET_DIR="/Volumes/USBNVME16TB/temp_agent_space/cargo-target" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary debug
)"
assert_eq \
    "$cargo_target_binary" \
    "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/debug/ee" \
    "CARGO_TARGET_DIR determines debug binary"

metadata_binary="$(
    unset EE_BINARY CARGO_TARGET_DIR
    PATH="$FAKE_BIN:$PATH" ee_resolve_binary release
)"
assert_eq \
    "$metadata_binary" \
    "/fixture/cargo-metadata-target/release/ee" \
    "cargo metadata target_directory fallback"

fallback_binary="$(
    unset EE_BINARY CARGO_TARGET_DIR
    PATH="/nonexistent" ee_resolve_binary debug
)"
assert_eq "$fallback_binary" "$REPO_ROOT/target/debug/ee" "repo target fallback"

printf 'scratch retained for audit: %s\n' "$SCRATCH_ROOT"
