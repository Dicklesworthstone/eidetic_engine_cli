# TLA+ Proofs

This directory holds TLA+ protocol models for cross-agent coordination rules.

`agent_mail_coordination.tla` models the file-reservation lifecycle at the
level needed for the safety property `NoOverlap`: an exclusive reservation may
not be granted when another active exclusive reservation overlaps the same
path. The model is intentionally small so it can run as a non-blocking proof
stage when TLC is installed.

