# RCH Verification Wrapper

Use `scripts/rch_verify.sh` for focused Rust verification in this repository.
It always builds an explicit `rch exec -- env TMPDIR=/tmp ...` invocation and
emits a JSON proof that can be copied into a Beads comment.

Examples:

```bash
scripts/rch_verify.sh --dry-run -- cargo test --lib output::streaming -- --nocapture
scripts/rch_verify.sh -- cargo clippy --all-targets -- -D warnings
scripts/rch_verify.sh -- cargo fmt --check
scripts/rch_verify.sh --bead-id bd-123 --ledger .ee/derived/rch/runs.jsonl --summary -- cargo test --test mesh_off_no_network -- --nocapture
```

Accepted verifier shapes are `cargo check`, `cargo test`, `cargo bench`,
`cargo clippy`, and `cargo fmt --check`. `cargo fmt --check` is accepted for
contract consistency, but current RCH may classify it as non-compilation; its
proof therefore reports `would_offload=false`. Other commands are refused unless
`--allow-raw` is passed. Raw commands still run through explicit `rch exec`, but
the JSON marks `would_offload=false` because RCH may decline non-compilation
commands.

The wrapper sets these remote-safe defaults:

- `RCH_REQUIRE_REMOTE=1`
- `RCH_QUEUE_WHEN_BUSY=1`
- `RCH_COMPRESSION=0`
- RCH binary `/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch` when present, falling back to `rch`
- `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects`
- `RCH_ALIAS_PROJECT_ROOT=/data/projects`
- remote command `TMPDIR=/tmp`
- remote command `CARGO_TARGET_DIR=/tmp/ee-rch-verify-target`

The JSON proof schema is `ee.rch.verify.v1` and includes the command kind,
remote-required flag, planned or actual RCH invocation, worker id when observed,
remote project root, remote target dir, exit code, elapsed time, command hash,
first Rust compiler error location, output tail, source attribution, dirty-state
hashes, degradation codes, and the source-state/worker-state degraded-code
partitions described below.

## Source Attribution Modes

The wrapper has three source-attribution modes. Pick the weakest mode that is
honest for the claim you need to make.

| Mode | Command shape | Runs RCH? | Use when | Proof status to expect |
|---|---|---:|---|---|
| Live checkout | `scripts/rch_verify.sh -- cargo test ...` | yes | The current checkout state is the thing being verified, including any dirty paths. | `verification_attribution=live_dirty_checkout`; may still pass remotely. |
| Strict clean checkout | `scripts/rch_verify.sh --require-clean-tree -- cargo test ...` | only if clean | You need proof that no tracked, Beads, scratch, or unsafe untracked paths influenced the run. | Clean: `strict_clean_tree`; dirty: `source_state_refused` before RCH. |
| Committed tree manifest | `scripts/rch_verify.sh --committed-tree --treeish HEAD -- cargo test ...` | no, currently refuses | You need a deterministic manifest for a committed source tree while the shared checkout is dirty. | `committed_tree_unsupported` until safe source materialization exists. |

Strict clean mode is the closeout mode for "this exact checkout is clean and
remote-verified." Committed-tree mode is currently a source-proof mode, not a
remote execution mode: it resolves the requested treeish, records `git_tree`,
`resolved_commit`, `source_manifest_hash`, `source_manifest_file_count`, and
`source_manifest_byte_count`, then refuses before RCH with
`rch_verify_committed_tree_unsupported`. If `Cargo.toml` contains path
dependencies, the proof also carries
`rch_verify_committed_tree_path_deps_unsupported`; do not reinterpret that as a
successful verification of the live checkout.

Copy-paste examples:

```bash
# Default live-checkout proof. Honest if dirty source is intentionally in scope.
scripts/rch_verify.sh --bead-id bd-XXXX --summary -- \
  cargo test --lib focused_case_name -- --nocapture

# Strict proof. Refuses before RCH if .beads, source, or scratch paths are dirty.
scripts/rch_verify.sh --bead-id bd-XXXX --summary --require-clean-tree -- \
  cargo test --test rch_verify_contract strict_clean_tree -- --nocapture

# Committed-tree source proof. Computes a manifest, then refuses until the
# source materialization protocol exists.
scripts/rch_verify.sh --bead-id bd-XXXX --summary --committed-tree --treeish HEAD -- \
  cargo test --test rch_verify_contract committed_tree_ -- --nocapture
```

These modes never authorize `git worktree`, `git stash`, `git reset`,
`git checkout`, destructive cleanup, or local Cargo fallback. If the proof is
blocked by dirty state, record that state and coordinate; do not "clean it up"
by deleting files.

For bead closeout evidence, pass `--ledger <path>` to append one derived
JSONL row with schema `ee.rch.verify.ledger.v1`, and pass `--summary` to include
a Markdown summary in the proof. Use `--no-write` with `--ledger` when doing a
read-only investigation that should render the proof without writing the
derived ledger.

Ledger rows include `verifier_id`, optional `bead_id`, `command`,
`command_hash`, `started_at`, `completed_at`, `elapsed_ms`, `worker_id`,
`remote_project_root`, `remote_target_dir`, `rch_location`, `exit_code`,
`status`, `first_error_file`, `first_error_line`, `stdout_tail`, `stderr_tail`,
`transcript_path`, `source_state_degraded_codes`, and
`worker_state_degraded_codes`. Retained tails redact private `/Users/<name>`
prefixes and obvious `token=...` / `secret=...` / `password=...` fragments while
preserving remote `/data/projects/...` and local `/Volumes/...` evidence.

`status` is one of `dry_run`, `remote_pass`, `pass_without_remote_marker`,
`remote_failure`, `rch_environment_failure`, `capacity_or_timeout`, or
`refused`, `source_state_refused`, or `committed_tree_unsupported`. Use
`rch_environment_failure` for topology/local-fallback blockers and
`capacity_or_timeout` for worker capacity, timeout, or all-workers-offline
signals; those are not code failures. Use `source_state_refused` for dirty
checkout ambiguity and `committed_tree_unsupported` for an intentionally
non-executing committed-tree proof.

## Degraded-Code Taxonomy

`degraded_codes` remains the complete ordered list of verifier degradations.
Consumers that need to route action should use the two narrower lists:

- `source_state_degraded_codes`: source attribution blockers, such as dirty
  tracked files, Beads metadata churn, scratch artifacts, unsafe untracked paths,
  or committed-tree manifest limitations. These mean the source proof is weaker
  than the closeout claim needs.
- `worker_state_degraded_codes`: RCH worker, topology, capacity, remote-checkout,
  or local-fallback blockers. These mean the same source may verify after the
  worker fleet, root mapping, or queue state is fixed.

The two lists are intentionally disjoint. Generic command failure codes such as
`rch_verify_remote_command_failed` stay only in `degraded_codes`; they provide
overall status context but are not enough by themselves to tell an agent whether
to fix source state or wait for RCH capacity.

These are script proof fields, not new `ee degraded[]` emissions. Do not add
failure-mode catalog fixtures under `tests/fixtures/failure_modes/` only because
a code appears in `source_state_degraded_codes` or
`worker_state_degraded_codes`. Fixture registration is required only if the code
is emitted through an `ee` command response envelope. For the RCH wrapper, the
owned contract is the `ee.rch.verify.v1` proof plus
`tests/rch_verify_contract.rs`.

## Beads and Agent Mail Templates

For Beads comments, paste the summary plus the fields that make attribution
auditable:

```text
RCH proof for <bead>:
- command_hash: <proof.command_hash>
- status: <proof.status>
- verification_attribution: <proof.verification_attribution>
- git_head: <proof.git_head>
- git_tree: <proof.git_tree>
- dirty_status_hash: <proof.dirty_status_hash>
- source_manifest_hash: <proof.source_manifest_hash or none>
- worker_id: <proof.worker_id or none>
- exit_code: <proof.exit_code>
- degraded_codes: <proof.degraded_codes or none>
- source_state_degraded_codes: <proof.source_state_degraded_codes or none>
- worker_state_degraded_codes: <proof.worker_state_degraded_codes or none>
- first_error: <proof.first_error_file>:<proof.first_error_line or none>
```

Agent Mail handoff phrasing should distinguish proof quality:

- `remote_pass` + `strict_clean_tree`: "Committed implementation verified from
  a clean checkout."
- `remote_pass` + `live_dirty_checkout`: "Remote run passed, but attribution is
  live dirty checkout; inspect `dirty_paths_sample` before closing."
- `source_state_refused`: "Code may be implemented, but clean proof is blocked
  by dirty source state."
- `committed_tree_unsupported`: "Committed source manifest was computed, but no
  safe remote materialization exists yet; this is not a remote Cargo pass."

Dirty `.beads/issues.jsonl` is metadata churn, but it still invalidates strict
clean proof because it changes the shared checkout. If the only dirty path is a
Beads export that you own, run `br doctor --json`, then `br sync --flush-only`,
commit the tracker export, and rerun strict mode. If the Beads file is reserved
or contains another agent's updates, coordinate through Agent Mail and use
live-checkout or committed-tree manifest wording rather than claiming a clean
proof.

## Compile-Blocker Routing

Use `scripts/rch_compile_blocker_router.py` when a remote verifier reaches
Cargo and fails on compiler diagnostics. It is read-only: it consumes a
transcript plus optional Agent Mail reservation JSON and emits
`ee.rch.compile_blocker_route.v1` JSON with an Agent Mail-ready Markdown
summary.

Examples:

```bash
scripts/rch_compile_blocker_router.py failed-rch.txt \
  --command "cargo test --lib why_toon_matches_json_contract -- --nocapture" \
  --bead-id bd-123 \
  --agent-name SilentLark \
  --reservations reservations.json \
  --json
```

Routing decisions:

- `self_fix_allowed`: the first diagnostic file is reserved by the current
  agent.
- `reserved_by_other_agent`: an active exact or glob reservation owns the first
  diagnostic file.
- `no_owner_found`: the first diagnostic is local to this repo, but no active
  reservation matched.
- `upstream_dependency_failure`: the first diagnostic is under a sibling
  `/data/projects/...` dependency rather than this repo.
- `environment_failure`: the transcript is an RCH topology/local-fallback
  blocker or lacks an actionable compiler diagnostic.

## Control-Plane Fixtures and E2E

`bd-1h8ji.6` pins the verification control plane with a dedicated fixture
catalog under `tests/fixtures/rch_verify_control_plane/`. Each fixture has
schema `ee.rch.verify_control_plane_fixture.v1`, one `expected_status_class`,
and an exact `summary_markdown` golden. The catalog covers remote Cargo success,
focused test success, Rust compile failure, RCH-E327 topology refusal, worker
capacity waits, daemon timeouts, and local-Cargo hook bypass detection.

Run the focused contract through RCH:

```bash
scripts/rch_verify.sh --bead-id bd-1h8ji.6 --summary -- \
  cargo test --test rch_verify_control_plane -- --nocapture
```

The CI-safe e2e driver is `scripts/e2e_overhaul/rch_verify_control_plane.sh`.
Default mode uses deterministic fake RCH transcripts so it can verify JSON proof
generation, explicit `rch exec` invocation shape, phase logs, and final summary
rows without starting a heavy build:

```bash
scripts/e2e_overhaul/rch_verify_control_plane.sh
```

To run the optional heavyweight lane, opt in explicitly:

```bash
RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 \
  scripts/e2e_overhaul/rch_verify_control_plane.sh
```

The e2e emits one JSONL event per phase with schema `ee.test_event.v1`,
`surface=rch_verification_control_plane`, `phase`, `status`, `elapsed_ms`,
`command_hash`, `worker_id`, and `degraded_codes`. The cleanup phase does not
delete files; it records `status=no_delete_by_policy` and leaves the temporary
proof directory in `/tmp`.

## Runbook Command Example Lint

`scripts/check-rch-doc-examples.py` keeps this page and
`docs/rch_runbook.md` from drifting back to copy-pasteable local Cargo
examples. It scans fenced shell blocks, allows commands wrapped through
`scripts/rch_verify.sh` or explicit `rch exec`, and rejects direct
`cargo build/check/test/bench/clippy` examples that would bypass RCH.
RCH-specific fenced blocks in AGENTS.md and README.md are scanned too.

```bash
python3 scripts/check-rch-doc-examples.py --json
```

The copy-paste smoke harness extracts the first documented dry-run verifier
example and executes that exact docs text. It expects an `ee.rch.verify.v1`
proof with `status=dry_run` and an explicit `rch exec` invocation, so the
default lane never starts local Cargo or a remote build.

```bash
scripts/e2e_overhaul/rch_runbook_docs_smoke.sh
```

Both surfaces emit deterministic machine output. The e2e emits
`ee.test_event.v1` lines with `surface=rch_doc_examples`, `source_file`,
`fenced_block_index`, `normalized_command`, `command_hash`, `phase`,
`status`, `elapsed_ms`, `degraded_codes`, and `first_failure_diagnosis`.
