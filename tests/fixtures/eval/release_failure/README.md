# release_failure Evaluation Fixture

Fixture ID: `fx.release_failure.v1`

Scenario: `usr_pre_task_brief`

This fixture proves that release preparation context includes concrete prior
failure evidence before an agent edits, commits, or pushes release work.

The source memories are synthetic and intentionally secret-free. The expected
agent-facing signal is that `ee context "prepare release" --json` can surface:

- the exact cargo verification commands
- the prior release workflow failure mode
- provenance for both memories
- stable degraded branches for missing semantic or graph capability

Generated run artifacts belong under
`target/ee-e2e/usr_pre_task_brief/<run-id>/`.
