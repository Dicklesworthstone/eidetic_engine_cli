# Inside a 23-hour autonomous LLM swarm: what shipped, what nearly broke, what we'd change

## Intro

This run was a long-form stress test of `ee`, the Eidetic Engine CLI: a local-first Rust memory substrate for coding agents. The swarm worked directly on `main`, in a single shared checkout, under the same constraints human maintainers care about: no worktrees, no local Cargo fallback, no destructive cleanup, and no hand-waving around dirty peer files.

The session started from `93880a3c` and ran for roughly 23 hours. The implementer phase landed 441 commits; after review beads were filed the visible range had grown to 445 commits. The initial shape was five panes, but the effective write force was closer to three steady contributors: p3/LilacRidge on db, models, policy, search, and serve; p4/CalmBay on cass, graph, steward, mesh, and scripts; p5/SwiftKnoll on tests, output, core, and CLI.

The orchestration loop was deliberately blunt. Every 300 seconds the user-orchestrator nudged panes: run `bv --robot-next`, avoid active beads, pick a leaf, reserve files, implement, verify through RCH, close the bead, sync, commit, push. Across about 302 implementer ticks, the orchestrator also handled roughly a dozen stalled-bead recoveries. That boring loop was the product: it kept the swarm from running as five disconnected narratives.

## What shipped

Convergence mattered more than raw commit volume. Parent epics closed instead of accumulating leaves forever. The Agent-first UX overhaul parent `bd-17c65` closed in `10e19355`, after the run had pushed response envelope work, degraded-code taxonomy cleanup, schema exports, agent onboarding docs, verification surfaces, and many smaller CLI affordances. The agent-ergonomics parent `bd-3qs2i` closed in `43764cd0`, after repeated consolidation work around degraded-code docs, failure-mode fixtures, skill validation, changelog coverage, and agent-facing smoke evidence.

Swarm-scale operations also moved from aspiration to explicit surfaces. `bd-1zb7k` closed its swarm-scale rollups in `36bef307`, with surrounding commits covering QoS posture, host calibration, incident drills, worker-pressure reporting, flight recording, completion-audit rollups, and reusable verification posture. The pack delta protocol parent `bd-muovx` closed in `c93adf8c`; its children added the kernel, schemas, and documented fallback modes for prior-pack unknown, unsupported format, and delta larger than full.

The localhost serve v2 line was one of the clearest late-session wins. `bd-knu7t` closed in `434e47d1` after a sequence of small commits: primitives in `09211ab6`, response framing in `229b565b`, transport exchange in `a9009f75`, listener bind in `5f54c068`, single-connection exchange in `e1633e51`, startup report in `6b5531e4`, foreground-once in `b41f5d20`, bind options in `9a21df68`, and status execution in `126eff34`.

The release was not clean, and the scars are visible. `a31708f7` bumped the version to `0.2.0`, then `4e688b80` restored HEAD compilation for the release. The `v0.2.0` tag was re-cut and now points at `52a84df0`, with the tag message explicitly documenting the initial broken target.

## Stalled-Bead Recovery

The orchestration loop mattered most when panes went quiet while still owning work. The user-orchestrator repeatedly treated stale `in_progress` beads as operational state to manage, not social awkwardness to avoid. If a bead had gone silent and recent commits did not show progress, the orchestrator reopened it, reassigned it, or forced a rollup decision.

The clearest example was `bd-3qs2i.8`, the post-merge consolidation bead. It was reopened at `4b5687c5` after being stalled for 108 minutes. LilacRidge then made partial progress and correctly refused to close it when the closure linter failed after 119 seconds. SwiftKnoll tried again, hit the same consolidation class, filed `bd-3qs2i.8.2`, and later closed the bead only after the schema registry gap for `ee.failure_mode_fixture.v1` was fixed in `e919b629`. The hard budget was raised in `96d28454`.

That four-cycle saga is what kept this swarm honest. One agent did not get to declare success because it had spent a long time. The bead stayed open until the artifact, docs, schema registry, and static proof lined up. The drawback was detector fragility: the stalled-bead tooling depended on timestamp parsing that was easy to break on subsecond ISO strings. A better detector would turn "silent for N minutes" into a first-class Beads or Agent Mail query instead of a brittle `jq` pipeline.

## Coordination Tensions

The biggest tension came from shared state. The index, the tracker, and the staging area were all touched by every closeout. Agent Mail reservations helped when they were narrow, but broad reservations on `.beads/issues.jsonl` created a mismatch between protocol and practice. LilacRidge held a disciplined broad reservation; other panes kept doing normal `br close` and `br sync --flush-only` operations because Beads relies on SQLite locking and JSONL atomicity, not reservation enforcement. The orchestrator eventually overrode the reservation for `bd-hdi3l`. That was pragmatic, but it exposed the limit: advisory locks work for source files; they are awkward for high-churn tracker files that every closeout must touch.

The pre-commit hook was the second pressure point. It auto-staged files, which is useful for keeping tracker state attached to commits, but dangerous in a shared checkout. One visible incident was `9fe79ddc`, where a normal feature commit included peer WIP and unrelated deletions; `1349740d` followed to restore the swept work. The same failure mode showed up around plan-file renames and tracker churn: if an agent trusted the index instead of pathspecs, it could commit another pane's state.

The fix was procedural and effective: always inspect `git diff --cached --name-only`, unstage unrelated paths by name, and commit with an explicit pathspec. Late-session prompts started saying it directly: `git commit -m '...' -- <my-paths>`. That small shell habit mattered more than any abstraction. It let the swarm keep committing directly to `main` without branches, worktrees, or stash piles.

RCH was another tension. All Cargo verification was supposed to go through remote compilation, but the remote topology repeatedly failed before Cargo, with all workers failing preflight or refusing local fallback. The practical compromise was the RCH-E327 recipe: make one honest remote attempt, then use static proof (`rustfmt`, `git diff --check`, `jq`, `bash -n`, `py_compile`) while citing `bd-17c65.10.17.1.2` and `.1.4` as remediation. That kept work moving without normalizing local Cargo.

## Review Pass Findings

The review pass filed seven `[REVIEW-*]` beads: two high, four medium, one low. The two high findings were both contract bugs that normal implementation momentum had missed.

SwiftKnoll's p5 review found that context delta was documented as a public `ee context --since <pack-hash> --json` surface, but the CLI never wired a `--since` flag and the context path never called the new delta kernel. The finding is `bd-1es1m`; its evidence points at docs commit `9a6c1046`, fixture commit `8df1dae0`, and the absence of CLI fields in `src/cli/mod.rs`. The companion medium finding, `bd-1h96m`, showed that the Rust delta envelope from `cf8dd5cd` did not match `docs/schemas/ee.context.delta.v1.json`: the schema wanted a response envelope with `data.contextDelta`, while Rust modeled a direct snake_case payload.

CalmBay's p4 review found a budget hole in cooperative graph refresh. `bd-1mgl0` cites `6dde7f533`: PageRank gets a divided sub-budget, but HITS calls `compute_hits_with_cx`, whose internals use the full default background budget. In a scoped-thread refresh, that means one algorithm can overrun the cooperative contract and hold the refresh hostage. CalmBay also filed `bd-2tf9h` against `5a49f955`: the CUSUM maintenance e2e fixture pins daemon foreground output to `ee.response.v1`, even though the agent-facing contract has moved to `ee.response.v2`.

LilacRidge's p3 review found two serve v2 correctness bugs and one search cleanup bug. `bd-da9h1` cites `09211ab6` and `a9009f75`: unknown endpoints are marked auth-free, but the transport auth gate rejects the `"not_required"` state before dispatch reaches the intended 404 branch. `bd-2ysyd` cites `09211ab6`: query percent-decoding casts bytes to `char`, corrupting UTF-8 search and context strings. `bd-2oyx9` cites `7ba3dc44`: the search hot-path Bloom prefilter is computed into `_negation_known_to_prefilter` and then discarded, so diagnostics imply an optimization that does not affect selection.

All review Cargo attempts stayed fail-closed under RCH. The review commits were tracker-only: `1cb6dc66` for p3 findings and `3d8004a0` for p4 plus p5 findings.

## What We'd Change

First, assign domains at minute zero. The swarm eventually converged on p3 for db/models/policy/search/serve, p4 for cass/graph/steward/mesh/scripts, and p5 for tests/output/core/cli. That worked once it was explicit. It should be part of bootstrap, not a convention learned halfway through a crowded checkout.

Second, split file reservations into source reservations and tracker-operation reservations. A source reservation should stay exclusive and respected. A `.beads/issues.jsonl` reservation should either be very short, automatically released after a closeout, or replaced with a Beads-native mutation lease. The JSONL file is a shared ledger, not a feature file.

Third, make stalled-bead detection a productized query. The orchestrator should not have to infer liveness from ad hoc `jq` timestamp parsing. It needs "show me in-progress beads with no commits, comments, or mailbox activity for N minutes," with fractional timestamps handled correctly.

Fourth, make pathspec commits mandatory in swarm mode. The late-session rule should become an enforced hook: if `AGENT_NAME` is set and the index contains paths outside the explicit commit pathspec, fail with a repair message. That single guard would have prevented the peer-WIP sweep class seen around `9fe79ddc` and repaired by `1349740d`.
