# Confounder Checklist

Use this checklist before recommending causal credit adjustments. Each item must
be verified or documented as unaddressed before determining evidence tier.

## Required Checks

### Selection Bias

- [ ] Baseline and candidate groups are comparable
- [ ] No systematic differences in group assignment
- [ ] Missing data patterns are similar across groups
- [ ] Source: `ee causal compare --json` → `baselineDefinition`, `candidateDefinition`

### Confounding Variables

- [ ] Known confounders are identified
- [ ] Confounders are controlled or adjusted for
- [ ] Uncontrolled confounders are documented in output
- [ ] Source: `ee causal estimate --json` → `confounders`, `controlMethod`

### Measurement Error

- [ ] Outcome metric is reliable and stable
- [ ] Measurement method is consistent across groups
- [ ] No systematic measurement bias
- [ ] Source: `ee causal trace --json` → `metricDefinition`, `measurementSource`

### Survivorship Bias

- [ ] Failed cases are represented in the data
- [ ] Dropouts are accounted for or noted
- [ ] Success-only sampling is flagged
- [ ] Source: `ee causal estimate --json` → `sampleComposition`

### Time Confounding

- [ ] Temporal factors are controlled or noted
- [ ] Seasonal effects are considered
- [ ] Trend effects are accounted for
- [ ] Source: `ee causal trace --json` → `timeRange`, `temporalControls`

### Regression to Mean

- [ ] Extreme baseline values are flagged
- [ ] Mean reversion is considered
- [ ] Statistical correction applied if needed
- [ ] Source: `ee causal estimate --json` → `baselineDistribution`

## Checklist Result Interpretation

| Checks Passed | Evidence Tier Cap | Recommendation |
|---------------|-------------------|----------------|
| 0-2 | T1 | Hypothesis only |
| 3-4 | T2 | Review required |
| 5 | T3 | Conditional action |
| 6 | T4+ | Full recommendation (if experiment-backed) |

## Documenting Unchecked Items

For each unchecked item, document:

```yaml
uncheckedConfounders:
  - item: "<checklist item>"
    reason: "<why not checked>"
    impact: "<potential effect on conclusion>"
    mitigation: "<how addressed or why acceptable>"
```

## Special Cases

### Observational Data Only

When no experiment exists, the maximum tier is T3 regardless of checklist
completion. Document:

- All known confounders
- Sensitivity analysis if available
- Explicit assumption that association may not be causal

### Degraded Evidence

When `ee causal` commands return degraded output:

- Flag which checklist items cannot be verified
- Lower the effective tier appropriately
- Document repair commands for the degraded state
