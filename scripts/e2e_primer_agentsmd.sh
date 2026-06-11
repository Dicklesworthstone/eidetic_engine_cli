#!/usr/bin/env bash
# bd-39tzu.5 — Primer + AGENTS.md bridge end-to-end (real binary, ADR 0065).
#
# Scenario: temp workspace -> seed procedural rules -> primer cold (assert the
# primer_cache_cold degraded entry and per-line provenance refs) -> primer warm
# (assert cacheHit=true and byte-identity modulo the cache flag) -> remember a
# new rule (assert generation invalidation changes the primer) -> export the
# managed block to a scratch file (assert markers, then a backup on the first
# mutation) -> drift diagnostic (assert stale-generation + contradiction
# findings) -> hand-edit inside the block (assert export refuses with
# agentsmd_unmanaged_edit_detected) -> import a fixture AGENTS.md (assert
# candidates with file:// provenance and NO direct memory writes; re-apply
# abstains already_imported).
#
# Every step logs ee.test_event.v1 lines via the shared harness; failing
# asserts print the offending jq filter and the harness summary fails the run.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "primer_agentsmd"

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS
# Every ee command in this flow (init/remember/primer/bridge) resolves the
# database at <workspace>/.ee/ee.db; the harness's EE_DATABASE_PATH redirect
# is not honored by these surfaces, so unset it to keep one database.
unset EE_DATABASE_PATH EE_INDEX_DIR

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "seed a procedural rule"
rule_a="$(ee_json remember \
    "Always run the verify script before pushing changes to main." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$rule_a" '.success == true' "remember rule A succeeds"

step "primer cold run reports primer_cache_cold and renders sections"
primer_cold="$(ee_json primer --workspace "$WS" --json)"
assert_jq "$primer_cold" '.success == true' "primer cold run succeeds"
assert_jq "$primer_cold" \
    '[.data.degraded[].code] | index("primer_cache_cold") != null' \
    "cold run carries primer_cache_cold (info)"
assert_jq "$primer_cold" '[.data.sections[].items[]] | length >= 1' \
    "cold primer renders at least one item"

step "primer warm run is a byte-identical cache hit"
primer_warm="$(ee_json primer --workspace "$WS" --json)"
assert_jq "$primer_warm" '.data.cache_hit == true' "second run is a cache hit"
cold_norm="$(printf '%s' "$primer_cold" | jq -S 'del(.data.cache_hit)')"
warm_norm="$(printf '%s' "$primer_warm" | jq -S 'del(.data.cache_hit)')"
assert_eq "$warm_norm" "$cold_norm" "warm payload is byte-identical modulo cache_hit"

step "primer markdown carries per-line provenance refs"
primer_md="$("$EE_BIN" primer --workspace "$WS" --format markdown 2>/dev/null || true)"
assert_contains "$primer_md" "[mem_" "markdown lines end with short memory refs"

step "a new memory invalidates the primer cache (generation advance)"
rule_b="$(ee_json remember \
    "Never regenerate goldens on a Mac-local checkout for this fixture." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$rule_b" '.success == true' "remember rule B succeeds"
primer_regen="$(ee_json primer --workspace "$WS" --json)"
assert_jq "$primer_regen" '.data.cache_hit == false' \
    "generation advance forces a fresh assembly"
assert_jq "$primer_regen" \
    '[.data.sections[].items[].line] | any(contains("regenerate goldens"))' \
    "fresh primer includes the new rule"

step "export agentsmd creates the managed block in a scratch file"
export_create="$(ee_json export agentsmd --file AGENTS.scratch.md --create \
    --workspace "$WS" --json)"
assert_jq "$export_create" '.data.status == "ok" and .data.created == true' \
    "export --create succeeds"
scratch="$WS/AGENTS.scratch.md"
assert_contains "$(cat "$scratch" 2>/dev/null)" "<!-- ee:agentsmd:begin generation=" \
    "scratch file carries the begin marker"
assert_contains "$(cat "$scratch" 2>/dev/null)" "<!-- ee:agentsmd:end -->" \
    "scratch file carries the end marker"

step "re-export after a memory write backs up before mutating"
rule_c="$(ee_json remember \
    "You MUST sign release tags with the project signing key." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$rule_c" '.success == true' "remember rule C succeeds"
export_update="$(ee_json export agentsmd --file AGENTS.scratch.md \
    --workspace "$WS" --json)"
assert_jq "$export_update" '.data.changed == true' "re-export reports changed"
assert_jq "$export_update" '.data.backupPath != null' "re-export reports a backup path"
assert_eq "$([ -f "$scratch.ee-backup" ] && echo present || echo missing)" "present" \
    "the .ee-backup sibling exists before the first mutation"

step "drift reports stale generation + file-vs-memory contradiction"
# Hand-written contradiction OUTSIDE the markers: Never-vs-Always against rule A.
printf -- '- Never run the verify script before pushing changes to main.\n' >>"$scratch"
# Advance the generation past the exported block.
rule_d="$(ee_json remember \
    "Prefer structured logging over print debugging in this fixture corpus." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$rule_d" '.success == true' "remember rule D succeeds"
drift_out="$(ee_json diag agentsmd-drift --file AGENTS.scratch.md \
    --workspace "$WS" --json)"
assert_jq "$drift_out" '.data.managedBlock.stale == true' \
    "drift flags the stale managed block"
assert_jq "$drift_out" '.data.managedBlock.hashMatches == true' \
    "untampered block hash still matches"
assert_jq "$drift_out" \
    '[.data.contradictions[] | select(.signal == "contradiction_link")] | length >= 1' \
    "drift surfaces the Never-vs-Always contradiction"
assert_jq "$drift_out" '.data.suggestedCommands | length >= 1' \
    "drift carries suggested commands"

step "hand edit inside the block makes export refuse"
# Insert a line just before the end marker (inside the managed block).
python3 - "$scratch" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text = text.replace(
    "<!-- ee:agentsmd:end -->",
    "sneaky hand edit inside the managed block\n<!-- ee:agentsmd:end -->",
    1,
)
open(path, "w").write(text)
PY
export_refused="$(ee_json export agentsmd --file AGENTS.scratch.md \
    --workspace "$WS" --json)"
assert_jq "$export_refused" '.data.status == "refused_unmanaged_edit"' \
    "export refuses the hand-edited block"
assert_jq "$export_refused" \
    '[.data.degraded[].code] | index("agentsmd_unmanaged_edit_detected") != null' \
    "refusal carries agentsmd_unmanaged_edit_detected (warning)"
assert_contains "$(cat "$scratch")" "sneaky hand edit inside the managed block" \
    "refused export leaves the hand edit untouched"

step "import agentsmd proposes candidates with file provenance, never memories"
cp "$REPO_ROOT/tests/fixtures/agentsmd/sample_agents.md" "$WS/AGENTS.md"
memories_before="$(ee_json memory list --workspace "$WS" --json \
    | jq '[.data.memories[]?] | length')"
import_dry="$(ee_json import agentsmd --workspace "$WS" --json)"
assert_jq "$import_dry" '.data.dryRun == true' "import defaults to dry run"
assert_jq "$import_dry" '.data.proposals | length >= 1' "import proposes statements"
assert_jq "$import_dry" \
    '[.data.proposals[].evidence[]] | all(startswith("file://"))' \
    "every proposal carries file:// provenance"
assert_jq "$import_dry" '.data.applied == null' "dry run writes nothing"
import_apply="$(ee_json import agentsmd --apply --workspace "$WS" --json)"
assert_jq "$import_apply" '.data.applied.candidateIds | length >= 1' \
    "apply writes pending curation candidates"
memories_after="$(ee_json memory list --workspace "$WS" --json \
    | jq '[.data.memories[]?] | length')"
assert_eq "$memories_after" "$memories_before" \
    "import writes candidates only, never direct memories"
import_again="$(ee_json import agentsmd --apply --workspace "$WS" --json)"
assert_jq "$import_again" \
    '(.data.applied.candidateIds | length == 0) and ([.data.abstentions[].reason] | all(. == "already_imported"))' \
    "re-apply abstains already_imported and double-inserts nothing"

end_temp_workspace
harness_summary
