# ee.verification.reuse_advisory.v1

`ee.verification.reuse_advisory.v1` is the advisory result over existing verification-run evidence. It was introduced by `bd-1zb7k.15.2` so agents can decide whether to cite a recent equivalent run, wait for in-flight evidence, import missing logs, or rerun through the required remote path.

Required uses:

- Compare source, command, execution substrate, feature profile, generation, and strictness flags before recommending reuse.
- Emit a clear status such as `reusable_pass`, `stale_source`, `in_flight`, or `missing_evidence`.
- Include recovery actions that preserve the repository's remote-only verification rules.

Non-goals:

- It does not kill, pause, throttle, or schedule another agent's process.
- It does not mutate Beads, Agent Mail, source files, or verification records.
- It does not bless stale or mismatched evidence as closeout proof.
