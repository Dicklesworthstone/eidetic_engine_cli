#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 c9863): the real database is MOVED into
# the baseline and replaced by a NEW copy with one mid-file 4096-byte page of
# random bytes. SQLite's own integrity check fails; ee reports nothing.
FM="fm-state_files-sqlite-integrity-page-malformed"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

mv "$target/.ee/ee.db" "$base/ee.db.original"
pages=$(( $(wc -c < "$base/ee.db.original" | tr -d ' ') / 4096 ))
test "$pages" -ge 4
doctor_fixture_damaged_copy "$base/ee.db.original" "$target/.ee/ee.db" $(( pages / 2 ))

doctor_fixture_corrupt "$FM" "P0" "state_files"
sqlite3 "$target/.ee/ee.db" 'PRAGMA integrity_check;' > "$base/integrity-corrupt.txt" 2>&1 || true
if [ "$(head -n 1 "$base/integrity-corrupt.txt")" = "ok" ]; then
    printf 'page-malformed fixture: integrity_check still ok; the damaged page was not a live page\n' >&2
    exit 1
fi
printf 'real corruption confirmed: page %s of %s malformed (integrity_check fails)\n' $(( pages / 2 )) "$pages" >&2
