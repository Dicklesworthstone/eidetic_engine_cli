# Falsification Checklist

Every hypothesis needs at least one concrete falsification path.

## Hypothesis Types

| Hypothesis | Evidence That Supports It | Evidence That Falsifies Or Weakens It |
|---|---|---|
| Missing memory caused the failure | replay shows the needed memory appeared before the failed decision and changed the decision path | replay includes the memory but the same failure occurs |
| Noisy memory harmed the run | replay with the noisy memory removed avoids the failure | replay without the noisy memory still fails |
| Context pack ordering mattered | replay shows the relevant item was present but below the usable cutoff | replay shows the item was visible before the decision |
| Redaction hid critical evidence | redacted class maps to a field required for the task | replay with redacted-safe substitute still fails |
| Tooling failure dominated | replay evidence shows tool output, not context, caused the failure | replay with same tool result succeeds after context change |
| Prompt-injection-like evidence distorted analysis | quarantine metadata shows hostile instruction-like content near the decision | replay ignores quarantined content and failure still occurs |

## Required Falsification Fields

- hypothesis ID or memo index
- supporting evidence IDs
- falsifying evidence command
- expected schema
- degraded codes that would invalidate the test
- redaction requirement
- stop condition if the test cannot be run

## Refusal Phrase

Use this phrase when the evidence is weak:

```text
I cannot conclude that the run would have succeeded. The available evidence
supports only a hypothesis because replay-supported validation is missing or
contradictory.
```
