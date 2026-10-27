# Persistent candidate-pool defaults

`ee config set pack.candidate_pool 24` writes the workspace default to
`.ee/config.toml`. `ee config get pack.candidate_pool` reports the merged value
and its source. `ee config set pack.candidate_pool 24 --dry-run` previews the
write without creating or changing a file. An explicit `--config PATH` chooses
another configuration file, including `~/.config/ee/config.toml` for the user
layer.

For `ee pack`, `ee context`, and query-file packs, precedence is:

1. Explicit `--candidate-pool N`.
2. Query-file `budget.candidatePool`, when using a query file.
3. An applicable task-lens candidate-pool override.
4. Workspace configuration, then user configuration.
5. The built-in configuration default, **100**.

An explicit `--candidate-pool 100` is an override, not a signal to use the
configuration default. Runtime operating-profile caps still apply after this
resolution, so the effective request pool can be smaller than the configured
value. This setting controls the candidates considered for packing, not the
number guaranteed to appear in the final token-budgeted pack.

Both TOML configuration and `config set` require an integer from **1** through
**4294967295**. Invalid values are rejected without changing the config file.
Existing unrelated settings and comments are preserved.

Regression coverage lives in `config_candidate_pool_tests`,
`context_candidate_pool_tests`, `cli::candidate_pool_tests`, and the public
command test `tests/pack_candidate_pool_cli.py`.

## Relevance admission before packing

The candidate pool is a retrieval budget, not a request to fill the pack with
every retrieved item. With no `--relevance-floor` override, packing checks
uncalibrated local candidates for a dominant gap in the query's relevance
scores after the candidate sources join and before pagination. A clearly
separated weak tail is excluded even when the token budget has room for it.

The cutoff is computed separately for each retrieval score source. It requires
at least three candidates, a gap larger than half the total score spread, and
a gap at least one quarter of the best score. Equal scores stay together;
small, flat, and smoothly declining populations are retained. For example,
scores `0.3968, 0.3968, 0.1936, 0.1906, 0.1877` retain the first two items,
while `0.90, 0.70, 0.50, 0.30` have no dominant gap and stay eligible.

Required warning context, admitted focus items, explicit graph seeds, and
candidates from independently normalized stores are excluded from the fit
and retain their existing admission rules. Calibrated hits also retain their
existing semantics. Memory, procedural rule, and directly retrieved evidence
candidates participate under their own typed identities.

When the heuristic removes candidates, `context_filtered_results` reports
the source-specific cutoff and excluded counts with `heuristic_uncalibrated`.
This is a query-relative selection heuristic, not a probability calibration
or a false-admission guarantee. Memory omissions remain explainable through
the pack's why-not report. The policy version also enters the pack cache key,
so an older cached response cannot bypass the new admission step.

An explicit `--relevance-floor FLOAT` takes precedence over this heuristic.
Use `--relevance-floor 0` to disable adaptive admission; ordinary query,
policy, token-budget, and duplicate filters still apply.

## Prioritize context for an active bead

Pass `--bead-id ID` to use an existing task in the addressed workspace's
`.beads/issues.jsonl` as an additional ranking signal:

```sh
ee context "prepare the release" --bead-id bd-release --json
ee pack "prepare the release" --bead-id bd-release --read-only --json
ee pack build --query-file task.eeq.json --bead-id bd-release --json
ee why-not mem_example --task "prepare the release" --bead-id bd-release --json
```

The shared pack path compares the bead's normalized labels, title, and
description with already-admitted local memory tags and content. The existing
affinity policy adds at most **0.05** to a memory's ranking score before final
sorting and relevance admission. It never admits a denied source, adds tracker
prose to the query, or changes native rule, evidence, or nonlocal candidate
scores. Family-link inference is not enabled. Omitting the flag preserves the
ordinary ranking and response shape.

JSON responses expose `data.pack.beadAffinity`, including applied counts,
bounded per-memory explanations, and `determinismKey.beadContextHash`. The
identity binds the selected bead's normalized inputs rather than unrelated
tracker history. Raw title, description, labels, and matched tokens are not
copied into the response. `basePackHash` remains null because a separate
unbiased pack was not assembled.

Missing, malformed, oversized, or symlinked exports leave ranking unbiased
and report the existing bead-affinity diagnostic codes. Reads are capped at
64 MiB. Requests carrying a bead ID bypass the persistent pack cache so a
tracker edit cannot replay an earlier ranking. Candidate pagination in a query
document and `--cursor` envelope continuation are rejected with bead affinity;
build a single pack or omit the bead selector for paginated requests.
