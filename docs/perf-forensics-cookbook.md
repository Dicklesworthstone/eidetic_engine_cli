# Performance Forensics Cookbook

Operator and agent workflow for differential performance forensics using `ee perf`.

## Overview

Performance forensics compares normalized artifact summaries to detect regressions
without re-running benchmarks. This workflow complements benchmark generation (`fcq1`),
host profiling (`k8dp`), and support bundles — it does not replace them.

**Read-only by design:** All `ee perf` commands are read-only. They inspect existing
artifacts and produce comparison reports without mutating workspace state.

## Workflow

```
1. Capture baseline  →  2. Run new bench  →  3. Compare  →  4. Triage
```

---

## Step 1: Capture Baseline

A baseline is any normalized artifact summary (`ee.perf.artifact_summary.v1`). Sources:

| Source | How to capture |
|--------|----------------|
| Prior benchmark run | Save the JSON output from `ee bench smoke --json` |
| Support bundle | Extract from `bundle.zip/perf/artifact_summary.json` |
| Golden fixture | Use existing `tests/fixtures/golden/perf_artifact/*.json` |

Example baseline artifact structure:

```json
{
  "schema": "ee.perf.artifact_summary.v1",
  "artifactId": "baseline-smoke-001",
  "artifactKind": "benchmark_report",
  "profile": {"profileName": "workstation", "confidence": "high"},
  "metrics": {
    "elapsed_ms": {"kind": "measured", "value": 100.0, "unit": "ms"},
    "rss_bytes": {"kind": "measured", "value": 10485760.0, "unit": "bytes"}
  },
  "degraded": [],
  "redaction": "clean"
}
```

Save your baseline:

```bash
ee bench smoke --json > baseline.json
```

---

## Step 2: Run Candidate Benchmark

After code changes, run the same benchmark to produce a candidate artifact:

```bash
ee bench smoke --json > candidate.json
```

Ensure both artifacts:
- Use the same profile (constrained/portable/workstation/swarm)
- Target the same command family (pack, search, context, etc.)
- Are captured on comparable hardware or note the profile mismatch

---

## Step 3: Compare Artifacts

### Basic comparison

```bash
ee perf compare --baseline baseline.json --candidate candidate.json --json
```

### Interpreting results

The `summary.result` field indicates:

| Result | Meaning |
|--------|---------|
| `unchanged` | No significant regression detected |
| `regressed` | One or more metrics exceed threshold |
| `improved` | Candidate is faster (informational) |

Example regression output:

```json
{
  "schema": "ee.response.v1",
  "success": true,
  "data": {
    "report": {
      "summary": {
        "result": "regressed",
        "comparableMetricCount": 2
      },
      "deltas": [
        {
          "metric": "elapsed_ms",
          "baseline": 100.0,
          "candidate": 150.0,
          "delta": 50.0,
          "percentChange": 50.0,
          "severity": "warning"
        }
      ]
    }
  }
}
```

### Profile mismatch detection

When profiles differ, the comparison flags it:

```bash
# Comparing swarm artifact against constrained profile
ee perf compare --baseline constrained.json --candidate swarm.json --json
```

Output includes `data.report.degraded` with mismatch codes:

```json
{
  "degraded": [
    {"code": "profile_mismatch", "message": "Artifact profiles differ: constrained vs swarm"}
  ]
}
```

---

## Step 4: Budget Check

Verify a single artifact against a profile budget:

```bash
ee perf budget check --profile workstation --report artifact.json --json
```

### Available profiles

| Profile | Use case |
|---------|----------|
| `constrained` | CI runners, containers (<2 cores, <8 GiB) |
| `portable` | Laptops (2-5 cores, 8-15 GiB) |
| `workstation` | Desktops (6-11 cores, 16-31 GiB) |
| `swarm` | Build servers (12+ cores, 32+ GiB) |

### Budget verdict

```json
{
  "data": {
    "report": {
      "summary": {
        "result": "passed",
        "comparableMetricCount": 4
      },
      "requestedProfile": "constrained",
      "artifact": {"profile": "constrained"}
    }
  }
}
```

---

## Triage Findings

### Severity bands

| Severity | Action |
|----------|--------|
| `info` | No action needed |
| `warning` | Investigate; may indicate real regression |
| `error` | Significant regression; block merge |
| `critical` | Major regression; escalate immediately |

### Decision tree

```
regression detected?
├── no  → merge safe
└── yes → check severity
    ├── info/warning → investigate, may proceed with justification
    └── error/critical → do NOT merge
        ├── profile mismatch? → re-run on matching hardware
        ├── noisy metric? → check variance, add more samples
        └── real regression? → fix code or file regression issue
```

### When to escalate

| Condition | Escalation |
|-----------|------------|
| Unknown regression cause | Run `ee explain-performance` |
| Need profiler data | Attach `perf record` or flamegraph |
| Need full bundle | Generate `ee support bundle` |
| Cross-service impact | File regression issue with owner hints |

---

## Troubleshooting

### Degradation codes

| Code | Meaning | Next command |
|------|---------|--------------|
| `profile_mismatch` | Baseline and candidate profiles differ | Re-run benchmark on matching hardware |
| `missing_metric` | Metric present in baseline but absent in candidate | Check benchmark configuration |
| `hash_mismatch` | Content hash differs from observed hash | Re-run benchmark or check artifact integrity |
| `redacted_metric` | Metric was redacted for privacy | Use `--redaction=none` if authorized |
| `non_comparable` | Metric kinds differ (timing vs count) | Verify artifact schema versions match |

### Common scenarios

**Scenario: CI shows regression but laptop doesn't**

```bash
# Check profiles
jq '.profile.profileName' baseline.json candidate.json
# => If CI=constrained and laptop=workstation, profiles differ
# Fix: Run both benchmarks on same profile tier
```

**Scenario: Flaky regressions**

```bash
# Compare multiple runs to check variance
for i in 1 2 3; do ee bench smoke --json > "run_$i.json"; done
# Compare each pair and check if delta is consistent
```

**Scenario: Metric missing in candidate**

```bash
# Check metric availability
jq '.metrics | keys' baseline.json candidate.json
# If candidate is missing metrics, check benchmark flags
```

---

## Examples by Host Type

### Laptop (portable)

```bash
# Capture baseline
ee bench smoke --json > baseline.json

# After changes
ee bench smoke --json > candidate.json

# Compare
ee perf compare --baseline baseline.json --candidate candidate.json --json | \
  jq '.data.report.summary'

# Check budget
ee perf budget check --profile portable --report candidate.json --json
```

### CI Runner (constrained)

```bash
# Force constrained profile budgets
ee perf budget check --profile constrained --report ci_artifact.json --json

# Flag regressions in CI
ee perf compare --baseline golden/baseline.json --candidate ./candidate.json --json | \
  jq -e '.data.report.summary.result != "regressed"' || exit 1
```

### Build Server (swarm)

```bash
# Compare support bundle artifacts
ee perf compare \
  --baseline bundle_baseline/perf/summary.json \
  --candidate bundle_candidate/perf/summary.json \
  --json

# Swarm budgets are more generous
ee perf budget check --profile swarm --report swarm_artifact.json --json
```

---

## Reference

### Related commands

| Command | Purpose |
|---------|---------|
| `ee perf compare` | Compare two artifact summaries |
| `ee perf budget check` | Check artifact against profile budget |
| `ee bench smoke` | Generate benchmark artifact |
| `ee profile config plan` | View current profile configuration |
| `ee support bundle` | Generate full support bundle |

### Tested scenarios

These scenarios are covered by automated tests:

- `tests/e2e_perf_compare.rs`: regression detected, no regression, missing files, malformed JSON
- `tests/perf_budget_conformance.rs`: all 4 profiles, mismatch detection, error handling
- `tests/perf_artifact_contract.rs`: schema stability, serialization contracts

### Schemas

| Schema | Description |
|--------|-------------|
| `ee.perf.artifact_summary.v1` | Normalized performance artifact |
| `ee.response.v1` | Standard response envelope |
| `ee.error.v2` | Error response envelope |
