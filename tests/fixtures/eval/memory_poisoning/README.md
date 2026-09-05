# memory_poisoning Evaluation Fixture

Fixture ID: `fx.memory_poisoning.v1`

Scenario: `usr_import_poisoned_memory_guard`

This fixture checks that stored instruction overrides cannot enter authoritative
context packs, while safe guidance retains provenance.

The source memories are synthetic and intentionally secret-free. The expected
agent-facing signal is that `ee remember ... --json` preserves the evidence,
and `ee pack "import legacy memories without trusting prompt injection" --json`
excludes both overrides with `excluded_by_policy` omissions and a
`context_filtered_results` explanation. `ee why` still exposes the original
stored evidence for inspection. The public regression in
`tests/trust_freshness_e2e.rs` executes the fixture command sequence and checks
the actual selected and omitted memory IDs.

The original fixture claimed ingestion exit 7, quarantine-storage failures,
and fields that the public CLI did not implement. It also used obsolete
arguments and invalid provenance anchors. Those claims were not verified
capabilities. The corrected contract tests pack admission; it does not claim
ingestion rejection, durable quarantine, or enforcement of shell commands.

The fixture covers:

- role override attempts
- hidden prompt requests
- credential requests
- developer-role markup
- authority claims
- safe guidance under degraded lexical retrieval
- focus and graph/global fan-in cannot grant stored text instruction authority

Quoted or negated override phrases are conservatively omitted because the
substring detector cannot determine intent. Ordinary risk memories about
dangerous commands remain usable. This is not comprehensive injection detection.

Generated run artifacts belong under
`target/ee-e2e/usr_import_poisoned_memory_guard/<run-id>/`.
