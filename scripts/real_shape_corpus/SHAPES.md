# Real-shape CASS oracle corpus

bd-reality-core-convergence-1azkt.43. Authored (never copied) Claude Code and
Codex transcripts in the exact on-disk record envelopes the agents write, so
ee's import, projection, retrieval, pack and learn paths are measured on what
they meet in the field rather than on clean one-line fixtures.

Regenerate (byte-identical for the same seed):

    python3 scripts/real_shape_corpus/generate.py --out /tmp/real_shape_cass --digest

Scale variants are seeded recombinations of the authored core:

    python3 scripts/real_shape_corpus/generate.py --out /tmp/rs5k --target-records 5000

THIS LINE SAID "(never committed)" UNTIL 2026-10-22, and the committed corpus is
now a scale variant: `tests/fixtures/real_shape_cass` holds the `--target-records
4000` expansion (308 sessions, 4009 records, digest 7bcedd76…). The bead that
owns this corpus asks for "at least 12 sessions, roughly 3-6k records in total"
under that path, and the authored core alone is 14 sessions and 123 records --
two orders of magnitude short of the 4,919-span scale at which the original
probe measured the retrieval slowdown. See `tests/fixtures/real_shape_cass/SHAPES.md`
for the provenance of the committed tree. Regenerating the core alone still
works and is still the right thing for reviewing CONTENT; the committed variant
is what measurements run against.

Measure an `ee` binary against it (report mode, see `probe.py --help`):

    python3 scripts/real_shape_corpus/probe.py --ee target/debug/ee [--target-records 5000]

By default the probe serves the corpus through `scripts/real_shape_corpus/cass_fixture.py`,
a `cass` emulator for the `sessions`/`view`/`index`/`health` robot commands. Its
log lines are prefixed `stub`; a gate that must run real `cass` cannot mistake it.

To measure the real connector, point the probe at a verified CASS 0.8.0
executable and supply its SHA-256. The executable must be named `cass` and
its absolute path must satisfy EE's import policy. For the Linux amd64
executable extracted from the published `cass-linux-amd64.tar.gz`:

    python3 scripts/real_shape_corpus/probe.py --ee /path/to/ee \
        --real-cass /path/to/cass \
        --cass-sha256 97ad59c2abb5442a0bccf883d43695c1424f455c5c882b7b72bb3231cc3c2e98 \
        --target-records 4000 --enforce --out /tmp/ee-corpus-report

The probe indexes a copy of the generated Claude and Codex transcripts in
a private home and checks that CASS discovers each expected source. EE
imports from that same index. The summary records `cass_mode`, executable
identity and the indexer's observations. Message counts come from CASS.
Grading retains the original JSONL line coordinates for both connectors.

## Layout

| Path | Contents |
| --- | --- |
| `home/.claude/projects/<cwd-slug>/<uuid>.jsonl` | Claude Code sessions |
| `home/.codex/sessions/2026/09/<dd>/rollout-*.jsonl` | Codex sessions |
| `manifest.json` | v2 sessions (path, agent, scenario, source session identity, timestamps, labeled/unlabeled record counts), original-core label locators and all exact label instances |
| `judgments.json` | retrieval queries (graded relevant labels, distractors, calibration/evaluation split), negative queries, learn acceptance phrases, must-not-propose markers, admission ground truth |

## Concept and record identity

The generator now writes `ee.real_shape_cass.manifest.v2` and
`ee.real_shape_cass.judgments.v2`. The checked-in fixture remains the historical
v1 corpus. For the same seed, workspace and target count, v2 preserves every
transcript path and byte, including source UUIDs, timestamps and inserted noise.
The metadata changes deliberately change the whole-tree digest. The regeneration
test compares transcript bytes against the historical fixture and compares two
fresh v2 digests for determinism; it does not treat a v1 metadata digest as a v2
compatibility requirement.

`labels` retains one locator per concept for the authored core.
`label_instances` maps that same stable concept label to every exact occurrence
in the core and its seeded variants. Each locator contains `path`, 1-based
`line`, `slug`, `session_id`, `record_id` and `core`. A record ID is the
source session UUID followed by `:` and the 1-based line, so it is unique and
deterministic even for Codex records without their own UUID. Crate/version
substitutions affect payloads only; they never rename concept labels.

Each session records its base `scenario` slug, plus `labeled_record_count` and
`unlabeled_record_count`. A scenario connects related sessions; it does not make
every turn in those sessions relevant. Inserted build noise, metadata, adjacent
turns and other records without an authored label stay unlabeled.

Relevance is query-specific: a concept counts only when the query lists it in
`relevant`. Distractor checks use the query's `distractors` plus the preserved
unrelated-chatter labels `distract_espresso` and `distract_lunch`. Other concepts
and unlabeled records do not inherit relevance from a nearby answer or the
session's scenario. Exact record matching distinguishes the intended answer
from a neighboring irrelevant turn; scenario coverage remains a separate
diagnostic.

Scale variants expand the exact records eligible to match a concept. Each
query's recall denominator is still its distinct relevant concepts, and matching
several occurrences of one concept earns credit once. Pack query coverage
counts queries with at least one exact relevant match, so repeated answers to
one task cannot stand in for missing tasks. Original-only `recall_at_5` remains
a diagnostic; `equivalent_recall_at_5` names the expanded matching contract.
Historical original-only and exact-equivalent scores must not be conflated.

`generate.validate()` rejects duplicate or conflicting source identities,
invalid or missing locators, missing variant concepts, and inconsistent
labeled/unlabeled counts. It also checks query references and rejects concepts
listed as both relevant and distractors. Session and instance ordering do not
change identity.

## Record shapes (mirrors Claude Code 2.0.x, Codex CLI 0.46)

Claude Code, one JSON object per line:

| `type` | Required fields | `message.content` |
| --- | --- | --- |
| `user` (prompt) | `parentUuid`, `isSidechain`, `userType`, `cwd`, `sessionId`, `version`, `gitBranch`, `type`, `message`, `uuid`, `timestamp` (+ `promptId` on the first) | string |
| `user` (tool result) | as above + `toolUseResult` | `[{"type":"tool_result","tool_use_id","content","is_error"?}]` |
| `assistant` | as `user` + `requestId`; `message` carries `id`, `model`, `stop_reason`, `usage` | blocks: `text{text}`, `thinking{thinking,signature}`, `tool_use{id,name,input}` |
| `summary` | `type`, `summary`, `leafUuid` | — |

Sidechain records set `isSidechain: true`.

Codex rollout, one JSON object per line: `session_meta{payload:{id,cwd,cli_version,git}}`,
`turn_context`, then `response_item` payloads of type `message`
(`input_text` / `output_text` blocks), `reasoning` (`summary_text`,
`encrypted_content`), `function_call{name,arguments,call_id}` and
`function_call_output{call_id,output}` (output is a JSON string with
`output` and `metadata.exit_code`).

`generate.validate()` checks every record against this table; the probe
refuses a corpus that fails it, so an envelope-stripped ("clean") corpus can
never pass as real-shape.

## Scenario classes

failure -> diagnosis -> fix -> green arcs (crates.io publish, clippy under
`-D warnings`, a clock-dependent flaky test, a NOT NULL migration, rustc
E0502, a shared pytest fixture, Docker layer caching); user-stated rules; a
decision with alternatives; long build/test noise; secret-shaped strings
(redaction bait); prompt-injection text in a fetched README and in a user
turn (must quarantine); benign documentation mentions of `curl` and
`rm -rf target/` (false-positive bait); unrelated office chatter (pack
distractors); a sidechain subagent search.
