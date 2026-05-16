# Lean4 Proofs

This directory holds Lean4 proof artifacts for load-bearing `ee` invariants.

`pack_determinism.lean` models the context pack assembly boundary as a pure
transformation from canonical inputs to a content hash. The first theorem is a
small executable scaffold, not the final Rust-equivalence proof: it gives
`ee verify proofs` a real Lean artifact to discover and check while the
mechanized model is expanded.

