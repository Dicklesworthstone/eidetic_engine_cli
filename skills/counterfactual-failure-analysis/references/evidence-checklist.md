# Evidence Checklist

Use this checklist before writing a counterfactual failure-analysis memo.

## Required Inputs

- `ee status --workspace <workspace> --json`
- `ee lab capture --workspace <workspace> --json`
- `ee lab replay --workspace <workspace> --episode-id <episode-id> --json`
- `ee lab counterfactual --workspace <workspace> --episode-id <episode-id> --json`
- Optional wrapped bundle: `ee.skill_evidence_bundle.v1`

## Evidence Fields

- workspace path matches the requested workspace
- command argv is recorded
- schema names are present
- episode ID and run ID are present
- provenance IDs are present
- content hashes are present
- evidence bundle path and hash are present
- redaction status is `passed`
- `rawSecretsIncluded=false`
- redaction classes are listed
- trust class is listed
- degraded codes and repairs are preserved
- prompt-injection quarantine status is present
- durable mutation status is explicit
- direct DB scraping is forbidden

## Stop Conditions

- no capture evidence
- no replay evidence for a replay-supported claim
- malformed JSON
- missing provenance or hashes
- redaction failed or unknown
- `rawSecretsIncluded=true`
- prompt-injection-like evidence is not quarantined
- degraded `ee lab` output invalidates the requested conclusion
- stale bundle without a degraded code
- direct DB, `.ee/`, `.beads/`, index, or CASS scraping is required

## Pack-Diff Rule

Pack diffs can support only this class of statement:

```text
The counterfactual context changed in these named ways.
```

Pack diffs cannot support this class of statement without replay evidence and
validation:

```text
The agent would have succeeded.
```
