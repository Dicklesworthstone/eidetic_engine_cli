# Trusted team vs untrusted contractor

`ee team` is a **trusted-team** product. Reachability is never
authorization. The table below is the fitness test for whether a
workflow belongs on this surface.

| Question | Trusted teammate (Hana/Priya) | Untrusted contractor |
| --- | --- | --- |
| Join | Invite + live pair-key ceremony + active-member authorizer | Do not invite. Use a separate workspace and export redacted packs. |
| History share | Origin-owned metadata events, receiver-projected stubs | Never. Metadata still names projects and members. |
| Body share | Confirm-gated, hardened cache, durable Body-lane Allow, `exact`/`already_redacted` representations, hash-checked fetch, no filesystem resurrection | Never. Bodies are origin-owned secrets of the team. |
| IdP | Tailnet-attested or secretless OIDC with pinned CA | Do not enroll contractor IdP subjects into the team policy. |
| Daemon / steward | Optional user-scoped service on Unix | Irrelevant; they should not have a route. |
| Windows node | TeamJoin inbound TCP (`ee mesh hello-responder run`); Tailscale LocalAPI WhoIs stays Unix; `HardenedWindows` SID/DACL/reparse adapter compiles, no host soak | Same fail-closed rule; do not weaken storage to onboard them. |

## What "trusted" means here

- The joiner proved the invite and holds a pair key derived from the
  live transcript, not a pre-shared password.
- Origin events are Ed25519-signed. Peers never re-emit origin
  material. Inbound events project to local memory stubs.
- Body fetch is authorized from a durable Body-lane Allow on the
  authenticated peer, not from a path or a cache directory listing.
  Substituted cache bytes stay metadata-only.
- Unshare invalidates retrieval first. Files are not deleted (RULE 1)
  and filesystem presence never makes a body available again.

## What this is not

- Not a contractor portal.
- Not a public mesh.
- Not a replacement for local `ee pack`. After a granted BodyFetch,
  `ee search` / `ee pack --memory-scope team` is how a teammate's
  text is recalled, with `teamProvenance`.
- Not an excuse to store team keys in an ordinary directory on Windows.

If the other party is a contractor, keep them on local `ee` and send a
redacted context pack. Do not run `ee team invite`.
