# EE_* environment variables

This file documents every `EE_*` environment variable honored by `ee`.
The source of truth in code is `src/config/env_registry.rs`; update both the
registry and this table when adding a new variable.

`ee capabilities --json` exposes the same registry through
`data.envOverrides[]`. Sensitive variables may report that they are set, but
must not expose their current value.

| Name | Category | Type | Default | Controls | Notes |
|---|---|---|---|---|---|
| `EE_AGENT_MODE` | output | boolean flag | none | Use agent-oriented output defaults. | Optimizes renderer auto-detection for agent consumption. |
| `EE_CASS_BINARY` | integration | absolute path | none | Override the trusted cass import binary path. | Used before config and trusted PATH lookup for CASS import discovery. |
| `EE_DATABASE_PATH` | paths | path | none | Override the configured storage database path. | Equivalent to overriding the storage database path in config. |
| `EE_DEMO_EVIDENCE_ROOT` | paths | path | none | Override the demo evidence storage root. | Used by demo evidence capture surfaces. |
| `EE_EXPERIMENTAL_TRIAD` | output | boolean flag | none | Enable experimental ee pack/note/why aliases. | Gates the agent-triad spike surface. |
| `EE_FORMAT` | output | output format | none | Select the default output renderer. | Lower-priority compatibility alias for output format selection. |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | tuning | integer seconds | none | Override the harmful feedback burst window in seconds. | Overrides feedback policy timing from config. |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | tuning | integer count | none | Override the harmful feedback rate limit per source. | Overrides feedback rate limits from config. |
| `EE_HOOK_MODE` | output | boolean flag | none | Use hook-oriented machine output defaults. | Optimizes renderer auto-detection for hook protocols. |
| `EE_INDEX_DIR` | paths | path | none | Override the configured search index directory. | Equivalent to overriding the storage index directory in config. |
| `EE_JSON` | output | boolean flag | none | Request JSON output from renderer auto-detection. | Prefer explicit `--json` for scripts when possible. |
| `EE_LOG_FORMAT` | diagnostics | enum | none | Select structured log format. | `json` selects structured command-start logs on stderr. |
| `EE_LOG_JSON` | diagnostics | boolean flag | none | Enable JSON command-start logs on stderr. | Shortcut for JSON command logging. |
| `EE_MAX_TOKENS` | tuning | integer tokens | none | Override the default context pack token budget. | Applies when a command does not pass an explicit token budget. |
| `EE_NO_COLOR` | output | boolean flag | none | Disable colored diagnostics. | Mirrors the behavior of `NO_COLOR` for ee-specific control. |
| `EE_OUTPUT_FORMAT` | output | output format | none | Select the default output renderer. | Highest-priority environment output format selector. |
| `EE_PREFLIGHT_BYPASS_SECRET` | policy | secret string | none | Supply preflight bypass secret material. | Capabilities must never expose this current value. |
| `EE_PROFILE` | tuning | profile name | none | Override the default context pack profile. | Applies when pack/context profile is not specified explicitly. |
| `EE_REMEMBER_CURATION_SYNC_BUDGET_MS` | tuning | integer milliseconds | `50` | Override remember-time curation sync budget in milliseconds. | Registry-defined default is used when unset. |
| `EE_SECURITY_PROFILE` | policy | profile name | none | Select security profile. | Controls policy posture for security-sensitive operations. |
| `EE_TEST_LOG_LEVEL` | diagnostics | enum | none | Control structured test-log verbosity. | Used by the J1 structured E2E logging harness. |
| `EE_TEST_LOG_PATH` | diagnostics | path | none | Enable structured test logging at this JSONL path. | Used by Rust and shell E2E logging helpers. |
| `EE_TEST_LOG_TEST_ID` | diagnostics | string | none | Name the active structured test-log scenario. | Identifies events emitted by the test logging harness. |
| `EE_WORKSPACE` | paths | path | none | Override workspace root discovery. | Used after explicit `--workspace` and before cwd walk-up. |
| `EE_WORKSPACE_REGISTRY` | paths | path | none | Override the workspace alias registry database path. | Controls where workspace aliases are stored. |

