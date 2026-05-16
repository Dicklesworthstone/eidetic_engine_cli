# ee fuzz targets

These cargo-fuzz harnesses cover parser and schema surfaces where malformed
agent-generated input should produce structured errors, never panics.

## Graph and insights targets

- `insights_section_dispatch` drives arbitrary section names through
  `ee --json insights --section <name>` and checks success/error envelopes.
- `proximity_arg_parser` exercises clap parsing for
  `ee proximity <memory-a> <memory-b>` without opening a database.
- `ppr_weight_clamp` checks `ee context --ppr-weight=<float>` parsing plus the
  documented effective clamp behavior for finite, infinite, and NaN values.
- `insights_json_decode` decodes arbitrary JSON around the `ee.insights.v1`
  shape and checks generated insights output remains schema-tagged JSON.

## Local usage

Run short, logged sweeps from this directory:

```bash
cargo fuzz run insights_section_dispatch -- -max_total_time=300 -print_final_stats=1
cargo fuzz run proximity_arg_parser -- -max_total_time=300 -print_final_stats=1
cargo fuzz run ppr_weight_clamp -- -max_total_time=300 -print_final_stats=1
cargo fuzz run insights_json_decode -- -max_total_time=300 -print_final_stats=1
```

Nightly or pre-release runs should use `-max_total_time=900`. Keep crash
artifacts and minimization logs with the bead or release evidence so future
agents can reproduce the exact input that failed.

## Deliberate-panic proof

To prove CI is actually running a target, temporarily insert `assert!(false);`
inside the target loop and confirm the cargo-fuzz job fails with the target
name and minimized input in its log. Revert the injected panic before closing
the bead.
