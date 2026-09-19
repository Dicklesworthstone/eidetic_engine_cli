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

## Writing `expected_query_match`: the analyzer does not stem, and one case is unexplained

`expected_query_match` DECLARES the retrieval workload — `tests/eval_run_happy_path.rs`
builds the executed query set as the union across memories, so a query whose
terms are absent from the indexed surface retrieves nothing and fails with
`executed an empty retrieval for "<query>"`.

The indexed surface is **content + tags (as both title and tags) + level +
kind**, and **the analyzer does not stem**: a corpus saying `memory` does not
match a query saying `memories`. Three of the four empty retrievals recorded on
2026-09-16 were plural-form queries against singular corpus text.

**Anchor new queries on terms that appear VERBATIM in the memory they should
retrieve.** That holds under any tokenization and does not depend on a model of
the analyzer.

**UNEXPLAINED, and worth knowing before you trust a mental model of the
tokenizer:** `"instruction-like content"` retrieved EMPTY against
`mem_00000000000000000000000402`, yet the word `instruction` appears verbatim in
sibling memory `...403` ("the highest priority instruction"), and the query
`"role markup"` — written as two words — *does* retrieve `...403`, whose only
source of those terms is the hyphenated tag `role-markup`. A model where hyphens
split into tokens predicts `instruction-like` should have matched. It did not.

That contradiction is unresolved. It was worked around in `38e541973` by
replacing the query with `"ignore previous instructions"`, whose three terms are
all verbatim in the target memory, rather than by guessing the rule. If you are
about to reason about hyphens, stemming, or token boundaries here, measure it
first — this is the one place in these fixtures where the obvious model is known
to be wrong.
