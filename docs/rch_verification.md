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
first Rust compiler error location, output tail, and degradation codes.

For bead closeout evidence, pass `--ledger <path>` to append one derived
JSONL row with schema `ee.rch.verify.ledger.v1`, and pass `--summary` to include
a Markdown summary in the proof. Use `--no-write` with `--ledger` when doing a
read-only investigation that should render the proof without writing the
derived ledger.

Ledger rows include `verifier_id`, optional `bead_id`, `command`,
`command_hash`, `started_at`, `completed_at`, `elapsed_ms`, `worker_id`,
`remote_project_root`, `remote_target_dir`, `rch_location`, `exit_code`,
`status`, `first_error_file`, `first_error_line`, `stdout_tail`, `stderr_tail`,
and `transcript_path`. Retained tails redact private `/Users/<name>` prefixes and
obvious `token=...` / `secret=...` / `password=...` fragments while preserving
remote `/data/projects/...` and local `/Volumes/...` evidence.

`status` is one of `dry_run`, `remote_pass`, `pass_without_remote_marker`,
`remote_failure`, `rch_environment_failure`, `capacity_or_timeout`, or
`refused`. Use `rch_environment_failure` for topology/local-fallback blockers
and `capacity_or_timeout` for worker capacity, timeout, or all-workers-offline
signals; those are not code failures.

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
