#!/usr/bin/env bash
# K3 (bd-17c65.11.3) — Build docs/degraded_codes.md from the J6
# (bd-17c65.10.6) failure-mode fixture catalog.
#
# The generated file is the canonical agent-facing reference for every
# `degraded[]` code that can appear in an `ee` response. One section
# per code with: severity, surfaces, introduced-by bead, retirement status
# when applicable, trigger description + setup + invocation, expected
# emission snippet, repair hint, related taxonomy classification when known.
#
# This script is idempotent — running it twice against the same
# fixture set produces a byte-identical doc (sections sorted
# alphabetically by code, and NO embedded timestamp anywhere). A
# comment here used to describe "the header's generated-at line,
# intentionally floored to the date"; there is no such line and there
# never was one in the generated output. That is lucky rather than
# planned, and it is now load-bearing: byte-identical reruns are what
# make the fingerprint below meaningful, so do not add a timestamp.
#
# Wired into the K3 CI gate via tests/degraded_codes_doc_coverage.rs
# which walks the same fixture set and asserts every code appears
# as a section heading in the doc. That test also shells out to
# `--check` and requires exit 0, so the two arms are not independent.
#
# THE DOC CARRIES ITS OWN FINGERPRINT, WHICH IS WHY --check IS CHEAP.
# `--check` used to regenerate the whole 524 KB document from 615
# fixtures purely in order to diff it, which measured 85.67 s on an
# M-series Mac — and almost none of that was CPU (user 2.3 s), because
# the cost is ~5000 `jq` process spawns. At that price the check could
# not be a verify.sh stage: the non-benchmark budget had 4 s of
# headroom against its 608 s ceiling (measured 2026-11-08 at 604/608;
# the manifest header's own dated paragraphs say 10 s and CLAUDE.md
# says 6 s, and both are stale).
#
# So the generator now records two hashes in the doc's last two lines:
#
#   inputs=  every fixture's name and content, the taxonomy file, and
#            THIS SCRIPT. The doc is a pure function of those three, so
#            matching inputs means regenerating would reproduce it.
#   body=    the doc's own bytes less the body-marker line itself. This
#            is what catches a hand-edit, which the inputs hash cannot
#            see because a hand-edit changes no input.
#
# Together they are sound: body matches => the file is what was
# generated, inputs match => generating now yields the same thing. So a
# fast pass implies a full pass, and the fast path runs in ~0.16 s
# without spawning a single `jq`. `--check-full` keeps the byte diff as
# the authoritative arm, for when you distrust the scheme itself.
#
# Adding a timestamp, or any other non-input-derived content, breaks
# that soundness and would make `--check` lie.
#
# Usage:
#   scripts/build_degraded_codes_doc.sh              regenerate
#   scripts/build_degraded_codes_doc.sh --check      fingerprint compare (~0.16s)
#   scripts/build_degraded_codes_doc.sh --check-full byte diff (~86s)
#   scripts/build_degraded_codes_doc.sh --self-test  prove every failure direction
#
# Reads:
#   tests/fixtures/failure_modes/*.json
#   docs/degraded_code_taxonomy.md   (optional; pulls classification when present)
#
# Writes:
#   docs/degraded_codes.md  (atomic — staged to a tmp path then mv'd)
#
# Env overrides, for --self-test only (the arms drive this script
# against a throwaway tree rather than transliterating its rules):
#   EE_DEGRADED_FIXTURES_DIR, EE_DEGRADED_OUTPUT, EE_DEGRADED_TAXONOMY

set -euo pipefail

MODE="write"
case "${1:-}" in
    "") ;;
    --check) MODE="check" ;;
    --check-full) MODE="check-full" ;;
    --self-test) MODE="self-test" ;;
    *)
        echo "build_degraded_codes_doc.sh: usage: $0 [--check|--check-full|--self-test]" >&2
        exit 2
        ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_PATH="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURES_DIR="${EE_DEGRADED_FIXTURES_DIR:-$REPO_ROOT/tests/fixtures/failure_modes}"
OUTPUT="${EE_DEGRADED_OUTPUT:-$REPO_ROOT/docs/degraded_codes.md}"
TAXONOMY="${EE_DEGRADED_TAXONOMY:-$REPO_ROOT/docs/degraded_code_taxonomy.md}"

# The fast check needs no jq at all, so demanding it there would be a gate
# failing for a tool it does not use. Every other mode reads fixture fields.
if [ "$MODE" != "check" ] && ! command -v jq >/dev/null 2>&1; then
    echo "build_degraded_codes_doc.sh: jq is required for $MODE" >&2
    exit 2
fi

if [ ! -d "$FIXTURES_DIR" ]; then
    echo "build_degraded_codes_doc.sh: fixtures dir not found: $FIXTURES_DIR" >&2
    exit 3
fi

# --- the fingerprint -------------------------------------------------------
#
# Two markers, written as the doc's last two lines. HTML comments so they are
# invisible in rendered markdown and create no `## ` section for
# tests/degraded_codes_doc_coverage.rs to read as an orphan.
BODY_MARKER_RE='^<!-- degraded-codes-fingerprint body=sha256:'
INPUTS_MARKER_RE='^<!-- degraded-codes-fingerprint inputs=sha256:'

# Read one marker's hash out of a generated doc. Empty when absent, which the
# callers treat as "stale, regenerate" rather than as a pass.
marker_value() {
    local doc="$1" field="$2"
    sed -n "s|^<!-- degraded-codes-fingerprint ${field}=sha256:\\([0-9a-f]\\{64\\}\\) -->\$|\\1|p" \
        "$doc" | tail -1
}

# Hash of everything the generated doc is derived from.
#
# ONE `shasum` process for all 615 fixtures, not one per fixture: a per-file
# loop would reintroduce exactly the process-spawn cost this whole change
# exists to remove (measured 85.67s real against 2.31s user). Paths are
# relativised by running inside the fixtures dir so the hash does not depend
# on where the repo is checked out -- and so --self-test, which points
# FIXTURES_DIR at a temp tree, produces comparable values.
fingerprint_inputs() {
    {
        # Bump this label if the SCHEME changes. It participates in the hash,
        # so a scheme change forces a regeneration rather than silently
        # comparing old values under new rules.
        echo "ee.degraded_codes.fingerprint.v1"
        printf '%s\0' "${FIXTURE_RELPATHS[@]}" \
            | (cd "$FIXTURES_DIR" && xargs -0 shasum -a 256 --)
        if [ -f "$TAXONOMY" ]; then
            printf 'taxonomy %s\n' "$(shasum -a 256 < "$TAXONOMY" | awk '{print $1}')"
        else
            # Distinct from any content hash: a DELETED taxonomy changes the
            # doc (the cross-reference line disappears), so it must change the
            # fingerprint too.
            printf 'taxonomy absent\n'
        fi
        printf 'generator %s\n' "$(shasum -a 256 < "$SCRIPT_PATH" | awk '{print $1}')"
    } | shasum -a 256 | awk '{print $1}'
}

# Hash of the doc's own bytes, less the body-marker line. Self-referential
# content cannot be inside its own digest, the same reason the pack hash
# excludes its footer.
#
# `|| true` because grep exits 1 when it selects nothing, and `set -o pipefail`
# would turn an empty file into a script failure rather than a hash mismatch.
doc_body_hash() {
    { grep -v "$BODY_MARKER_RE" "$1" || true; } | shasum -a 256 | awk '{print $1}'
}

# --- fixture collection ----------------------------------------------------
#
# The PATH list is cheap (no jq) and is all the fingerprint needs, so it is
# gathered for every mode. The CODE-sorted list costs a jq per fixture and is
# gathered only by the modes that render or diff the document.
FIXTURE_RELPATHS=()
while IFS= read -r f; do
    FIXTURE_RELPATHS+=("$f")
done < <(cd "$FIXTURES_DIR" && find . -maxdepth 1 -type f -name '*.json' | LC_ALL=C sort)

if [ "${#FIXTURE_RELPATHS[@]}" -eq 0 ]; then
    echo "build_degraded_codes_doc.sh: no fixtures found under $FIXTURES_DIR" >&2
    exit 4
fi

FIXTURES=()

# Collect every fixture, validate its code, then sort by the fixture's code.
# Filenames normally match codes, but the generated contract is defined by
# JSON content rather than incidental path spelling.
#
# A fixture that loses its `.code` is still CAUGHT by the fast check, one step
# removed: its content hash moves, the fingerprint mismatches, and the
# regeneration the check tells you to run is what names the offending file.
collect_fixtures_by_code() {
    local f code
    FIXTURE_ROWS=()
    while IFS= read -r f; do
        code="$(jq -r '.code // empty' "$f")"
        if [ -z "$code" ]; then
            echo "build_degraded_codes_doc.sh: fixture $f missing .code" >&2
            exit 5
        fi
        FIXTURE_ROWS+=("${code}"$'\t'"${f}")
    done < <(find "$FIXTURES_DIR" -maxdepth 1 -type f -name '*.json' | LC_ALL=C sort)

    FIXTURES=()
    while IFS=$'\t' read -r _code fixture; do
        FIXTURES+=("$fixture")
    done < <(printf '%s\n' "${FIXTURE_ROWS[@]}" | LC_ALL=C sort -t $'\t' -k1,1)
}

# Render a value as a markdown code span.
#
# Repair and message strings routinely quote a command, so the value can
# itself contain backticks. A plain `$value` wrapper closes the span at
# the value's first backtick and leaves a stray one in the rendered doc.
# CommonMark allows a longer fence: pick one backtick longer than the
# longest run in the value and pad with spaces so the padding is stripped.
code_span() {
    local value="$1"
    local longest=0
    local run=0
    local i
    local char
    for ((i = 0; i < ${#value}; i++)); do
        char="${value:i:1}"
        if [ "$char" = '`' ]; then
            run=$((run + 1))
            if [ "$run" -gt "$longest" ]; then
                longest="$run"
            fi
        else
            run=0
        fi
    done
    if [ "$longest" -eq 0 ]; then
        printf '`%s`' "$value"
        return
    fi
    local fence=''
    while [ "${#fence}" -le "$longest" ]; do
        fence="${fence}"'`'
    done
    printf '%s %s %s' "$fence" "$value" "$fence"
}

# Build the file body. Header documents auto-generation contract;
# operators reading the doc see it cannot be hand-edited without
# being overwritten on the next regen.
#
# Emits everything EXCEPT the body-fingerprint line, which by construction
# cannot be inside the bytes it digests. The inputs marker IS emitted here, so
# the body hash covers it and the two markers are bound together: you cannot
# keep a stale inputs value beside a freshly hashed body.
build_catalog_body() {
    cat <<'HEADER'
# Degraded codes catalog

> **AUTO-GENERATED.** Do not edit by hand. This file is regenerated
> from `tests/fixtures/failure_modes/*.json` (the J6 catalog,
> bd-17c65.10.6) by `scripts/build_degraded_codes_doc.sh` (K3,
> bd-17c65.11.3). The CI gate
> `tests/degraded_codes_doc_coverage.rs` fails when a fixture lacks
> a matching section in this file, or when this file has an orphan
> section without a fixture.
>
> **To regenerate:**
> ```bash
> ./scripts/build_degraded_codes_doc.sh
> ./scripts/build_degraded_codes_doc.sh --check
> ```
>
> **To add a code:** drop a fixture under
> `tests/fixtures/failure_modes/<code>.json` matching the
> `ee.failure_mode_fixture.v1` schema, then re-run the script.
>
> **Companion docs:**
> - [`docs/degraded_code_taxonomy.md`](./degraded_code_taxonomy.md) —
>   classification (build_time / response_time / mixed) per the
>   bd-fptj3 taxonomy.
> - [`docs/migration_v0_1_to_v0_2.md`](./migration_v0_1_to_v0_2.md) —
>   the wire-contract delta the v0.2 release introduced; code
>   surfaces refer back to this catalog by code name.

---

## How agents consume this catalog

When an `ee` command returns a response with non-empty `data.degraded[]`,
each entry's `code` field matches one of the section headings below.
The agent should:

1. Look up the section by code.
2. Read **Surface** to confirm the code can fire on the command in question.
3. Read **Severity** to decide whether to retry / warn / abort.
4. If the entry includes `repair` or `recovery[]`, run those first.
5. The **Trigger** section explains when the code fires so an agent
   can avoid the failure mode on subsequent calls.

Per bd-17c65.5.2 (E2), `degraded[]` only emits per-response codes by
default — `response_time` codes, plus the response-time half of `mixed`
codes. Build-time feature gaps appear here for reference but are
surfaced through capabilities rather than the default per-response
array; pass `--include-non-affecting-degradations` to inspect every
documented category.

---

HEADER

    # Total counter line so a reader sees catalog size at a glance.
    echo ""
    echo "## Catalog summary"
    echo ""
    echo "- **Fixtures indexed:** $(printf '%s\n' "${FIXTURES[@]}" | wc -l | tr -d ' ')"
    echo "- **Generated by:** \`scripts/build_degraded_codes_doc.sh\` (K3 / bd-17c65.11.3)"
    if [ -f "$TAXONOMY" ]; then
        echo "- **Taxonomy cross-reference:** [\`degraded_code_taxonomy.md\`](./degraded_code_taxonomy.md)"
    fi
    echo ""
    echo "---"
    echo ""

    # One section per fixture, sorted alphabetically by code.
    for fixture in "${FIXTURES[@]}"; do
        code="$(jq -r '.code // empty' "$fixture")"
        severity="$(jq -r '.severity // "unspecified"' "$fixture")"
        surfaces="$(jq -r '(.surfaces // []) | join(", ")' "$fixture")"
        bead="$(jq -r '.introduced_by.bead // "(unknown)"' "$fixture")"
        epic="$(jq -r '.introduced_by.epic_letter // ""' "$fixture")"
        trigger_desc="$(jq -r '.trigger.description // "(no description provided)"' "$fixture")"
        invocation="$(jq -r '.trigger.invocation // ""' "$fixture")"
        repair_present="$(jq -r '.repair_present // false' "$fixture")"
        repair_unavailable_reason="$(jq -r '.repair_unavailable_reason // ""' "$fixture")"
        repair_contains="$(jq -r '.expected_emission.repair_contains // ""' "$fixture")"
        repair_string="$(jq -r '.expected_emission.repair_string // ""' "$fixture")"
        message_contains="$(jq -r '(.expected_emission.message_contains // []) | join(" ... ")' "$fixture")"
        retired="$(jq -r '.retired // false' "$fixture")"
        retired_bead="$(jq -r '.retired_by.bead // ""' "$fixture")"
        retired_reason="$(jq -r '.retired_by.reason // ""' "$fixture")"

        echo "## \`$code\`"
        echo ""
        echo "**Severity:** $severity"
        if [ -n "$surfaces" ]; then
            echo ""
            echo "**Surfaces:** $surfaces"
        fi
        if [ "$bead" != "(unknown)" ]; then
            if [ -n "$epic" ]; then
                echo ""
                echo "**Introduced by:** $bead (epic $epic)"
            else
                echo ""
                echo "**Introduced by:** $bead"
            fi
        fi
        echo ""
        if [ "$retired" = "true" ]; then
            echo "**Status:** retired by ${retired_bead:-unknown}. No current production path emits this code."
            if [ -n "$retired_reason" ] && [ "$retired_reason" != "null" ]; then
                echo ""
                echo "**Retirement reason.** $retired_reason"
            fi
            echo ""
            echo "**Historical trigger.** $trigger_desc"
        else
            echo "**Trigger.** $trigger_desc"
        fi

        # Setup commands (optional)
        setup_count="$(jq '(.trigger.setup_commands // []) | length' "$fixture")"
        if [ "$setup_count" -gt 0 ]; then
            echo ""
            echo "**Setup.**"
            echo ""
            echo '```bash'
            jq -r '.trigger.setup_commands[]' "$fixture"
            echo '```'
        fi

        if [ -n "$invocation" ] && [ "$invocation" != "null" ]; then
            echo ""
            if [ "$retired" = "true" ]; then
                echo "**Historical invocation.**"
            else
                echo "**Invocation.**"
            fi
            echo ""
            echo '```bash'
            echo "$invocation"
            echo '```'
        fi

        echo ""
        echo "**Expected emission.** Message contains: $(code_span "$message_contains")"
        if [ "$repair_present" = "true" ] && [ -n "$repair_string" ] && [ "$repair_string" != "null" ]; then
            echo ""
            echo "**Repair hint.** $repair_string"
        elif [ "$repair_present" = "true" ] && [ -n "$repair_contains" ] && [ "$repair_contains" != "null" ]; then
            echo ""
            echo "**Repair hint.** $(code_span "$repair_contains")"
        elif [ "$repair_present" = "true" ]; then
            echo ""
            echo "**Repair hint.** Present in fixture; see the response payload's \`error.repair\` field at runtime."
        elif [ -n "$repair_unavailable_reason" ] && [ "$repair_unavailable_reason" != "null" ]; then
            echo ""
            echo "**Repair hint.** $repair_unavailable_reason"
        else
            echo ""
            echo "**Repair hint.** Not provided — this code is informational; no operator action is required."
        fi

        echo ""
        echo "**Fixture.** [\`tests/fixtures/failure_modes/$(basename "$fixture")\`](../tests/fixtures/failure_modes/$(basename "$fixture"))"
        echo ""
        echo "---"
        echo ""
    done

    cat <<'FOOTER'
## Reporting drift

If a section here does not match the runtime emission you observe,
the gap is one of:

1. **Fixture stale** — the fixture in `tests/fixtures/failure_modes/`
   no longer matches the implementation. Update the fixture, re-run
   `./scripts/build_degraded_codes_doc.sh`.
2. **Implementation stale** — the code emits something the fixture
   asserted away. Update the implementation OR the fixture's
   `expected_emission` block; the J6 catalog gate
   (`tests/contracts/failure_mode_fixtures.rs`) tracks this.
3. **Doc stale** — this file fell out of sync because the regen
   script wasn't run. The CI gate
   (`tests/degraded_codes_doc_coverage.rs`) catches this.

For new degraded codes, follow the J6 fixture-authoring workflow in
`tests/fixtures/failure_modes/` (see any existing fixture as a
template), then regenerate.
FOOTER

    printf '<!-- degraded-codes-fingerprint inputs=sha256:%s -->\n' "$(fingerprint_inputs)"
}

# --- modes -----------------------------------------------------------------

run_check_fast() {
    local want_inputs want_body have_inputs have_body
    want_inputs="$(marker_value "$OUTPUT" inputs)"
    want_body="$(marker_value "$OUTPUT" body)"

    # FAIL CLOSED on a doc with no fingerprint. An older generated doc has no
    # markers, and treating "nothing to compare" as agreement is the silent
    # false pass this gate exists to avoid.
    if [ -z "$want_inputs" ] || [ -z "$want_body" ]; then
        echo "build_degraded_codes_doc.sh: $OUTPUT carries no fingerprint" >&2
        echo "  (inputs marker: ${want_inputs:-MISSING}, body marker: ${want_body:-MISSING})" >&2
        echo "  This is not a pass. Run ./scripts/build_degraded_codes_doc.sh" >&2
        return 1
    fi

    have_body="$(doc_body_hash "$OUTPUT")"
    if [ "$have_body" != "$want_body" ]; then
        echo "build_degraded_codes_doc.sh: $OUTPUT was EDITED after generation" >&2
        echo "  recorded body sha256: $want_body" >&2
        echo "  actual   body sha256: $have_body" >&2
        echo "  The file says AUTO-GENERATED; discard the edit and change a" >&2
        echo "  fixture instead, then run ./scripts/build_degraded_codes_doc.sh" >&2
        return 1
    fi

    have_inputs="$(fingerprint_inputs)"
    if [ "$have_inputs" != "$want_inputs" ]; then
        echo "build_degraded_codes_doc.sh: $OUTPUT is stale" >&2
        echo "  recorded input sha256: $want_inputs" >&2
        echo "  actual   input sha256: $have_inputs" >&2
        echo "  A fixture, docs/degraded_code_taxonomy.md, or this generator" >&2
        echo "  changed since the doc was built." >&2
        echo "  Run ./scripts/build_degraded_codes_doc.sh" >&2
        return 1
    fi

    echo "build_degraded_codes_doc.sh: $OUTPUT fingerprint matches (${#FIXTURE_RELPATHS[@]} fixtures)" >&2
    return 0
}

run_check_full() {
    collect_fixtures_by_code
    # The doc minus its body marker IS build_catalog_body's output, so compare
    # those directly. The old form called the generator TWICE on the red path
    # -- once for `diff -q`, once for `diff -u` -- which made a failing check
    # cost double. One build, one diff, reused.
    local rendered status
    rendered="$(mktemp "${TMPDIR:-/tmp}/degraded-codes-render.XXXXXX")"
    build_catalog_body > "$rendered"
    status=0
    if ! diff -u <({ grep -v "$BODY_MARKER_RE" "$OUTPUT" || true; }) "$rendered" >&2; then
        echo "build_degraded_codes_doc.sh: $OUTPUT is stale; run ./scripts/build_degraded_codes_doc.sh" >&2
        status=1
    fi
    # The byte diff says nothing about the body marker, so check it too --
    # otherwise --check-full would be WEAKER than --check on a hand-edit to
    # that one line.
    local want_body have_body
    want_body="$(marker_value "$OUTPUT" body)"
    have_body="$(doc_body_hash "$OUTPUT")"
    if [ "$want_body" != "$have_body" ]; then
        echo "build_degraded_codes_doc.sh: body fingerprint line does not match the body" >&2
        status=1
    fi
    rm -f "$rendered"
    if [ "$status" -eq 0 ]; then
        echo "build_degraded_codes_doc.sh: $OUTPUT matches the code-sorted fixture catalog" >&2
    fi
    return "$status"
}

run_write() {
    collect_fixtures_by_code
    STAGE_OUTPUT="$(mktemp "${OUTPUT}.XXXXXX")"
    trap 'rm -f "$STAGE_OUTPUT"' EXIT
    build_catalog_body > "$STAGE_OUTPUT"
    printf '<!-- degraded-codes-fingerprint body=sha256:%s -->\n' \
        "$(shasum -a 256 < "$STAGE_OUTPUT" | awk '{print $1}')" >> "$STAGE_OUTPUT"
    # Atomically replace the output file so a partial run doesn't leave
    # half a doc behind.
    mv "$STAGE_OUTPUT" "$OUTPUT"
    trap - EXIT
    echo "build_degraded_codes_doc.sh: wrote $OUTPUT ($(wc -l < "$OUTPUT" | tr -d ' ') lines, $(printf '%s\n' "${FIXTURES[@]}" | wc -l | tr -d ' ') fixtures indexed)" >&2
}

# --- self-test -------------------------------------------------------------
#
# Every arm RE-INVOKES this script against a throwaway fixture tree rather
# than restating its rules. An arm that transliterates the logic agrees with
# the logic when the logic is wrong, which is how a colon-splitting bug in a
# sibling gate survived that gate's own first self-test.
#
# It never reads or writes the real docs/degraded_codes.md or the real
# fixtures: every invocation passes EE_DEGRADED_* overrides.
#
# NOTE ON `set -e`: every arm must capture the exit code as
# `rc=0; cmd || rc=$?`. The obvious `cmd; rc=$?` aborts the whole script the
# first time an arm's command fails -- which is every RED arm, i.e. the ones
# that matter. The first draft of this block had it wrong.
run_self_test() {
    local root pass=0 fail=0 rc m_in m_body zeros
    root="$(mktemp -d "${TMPDIR:-/tmp}/degraded-codes-selftest.XXXXXX")"
    mkdir -p "$root/fixtures" "$root/empty"

    arm() {
        local name="$1" want="$2" got="$3"
        if [ "$want" = "$got" ]; then
            printf '  ok   %s\n' "$name"
            pass=$((pass + 1))
        else
            printf '  FAIL %s (wanted %s, got %s)\n' "$name" "$want" "$got"
            fail=$((fail + 1))
        fi
    }

    # Two minimal fixtures. The shape only needs `.code` plus the fields the
    # renderer reads, since every other field has a default.
    cat > "$root/fixtures/alpha_code.json" <<'FIXTURE'
{"schema":"ee.failure_mode_fixture.v1","code":"alpha_code","severity":"warning",
 "surfaces":["search"],"introduced_by":{"bead":"bd-selftest"},
 "trigger":{"description":"alpha trigger","invocation":"ee search x"},
 "repair_present":false}
FIXTURE
    cat > "$root/fixtures/beta_code.json" <<'FIXTURE'
{"schema":"ee.failure_mode_fixture.v1","code":"beta_code","severity":"error",
 "surfaces":["pack"],"introduced_by":{"bead":"bd-selftest"},
 "trigger":{"description":"beta trigger","invocation":"ee pack y"},
 "repair_present":false}
FIXTURE

    run_under() {
        EE_DEGRADED_FIXTURES_DIR="$root/fixtures" \
        EE_DEGRADED_OUTPUT="$root/doc.md" \
        EE_DEGRADED_TAXONOMY="$root/taxonomy.md" \
            "$SCRIPT_PATH" "$@" >/dev/null 2>&1
    }

    # A fresh doc passes both arms.
    rc=0; run_under || rc=$?
    arm "arm01 generation succeeds" 0 "$rc"
    rc=0; run_under --check || rc=$?
    arm "arm02 a freshly generated doc passes the fast check" 0 "$rc"
    rc=0; run_under --check-full || rc=$?
    arm "arm03 ... and the authoritative byte diff agrees with it" 0 "$rc"

    m_in="$(marker_value "$root/doc.md" inputs)"
    m_body="$(marker_value "$root/doc.md" body)"
    rc=1
    if [ -n "$m_in" ] && [ -n "$m_body" ] && [ "$m_in" != "$m_body" ]; then rc=0; fi
    arm "arm04 both markers are present and distinct" 0 "$rc"

    # A fixture edit must RED the fast check. This is the failure the bead
    # observed twice in one day, so it is the arm that matters most.
    printf '\n' >> "$root/fixtures/alpha_code.json"
    rc=0; run_under --check || rc=$?
    arm "arm05 a one-byte FIXTURE edit reds the fast check" 1 "$rc"
    rc=0; run_under || rc=$?; run_under --check || rc=$?
    arm "arm06 ... and regenerating clears it" 0 "$rc"

    # An ADDED fixture: content hashes alone would miss this if the path list
    # were not part of the digest.
    cp "$root/fixtures/alpha_code.json" "$root/fixtures/gamma_code.json"
    sed -i.bak 's/alpha_code/gamma_code/g' "$root/fixtures/gamma_code.json"
    rm -f "$root/fixtures/gamma_code.json.bak"
    rc=0; run_under --check || rc=$?
    arm "arm07 an ADDED fixture reds the fast check" 1 "$rc"
    rc=0; run_under || rc=$?

    # ... and a REMOVED one, the direction that silently shrinks a catalog.
    mv "$root/fixtures/gamma_code.json" "$root/gamma_parked.json"
    rc=0; run_under --check || rc=$?
    arm "arm08 a REMOVED fixture reds the fast check" 1 "$rc"
    mv "$root/gamma_parked.json" "$root/fixtures/gamma_code.json"
    rc=0; run_under || rc=$?

    # A HAND-EDIT of the generated doc. The inputs hash CANNOT see this,
    # because a hand-edit changes no input -- the body hash is the only arm
    # that catches the thing the doc's own header forbids.
    printf 'hand-written line\n' >> "$root/doc.md"
    rc=0; run_under --check || rc=$?
    arm "arm09 a HAND-EDIT of the doc reds the fast check" 1 "$rc"
    rc=0; run_under || rc=$?

    # A doc with NO fingerprint must fail closed. Every doc generated before
    # this change looks like this, and treating "nothing to compare" as
    # agreement is the silent false pass the whole gate exists to avoid.
    { grep -v '^<!-- degraded-codes-fingerprint' "$root/doc.md" || true; } > "$root/stripped"
    cp "$root/stripped" "$root/doc.md"
    rc=0; run_under --check || rc=$?
    arm "arm10 a doc carrying no fingerprint FAILS CLOSED" 1 "$rc"
    rc=0; run_under || rc=$?

    # The taxonomy file appearing changes the doc, so it must change the
    # fingerprint. absent-vs-present is a real branch in the renderer, which
    # is why the manifest spells `taxonomy absent` rather than omitting it.
    printf '# taxonomy\n' > "$root/taxonomy.md"
    rc=0; run_under --check || rc=$?
    arm "arm11 a taxonomy file APPEARING reds the fast check" 1 "$rc"
    rc=0; run_under --check-full || rc=$?
    arm "arm12 ... and the byte diff reds on the same change" 1 "$rc"
    rc=0; run_under || rc=$?; run_under --check || rc=$?
    arm "arm13 ... and regenerating clears both" 0 "$rc"

    # The body marker itself must be covered. Corrupting ONLY that line leaves
    # every other byte and every input untouched, so this is the one case
    # where --check-full could be weaker than --check.
    zeros="$(printf '0%.0s' $(seq 64))"
    sed -i.bak "s|body=sha256:[0-9a-f]\\{64\\}|body=sha256:${zeros}|" "$root/doc.md"
    rm -f "$root/doc.md.bak"
    rc=0; run_under --check || rc=$?
    arm "arm14 a corrupted body marker reds the fast check" 1 "$rc"
    rc=0; run_under --check-full || rc=$?
    arm "arm15 ... and reds --check-full too, not just the fast arm" 1 "$rc"
    rc=0; run_under || rc=$?

    # Environment failures must stay distinguishable from staleness.
    rc=0
    EE_DEGRADED_FIXTURES_DIR="$root/empty" EE_DEGRADED_OUTPUT="$root/doc2.md" \
        "$SCRIPT_PATH" --check >/dev/null 2>&1 || rc=$?
    arm "arm16 an empty fixture dir is exit 4, never a vacuous pass" 4 "$rc"

    rc=0
    EE_DEGRADED_FIXTURES_DIR="$root/nope" EE_DEGRADED_OUTPUT="$root/doc3.md" \
        "$SCRIPT_PATH" --check >/dev/null 2>&1 || rc=$?
    arm "arm17 a missing fixture dir is exit 3, distinct from empty" 3 "$rc"

    rc=0
    "$SCRIPT_PATH" --no-such-flag >/dev/null 2>&1 || rc=$?
    arm "arm18 an unknown flag exits 2 rather than regenerating" 2 "$rc"

    # Clean up without a recursive delete: only the files these arms created,
    # then rmdir, which refuses rather than removing anything unexpected.
    rm -f "$root/fixtures"/*.json "$root"/doc.md "$root"/doc2.md "$root"/doc3.md \
        "$root"/stripped "$root"/taxonomy.md
    rmdir "$root/fixtures" "$root/empty" "$root" 2>/dev/null || \
        printf '  (left %s in place; it held files these arms did not create)\n' "$root"

    printf '\nbuild_degraded_codes_doc.sh self-test: %s/%s arms passed\n' \
        "$pass" "$((pass + fail))"
    [ "$fail" -eq 0 ]
}

case "$MODE" in
    check) run_check_fast ;;
    check-full) run_check_full ;;
    write) run_write ;;
    self-test) run_self_test ;;
esac
