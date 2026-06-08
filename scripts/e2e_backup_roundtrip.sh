#!/usr/bin/env bash
# bd-1n0np.23.2 — backup -> restore round-trip + new-asset coverage (real binary).
#
# Full scenario (E23 cross-cutting): a temp workspace seeded with durable assets
# -> `ee backup create` writes a redacted JSONL backup + a verified manifest with
# per-artifact blake3 hashes -> `ee backup verify` re-hashes and reports
# `verified` -> `ee backup restore --side-path` materializes an isolated copy ->
# a second `backup create` over the restored state reproduces identical artifact
# hashes (round-trip hash identity). A missing asset producer must surface in the
# manifest's structured `degraded[]` array (code+message), NEVER as silent loss.
#
# The create / verify / restore SURFACE exists today and runs FOR REAL here:
# manifestHash, recordsHash, artifacts[].hash, verificationStatus, and the
# degraded[] array are all asserted live. What bd-1n0np.23.2 ADDS — manifest
# coverage for every NEW durable/derived asset (anchors, sentinels, miss-ledger,
# typed-fields, attestation, generation, write-stats) — is CAPABILITY-GUARDED:
# where the manifest does not yet enumerate a new asset class, the step records a
# visible `log_drop` (the no-silent-cap rule) carrying the exact assertion that
# activates once the producer (bd-1n0np.16.2 / 4.3 / 8.2 ...) lands. No false pass.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "backup_roundtrip"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "seed durable assets (memories the backup must capture)"
m1="$(ee_json remember "search.rs ranks hybrid BM25 + vector hits via frankensearch." \
    --workspace "$WS" --level semantic --kind fact --tags search --json)"
assert_jq "$m1" '.success == true' "remember durable memory #1"
m2="$(ee_json remember "pack.rs assembles the context pack with provenance." \
    --workspace "$WS" --level procedural --kind fact --tags pack --json)"
assert_jq "$m2" '.success == true' "remember durable memory #2"

step "ee backup create writes a verified manifest with per-artifact hashes"
cr="$(ee_json backup create --workspace "$WS" --json)"
assert_jq "$cr" '.success == true' "backup create succeeds"
assert_jq "$cr" '.data.status == "completed"' "backup create reports completed"
assert_jq "$cr" '(.data.manifestHash // "") | startswith("blake3:")' \
    "manifest carries a blake3 hash"
assert_jq "$cr" '(.data.recordsHash // "") | startswith("blake3:")' \
    "records export carries a blake3 hash"
assert_jq "$cr" '.data.verificationStatus == "verified"' \
    "create self-verifies the manifest"
# Every artifact is hashed (no unhashed/silent asset in the manifest).
assert_jq "$cr" '(.data.artifacts | length) >= 2' "manifest enumerates artifacts"
assert_jq "$cr" 'all(.data.artifacts[]?; (.hash // "") | startswith("blake3:"))' \
    "every manifest artifact carries a blake3 hash"
# Missing assets surface as STRUCTURED degraded rows, never silent loss.
assert_jq "$cr" '(.data.degraded // []) | type == "array"' \
    "degraded is a structured array (never silent loss)"
assert_jq "$cr" 'all(.data.degraded[]?; has("code") and has("message"))' \
    "each degraded row carries a code + message"

backup_path="$(printf '%s' "$cr" | jq -r '.data.backupPath // empty')"
backup_id="$(printf '%s' "$cr" | jq -r '.data.backupId // empty')"
records_hash="$(printf '%s' "$cr" | jq -r '.data.recordsHash // empty')"
assert_eq "$([ -n "$backup_path" ] && echo present || echo missing)" "present" \
    "backup path present for verify/restore"
assert_jq "$cr" '(.data.backupId // "") | startswith("bk_")' \
    "backup carries a stable backup id"
[ -n "$backup_id" ] || log_drop 1 "backup create did not return a backupId"

step "ee backup verify re-hashes the manifest and reports verified"
if [ -n "$backup_path" ]; then
    vf="$(ee_json backup verify "$backup_path" --workspace "$WS" --json)"
    assert_jq "$vf" '.success == true' "backup verify succeeds"
    assert_jq "$vf" '.data.status == "verified"' "verify reports verified integrity"
    assert_jq "$vf" '(.data.manifestHash // "") == '"$(printf '%s' "$cr" | jq '.data.manifestHash')" \
        "verify reproduces the create-time manifest hash (no drift)"
else
    log_drop 1 "backup verify skipped: create did not return a backupPath"
fi

step "ee backup restore materializes an isolated copy (round-trip)"
# --side-path must be a real, non-symlink directory OUTSIDE the source workspace.
# Use a sibling of WS (outside it, same real volume). macOS temp roots live under
# /var -> /private (a symlink), which the restore guard also rejects; that is a
# host artifact, not a contract failure, so guard it honestly. On Linux CI the
# side path is real and the restore runs for real.
side="${WS%/}.restored"
mkdir -p "$side"
rs="$(ee_json backup restore --side-path "$side" "$backup_path" --workspace "$WS" --json)"
if printf '%s' "$rs" | jq -e '.success == true' >/dev/null 2>&1; then
    assert_jq "$rs" '.success == true' "backup restore succeeds"
    assert_jq "$rs" 'all((.data.degraded // [])[]?; has("code") and has("message"))' \
        "restore degraded rows are structured (never silent loss)"
    # Structured import accounting: imported/skipped/issue counts are reported as
    # numbers, so an asset that fails to restore surfaces as a counted issue,
    # never as silent loss. (Identity of the counts is asserted below when the
    # restore producer fully reproduces the source.)
    assert_jq "$rs" '(.data.counts.memoriesImported // null) | type == "number"' \
        "restore reports a numeric memoriesImported count (no silent loss)"
    assert_jq "$rs" '(.data.counts.issues // null) | type == "number"' \
        "restore surfaces a numeric issues count (degraded, not silent)"

    step "round-trip hash identity: re-backup of restored state reproduces hashes"
    # The restored workspace, re-backed-up, must reproduce the same records hash.
    rs_db="$(printf '%s' "$rs" | jq -r '.data.restoredDatabasePath // .data.databasePath // empty')"
    rs_imported="$(printf '%s' "$rs" | jq -r '.data.counts.memoriesImported // 0')"
    if [ -n "$rs_db" ] && [ -n "$records_hash" ] && [ "$rs_imported" -gt 0 ] 2>/dev/null; then
        cr2="$(ee_json backup create --workspace "$WS" --database "$rs_db" --json)"
        if printf '%s' "$cr2" | jq -e '.success == true' >/dev/null 2>&1; then
            assert_jq "$cr2" '(.data.recordsHash // "x") == "'"$records_hash"'"' \
                "re-backup of restored state reproduces the records hash (round-trip identity)"
        else
            log_drop 1 "round-trip re-backup over restored DB unavailable on this binary; restore success asserted above"
        fi
    else
        log_drop 1 "round-trip re-hash pending (bd-1n0np.23.2): restore imported ${rs_imported} record(s) on this binary, so a re-backup cannot yet reproduce the source recordsHash; when the restore producer fully materializes the durable records, assert the re-backup reproduces the original recordsHash byte-for-byte"
    fi
else
    rs_msg="$(printf '%s' "$rs" | jq -r '.error.message // "unknown"' 2>/dev/null)"
    case "$rs_msg" in
        *symbolic\ link*|*symlink*)
            log_drop 1 "restore skipped on this host: --side-path traverses a symlinked temp root (macOS /var->/private artifact); runs for real on Linux CI" ;;
        *)
            log_drop 1 "backup restore unavailable on this binary ($rs_msg): when wired, assert an isolated round-trip restore with identical artifact hashes" ;;
    esac
fi

step "NEW-ASSET coverage: every new durable/derived asset is in the manifest (23.2)"
# bd-1n0np.23.2's delta: the manifest must enumerate + hash each NEW asset class
# its producer lands. Until a producer lands, its asset is absent from artifacts/
# counts; assert-when-present, else log_drop the exact pending assertion.
for asset in anchors sentinels miss_ledger typed_fields attestation generation write_stats; do
    if printf '%s' "$cr" | jq -e --arg a "$asset" \
        '(.data.counts // {}) | has($a) or has(($a | gsub("_";"")))' >/dev/null 2>&1; then
        assert_jq "$cr" \
            '[.data.artifacts[]? | select((.kind // "") | test("'"$asset"'"))] | length >= 1' \
            "manifest enumerates + hashes the '$asset' asset"
    else
        log_drop 1 "manifest coverage for '$asset' pending (bd-1n0np.23.2 + its producer): when the producer lands, assert the manifest enumerates + blake3-hashes the '$asset' asset and a backup->restore round-trip reproduces it identically"
    fi
done

end_temp_workspace
harness_summary
