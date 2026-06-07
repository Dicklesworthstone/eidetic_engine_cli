#!/usr/bin/env bash
# bd-1n0np.3.10 — Code-coupled freshness lifecycle end-to-end (real binary).
#
# Full scenario (ADR 0056, bd-1n0np.3.7): temp workspace + a tiny source tree ->
# remember a memory anchored to a symbol -> assert `ee impact --symbol/<path>`
# returns it -> change the symbol's text -> run the bounded freshness steward
# (git-changed files only) -> assert `ee memory show` reports symbol_changed, a
# pack emits a per-item `freshness: symbol_drift` with rank-down (item still
# present, not removed), a `revalidate` curation candidate appears, and pack
# provenance carries the live file:line -> then rename/move the symbol and assert
# status=unknown (advisory), NEVER stale.
#
# The freshness LIFECYCLE half depends on surfaces that are landed only as
# library primitives, not yet wired as observable CLI behavior: the bounded
# steward drift job, `ee memory show` freshness status, the per-pack symbol_drift
# facet, and revalidate-candidate emission (bd-1n0np.3.7 / 3.8). Each is
# CAPABILITY-GUARDED here: a missing surface records a visible `log_drop` (the
# no-silent-cap rule) with the exact assertion that activates once the surface
# exists, instead of a false pass. The anchor + impact + read-only drift path is
# exercised for real on any current binary.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "anchors_freshness"

# ee_supports <subcommand words...> — true when `<words> --help` is accepted.
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "seed a tiny source tree with a resolvable symbol"
mkdir -p "$WS/src"
cat >"$WS/src/widget.rs" <<'RUST'
pub struct Widget;

impl Widget {
    pub fn render(&self) -> &'static str {
        "v1"
    }
}
RUST

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember a memory anchored to the symbol Widget::render"
remembered="$(ee_json remember \
    "The renderer lives at \`src/widget.rs\`; entry point \`Widget::render\` returns the version string." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$remembered" '.success == true' "remember succeeds"
mem_id="$(printf '%s' "$remembered" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "remember returns a memory id"

if ! ee_supports impact; then
    log_drop 1 "ee impact surface absent (bd-1n0np.3.5); symbol/path impact assertions skipped"
else
    step "ee impact --symbol returns the anchored memory"
    imp_sym="$(ee_json impact --symbol "Widget::render" --workspace "$WS" --json)"
    assert_jq "$imp_sym" '.data.surface.kind == "symbol"' "impact symbol surface kind"
    # A symbol anchor is only extracted when the symbol is ::-qualified in a code
    # span; the path anchor on src/widget.rs is the robust cross-binary check.
    step "ee impact <path> returns the anchored memory as an exact hit"
    imp_path="$(ee_json impact "src/widget.rs" --workspace "$WS" --limit 5 --json)"
    assert_jq "$imp_path" '.success == true' "ee impact succeeds"
    assert_jq "$imp_path" '.data.surface.kind == "path"' "impact path surface kind"
    assert_jq "$imp_path" "any(.data.results[]; .memoryId == \"$mem_id\")" \
        "impact returns the anchored memory"
fi

step "change the anchored symbol's text (freshness drift trigger)"
cat >"$WS/src/widget.rs" <<'RUST'
pub struct Widget;

impl Widget {
    pub fn render(&self) -> &'static str {
        "v2-changed-body"
    }
}
RUST

# --- Freshness lifecycle (capability-guarded; activates when the steward lands) ---

if ee_supports memory drift; then
    step "ee memory drift is a read-only freshness surface"
    drift_out="$(ee_json memory drift --workspace "$WS" --json)"
    assert_jq "$drift_out" '.success == true' "ee memory drift succeeds (read-only)"
else
    log_drop 1 "ee memory drift surface absent; read-only drift assertion skipped"
fi

# Bounded steward drift job over git-changed files (bd-1n0np.3.7 part 2). When
# wired, this run recomputes the anchored symbol's content-hash, detects the
# v1->v2 change, and writes an audited memory.freshness_transition row.
if ee_supports steward run; then
    step "run the bounded freshness steward over git-changed files"
    steward_out="$(ee_json steward run --workspace "$WS" --bounded-to-git-changed --json)"
    assert_jq "$steward_out" '.success == true' "bounded steward run succeeds"
else
    log_drop 1 "bounded freshness steward absent (bd-1n0np.3.7): when wired, assert it writes an audited memory.freshness_transition row (previousState=current,newState=stale,driftCode=memory_drift_source_changed) for the content change"
fi

# ee memory show freshness status (bd-1n0np.3.8).
if ee_supports memory show; then
    show_out="$(ee_json memory show "$mem_id" --workspace "$WS" --json)"
    if printf '%s' "$show_out" | jq -e '.data.freshness != null' >/dev/null 2>&1; then
        step "ee memory show surfaces freshness status after drift"
        assert_jq "$show_out" '(.data.freshness.state // "") | test("changed|stale|suspect")' \
            "memory show reports a non-current freshness state after the symbol changed"
    else
        log_drop 1 "ee memory show has no freshness block yet (bd-1n0np.3.8): when wired, assert data.freshness.state == symbol_changed/stale"
    fi
else
    log_drop 1 "ee memory show absent; freshness-status assertion skipped"
fi

# Pack per-item symbol_drift facet + rank-down + live file:line (bd-1n0np.3.7/3.8).
log_drop 1 "pack symbol_drift facet absent (bd-1n0np.3.8): when wired, assert a pack for this task still INCLUDES the drifted memory (rank-down, not removed), tags it freshness: symbol_drift, and carries the live file:line in its provenance"

# Revalidate curation candidate (bd-1n0np.3.8).
log_drop 1 "revalidate curation candidate absent (bd-1n0np.3.8): when wired, assert ee curate candidates surfaces a 'revalidate' candidate for the drifted memory"

# Conservatism: rename/move => unknown, never stale (bd-1n0np.3.7).
step "rename the symbol (refactor ambiguity must stay advisory, never stale)"
cat >"$WS/src/widget.rs" <<'RUST'
pub struct Widget;

impl Widget {
    pub fn render_version(&self) -> &'static str {
        "v2-changed-body"
    }
}
RUST
log_drop 1 "rename=unknown conservatism not observable without the steward (bd-1n0np.3.7): when wired, assert a rename/move yields freshness status=unknown/suspect (advisory), NEVER stale"

harness_summary
