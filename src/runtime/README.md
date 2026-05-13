# Runtime Determinism

N4.2 introduces `ee::runtime::determinism`, a small capability-token
substrate for code that must not consume ambient wall-clock state,
filesystem order, map iteration order, or random bytes accidentally.

The token is constructed at an entry point from an explicit seed, stable
workspace seed material, a timestamp truncated to second precision, or an
already-read environment value. It is move-only and not `Sync`; callers split
child scopes explicitly with `Deterministic::child(label)` before handing a
deterministic stream to another subsystem.

This module does not yet thread the token through retrieval, scoring, MMR, or
pack assembly. That mechanical integration is owned by N4.3. The initial design
is pinned to the N4.1 inventory hash:

`blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc`
