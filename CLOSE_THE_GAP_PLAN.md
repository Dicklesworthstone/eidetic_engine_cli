# CLOSE_THE_GAP_PLAN — `ee` (Eidetic Engine CLI) — **PART III (2026-08-17)**

> Mesh / team-confederation honesty closeout after the Unix EE-to-EE campaign.
>
> **Status: ACTIVE (Part III).** Parts I and II (2026-05) are archived at
> `docs/archive/close_the_gap_2026-05.md`. This file is the in-place Part III
> revision required by `AGENTS.md` *Reality-Check Cadence*. Do not create a
> second plan file at the repo root.
>
> Companion: `docs/adr/0086-team-memory-confederation.md`,
> `docs/mesh/team_confederation_plan.md`,
> `docs/mesh/verification_matrix.md`, `README.md`.

---

## 0. Premise

The 2026-08-17 mesh-campaign reality check found an inverted tracker, not an
unbuilt product:

- Unix live EE-to-EE works on `main`: create/invite/join, inbound listen,
  `TcpMeshForegroundSyncTransport` EventFetch + grant-gated BodyFetch,
  hydrate, `--memory-scope team` search/pack, `teamProvenance`, P4.4/P4.5,
  US-5 last-sync/reachability.
- README and `docs/mesh/real_tailscale_smoke.md` still said the production
  supervisor used a no-op transport. That claim is false as of this Part III
  honesty edit.
- Beads still showed ~52 open `bd-tc-epic-qzk7o.*` children. Most of those
  slices are shipped. Open-count was being misread as "not built."
- Two-human Tailscale, Windows-host soak, production IdP vendor soak, T2.7
  frame/session fuzz beyond origin properties, and the T5.7 publication fence
  are **real remainders**. They are not an excuse to rebuild transport.

**Non-negotiables for Part III:**

- Do not rebuild shipped Unix team-confed.
- Do not steal `bd-d67os.28` (NavyLotus; T5.7 fence).
- Do not start `bd-1nl13`.
- Do not invent a T6.7 ceremony. `.7.7` waits for the remainder children.
- Do not close the epic until the remainder children close.
- ADR 0086 Context stays historical (2026-07-30). Correct the plan and
  README, not the ADR's original problem statement.
- No file deletion. No worktrees. No local Cargo on this Mac.

---

## 1. What is already true

Unix product on `main` (proof ledger:
`docs/mesh/verification_matrix.md`):

| Surface | State |
| --- | --- |
| `ee team create` / `invite` / `join` | Live signed TCP; join first-sync imports origin genesis; invite `--wait` waits for it |
| Inbound listen | `ee mesh hello-responder run` / `ee daemon --foreground`; Tailscale LocalAPI or loopback `TeamJoinLocalApi` |
| Foreground sync | `TcpMeshForegroundSyncTransport` — not `Noop` |
| Unified recall | Authorized BodyFetch hydrates stubs; search/pack/ask/why carry `teamProvenance` |
| Conflicts / insights / why | P4.4 precedence, T5.6 `peerConflicts`, P4.2 elevation, T5.8 origin-time invariance |
| Status | US-5 `lastSeenAt` + reachability (`self` / `never_synced` / `synced` / `soft_stale` / `hard_stale`) |
| Fake IdP + identity_attest | T7.1–T7.6 proven against the fake harness |
| Windows inbound compile | `x86_64-pc-windows-gnu --lib` compiles; TeamJoin TCP is not Unix-gated |

---

## 2. What is still a gap

| Gap | Bead | Close when |
| --- | --- | --- |
| Two distinct humans on a real Tailscale tailnet exchange memory; US-4 search/pack works; cursors advance; no deferred sync code | `bd-tc-epic-qzk7o.3.8` (T2.6) | Opt-in smoke on a real tailnet with two `ee` processes, not this local-surface script alone |
| Frame/session/bootstrap fuzz beyond `tests/property_origin_stream.rs` | `bd-tc-epic-qzk7o.3.9` (T2.7) | Fuzz targets or properties cover frame v2, session, bootstrap; origin slice already landed |
| Source-snapshot publication fence | `bd-d67os.28` then `.6.7` | NavyLotus closes the fence; `.6.7` reuses it. Isolated protocol tests (2026-08-17) are not fence close |
| Windows-host DACL / inbound crash / owner-only key-path | `bd-tc-epic-qzk7o.12` + `.2.4` | Runtime soak on a Windows host, not only cross-compile |
| Production Entra / Okta / Google IdP soak | `bd-tc-epic-qzk7o.8.8` | Vendor device-flow against a real secretless public client |
| Program closeout | `bd-tc-epic-qzk7o.7.7` (T6.7) | After the rows above; verification-matrix rollup only |

---

## 3. Tracker policy for this bridge

1. Close a shipped `bd-tc-epic-qzk7o.*` child only with verification-matrix
   evidence (test name + isolated host + duration + commit). No abstention
   close. No "docs-only" close of an implements-surface bead.
2. Split environment remainders into explicit children instead of leaving
   fifty implementation beads open.
3. Keep `.3.8`, `.3.9`, `.2.4`, `.6.7`, `.7.7`, `.12`, `.8.8`, the
   milestone parents that still have remainder children, and the epic open.
4. Unblock `.2.4` from "blocked" once T5.9's body-approval consumer is on
   `main`. Remaining work is Windows key-path, not missing Unix crypto.
5. Comment `.6.7` that protocol tests passed and the fence stays
   `bd-d67os.28`.
6. After README honesty lands, close `.2.5` (T1.7). That bead existed to
   stop README from lying about mesh.

---

## 4. Docs honesty landed in this Part III opening

- `README.md` Mesh / Team / Limitations / FAQ now describe live Unix
  `TcpMeshForegroundSyncTransport` and name the remainders.
- `docs/mesh/real_tailscale_smoke.md` no longer claims a no-op transport.
- `docs/mesh/operator_onboarding.md` points at `ee team` and the ledger.
- `docs/mesh/verification_matrix.md` has an explicit remainder table.
- `docs/mesh/team_confederation_plan.md` status line matches `main`.
- ADR 0086 historical Context is **not** rewritten.

---

## 5. Close criteria for Part III

Archive this file to `docs/archive/close_the_gap_2026-08.md` and start
Part IV **in this same path** only when:

- `.3.8` has a two-human Tailscale proof artifact.
- `.12` has a Windows-host soak artifact (or an explicit fail-closed
  product decision recorded in the matrix).
- `.8.8` has a production IdP soak artifact (or an explicit "fake-IdP is
  the v1 ceiling" decision).
- `.3.9` either grows the remaining fuzz or is narrowed and closed with
  the origin-slice evidence plus a filed follow-up.
- `bd-d67os.28` closes and `.6.7` reuses the fence (or `.6.7` is rewritten
  as honesty-only with a new implements-surface sibling).
- `.7.7` can then write the T6.7 rollup without inventing ceremony.
- README / smoke / matrix still match the code.

Until then, this file stays at the repo root.
