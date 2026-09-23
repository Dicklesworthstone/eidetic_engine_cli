#!/usr/bin/env bash
set -euo pipefail

doctor_fixture_target() {
    if [ -n "${EE_DOCTOR_FIXTURE_TARGET:-}" ]; then
        printf '%s\n' "$EE_DOCTOR_FIXTURE_TARGET"
        return 0
    fi
    printf 'EE_DOCTOR_FIXTURE_TARGET is required\n' >&2
    return 2
}

doctor_fixture_marker_dir() {
    local target="${1:?target required}"
    printf '%s\n' "$target/.ee/doctor-fixtures"
}

doctor_fixture_content_digest() {
    local target="${1:?target required}"
    # Exclude the doctor's own audit trail, the fixture's baseline directory,
    # the test wrapper's assert.* capture files, and macOS HFS+/ExFAT resource
    # fork sidecars (the `._*` files that appear on non-HFS volumes).
    #
    # Two runtime files are CLASSIFIED rather than byte-compared (bd-2oh15,
    # orchestrator decision on c9818). A real doctor fix + undo changed exactly
    # these two outside .doctor/:
    # - `ee.db-shm` is excluded: a transient SQLite shared-memory index derived
    #   from the WAL and rewritten by readers. It holds no source of truth.
    # - `ee.write.lock` is excluded from the BYTES but checked by its semantics
    #   (doctor_fixture_assert_write_lock_monotonic): a monotonic epoch counter
    #   that no honest undo can restore.
    # Everything else, including ee.db, ee.db-wal and .doctor.lock, stays
    # byte-compared.
    find "$target" -type f \
        -not -path '*/.doctor/*' \
        -not -path '*/.fixture_baseline/*' \
        -not -path '*/.ee/doctor-fixtures/*' \
        -not -path '*/.ee/ee.db-shm' \
        -not -path '*/.ee/ee.write.lock' \
        -not -name '.assert.stdout' \
        -not -name '.assert.stderr' \
        -not -name '._*' \
        -exec shasum -a 256 -- {} + | LC_ALL=C sort | shasum -a 256
}

# Print the flock-gate epoch stored in an ee.write.lock file, or fail. Format
# (src/db/mod.rs read_flock_gate_epoch): exactly 21 bytes, 20 ASCII digits,
# then a newline; a zero-padded decimal u64. Anything else is unreadable.
doctor_fixture_write_lock_epoch() {
    local lock="${1:?lock path required}"
    [ -f "$lock" ] && [ ! -L "$lock" ] || return 1
    [ "$(wc -c < "$lock" | tr -d ' ')" = "21" ] || return 1
    local digits
    digits="$(head -c 20 "$lock")"
    [[ "$digits" =~ ^[0-9]{20}$ ]] || return 1
    # The 21st byte must be the newline (command substitution strips it).
    [ -z "$(tail -c 1 "$lock")" ] || return 1
    printf '%s\n' "$digits"
}

doctor_fixture_record_write_lock() {
    local target="${1:?target required}"
    local lock="$target/.ee/ee.write.lock"
    if [ -e "$lock" ] || [ -L "$lock" ]; then
        if ! doctor_fixture_write_lock_epoch "$lock" > "$target/.fixture_baseline/write-lock.before"; then
            printf 'fixture corrupt: baseline ee.write.lock is unreadable: %s\n' "$lock" >&2
            return 1
        fi
    else
        printf 'absent\n' > "$target/.fixture_baseline/write-lock.before"
    fi
}

# ee.write.lock is a monotonic epoch: after undo it must still exist and its
# epoch must be >= the baseline. The fixed-width 20-digit encoding makes a string
# comparison exact. A lock absent at the baseline must stay absent: a created
# lock is a new path the byte digest no longer sees.
doctor_fixture_assert_write_lock_monotonic() {
    local fm_id="${1:?fm id required}"
    local target="${2:?target required}"
    local lock="$target/.ee/ee.write.lock"
    local before after
    before="$(cat "$target/.fixture_baseline/write-lock.before")"
    if [ "$before" = "absent" ]; then
        if [ -e "$lock" ] || [ -L "$lock" ]; then
            printf 'fixture assert: ee.write.lock was absent at the baseline but exists after undo for %s\n' \
                "$fm_id" >&2
            return 1
        fi
        return 0
    fi
    if ! after="$(doctor_fixture_write_lock_epoch "$lock")"; then
        printf 'fixture assert: ee.write.lock is missing or unreadable after undo for %s\n' "$fm_id" >&2
        return 1
    fi
    if [[ "$after" < "$before" ]]; then
        printf 'fixture assert: ee.write.lock epoch went backwards (%s -> %s) for %s\n' \
            "$before" "$after" "$fm_id" >&2
        return 1
    fi
}

doctor_fixture_corrupt() {
    local fm_id="${1:?fm id required}"
    local severity="${2:?severity required}"
    local subsystem="${3:?subsystem required}"
    local target
    target="$(doctor_fixture_target)"
    local marker_dir
    marker_dir="$(doctor_fixture_marker_dir "$target")"
    mkdir -p "$marker_dir" "$target/.fixture_baseline"
    # Hash each regular file's bytes AND its pathname, then hash the sorted
    # manifest. A same-name content change must not survive undo unnoticed.
    doctor_fixture_content_digest "$target" > "$target/.fixture_baseline/before.sha256"
    doctor_fixture_record_write_lock "$target"
    printf '{"schema":"ee.doctor_fixture_marker.v1","fmId":"%s","severity":"%s","subsystem":"%s","state":"corrupt"}\n' \
        "$fm_id" "$severity" "$subsystem" > "$marker_dir/$fm_id.json"
    printf 'corrupt fixture prepared: %s\n' "$fm_id" >&2
}

doctor_fixture_assert_health_report() {
    local fm_id="${1:?fm id required}"
    local report="${2:?report required}"
    # success means the command ran, not that the workspace is healthy.
    # The default JSON renderer exposes coreChecks (the full renderer uses
    # checks). Reject missing/empty populations and multiple JSON documents
    # as well as non-ok health, even when the process exits successfully.
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.command == "doctor" and .data.posture == "ok" and
            .data.healthy == true and
            (.data.coreChecks | type == "array" and length > 0 and
                all(.[]; .tier == "core" and .severity == "ok")) and
            (.data.actionable | type == "array" and length == 0))
    ' "$report" >/dev/null; then
        printf 'fixture assert: post-fix health not established for %s; see %s\n' \
            "$fm_id" "$report" >&2
        return 1
    fi
}

# Guidance-only failure modes (bd-2oh15, orchestrator decision C): doctor's
# repair for these writes state that cannot pass through
# doctor_runtime::mutate() or be undone (for example, an index rebuild writes
# SQLite rows), so doctor records guidance instead of repairing. The contract is
# honesty, not repair: --fix must report the finding as guidance_recorded and
# never as applied, the finding must still be reported afterwards, and the path
# the real repair would create must still be absent.
doctor_fixture_assert_guidance_only() {
    local fm_id="${1:?fm id required}"
    local finding_code="${2:?finding code required}"
    local check_name="${3:?check name required}"
    local error_code="${4:?error code required}"
    local absent_rel="${5:?path that must stay absent required}"
    local target
    target="$(doctor_fixture_target)"
    test -f "$(doctor_fixture_marker_dir "$target")/$fm_id.json"
    if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
        printf 'fixture assert: %s requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' \
            "$fm_id" >&2
        return 2
    fi
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json"
    if ! jq -es --arg code "$finding_code" '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            (.data.guidanceOnlyFixerCount | type == "number" and . >= 1) and
            (.data.fixerResults | type == "array") and
            any(.data.fixerResults[]; .findingCode == $code and .outcome == "guidance_recorded") and
            all(.data.fixerResults[]; .findingCode != $code or .outcome == "guidance_recorded"))
    ' "$target/.fixture_baseline/doctor-fix.json" >/dev/null; then
        printf 'fixture assert: %s fix did not record %s as guidance_recorded; see %s\n' \
            "$fm_id" "$finding_code" "$target/.fixture_baseline/doctor-fix.json" >&2
        return 1
    fi
    "$ee_bin" doctor --workspace "$target" --json > "$target/.fixture_baseline/doctor-after.json"
    if ! jq -es --arg check "$check_name" --arg error "$error_code" '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.healthy == false and
            any(.data.actionable[]; .name == $check and .errorCode == $error))
    ' "$target/.fixture_baseline/doctor-after.json" >/dev/null; then
        printf 'fixture assert: %s after guidance, doctor no longer reports %s %s; a real repair happened or the report is unreadable; see %s\n' \
            "$fm_id" "$check_name" "$error_code" "$target/.fixture_baseline/doctor-after.json" >&2
        return 1
    fi
    if [ -e "$target/$absent_rel" ] || [ -L "$target/$absent_rel" ]; then
        printf 'fixture assert: %s guidance-only fix created %s\n' "$fm_id" "$absent_rel" >&2
        return 1
    fi
    printf 'guidance-only fixture confirmed: %s (%s guidance_recorded, %s %s unchanged)\n' \
        "$fm_id" "$finding_code" "$check_name" "$error_code" >&2
}

doctor_fixture_assert() {
    local fm_id="${1:?fm id required}"
    local severity="${2:?severity required}"
    local subsystem="${3:?subsystem required}"
    local target
    target="$(doctor_fixture_target)"
    local marker_dir
    marker_dir="$(doctor_fixture_marker_dir "$target")"
    test -f "$marker_dir/$fm_id.json"

    if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" = "1" ]; then
        local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
        # ee's CLI surface (bd-3boan): --fix and --undo <RUN_ID> are flags, not
        # subcommands. The --fix flag declares `conflicts_with --only`, so we
        # invoke --fix on its own and pass --only on the read-only diagnose
        # call. That selector is currently advisory in the CLI; it does not
        # establish per-FM detector coverage. Assert the reported core health.
        "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json"
        "$ee_bin" doctor --workspace "$target" --only "$fm_id" --json > "$target/.fixture_baseline/doctor-after.json"
        doctor_fixture_assert_health_report "$fm_id" "$target/.fixture_baseline/doctor-after.json"
        local run_id
        run_id="$(jq -r '.runId // .data.runId // empty' "$target/.fixture_baseline/doctor-fix.json")"
        if [ -z "$run_id" ]; then
            printf 'fixture assert: could not extract runId from fix output\n' >&2
            cat "$target/.fixture_baseline/doctor-fix.json" >&2
            return 1
        fi
        "$ee_bin" doctor --workspace "$target" --undo "$run_id" --json > "$target/.fixture_baseline/doctor-undo.json"
        doctor_fixture_content_digest "$target" > "$target/.fixture_baseline/after-undo.sha256"
        if ! cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-undo.sha256"; then
            printf 'fixture assert: undo contents differ from the baseline for %s\n' "$fm_id" >&2
            return 1
        fi
        doctor_fixture_assert_write_lock_monotonic "$fm_id" "$target"
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}

# ---------------------------------------------------------------------------
# Real-corruption building blocks (bd-2oh15 P0 tranche). MOVES ONLY: an original
# is moved into .fixture_baseline before a damaged version is written as a NEW
# file. Nothing is deleted, and nothing is overwritten in place.
# ---------------------------------------------------------------------------

# Refuse a symlink or non-empty target rather than touching somebody's state.
doctor_fixture_prepare_target() {
    local target="${1:?target required}"
    if [ -L "$target" ]; then
        printf 'fixture refuses a symlink target: %s\n' "$target" >&2
        return 2
    fi
    mkdir -p "$target"
    if [ -n "$(find "$target" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        printf 'fixture requires an empty target: %s\n' "$target" >&2
        return 2
    fi
    mkdir -p "$target/.fixture_baseline"
}

# A real store with one source memory and a populated index, asserted healthy
# before any damage. A failure mode is only meaningful against a real baseline.
doctor_fixture_healthy_store() {
    local fm_id="${1:?fm id required}"
    local target="${2:?target required}"
    local ee_bin="${3:?ee binary required}"
    local base="$target/.fixture_baseline"
    "$ee_bin" --workspace "$target" init --skip-boilerplate --json > "$base/init.json"
    "$ee_bin" remember "Preserve this source memory while $fm_id is exercised." \
        --workspace "$target" --level procedural --kind rule --json > "$base/remember.json"
    "$ee_bin" index rebuild --workspace "$target" --json > "$base/index-healthy.json"
    jq -e '.schema == "ee.response.v2" and .success == true and .data.memories_indexed >= 1' \
        "$base/index-healthy.json" >/dev/null
    "$ee_bin" doctor --workspace "$target" --json > "$base/doctor-healthy.json"
    doctor_fixture_assert_health_report "$fm_id" "$base/doctor-healthy.json"
}

# Write <dst> as a NEW file: <src> with one 4096-byte page replaced by random
# bytes. <src> is read, never modified.
doctor_fixture_damaged_copy() {
    local src="${1:?source required}"
    local dst="${2:?destination required}"
    local page="${3:?page index required}"
    local off=$(( page * 4096 ))
    {
        head -c "$off" "$src"
        head -c 4096 /dev/urandom
        tail -c +"$(( off + 4096 + 1 ))" "$src"
    } > "$dst"
}

doctor_fixture_sha256() {
    shasum -a 256 -- "${1:?path required}" | cut -d' ' -f1
}

# NOT-DETECTED contract (orchestrator decision, bd-2oh15): a PINNED GAP.
# doctor reports the damaged workspace healthy, --fix takes no action, and the
# damaged artifact is byte-identical afterwards. The fixture's own assert.sh
# must separately prove the damage is real with an independent witness. The day
# doctor starts detecting this FM, this assertion FAILS on purpose: upgrade the
# spec label and the fixture together (spec rule V6). Pinned gaps never count
# toward coverage; the harness reports them as a separate GAPS count.
# <artifact_rel> may be '-' when the damage is not a single file.
doctor_fixture_assert_pinned_gap() {
    local fm_id="${1:?fm id required}"
    local artifact_rel="${2:?artifact path or - required}"
    shift 2
    local target
    target="$(doctor_fixture_target)"
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    local base="$target/.fixture_baseline"
    local before=""
    if [ "$artifact_rel" != "-" ]; then
        before="$(doctor_fixture_sha256 "$target/$artifact_rel")"
    fi
    "$ee_bin" doctor --workspace "$target" "$@" --json > "$base/gap-doctor.json"
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.healthy == true and .data.posture == "ok" and
            (.data.actionable | type == "array" and length == 0))
    ' "$base/gap-doctor.json" >/dev/null; then
        printf 'fixture assert: %s is a PINNED GAP but doctor now reports something; upgrade the spec and fixture (V6); see %s\n' \
            "$fm_id" "$base/gap-doctor.json" >&2
        return 1
    fi
    "$ee_bin" doctor --workspace "$target" --fix --json > "$base/gap-fix.json"
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.actionCount == 0 and (.data.fixerResults | length) == 0)
    ' "$base/gap-fix.json" >/dev/null; then
        printf 'fixture assert: %s is a PINNED GAP but --fix acted; upgrade the spec and fixture (V6); see %s\n' \
            "$fm_id" "$base/gap-fix.json" >&2
        return 1
    fi
    if [ -n "$before" ] && [ "$(doctor_fixture_sha256 "$target/$artifact_rel")" != "$before" ]; then
        printf 'fixture assert: %s damaged artifact %s changed during doctor/--fix\n' \
            "$fm_id" "$artifact_rel" >&2
        return 1
    fi
    printf 'pinned gap confirmed: %s (doctor healthy, --fix 0 actions, damage intact) -- NOT coverage\n' \
        "$fm_id" >&2
}

# REPORT-ONLY contract: doctor detects the FM (check + error code), but the
# dispatch table has no entry for it, so --fix takes no action and records no
# fixer result. The damage is untouched and still reported afterwards.
# <view> is 'concise' (actionable[]) or 'full' (every check, --full).
doctor_fixture_assert_report_only() {
    local fm_id="${1:?fm id required}"
    local check_name="${2:?check name required}"
    local error_code="${3:?error code required}"
    local view="${4:?concise or full required}"
    local artifact_rel="${5:?artifact path or - required}"
    local target
    target="$(doctor_fixture_target)"
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    local base="$target/.fixture_baseline"
    local before="" doctor_args=(--json) filter
    if [ "$view" = "full" ]; then
        doctor_args=(--full --json)
        filter='any(.. | objects | select(has("name") and has("errorCode")); .name == $check and .errorCode == $code)'
    else
        filter='any(.data.actionable[]?; .name == $check and .errorCode == $code)'
    fi
    if [ "$artifact_rel" != "-" ]; then
        before="$(doctor_fixture_sha256 "$target/$artifact_rel")"
    fi
    local phase
    for phase in before after; do
        "$ee_bin" doctor --workspace "$target" "${doctor_args[@]}" > "$base/report-$phase.json"
        if ! jq -es --arg check "$check_name" --arg code "$error_code" \
            "length == 1 and (.[0] | .schema == \"ee.response.v2\" and .success == true and ($filter))" \
            "$base/report-$phase.json" >/dev/null; then
            printf 'fixture assert: %s: doctor (%s) did not report %s %s %s --fix; see %s\n' \
                "$fm_id" "$view" "$check_name" "$error_code" "$phase" "$base/report-$phase.json" >&2
            return 1
        fi
        if [ "$phase" = before ]; then
            "$ee_bin" doctor --workspace "$target" --fix --json > "$base/report-fix.json"
            if ! jq -es '
                length == 1 and (.[0] |
                    .schema == "ee.response.v2" and .success == true and
                    .data.actionCount == 0 and (.data.fixerResults | length) == 0)
            ' "$base/report-fix.json" >/dev/null; then
                printf 'fixture assert: %s is REPORT-ONLY but --fix acted; upgrade the spec and fixture (V6); see %s\n' \
                    "$fm_id" "$base/report-fix.json" >&2
                return 1
            fi
        fi
    done
    if [ -n "$before" ] && [ "$(doctor_fixture_sha256 "$target/$artifact_rel")" != "$before" ]; then
        printf 'fixture assert: %s artifact %s changed during a report-only --fix\n' \
            "$fm_id" "$artifact_rel" >&2
        return 1
    fi
    printf 'report-only fixture confirmed: %s (%s %s reported, --fix 0 actions, still reported)\n' \
        "$fm_id" "$check_name" "$error_code" >&2
}

# The harness bucket of a fixture, from its manifest label (bd-2oh15 strand 4,
# ruling c9954). Prints one of:
#   coverage  REPAIR, GUIDANCE-ONLY: must pass.
#   gap       NOT-DETECTED, PINNED-DEFECT: must still reproduce its pinned gap.
#   untested  UNCLASSIFIED, UNRESOLVED: marker-only; never a pass, never a failure.
#   out_of_scope  OUT-OF-SCOPE: not a doctor failure mode (manifest scopeReason);
#             never run, never a pass, pinned by exact id in the ratchet.
#   unknown:<label>  anything else, including a fixture missing from the manifest.
doctor_fixture_bucket() {
    local fm_id="${1:?fm id required}"
    local manifest="${2:?manifest path required}"
    local label
    label="$(jq -r --arg id "$fm_id" '[.fixtures[] | select(.id == $id) | .label] | first // ""' "$manifest")"
    case "$label" in
        REPAIR | GUIDANCE-ONLY) printf 'coverage\n' ;;
        NOT-DETECTED | PINNED-DEFECT) printf 'gap\n' ;;
        UNCLASSIFIED | UNRESOLVED) printf 'untested\n' ;;
        OUT-OF-SCOPE) printf 'out_of_scope\n' ;;
        *) printf 'unknown:%s\n' "$label" ;;
    esac
}

# The manifest label of a fixture ("" when absent).
doctor_fixture_label() {
    local fm_id="${1:?fm id required}"
    local manifest="${2:?manifest path required}"
    jq -r --arg id "$fm_id" '[.fixtures[] | select(.id == $id) | .label] | first // ""' "$manifest"
}

# THE shared UNTESTED ratchet pin (bd-2oh15 rulings c9954 and the follow-up):
# one pin for every sub-harness that counts fixtures. It is exact in both
# directions: more untested fixtures than the pin fails (a new fixture must
# arrive classified), and fewer also fails until the pin is lowered in the same
# commit that classified the fixture, so the pin can only move down.
DOCTOR_FIXTURE_PIN_UNCLASSIFIED=12
DOCTOR_FIXTURE_PIN_UNRESOLVED=1
# OUT-OF-SCOPE fixtures are pinned by EXACT id (bd-2oh15 ruling on c9985), so
# relabelling a fixture OUT-OF-SCOPE can never be used to satisfy the pins
# above. Sorted, space separated. Each carries a manifest scopeReason.
DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS="fm-policy_safety-redaction-class-coverage-gap fm-policy_safety-trauma-guard-policy-denied-exit-7"

# Counts the UNCLASSIFIED and UNRESOLVED fixture directories under
# <fixtures_src>, prints the counts for <harness>, and fails unless both equal
# their pins. Also prints the OUT-OF-SCOPE set on its own line and fails
# unless it equals DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS exactly.
doctor_fixture_untested_ratchet() {
    local harness="${1:?harness name required}"
    local src="${2:?fixtures source required}"
    local manifest="$src/manifest.json"
    local unclassified=0 unresolved=0 out_of_scope="" fm_dir fm_id label
    for fm_dir in "$src"/fm-*; do
        [ -d "$fm_dir" ] || continue
        fm_id="$(basename "$fm_dir")"
        label="$(doctor_fixture_label "$fm_id" "$manifest")"
        case "$label" in
            UNCLASSIFIED) unclassified=$((unclassified + 1)) ;;
            UNRESOLVED) unresolved=$((unresolved + 1)) ;;
            OUT-OF-SCOPE) out_of_scope="$out_of_scope $fm_id" ;;
        esac
    done
    out_of_scope="$(printf '%s\n' $out_of_scope | LC_ALL=C sort | tr '\n' ' ' | sed 's/ *$//')"
    printf '%s: %s UNCLASSIFIED (not tested, pin %s); %s UNRESOLVED (not tested, pin %s)\n' \
        "$harness" "$unclassified" "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" \
        "$unresolved" "$DOCTOR_FIXTURE_PIN_UNRESOLVED" >&2
    printf '%s: %s OUT-OF-SCOPE (not doctor failure modes; never run, never a pass): %s\n' \
        "$harness" "$(printf '%s\n' $out_of_scope | grep -c .)" "${out_of_scope:-none}" >&2
    if [ "$out_of_scope" != "$DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS" ]; then
        printf '%s: OUT-OF-SCOPE set changed; it is pinned by exact id (expected: %s)\n' \
            "$harness" "$DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS" >&2
        return 1
    fi
    if [ "$unclassified" -gt "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" ] ||
        [ "$unresolved" -gt "$DOCTOR_FIXTURE_PIN_UNRESOLVED" ]; then
        printf '%s: UNTESTED ratchet exceeded; classify the new fixture instead of raising the pin\n' \
            "$harness" >&2
        return 1
    fi
    if [ "$unclassified" -lt "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" ] ||
        [ "$unresolved" -lt "$DOCTOR_FIXTURE_PIN_UNRESOLVED" ]; then
        printf '%s: UNTESTED ratchet pin is stale; lower DOCTOR_FIXTURE_PIN_* in tests/doctor_fixtures/lib.sh to %s/%s\n' \
            "$harness" "$unclassified" "$unresolved" >&2
        return 1
    fi
}
