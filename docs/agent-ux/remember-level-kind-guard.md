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
by prefix: custom kinds that merely resemble a level name stay accepted
byte-for-byte.

## Behavior

| Input | Outcome |
|---|---|
| `--kind episodic` (any level token) | `remember_kind_is_level` usage error, exit 1 |
| `--level rule` (any canonical kind token) | `remember_level_is_kind` usage error, exit 1 |
| `--kind episodic-note` (custom lookalike) | accepted unchanged, stored byte-for-byte |
| `--kind Episodic`, `--level " decision "` | normalized before matching, then rejected |
| both flags cross-wired | `remember_kind_is_level` wins deterministically |

The guard runs in the shared remember core, so the single write path, `ee
note`, `--batch` lines, and the serve surface (HTTP 400) all behave
identically.

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
      "didYouMean": { "argument": "--level", "value": "episodic" },
      "memoryLevels": ["working", "episodic", "semantic", "procedural"],
      "canonicalKinds": ["rule", "fact", "decision", "failure", "command",
                         "convention", "anti-pattern", "risk", "playbook-step"],
      "recovery": []
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

Custom kinds keep working, including level-prefixed ones:

```bash
ee remember "auth retry works" --kind episodic-note --json
# -> exit 0, data.kind = "episodic-note" (byte-for-byte)
```

Agents should key on `error.code` and `error.details.didYouMean` — the
`provided` field echoes the offending token exactly as given, and
`memoryLevels` / `canonicalKinds` enumerate the valid vocabulary for
self-repair without a second round trip. Exit code is 1 (usage) in both
directions; the JSON envelope is written to stdout.
