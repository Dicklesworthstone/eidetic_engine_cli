# Situation Framing E2E Fixture Matrix

Use this matrix when building the shared project-local skill harness for `situation-framing`.

| Fixture ID | Task Input | Required Evidence | Expected Classification | Required Gate |
|---|---|---|---|---|
| `sf_bug_fix_release` | Fix failing release workflow | `ee status`, `ee context`, `ee search --explain` | `bug_fix` | Prior failure provenance or evidence gap |
| `sf_feature_memory_rule` | Add a new procedural memory feature | `ee status`, `ee context`, `ee capabilities` | `feature` | Durable mutation must be explicit `ee` command |
| `sf_refactor_storage` | Refactor storage path resolution | `ee status`, `ee search --explain` | `refactor` | No direct DB scraping |
| `sf_investigate_slow_search` | Investigate slow search | `ee status`, `ee search --explain`, `ee doctor --fix-plan` | `investigation` | Degraded search must stop ranking claims |
| `sf_docs_update` | Update docs for install flow | `ee status`, `ee context` | `docs` | No unsupported feature-loss claims |
| `sf_deploy_release` | Prepare deploy or release | `ee status`, `ee context`, `ee why` | `deploy` | Risk checks cite provenance IDs |
| `sf_ambiguous_request` | Make it better | `ee status` | `ambiguous` | Output asks for evidence or decision point |
| `sf_missing_evidence` | Explain prior incident with no context | `ee status`, failed `ee search --explain` | `investigation` | `evidenceGaps` names missing evidence |
| `sf_degraded_cli` | Continue when CASS/search is unavailable | degraded `ee.response.v1` | `investigation` | Names degraded code, effect, and repair |

Each fixture log records:

- `skillPath`
- `fixtureId`
- `fixtureHash`
- `taskInputHash`
- `referencedEeCommands`
- `evidenceBundlePath`
- `evidenceBundleHash`
- `degradedCodes`
- `redactionStatus`
- `promptInjectionQuarantineStatus`
- `outputArtifactPath`
- `requiredSectionCheck`
- `firstFailureDiagnosis`
