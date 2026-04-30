# dangerous_cleanup Evaluation Fixture

Fixture ID: `fx.dangerous_cleanup.v1`

Scenario: `usr_pre_cleanup_guard`

This fixture proves that cleanup context includes explicit safety rules before
an agent touches workspace files or generated artifacts.

The source memories are synthetic and intentionally secret-free. The expected
agent-facing signal is that `ee context "clean up generated build artifacts safely" --json`
can surface:

- inspection commands for workspace state
- forbidden destructive cleanup commands
- the requirement for explicit written permission
- provenance for the safety rule and prior near-miss evidence
- stable degraded branches for missing semantic, graph, or policy capability

Generated run artifacts belong under
`target/ee-e2e/usr_pre_cleanup_guard/<run-id>/`.
