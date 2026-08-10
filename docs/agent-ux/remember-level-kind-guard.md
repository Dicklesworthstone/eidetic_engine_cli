# Remember Level/Kind Cross-Wiring Guard

`ee remember` carries two adjacent taxonomies: `--level`
(`working`, `episodic`, `semantic`, `procedural`) and `--kind`
(canonical: `rule`, `fact`, `decision`, `failure`, `command`, `convention`,
`anti-pattern`, `risk`, `playbook-step`, plus free-form custom kinds).
Because kinds are free-form, passing a level token as `--kind` used to be
accepted silently, freezing a level name into the kind column and polluting
the retrieval filters the canonical set keeps portable.

The guard (bd-remember-level-kind-validation-zau2l) rejects exactly the
cross-wired tokens with did-you-mean guidance. It matches the **normalized
token exactly** — trim/case (and `_`/`-` unification for kinds) — and never
by prefix. Custom kinds that merely resemble a level name stay accepted and
continue through the established `MemoryKind` canonicalization contract
(trimmed lowercase kebab-case); the guard adds no new normalization.

## Behavior

| Input | Outcome |
|---|---|
| `--kind episodic` (any level token) | `remember_kind_is_level` usage error, exit 1 |
| `--level rule` (any canonical kind token) | `remember_level_is_kind` usage error, exit 1 |
| `--kind EpisodicNote` (custom lookalike) | accepted, stored in the established canonical form `episodic-note` |
| `--kind Episodic`, `--level " decision "` | normalized before matching, then rejected |
| both flags cross-wired | `remember_kind_is_level` wins deterministically |

The guard is implemented in the shared remember core
(`remember_level_kind_cross_wire_error` in `src/core/memory.rs`). Entry guards
run before idempotency replay, reinforcement, global-store bootstrap, and ID
allocation; the preparation-layer guard remains as defense in depth. The
real-binary contracts cover `ee remember`, `ee note`, and `--global`; focused
core regressions cover keyed replay, reinforcement, batch isolation, and
seeded ID allocation. The serve and daemon callers use the same guarded core
boundary.

## Machine-facing error shape (`ee.error.v2`)

```bash
ee remember "auth retry works" --kind episodic --json
```

```json
{
  "schema": "ee.error.v2",
  "error": {
    "code": "remember_kind_is_level",
    "message": "`episodic` is a memory level, not a kind — did you mean `--level episodic`? ...",
    "severity": "low",
    "repair": "ee remember \"<content>\" --level episodic --kind <kind> --json",
    "repairKind": "template",
    "details": {
      "failureModeCode": "remember_kind_is_level",
      "argument": "--kind",
      "provided": "episodic",
      "providedTruncated": false,
      "didYouMean": { "argument": "--level", "value": "episodic" },
      "memoryLevels": ["working", "episodic", "semantic", "procedural"],
      "canonicalKinds": ["rule", "fact", "decision", "failure", "command",
                         "convention", "anti-pattern", "risk", "playbook-step"],
      "recovery": [{
        "priority": 1,
        "kind": "flag",
        "rationale": "Move the recognized level token to --level and choose the intended memory kind.",
        "riskClass": "mutating_local_repair",
        "requiresHumanApproval": false,
        "mutatesExternalState": false,
        "mutatesTrackerState": false,
        "privacyClass": "bounded_command_no_raw_state",
        "flagName": "--level",
        "valueHint": "episodic",
        "example": "ee remember \"<content>\" --level episodic --kind <kind> --json",
        "resultsIn": "The request is validated with separate level and kind taxonomies."
      }]
    }
  }
}
```

The inverse direction is symmetric:

```bash
ee remember "auth retry works" --level rule --json
# -> error.code = "remember_level_is_kind"
# -> error.details.didYouMean = { "argument": "--kind", "value": "rule" }
```

Custom kinds keep working, including level-prefixed ones. Their existing
canonicalization remains in force:

```bash
ee remember "auth retry works" --kind EpisodicNote --json
# -> exit 0, data.kind = "episodic-note" (canonical custom-kind form)
```

Agents should key on `error.code` and `error.details.didYouMean` — the
`provided` field echoes at most 128 UTF-8 bytes of the offending token and
sets `providedTruncated: true` when bounded, while
`memoryLevels` / `canonicalKinds` enumerate the valid vocabulary for
self-repair without a second round trip. Exit code is 1 (usage) in both
directions; the JSON envelope is written to stdout.
