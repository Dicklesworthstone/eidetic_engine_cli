# Real-shape CASS oracle corpus

bd-reality-core-convergence-1azkt.43. Authored (never copied) Claude Code and
Codex transcripts in the exact on-disk record envelopes the agents write, so
ee's import, projection, retrieval, pack and learn paths are measured on what
they meet in the field rather than on clean one-line fixtures.

Regenerate (byte-identical for the same seed):

    python3 scripts/real_shape_corpus/generate.py --out /tmp/real_shape_cass --digest

Scale variants (never committed) are seeded recombinations of the authored core:

    python3 scripts/real_shape_corpus/generate.py --out /tmp/rs5k --target-records 5000

Measure an `ee` binary against it (report mode, see `probe.py --help`):

    python3 scripts/real_shape_corpus/probe.py --ee target/debug/ee [--target-records 5000]

The probe serves the corpus through `scripts/real_shape_corpus/cass_fixture.py`,
a `cass` emulator for the `sessions`/`view`/`index`/`health` robot commands. Its
log lines are prefixed `stub`; a gate that must run real `cass` cannot mistake it.

## Layout

| Path | Contents |
| --- | --- |
| `home/.claude/projects/<cwd-slug>/<uuid>.jsonl` | Claude Code sessions |
| `home/.codex/sessions/2026/09/<dd>/rollout-*.jsonl` | Codex sessions |
| `manifest.json` | sessions (path, agent, timestamps, record counts) and label locators (`path` + 1-based `line`) |
| `judgments.json` | retrieval queries (graded relevant labels, distractors, calibration/evaluation split), negative queries, learn acceptance phrases, must-not-propose markers, admission ground truth |

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
