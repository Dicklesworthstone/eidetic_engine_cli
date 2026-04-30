# Specificity Fixture Corpus

These fixtures pin the deterministic curation specificity contract for
EE-CURATE-SPEC-001.

Positive fixtures contain concrete commands, paths, metrics, branches, or
provenance that should score above the default `[curation].specificity_min`
threshold of `0.45`.

Negative fixtures are intentionally generic, adversarial, or concrete-looking
without enough trustworthy structure. They should stay below the threshold or be
rejected by secondary instruction-like checks.
