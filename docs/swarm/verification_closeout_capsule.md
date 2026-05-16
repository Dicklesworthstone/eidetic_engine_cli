# ee.verification.closeout_capsule.v1

`ee.verification.closeout_capsule.v1` is the compact evidence shape for Beads comments, Agent Mail updates, and support bundles. It was introduced by `bd-1zb7k.15.3` to make closeout proof consistent without copying raw command output.

Required uses:

- Summarize the command, source hash, execution substrate, result, pass/fail counts, artifact references, caveats, and support-bundle metadata.
- Redact local paths and raw output by default.
- Preserve failure-mode codes when evidence is incomplete, local execution is disallowed, or source hashes no longer match.

Non-goals:

- It is not a substitute for the underlying `ee.verification.run.v1` record.
- It does not run verification commands or infer success without retained evidence.
- It does not include raw stdout, stderr, local paths, mail bodies, or secret-bearing logs.
