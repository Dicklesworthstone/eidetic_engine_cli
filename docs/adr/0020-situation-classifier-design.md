# ADR-0020: Situation Classifier Design

## Status

Accepted

## Context

`ee situation classify` reads a task string and labels it with a typed situation
category so retrieval, packing, and steward jobs can route work
situation-aware. The walking-skeleton classifier was a static keyword catalog
that intentionally capped its public confidence below the medium threshold
(`0.49`) — the surface was honest about the fact that single keyword hits are
not real evidence. `eidetic_engine_cli-oofg` raises that bar: it requires that
release-flavored, exploration-flavored, and incident-response-flavored tasks
land their respective categories at or above `0.7` confidence so downstream
retrieval can actually condition on the label, while keeping the
"single keyword is not evidence" honesty rule intact.

Constraints carried forward from the franken-stack mandate:

- No LLM call. The classifier must be deterministic and runnable without
  network or model assets.
- No new dependency. Heuristics use the same keyword-catalog substrate the
  pre-`oofg` classifier used.
- Stable wire output. The serialized `category` strings must remain
  byte-stable; new variants are additive.

## Decision

We taxonomize tasks with thirteen `SituationCategory` variants:
`bug_fix`, `feature`, `refactor`, `investigation`, `documentation`, `testing`,
`configuration`, `deployment`, `release`, `exploration`,
`incident_response`, `review`, `unknown`. The new variants
(`release`, `exploration`, `incident_response`) are split off rather than
collapsed into existing categories so retrieval can distinguish (a) cutting a
version from (b) rolling out infrastructure, and (c) reactive triage of a live
failure from (d) open-ended investigation of a slow-burning question.

Classification is a sum-of-keyword scoring pass over disjoint per-category
keyword lists. The previous classifier's `release` keyword was moved out of
`Deployment` and into `Release`; `Deployment` now owns rollout-flavored tokens
only (`deploy`, `rollout`, `canary`, `staging`, `rollback`, `production`,
`publish`, `ship`).

Confidence is computed as the maximum per-category score, then capped using a
density-aware rule:

- One matching keyword: score is capped at `0.49`, label is forced to `low`.
  Single keywords remain "task-shaped guessing" rather than evidence.
- Two matching keywords with raw score `>= 0.7`: score is capped at `0.85`,
  label is computed from the standard `SituationConfidence::threshold` bands.
- Three or more matching keywords: score is capped at `0.95`, label is
  computed from the standard bands.

Multi-keyword evidence is treated as evidence because three distinct vocabulary
items aligning on the same category cannot be coincidence in any task short
enough to type. The cap remains short of `1.0` so a `verified`-grade label is
reserved for situations that pull persisted evidence (memories, prior
fixtures) into the score, not just heuristics.

The list of available categories is exposed in `SituationCategory::ALL` and
both `SituationCategory::as_str` and `SituationCategory::from_str` accept the
new variants plus their natural aliases (`debug`, `debugging`,
`exploration`, `spike`, `discovery`, `feasibility`, `incident`, `outage`,
`p0`, `p1`, `sev1`, `oncall`, `cut_release`, `version_bump`, etc.).

## Consequences

The classifier now passes the bead acceptance gate (release-flavored task
returns `release` with `>= 0.7` confidence). A worked example:
`"prepare release v0.2.0 changelog and tag"` matches `release`, `changelog`,
`tag` (three signals), so the lift kicks in and the surface returns
`(category: release, confidence_score: ~0.95, confidence: high)`. By contrast
`"ship the change"` matches one keyword and stays at `low`/`<0.5` so the
surface remains honest about the absence of real evidence.

Three follow-on workstreams remain in scope but out of this ADR:

1. Persisted situation records and `situation_links` graph edges (depends on a
   `db/mod.rs` migration that is currently held by a peer agent).
2. `ee situation link <sit-id> <mem-id>` round-trip through the DB.
3. Retrieval boost in `ee context recall` keyed on the matched category. This
   should be a configurable weight rather than a hard filter so a
   misclassified situation cannot suppress otherwise-relevant memories.

## Verification

`tests/situation_persistence.rs` (14 cases) exercises:

- release-flavored task → `release` with `>= 0.7` confidence
- single-keyword task remains `low`/`< 0.5` (honesty rule)
- unknown text → `unknown` with no signals
- ambiguous text exposes alternatives
- incident-response multi-signal task crosses `0.7`
- exploration multi-signal task crosses `0.7`
- refactor two-signal task crosses `0.7`
- new variants round-trip through `as_str` / `from_str`
- new aliases (`incident-response`, `oncall`, `spike`, `cut_release`) parse
- `release` keyword no longer routes to `Deployment`
- pure rollout vocabulary still lands `Deployment` with `>= 0.7`
- `data_json()` carries the new `category` strings byte-for-byte
- `SituationCategory::ALL` includes the new variants
