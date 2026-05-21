# CUSUM Tuning

`ee` uses a two-sided CUSUM detector to decide when workspace activity has
shifted enough to justify event-driven steward work. The detector is pure and
per-workspace; the steward adapter schedules maintenance only after a
threshold-crossing observation.

## Parameters

| Parameter | Default | Meaning |
|---|---:|---|
| `threshold_h` | `5.0` | Larger values reduce false alarms and increase detection latency. |
| `slack_k` | `0.5` | Larger values filter small gradual drift before it accumulates. |
| `baseline_variance` | `1.0` | Used to normalize observations until a learned baseline is available. |
| `min_observations` | `30` | Fewer observations are treated as an underpowered baseline. |

The update rule normalizes each observation as:

```text
z_t = (x_t - baseline_mean) / sqrt(baseline_variance)
cusum_positive = max(0, cusum_positive + z_t - slack_k)
cusum_negative = max(0, cusum_negative - z_t - slack_k)
```

If either accumulator exceeds `threshold_h`, the event direction is
`increase` or `decrease`, both accumulators are reset, and the steward schedules
event-driven maintenance with reason `cusum_regime_change`.

## Defaults

The defaults target agent-workspace activity signals where a step shift of
roughly three standard deviations should fire within five observations, while
stationary small oscillations remain steady. Lower `threshold_h` values are
appropriate for short-lived debugging sessions; higher values are better for
large shared workspaces where false positives are expensive.

`slack_k` is the main gradual-drift control. With `threshold_h = 2.0`, a
`0.75` normalized drift fires under `slack_k = 0.1` but is filtered by
`slack_k = 0.8`. Use that relationship when tuning for quiet repositories:
raise `slack_k` before raising `threshold_h` if tiny regime wobble is the
problem.

## Failure Signals

Two failure-mode fixtures document the agent-visible CUSUM signals:

| Code | Severity | Meaning |
|---|---|---|
| `cusum_baseline_underpowered` | `info` | Fewer than 30 observations are available for the workspace baseline. |
| `cusum_regime_change_detected` | `warning` | A threshold crossing scheduled steward maintenance. |

The e2e harness at `scripts/e2e_overhaul/cusum_maintenance.sh` records
`ee.test_event.v1` CUSUM events and confirms that the foreground steward daemon
can run the jobs scheduled by a regime-change event.
