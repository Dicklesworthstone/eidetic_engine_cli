#!/usr/bin/env bash
# Proof harness for install.sh's GitHub-outage resilience (bd-3usjw, client
# 429 incident 2026-08-17). Extracts the REAL function bodies out of the
# shipped install.sh (never a hand-copied paraphrase) and drives them with
# local stubs -- no network calls, so this cannot repeat the client 429.
#
# Grades on explicit assertions, not on this script exiting 0 by accident:
# every check below prints PASS/FAIL for a specific, named claim, and the
# script's own exit code is the logical AND of all of them.
#
#   1. Asset-matrix fallback: the latest release lacks this platform's
#      tarball -> resolver selects the newest STABLE release that has it,
#      warns clearly, and never selects a draft or prerelease even when one
#      ships the asset and is newer (the exact bug class fixed once already
#      in install.ps1's Get-LatestVersion).
#   2. HTTP 429 is retried (not aborted), honours Retry-After when present,
#      HTTP 503 without Retry-After is retried with backoff, and HTTP 404
#      fails FAST -- exactly one attempt, no retry -- because retrying
#      cannot change a 404's answer.
#
# Usage: bash tests/test_install_sh_resilience.sh

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_SH="$REPO_ROOT/install.sh"

PASS_COUNT=0
FAIL_COUNT=0

assert() {
  local description="$1"
  local ok="$2"
  if [ "$ok" -eq 0 ]; then
    printf 'PASS: %s\n' "$description"
    PASS_COUNT=$((PASS_COUNT + 1))
  else
    printf 'FAIL: %s\n' "$description"
    FAIL_COUNT=$((FAIL_COUNT + 1))
  fi
}

# ---------------------------------------------------------------------------
# Extract the real function/constant definitions from install.sh: everything
# above the `# Main flow` section's top-level, unconditional invocations
# (print_header, detect_platform, resolve_version, ...) at the bottom of the
# file. This is the actual shipped code, sourced verbatim -- not a
# reimplementation that could quietly drift from it.
#
# Callable more than once on purpose: Part 1 below deliberately overrides
# ee_curl as a stub to drive resolve_version, and never restores it. Part 2
# tests the REAL ee_curl, so it re-sources fresh immediately before running,
# rather than silently testing Part 1's leftover stub instead of shipped code.
# ---------------------------------------------------------------------------
load_real_install_sh_functions() {
  local main_flow_line lib_file
  main_flow_line="$(grep -n '^# Main flow$' "$INSTALL_SH" | head -1 | cut -d: -f1)"
  if [ -z "$main_flow_line" ]; then
    echo "FAIL: could not locate install.sh's main-flow section header ('# Main flow')"
    exit 1
  fi
  lib_file="$(mktemp)"
  head -n "$((main_flow_line - 1))" "$INSTALL_SH" > "$lib_file"
  # shellcheck disable=SC1090
  source "$lib_file"
  rm -f "$lib_file"
}

load_real_install_sh_functions

# install.sh's own `set -euo pipefail` (sourced above) now governs this
# shell too. Drop -e for the rest of this harness: every assertion below is
# deliberately structured to run a fallible command and grade its exit code
# itself, which -e would short-circuit on the first failing case.
set +e

# ===========================================================================
# PART 1 -- asset-matrix fallback (resolve_version)
# ===========================================================================

echo
echo "=== Part 1: asset-matrix fallback ==="

# --- 1a: latest release lacks the asset; an OLDER stable release has it ---
OWNER="Dicklesworthstone"; REPO="eidetic_engine_cli"
TARGET="aarch64-unknown-linux-gnu"
FROM_SOURCE=0; ARTIFACT_URL=""; VERSION=""
EE_LAST_KNOWN_GOOD_TAG="v0.13.0"

ee_curl() {
  for a in "$@"; do
    case "$a" in
      *releases/latest)
        printf '%s' '{"tag_name":"v0.13.1","draft":false,"prerelease":false,"assets":[{"name":"ee-aarch64-apple-darwin.tar.xz"},{"name":"ee-x86_64-unknown-linux-gnu.tar.xz"}]}'
        return 0
        ;;
      *'releases?per_page=20')
        printf '%s' '[{"tag_name":"v0.13.1","draft":false,"prerelease":false,"assets":[{"name":"ee-aarch64-apple-darwin.tar.xz"},{"name":"ee-x86_64-unknown-linux-gnu.tar.xz"}]},{"tag_name":"v0.14.0-rc1","draft":false,"prerelease":true,"assets":[{"name":"ee-aarch64-unknown-linux-gnu.tar.xz"}]},{"tag_name":"v0.13.0","draft":false,"prerelease":false,"assets":[{"name":"ee-x86_64-pc-windows-msvc.tar.xz"},{"name":"ee-aarch64-unknown-linux-gnu.tar.xz"}]}]'
        return 0
        ;;
    esac
  done
  return 1
}

# NOTE: resolve_version must run in THIS shell, not a $(...) subshell --
# command substitution would run it in a subshell and silently discard its
# VERSION="$tag" assignment, making every assertion below pass or fail on
# stale/empty state regardless of what resolve_version actually did.
RESULT_LOG_FILE_1A="$(mktemp)"
resolve_version > "$RESULT_LOG_FILE_1A" 2>&1
RESULT_LOG="$(cat "$RESULT_LOG_FILE_1A")"
rm -f "$RESULT_LOG_FILE_1A"

[ "$VERSION" = "v0.13.0" ]
assert "1a: latest (v0.13.1) lacks the aarch64-linux-gnu tarball -> resolver selects v0.13.0 (the newest STABLE release that has it), skipping the newer v0.14.0-rc1 PRERELEASE that also has it" $?

printf '%s' "$RESULT_LOG" | grep -qi "does not include"
assert "1a: resolver warns clearly that the latest release does not include the platform tarball" $?

printf '%s' "$RESULT_LOG" | grep -qi "v0.14.0-rc1"
never_selected_prerelease=1
[ "$VERSION" != "v0.14.0-rc1" ] && never_selected_prerelease=0
assert "1a: resolved VERSION is never the prerelease v0.14.0-rc1, even though it ships the asset and is newer than the selected v0.13.0" $never_selected_prerelease

# --- 1b: draft release ships the asset; must still be skipped ---
VERSION=""
ee_curl() {
  for a in "$@"; do
    case "$a" in
      *releases/latest)
        printf '%s' '{"tag_name":"v0.12.0","draft":false,"prerelease":false,"assets":[{"name":"ee-aarch64-apple-darwin.tar.xz"}]}'
        return 0
        ;;
      *'releases?per_page=20')
        printf '%s' '[{"tag_name":"v0.14.0-draft","draft":true,"prerelease":false,"assets":[{"name":"ee-aarch64-unknown-linux-gnu.tar.xz"}]},{"tag_name":"v0.13.0","draft":false,"prerelease":false,"assets":[{"name":"ee-aarch64-unknown-linux-gnu.tar.xz"}]}]'
        return 0
        ;;
    esac
  done
  return 1
}
resolve_version >/dev/null 2>&1
[ "$VERSION" = "v0.13.0" ]
assert "1b: a DRAFT release shipping the asset is skipped in favor of the newest stable release that has it" $?

# --- 1c: GitHub API entirely unusable -> pinned last-known-good ---
VERSION=""
ee_curl() { return 1; }
RESULT_LOG_FILE_1C="$(mktemp)"
resolve_version > "$RESULT_LOG_FILE_1C" 2>&1
RESULT_LOG="$(cat "$RESULT_LOG_FILE_1C")"
rm -f "$RESULT_LOG_FILE_1C"
[ "$VERSION" = "v0.13.0" ]
assert "1c: GitHub release API entirely unreachable -> falls back to the pinned last-known-good tag (v0.13.0) instead of aborting" $?
printf '%s' "$RESULT_LOG" | grep -qi "pinned last-known-good"
assert "1c: fallback to the pinned tag is announced, not silent" $?

# ===========================================================================
# PART 2 -- HTTP status retry classification (ee_curl)
# ===========================================================================

echo
echo "=== Part 2: HTTP 429/503/404 retry classification ==="

# Restore the REAL ee_curl -- Part 1 overrode it as a stub and left it
# overridden (see load_real_install_sh_functions's comment above).
load_real_install_sh_functions
set +e

STUB_DIR="$(mktemp -d)"
CURL_COUNTER_FILE="$(mktemp)"

cat > "$STUB_DIR/curl" <<'STUBEOF'
#!/usr/bin/env bash
# Local curl stub for test_install_sh_resilience.sh. Never touches the
# network; simulates one scripted HTTP status sequence per EE_TEST_SCENARIO.
set -uo pipefail

count=0
[ -f "$EE_TEST_CURL_COUNTER_FILE" ] && count="$(cat "$EE_TEST_CURL_COUNTER_FILE")"
count=$((count + 1))
printf '%s' "$count" > "$EE_TEST_CURL_COUNTER_FILE"

hdr_file=""
out_file=""
args=("$@")
i=0
while [ "$i" -lt "${#args[@]}" ]; do
  case "${args[$i]}" in
    -D) i=$((i + 1)); hdr_file="${args[$i]}" ;;
    -o) i=$((i + 1)); out_file="${args[$i]}" ;;
  esac
  i=$((i + 1))
done

write_status() {
  local status="$1"; shift
  {
    printf 'HTTP/1.1 %s Status\r\n' "$status"
    for h in "$@"; do printf '%s\r\n' "$h"; done
    printf '\r\n'
  } > "$hdr_file"
}

succeed() {
  write_status 200
  if [ -n "$out_file" ]; then printf 'stub-ok-body\n' > "$out_file"; else printf 'stub-ok-body\n'; fi
  exit 0
}

case "$EE_TEST_SCENARIO" in
  429_then_200)
    if [ "$count" -eq 1 ]; then write_status 429 'Retry-After: 2'; exit 22; else succeed; fi
    ;;
  503_backoff_then_200)
    if [ "$count" -le 2 ]; then write_status 503; exit 22; else succeed; fi
    ;;
  404_fatal)
    write_status 404
    exit 22
    ;;
  *)
    echo "unknown EE_TEST_SCENARIO: $EE_TEST_SCENARIO" >&2
    exit 99
    ;;
esac
STUBEOF
chmod +x "$STUB_DIR/curl"

run_ee_curl_scenario() {
  local scenario="$1"
  : > "$CURL_COUNTER_FILE"
  EE_TEST_SCENARIO="$scenario" EE_TEST_CURL_COUNTER_FILE="$CURL_COUNTER_FILE" \
    PATH="$STUB_DIR:$PATH" \
    ee_curl "https://api.github.com/test-endpoint"
}

# --- 2a: 429 with Retry-After is retried and eventually succeeds ---
PROXY_ARGS=()
start_ts=$(date +%s)
out="$(run_ee_curl_scenario 429_then_200)"
ee_curl_status=$?
elapsed=$(( $(date +%s) - start_ts ))
attempts="$(cat "$CURL_COUNTER_FILE")"

[ "$ee_curl_status" -eq 0 ] && [ "$out" = "stub-ok-body" ]
assert "2a: HTTP 429 is retried rather than aborted -- ee_curl succeeds once the stub stops returning 429" $?

[ "$attempts" -eq 2 ]
assert "2a: exactly 2 attempts were made (1st got 429, 2nd succeeded) -- proves a retry actually fired, not a lucky first try" $?

[ "$elapsed" -ge 2 ]
assert "2a: Retry-After: 2 was honoured -- ee_curl actually waited (elapsed ${elapsed}s >= 2s) rather than retrying immediately" $?

# --- 2b: 503 with no Retry-After retries with backoff and succeeds ---
out="$(run_ee_curl_scenario 503_backoff_then_200)"
ee_curl_status=$?
attempts="$(cat "$CURL_COUNTER_FILE")"

[ "$ee_curl_status" -eq 0 ] && [ "$out" = "stub-ok-body" ]
assert "2b: HTTP 503 (no Retry-After) is retried with backoff rather than aborted" $?

[ "$attempts" -eq 3 ]
assert "2b: exactly 3 attempts were made (2x 503, 3rd succeeded)" $?

# --- 2c: 404 fails FAST -- exactly one attempt, no retry ---
out="$(run_ee_curl_scenario 404_fatal)"
ee_curl_status=$?
attempts="$(cat "$CURL_COUNTER_FILE")"

[ "$ee_curl_status" -ne 0 ]
assert "2c: HTTP 404 is reported as a failure (ee_curl returns nonzero)" $?

[ "$attempts" -eq 1 ]
assert "2c: HTTP 404 makes exactly ONE attempt -- retrying was correctly refused because retrying cannot change a 404's answer" $?

rm -rf "$STUB_DIR"
rm -f "$CURL_COUNTER_FILE"

# ===========================================================================
echo
echo "=== Summary: $PASS_COUNT passed, $FAIL_COUNT failed ==="
[ "$FAIL_COUNT" -eq 0 ]
