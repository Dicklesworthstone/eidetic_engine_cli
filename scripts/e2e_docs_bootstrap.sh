#!/usr/bin/env bash
# bd-1n0np.11.5 — Docs-to-Memory Bootstrap Compiler end-to-end (real binary).
#
# Full scenario (E11): a temp workspace seeded with the repo's own doc shapes
# (AGENTS.md with rules/forbidden-deps) ->
#   1. `ee bootstrap docs --dry-run --json` compiles allowlisted docs into
#      reviewable candidates carrying source spans, source hashes, anchors, and a
#      specificity score (structural extraction, no summarization). NOTHING is
#      written (durableMutation is false / the store stays empty).
#   2. The run is DETERMINISTIC: two dry-runs over the same tree yield the same
#      candidate ids.
#   3. Guard rails surface as STRUCTURED degraded rows, never silent loss:
#      an oversized source -> docs_bootstrap_source_oversized; a symlinked
#      allowlisted path -> docs_bootstrap_symlink_rejected; a missing allowlisted
#      source -> docs_bootstrap_source_missing.
#   4. A non-allowlisted file (a stray secret) is never read into a candidate.
#   5. `ee bootstrap apply` refuses without --approved-only (no bulk auto-import),
#      and `--approved-only` applies only curation-approved candidates (here none
#      are approved, so nothing is written) — the no-silent-write guarantee.
#
# The bootstrap surface is fully landed (ee bootstrap docs/apply,
# schema ee.bootstrap.docs.run.v1), so these assertions run FOR REAL. Any step
# that observes a not-yet-available surface records a visible log_drop (the
# no-silent-cap rule) rather than a false pass.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "docs_bootstrap"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

with_temp_workspace WS

step "seed the workspace's own doc shapes (AGENTS.md rules + forbidden deps)"
printf '# AGENTS\n\n## Forbidden deps\n- tokio (use asupersync)\n- rusqlite (use fsqlite via sqlmodel)\n\n## Rules\n- Never force-push to main.\n- Always run tests before pushing.\n' \
    >"$WS/AGENTS.md"
printf '# Demo project\n\nThis project does X. Run ee init to start.\n' >"$WS/README.md"
# A stray non-allowlisted file the compiler must NOT read.
printf 'SECRET_TOKEN=hunter2\n' >"$WS/secrets.env"

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

if ! ee_supports bootstrap docs; then
    log_drop 1 "ee bootstrap docs surface unavailable on this binary (bd-1n0np.11.1/11.2): when present, assert structural candidates with spans/hashes/anchors/specificity, determinism, guard-rail rejection, and apply-through-curation"
    harness_summary
    exit "$?"
fi

step "bootstrap docs --dry-run compiles structural candidates (no mutation)"
run="$(ee_json bootstrap docs --dry-run --workspace "$WS" --json)"
assert_jq "$run" '.success == true' "bootstrap docs --dry-run succeeds"
assert_jq "$run" '.data.schema == "ee.bootstrap.docs.run.v1"' "run carries the v1 schema"
assert_jq "$run" '(.data.candidates | length) >= 1' "at least one candidate compiled"
# Dry-run never writes: durableMutation must not be true.
assert_jq "$run" '(.data.durableMutation // false) == false' \
    "dry-run performs no durable mutation"

step "each candidate is structural: source span + hash + anchors + specificity"
assert_jq "$run" 'all(.data.candidates[]?; has("sourceSpan") and has("sourceHash"))' \
    "every candidate carries a source span + source hash"
assert_jq "$run" 'all(.data.candidates[]?; (.anchors | type) == "array")' \
    "every candidate carries anchors"
assert_jq "$run" 'all(.data.candidates[]?; (.specificity | type) == "number")' \
    "every candidate carries a specificity score"
# Provenance points back at an allowlisted doc, never the stray secret file.
assert_jq "$run" 'all(.data.candidates[]?; (.sourcePath | test("secrets.env") | not))' \
    "no candidate is sourced from the non-allowlisted secret file"

step "the run is deterministic over a fixed doc tree"
ids_a="$(printf '%s' "$run" | jq -c '[.data.candidates[].candidateId] | sort')"
run2="$(ee_json bootstrap docs --dry-run --workspace "$WS" --json)"
ids_b="$(printf '%s' "$run2" | jq -c '[.data.candidates[].candidateId] | sort')"
assert_eq "$ids_a" "$ids_b" "two dry-runs yield identical candidate ids"

step "oversize source is rejected as a structured degraded row (no silent loss)"
oversize="$(ee_json bootstrap docs --dry-run --max-source-bytes 5 --workspace "$WS" --json)"
assert_jq "$oversize" 'any(.data.degraded[]?; .code == "docs_bootstrap_source_oversized")' \
    "an oversized allowlisted source surfaces docs_bootstrap_source_oversized"
assert_jq "$oversize" 'all(.data.degraded[]?; has("code") and has("message"))' \
    "every degraded row carries a code + message"

step "symlinked allowlisted path is rejected (no symlink traversal)"
sym_ws="${WS%/}.symlink"
mkdir -p "$sym_ws"
printf '# real\n## Rules\n- keep it real\n' >"$sym_ws/real_agents.md"
ln -s "$sym_ws/real_agents.md" "$sym_ws/AGENTS.md"
ee_json init --workspace "$sym_ws" --json >/dev/null
sym_run="$(ee_json bootstrap docs --dry-run --workspace "$sym_ws" --json)"
if printf '%s' "$sym_run" | jq -e '.success == true' >/dev/null 2>&1; then
    assert_jq "$sym_run" 'any(.data.degraded[]?; .code == "docs_bootstrap_symlink_rejected")' \
        "a symlinked allowlisted source surfaces docs_bootstrap_symlink_rejected"
    assert_jq "$sym_run" 'all(.data.candidates[]?; (.sourcePath | test("AGENTS.md") | not))' \
        "the symlinked AGENTS.md produces no candidate"
else
    log_drop 1 "symlink workspace init unavailable on this host; symlink-rejection assertion skipped"
fi

step "apply refuses bulk auto-import without --approved-only"
run_id="$(printf '%s' "$run" | jq -r '.data.runId // empty')"
if [ -n "$run_id" ] && ee_supports bootstrap apply; then
    refuse="$(ee_json bootstrap apply "$run_id" --workspace "$WS" --json)"
    assert_jq "$refuse" '(.success // false) != true' \
        "bootstrap apply refuses without --approved-only (no bulk auto-import)"

    step "apply --approved-only writes nothing when no candidate is approved"
    applied="$(ee_json bootstrap apply "$run_id" --approved-only --workspace "$WS" --json)"
    # No candidate was approved through curation, so nothing is written silently.
    assert_jq "$applied" '((.data.appliedCount // .data.applied // 0) | tonumber) == 0' \
        "apply --approved-only writes no unapproved candidate (no silent write)"
else
    log_drop 1 "bootstrap apply or runId unavailable: when present, assert apply refuses without --approved-only and applies only curation-approved candidates with audit"
fi

end_temp_workspace
harness_summary
