# memory_poisoning Evaluation Fixture

Fixture ID: `fx.memory_poisoning.v1`

Scenario: `usr_import_poisoned_memory_guard`

This fixture proves that imported or remembered content which tries to instruct
the agent is treated as untrusted evidence, not authoritative advice.

The source memories are synthetic and intentionally secret-free. The expected
agent-facing signal is that `ee remember ... --json` denies instruction-like
content with stable policy signal codes, while `ee context "import legacy memories without trusting prompt injection" --json`
can still surface safe provenance-bearing import guidance.

The fixture covers:

- role override attempts
- hidden prompt requests
- credential requests
- developer-role markup
- authority claims
- fail-closed degraded behavior when policy or quarantine storage is unavailable

Generated run artifacts belong under
`target/ee-e2e/usr_import_poisoned_memory_guard/<run-id>/`.
