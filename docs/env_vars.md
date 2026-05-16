# EE_* environment variables

This file documents every `EE_*` environment variable honored by `ee`.
The source of truth in code is `src/config/env_registry.rs`; update both the
registry and this table when adding a new variable.

`ee capabilities --json` exposes the same registry through
`data.envOverrides[]`. Sensitive variables may report that they are set, but
must not expose their current value.

| Name | Category | Type | Default | Controls | Notes |
|---|---|---|---|---|---|
| `EE_AGENT_NAME` | output | string | none | Identify the current agent for scoped memory retrieval. | Used by agent-aware memory and context surfaces. |
| `EE_AGENT_MODE` | output | boolean flag | none | Use agent-oriented output defaults. | Optimizes renderer auto-detection for agent consumption. |
| `EE_CASS_BINARY` | integration | absolute path | none | Override the trusted cass import binary path. | Used before config and trusted PATH lookup for CASS import discovery. |
| `EE_DATABASE_PATH` | paths | path | none | Override the configured storage database path. | Equivalent to overriding the storage database path in config. |
| `EE_DEMO_EVIDENCE_ROOT` | paths | path | none | Override the demo evidence storage root. | Used by demo evidence capture surfaces. |
| `EE_DIAG_FORCE_CAPABILITY_GAP` | diagnostics | comma-separated tokens | none | Force selected capability probes to report build-gap diagnostics. | Diagnostics-only fixture control; accepts `runtime`, `storage`, `search`, `graph`, `science`, or `all`. |
| `EE_DISABLE_TOON` | output | boolean flag | none | Disable TOON output capability reporting and auto-selection. | Forces TOON capability diagnostics to report unavailable and makes renderer auto-detection fall back to JSON. |
| `EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS` | tuning | boolean flag | none | Disable Frankensearch neighbors during remember-time proposal. | Forces remember-time curation proposal to use deterministic tag-overlap neighbors only. |
| `EE_EXPERIMENTAL_TRIAD` | output | boolean flag | none | Compatibility no-op for the promoted ee pack/note/why aliases. | Retained so spike-era scripts continue to run; it no longer gates behavior. |
| `EE_FORMAT` | output | output format | none | Select the default output renderer. | Lower-priority compatibility alias for output format selection. |
| `EE_GRAPH_WITNESSES_RETENTION_DAYS` | tuning | integer days | `30` | Override the default graph algorithm witness retention window in days. | Maps to `[graph.witnesses].retention_days`; per-algorithm config overrides still come from config files or CLI flags. |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | tuning | integer seconds | none | Override the harmful feedback burst window in seconds. | Overrides feedback policy timing from config. |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | tuning | integer count | none | Override the harmful feedback rate limit per source. | Overrides feedback rate limits from config. |
| `EE_HOOK_MODE` | output | boolean flag | none | Use hook-oriented machine output defaults. | Optimizes renderer auto-detection for hook protocols. |
| `EE_INDEX_DIR` | paths | path | none | Override the configured search index directory. | Equivalent to overriding the storage index directory in config. |
| `EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS` | tuning | integer count | `200` | Override index publish advisory-lock retry attempts. | Used by Frankensearch writers. |
| `EE_JSON` | output | boolean flag | none | Request JSON output from renderer auto-detection. | Prefer explicit `--json` for scripts when possible. |
| `EE_L2_PACK_CACHE_BYTES` | tuning | integer bytes | none | Override the L2 pack cache byte cap per workspace. | Maps to `[cache.pack_l2].max_bytes`; default is 1 GiB. |
| `EE_L2_PACK_CACHE_DIR` | paths | path | none | Override the L2 pack cache root directory. | Maps to `[cache.pack_l2].directory`; entries are stored below a workspace-specific subdirectory. |
| `EE_L2_PACK_CACHE_DISABLE` | tuning | boolean flag | none | Disable L2 pack cache lookup and writes. | Inverts `[cache.pack_l2].enabled` for `ee context` once L2 runtime wiring lands. |
| `EE_LOG_FORMAT` | diagnostics | enum | none | Select structured log format. | `json` selects structured command-start logs on stderr. |
| `EE_LOG_JSON` | diagnostics | boolean flag | none | Enable JSON command-start logs on stderr. | Shortcut for JSON command logging. |
| `EE_MAX_TOKENS` | tuning | integer tokens | none | Override the default context pack token budget. | Applies when a command does not pass an explicit token budget. |
| `EE_MESH_ENABLED` | mesh | boolean flag | `false` | Enable optional mesh-memory surfaces. | Disabled by default; ordinary local-first commands must not open network listeners or require peer configuration when unset. |
| `EE_MESH_MODE` | mesh | enum | `off` | Select the default mesh command mode. | Accepted values are `off`, `cache`, `revisable`, and `blocking`; explicit `--mesh` command flags take precedence. |
| `EE_NO_COLOR` | output | boolean flag | none | Disable colored diagnostics. | Mirrors the behavior of `NO_COLOR` for ee-specific control. |
| `EE_OUTPUT_FORMAT` | output | output format | none | Select the default output renderer. | Highest-priority environment output format selector. |
| `EE_PREFLIGHT_BYPASS_SECRET` | policy | secret string | none | Supply preflight bypass secret material. | Capabilities must never expose this current value. |
| `EE_PROFILE` | tuning | profile name | none | Override the default context pack profile. | Applies when pack/context profile is not specified explicitly. |
| `EE_PPR_CACHE_ENTRIES` | tuning | integer count | `4096` | Override the in-process PPR prefetch cache entry cap. | Set to `0` to disable prefetch entries while keeping the algorithm result cache intact. |
| `EE_READ_POOL_ACQUIRE_TIMEOUT_MS` | tuning | integer milliseconds | `5000` | Override the read-side connection pool acquire timeout in milliseconds. | When all pooled reads are active, context waits this long before opening a one-shot ad-hoc read connection. |
| `EE_READ_POOL_DISABLE_PIN` | tuning | boolean flag | none | Disable read-side snapshot pinning. | Inverts `[storage.read_pool].pin_snapshot` for read-heavy status/context paths. |
| `EE_READ_POOL_IDLE_TIMEOUT_S` | tuning | integer seconds | none | Override the read-side connection pool idle timeout in seconds. | Maps to `[storage.read_pool].idle_timeout_seconds`; idle pooled handles are closed after the configured age. |
| `EE_READ_POOL_SIZE` | tuning | integer count | none | Override the read-side connection pool size. | Maps to `[storage.read_pool].size`; pool construction normalizes zero to one connection. |
| `EE_REMEMBER_CURATION_SYNC_BUDGET_MS` | tuning | integer milliseconds | `50` | Override remember-time curation sync budget in milliseconds. | Registry-defined default is used when unset. |
| `EE_SECURITY_PROFILE` | policy | profile name | none | Select security profile. | Controls policy posture for security-sensitive operations. |
| `EE_SCIENCE_BACKEND_PATH` | integration | path | none | Configure an optional science analytics backend path; missing paths report backend-unavailable. | Used by science-status and analytics commands to surface configured backend outages. |
| `EE_TEST_LOG_LEVEL` | diagnostics | enum | none | Control structured test-log verbosity. | Used by the J1 structured E2E logging harness. |
| `EE_TEST_LOG_PATH` | diagnostics | path | none | Enable structured test logging at this JSONL path. | Used by Rust and shell E2E logging helpers. |
| `EE_TEST_LOG_TEST_ID` | diagnostics | string | none | Name the active structured test-log scenario. | Identifies events emitted by the test logging harness. |
| `EE_TAILSCALE_BINARY_OVERRIDE` | mesh | absolute path | none | Test-only override for the tailscale binary used by fake-tailnet harnesses. | Reserved for deterministic fake Tailscale tests; production mesh code must default to normal discovery when unset. |
| `EE_TAILSCALE_PROBE_TIMEOUT_MS` | mesh | integer milliseconds | `1500` | Override the local Tailscale probe timeout budget. | Applies to optional mesh-local Tailscale CLI/socket probes; ignored when mesh is disabled. |
| `EE_TAILSCALE_PROBE_SOCKET_OVERRIDE` | mesh | path | none | Test-only override for fake mesh hello responder socket discovery. | Reserved for deterministic fake Tailscale tests; production mesh code must default to normal Tailscale peer probing when unset. |
| `EE_WORKSPACE` | paths | path | none | Override workspace root discovery. | Used after explicit `--workspace` and before cwd walk-up. |
| `EE_WORKSPACE_REGISTRY` | paths | path | none | Override the workspace alias registry database path. | Controls where workspace aliases are stored. |
