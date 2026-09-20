#!/usr/bin/env bash
set -euo pipefail
# Hosted only: the local checkout's RCH-only build policy remains unchanged.
test "${GITHUB_ACTIONS:-}" = true
run_group() {
  local filter=$1 expected=$2 label=$3 status=0
  cargo test --locked --lib "$filter" -- --test-threads=2 > "$RUNNER_TEMP/workflow-$label.log" 2>&1 || status=$?
  tail -n 100 "$RUNNER_TEMP/workflow-$label.log"
  test "$status" -eq 0
  if [ "$expected" = any ]; then
    grep -Eq 'test result: ok\. [1-9][0-9]* passed; 0 failed; 0 ignored;' "$RUNNER_TEMP/workflow-$label.log"
  else
    grep -Fq "test result: ok. $expected passed; 0 failed; 0 ignored;" "$RUNNER_TEMP/workflow-$label.log"
  fi
}
check_source() {
  local label=$1
  cargo check --locked --all-targets > "$RUNNER_TEMP/workflow-$label-check.log" 2>&1 || { tail -n 100 "$RUNNER_TEMP/workflow-$label-check.log"; return 1; }
  cargo clippy --locked --lib -- -D warnings > "$RUNNER_TEMP/workflow-$label-clippy.log" 2>&1 || { tail -n 100 "$RUNNER_TEMP/workflow-$label-clippy.log"; return 1; }
  tail -n 20 "$RUNNER_TEMP/workflow-$label-check.log"
  tail -n 20 "$RUNNER_TEMP/workflow-$label-clippy.log"
  git diff --check
}
publish() {
  local label=$1 message=$2 details=$3
  shift 3
  if ! git diff --quiet -- "$@"; then
    local base
    base=$(git rev-parse HEAD)
    git add -- "$@"
    git commit -m "$message" -m "$details"
    for attempt in 1 2 3; do
      if git push origin HEAD:main; then break; fi
      test "$attempt" -lt 3
      git fetch origin main
      git diff --exit-code "$base" origin/main -- "$@"
      git merge --no-edit origin/main
      base=$(git rev-parse origin/main)
    done
  fi
  git rev-parse HEAD > "$RUNNER_TEMP/workflow-$label-source-sha.log"
}
git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
first=(src/core/backup.rs src/core/backup_evidence_export.rs src/core/jsonl_import.rs src/core/jsonl_recovery.rs src/core/memory.rs src/models/jsonl.rs src/output/jsonl_export.rs src/core/jsonl_workflow_tests.rs)
if ! grep -Fq 'mod workflow_tests;' src/core/jsonl_import.rs; then
  git apply --check scripts/delivery/workflow-recovery-20260920.patch
  git apply scripts/delivery/workflow-recovery-20260920.patch
fi
if ! grep -Fq 'pub(crate) fn is_recovery_identity_alias' src/output/jsonl_export.rs; then
  git apply --check scripts/delivery/workflow-alias-20260920.patch
  git apply scripts/delivery/workflow-alias-20260920.patch
fi
rustfmt --edition 2024 --config skip_children=true "${first[@]}"
run_group core::jsonl_import::workflow_tests 6 membership
run_group core::jsonl_import::recovery::tests any publication
run_group core::backup::evidence_export::tests 6 identities
run_group core::jsonl_import::typed_fields_tests any typed-fields
check_source membership
publish membership 'fix(recovery): preserve workflow membership across memory export and import' 'Carry optional workflow IDs through backup capture, validation, redaction, import, duplicate checks and the source-bound recovery fence. Share the exact fixed-point identity grammar with task/baseline history without exempting arbitrary key_ strings. Six workflow tests plus recovery, identity and typed-field controls, all-target checking and production Clippy passed before this commit.' "${first[@]}"

second=(src/core/backup.rs src/core/backup_workflow_recovery_tests.rs src/db/mod.rs)
patch=scripts/delivery/workflow-completion-20260920.patch
if ! grep -Fq 'mod workflow_recovery_tests;' src/core/backup.rs; then
  git apply --include=src/core/backup.rs --check "$patch"
  git apply --include=src/core/backup.rs "$patch"
fi
rustfmt --edition 2024 --config skip_children=true src/core/backup.rs src/core/backup_workflow_recovery_tests.rs
if ! grep -Fq 'The lifecycle change, audit and reindex request form one durable' src/db/mod.rs; then
  status=0
  cargo test --locked --lib core::backup::workflow_recovery_tests::workflow_completion_queues_only_promoted_memories_and_repeated_close_is_a_noop -- --exact > "$RUNNER_TEMP/workflow-before-completion.log" 2>&1 || status=$?
  tail -n 60 "$RUNNER_TEMP/workflow-before-completion.log"
  test "$status" -ne 0
  grep -Fq 'running 1 test' "$RUNNER_TEMP/workflow-before-completion.log"
  grep -Fq 'promotion must enqueue one index job per eligible memory' "$RUNNER_TEMP/workflow-before-completion.log"
  grep -Fq 'test result: FAILED. 0 passed; 1 failed;' "$RUNNER_TEMP/workflow-before-completion.log"
  git apply --include=src/db/mod.rs --check "$patch"
  git apply --include=src/db/mod.rs "$patch"
fi
rustfmt --edition 2024 --config skip_children=true "${second[@]}"
run_group core::backup::workflow_recovery_tests 4 lifecycle
run_group core::jsonl_import::workflow_tests 6 membership-after-completion
check_source completion
publish completion 'fix(workflow): atomically schedule retrieval updates when completing memories' 'Enqueue one durable single-document index job in the same transaction as each workflow promotion and audit. Prove the previous missing-job behavior fails, then pass real queue/idempotence and second-row rollback controls. Exercise two independent backup/restore generations across four privacy levels through scoped recall, why and public workflow completion; reject membership corruption at both publication fences without changing row counts. Four new no-mock tests, membership controls, all-target checking and production Clippy passed before this commit.' "${second[@]}"
git archive HEAD | gzip > "$RUNNER_TEMP/workflow-source.tar.gz"
