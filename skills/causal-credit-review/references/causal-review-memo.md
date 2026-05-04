# Causal Review Memo Template

Use this template when writing causal credit review memos. All sections are
required unless marked optional.

## Header

```yaml
schema: ee.skill.causal_credit_review.v1
reviewQuestion: "<the specific question being analyzed>"
workspace: "<workspace path>"
generatedAt: "<ISO 8601 timestamp>"
reviewer: "<skill or agent identifier>"
```

## Evidence Summary

```yaml
evidenceBundle:
  path: "<bundle file path>"
  hash: "<SHA-256 hash of bundle>"
  redactionStatus: passed | failed | unknown
  trustClass: "<trust class from bundle>"
  createdAt: "<bundle creation time>"
```

## Mechanical Measurements

This section contains only facts from `ee causal` command JSON. No interpretation.

```yaml
mechanicalMeasurements:
  traceAvailable: <true/false>
  estimateAvailable: <true/false>
  compareAvailable: <true/false>
  promotePlanAvailable: <true/false>
  sampleSize: <integer>
  upliftEstimate: <float or null>
  confidenceInterval: "<lower, upper>"
  pValue: <float or null>
  effectSize: <float or null>
  evidenceIds:
    - "<evidence ID from ee causal>"
  degradedCodes:
    - "<any degraded codes>"
```

## Confounder Assessment

```yaml
confounderAssessment:
  confoundersIdentified:
    - "<confounder 1>"
    - "<confounder 2>"
  confoundersControlled: true | false | partial
  controlMethod: "<experimental design, statistical adjustment, or none>"
  sensitivityAnalysis: present | missing | not_applicable
  checklistPassed: <number of 6>
  uncheckedItems:
    - item: "<checklist item>"
      reason: "<why not verified>"
```

## Evidence Tier Determination

```yaml
evidenceTier:
  tier: T0 | T1 | T2 | T3 | T4 | T5
  reason: "<why this tier was assigned>"
  upgradePathway: "<what evidence would raise the tier>"
  degradedAdjustment: "<if tier was lowered due to degraded state>"
```

## Agent Interpretation

This section contains skill judgment. Clearly labeled as interpretation.

```yaml
agentInterpretation:
  associationStrength: strong | moderate | weak | none
  causalPlausibility: supported | plausible | weak | not_established
  confidenceLevel: high | medium | low | refused
  rationale: |
    <Evidence-linked explanation of the interpretation.
    Reference specific evidence IDs and measurements.
    State assumptions explicitly.>
```

## Recommendation

```yaml
recommendation:
  action: promote | demote | reroute | hold | refuse
  targetIds:
    - "<memory or artifact ID>"
  actionGated: true | false
  gateReason: "<why action is gated, or n/a>"
  conditions:
    - "<condition for action, if any>"
```

## Risk Assessment

```yaml
risks:
  - risk: "<potential downside of recommendation>"
    likelihood: high | medium | low
    mitigation: "<how to address the risk>"
```

## Assumptions (Optional if none)

```yaml
assumptions:
  - "<assumption required for interpretation>"
```

## Unsupported Claims (Optional if none)

```yaml
unsupportedClaims:
  - claim: "<claim that cannot be made>"
    reason: "<why not supported by evidence>"
```

## Degraded State (Optional if none)

```yaml
degradedState:
  - code: "<degraded code from ee>"
    effect: "<what cannot be concluded>"
    repair: "<explicit repair command>"
```

## Follow-Up Commands

```yaml
recommendedFollowUpCommands:
  - "<ee command with full args>"
```

## Wording Constraints

- Use `associated with` not `caused`
- Use `suggests` not `proves`
- Use `may improve` not `will improve`
- Reference evidence IDs in rationale
- State confidence level explicitly
- Flag degraded state prominently
