#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-sqlite-integrity-page-malformed"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# Independent witness: SQLite itself says the database is malformed.
sqlite3 "$target/.ee/ee.db" 'PRAGMA integrity_check;' > "$base/integrity-assert.txt" 2>&1 || true
if [ "$(head -n 1 "$base/integrity-assert.txt")" = "ok" ]; then
    printf 'fixture assert: %s witness lost: integrity_check reports ok\n' "$FM" >&2
    exit 1
fi
# UPGRADED FROM A PINNED GAP TO GUIDANCE-ONLY (bd-ooj2h, spec rule V6).
#
# This fixture asserted doctor was BLIND to a malformed B-tree page. That
# stopped being true and the harness said so, in the words its own pinned-gap
# assertion promised: "GAP no longer reproduces; the label is wrong, relabel
# it". It is the THIRD fixture to go this way after merge-conflict-markers and
# config-toml-malformed, i.e. doctor improving faster than its pins.
#
# It is GUIDANCE-ONLY, not a repair. Doctor detects the damage and explicitly
# declines to touch it -- its own repair text is "Inspect the reported access
# or I/O failure WITHOUT REPLACING the database or its sidecars" -- which is
# the right posture for a corrupt store and the reason this is not modelled on
# the quarantine_by_rename fixtures.
#
# THE ORACLE BELOW WAS MEASURED, 2026-10-20, on an RCH worker against a binary
# that job built (ee 0.18.0), by running `doctor --fix --json` against this
# fixture's own retained workspace. Nothing here is inferred from the fixture
# name, and `database_unavailable` in particular was confirmed rather than
# guessed -- src/core/doctor_fixers.rs maps database + EE-E207 to it, and the
# run agreed:
#
#   doctor        posture=blocked healthy=false
#                 actionable[] database     EE-E207 (error)
#                 actionable[] search_index EE-E300 (warning)
#   doctor --fix  EXIT 6
#                 status=completed_partial fixerDispatchPending=true
#                 unresolvedCoreCheckCount=2 guidanceOnlyFixerCount=1
#                 unresolved=[database/EE-E207, search_index/EE-E300]
#                 fixers=[{database_unavailable, manual, guidance_recorded}]
#   artifact      .ee/index-rebuild-request.json STAYS ABSENT
#
# Note search_index reports EE-E300 and gets NO fixer. That is deliberate:
# fix_finding_for_check returns None for search_index while the store is
# unreadable, because rebuilding an index over a malformed database would be
# the wrong repair. Hence guidanceOnlyFixerCount=1 against
# unresolvedCoreCheckCount=2.
"$ee_bin" doctor --workspace "$target" --json > "$base/guidance-before.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.posture == "blocked" and
        .data.healthy == false and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E207") and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E300"))
' "$base/guidance-before.json" >/dev/null
# A real repair would request an index rebuild; guidance must not.
test ! -e "$target/.ee/index-rebuild-request.json"
# Digest AFTER the doctor call above, so the persistent `.doctor.lock` already
# exists and the byte comparison measures the FIX, not the first lock
# acquisition. (`.doctor.lock` is excluded from the digest anyway -- see
# doctor_fixture_content_digest -- but taking the baseline here keeps this
# fixture correct independently of that exclusion.)
doctor_fixture_content_digest "$target" > "$base/guidance-before-fix.sha256"
doctor_fixture_assert_guidance_only "$FM" "database_unavailable" "database" "EE-E207" \
    ".ee/index-rebuild-request.json"
jq -es '
    length == 1 and (.[0] |
        .data.status == "completed_partial" and .data.fixerDispatchPending == true and
        .data.guidanceOnlyFixerCount == 1 and
        all(.data.fixerResults[]; .operation == "manual" and .outcome == "guidance_recorded"))
' "$base/doctor-fix.json" >/dev/null
jq -es '
    length == 1 and (.[0] |
        .data.posture == "blocked" and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E300"))
' "$base/doctor-after.json" >/dev/null
# The whole point of guidance-only: a corrupt store is REPORTED, never rewritten.
doctor_fixture_content_digest "$target" > "$base/guidance-after-fix.sha256"
if ! cmp -s "$base/guidance-before-fix.sha256" "$base/guidance-after-fix.sha256"; then
    printf 'fixture assert: %s guidance-only --fix changed workspace bytes\n' "$FM" >&2
    exit 1
fi
# The witness must still hold afterwards: if integrity_check now says ok, a
# repair happened behind the guidance and this fixture is no longer testing
# what it claims.
sqlite3 "$target/.ee/ee.db" 'PRAGMA integrity_check;' > "$base/integrity-after.txt" 2>&1 || true
if [ "$(head -n 1 "$base/integrity-after.txt")" = "ok" ]; then
    printf 'fixture assert: %s guidance-only --fix repaired the database; it must not\n' "$FM" >&2
    exit 1
fi
printf 'guidance-only confirmed: %s (EE-E207 blocked before and after; --fix exit 6, database_unavailable guidance only; bytes and integrity_check unchanged)\n' "$FM" >&2
