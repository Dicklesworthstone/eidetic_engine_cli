# Evidence-Tier Rubric

This rubric determines what recommendations are allowed based on the quality and
quantity of causal evidence from `ee causal` commands.

## Tier Definitions

### T0: No Evidence

- **Sample Size:** 0
- **Confounder Status:** Unknown
- **Allowed Actions:** Refuse recommendation; suggest evidence gathering
- **Next Step:** `ee causal trace --workspace <workspace> --json`

### T1: Observation Only

- **Sample Size:** 1-9
- **Confounder Status:** Not assessed
- **Allowed Actions:** Hypothesis formation only; no action recommendation
- **Next Step:** Gather more observations; consider experiment design

### T2: Underpowered

- **Sample Size:** 10-29
- **Confounder Status:** Known but not controlled
- **Allowed Actions:** Recommend review or experiment; no direct action
- **Next Step:** `ee learn experiment propose --workspace <workspace> --json`

### T3: Associational

- **Sample Size:** 30+
- **Confounder Status:** Known confounders documented
- **Allowed Actions:** Conditional recommendation with explicit caveats
- **Gating:** Must document assumptions and confounder limitations

### T4: Experiment-Backed

- **Sample Size:** 30+
- **Confounder Status:** Controlled via experiment design
- **Allowed Actions:** Full recommendation with standard review
- **Gating:** Experiment evidence must be present in bundle

### T5: Replicated

- **Sample Size:** 30+
- **Confounder Status:** Controlled and independently replicated
- **Allowed Actions:** High-confidence recommendation; expedited review
- **Gating:** Replication evidence must be documented

## Tier Upgrade Pathways

| From | To | Required |
|------|----|----------|
| T0 | T1 | At least 1 observation via `ee causal trace` |
| T1 | T2 | 10+ observations with documented confounders |
| T2 | T3 | 30+ observations OR sensitivity analysis |
| T3 | T4 | Experiment with controlled confounders |
| T4 | T5 | Independent replication of experiment |

## Recommendation Matrix

| Tier | Promote | Demote | Reroute | Hold | Experiment |
|------|---------|--------|---------|------|------------|
| T0 | ❌ | ❌ | ❌ | ✅ | ✅ |
| T1 | ❌ | ❌ | ❌ | ✅ | ✅ |
| T2 | ❌ | ❌ | ❌ | ✅ | ✅ |
| T3 | ⚠️ | ⚠️ | ⚠️ | ✅ | ✅ |
| T4 | ✅ | ✅ | ✅ | ✅ | Optional |
| T5 | ✅ | ✅ | ✅ | ✅ | Not needed |

Legend: ✅ = Allowed, ⚠️ = Conditional with caveats, ❌ = Not allowed

## Degraded Evidence Handling

When `ee causal` commands return degraded output:

1. Note the degraded code in output
2. Lower the effective tier by one level
3. Document the repair command
4. Refuse recommendations that require the unavailable data
