# EE_* environment variables

This file documents every `EE_*` environment variable honored by `ee`.
The source of truth for runtime variables is `src/config/env_registry.rs`;
update both the registry and the runtime table when adding a new runtime
variable.

`ee capabilities --json` exposes the same registry through
`data.envOverrides[]`. Sensitive variables may report that they are set, but
must not expose their current value.

## Detected-but-ignored embedding variables

These variables are not `EE_*` runtime controls. `ee doctor` detects only their
presence so it can explain that they do not affect the bundled local embedder;
values are never displayed, never passed to Frankensearch, and never used for
retrieval.

| Name | Category | Value read? | Effect |
|---|---|---|---|
| `EMBEDDING_MODEL` | embeddings | no | Presence adds an `ee doctor` info note that the active retrieval mode still comes from ee's bundled `potion-multilingual-128M` local embedder. |
| `OPENAI_API_KEY` | embeddings | no | Presence adds an `ee doctor` info note; local semantic retrieval never consumes API keys or remote embedding APIs. |

## Runtime variables

| Name | Category | Type | Default | Controls | Notes |
|---|---|---|---|---|---|
| `EE_AGENT_NAME` | output | string | none | Identify the current agent for scoped memory retrieval. | Used by agent-aware memory and context surfaces. |
| `EE_AGENT_MODE` | output | boolean flag | none | Use agent-oriented output defaults. | Optimizes renderer auto-detection for agent consumption. |
| `EE_AMBIENT_CONTEXT` | hooks | boolean flag | `true` | Enable or disable proactive ambient context hook injection. | Set to `false`, `0`, `off`, `no`, `disable`, or `disabled` to make generated harness hooks suppress all ambient injections. |
| `EE_AMBIENT_CONTEXT_STATE_DIR` | hooks | path | none | Override the ambient hook de-duplication state directory. | Defaults to `<workspace>/.ee/hook-state` in the generated hook scripts; the directory stores only per-session injection fingerprints. |
| `EE_AMBIENT_CONTEXT_VERBOSITY` | hooks | enum (`quiet`, `standard`, `verbose`) | `standard` | Select quiet, standard, or verbose ambient hook budgets. | `quiet` suppresses SessionStart orient and lowers pre-edit recall budget; `verbose` raises the bounded orient/recall ceilings. |
| `EE_ADAPTIVE_BACKOFF_MS` | tuning | integer milliseconds | `25` | Override the SRR5 noisy-neighbor soft backoff delay in milliseconds. | Applied only when swarm adaptive scheduling is enabled; backoff is advisory and must not alter retrieval results. |
| `EE_ADAPTIVE_NOISY_P99_MS` | tuning | integer milliseconds | `200` | Override the SRR5 per-agent p99 latency threshold for noisy-neighbor backoff. | Used by the adaptive scheduler to decide when a single agent should receive advisory delay. |
| `EE_AUDIT_LANE_BATCH_MAX` | tuning | integer count | `64` | Override the audit-lane writer batch size before flushing. | Used by the audit-lane writer once foreground audit emission is enabled; preserving the default keeps existing direct insert behavior unchanged. |
| `EE_AUDIT_LANE_CAPACITY` | tuning | integer count | `1024` | Override the audit-lane producer queue capacity. | Capacity is normalized by the runtime config before queue construction; full queues report `audit_backpressure` instead of silently dropping events. |
| `EE_AUDIT_LANE_FLUSH_MS` | tuning | integer milliseconds | `5` | Override the audit-lane time-based flush interval in milliseconds. | Bounds how long the audit writer waits before flushing a partial batch. |
| `EE_CASS_BINARY` | integration | absolute path | none | Override the trusted cass import binary path. | Used before config and trusted PATH lookup for CASS import discovery. |
| `EE_CASS_TIMEOUT_SECS` | integration | integer seconds | `30` | Override the CASS subprocess timeout in seconds for import and discovery calls. | Applied to every `ee import cass` subprocess call (`cass sessions` discovery, `cass view` span capture); layered above the `[cass] subprocess_timeout_secs` config key, below the hidden `--subprocess-timeout-ms` diagnostic flag. Large corpora may need several minutes for session discovery. |
| `EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR` | curation | float `0.0..=1.0` | `0.80` | Override the minimum confidence required by curation auto-promotion. | Reserved contract for threshold-promotion dry runs and apply paths; explicit CLI flags remain higher precedence. |
| `EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN` | curation | integer count | `10` | Override the maximum curation candidates auto-promotion may accept per run. | Keeps automated promotion bounded; `0` should behave as disabled when the promotion surface is wired. |
| `EE_CURATION_DERIVED_PREVIEW_LIMIT` | curation | integer count | `20` | Override the derived-candidate preview/reject listing limit. | Reserved for derived-candidate preview and reject ergonomics; JSON should report the effective non-secret value. |
| `EE_DAEMON_ENABLE_ECHO` | diagnostics | boolean flag | `false` | Enable the diagnostic `ee.daemon.echo` round-trip method. | Disabled by default so production daemon sockets never reflect caller-supplied params; enabled echo still routes params through the canonical redaction pipeline. |
| `EE_DAEMON_MAX_INFLIGHT` | tuning | integer count | `32` | Override the cap on in-flight `ee daemon` worker threads; saturated accepts emit `daemon_overloaded`. | Bounds peak per-daemon RSS at `inflight × per-worker-footprint` (bd-jnyui). Zero or unparseable values fall back to the registered default. |
| `EE_DATABASE_PATH` | paths | path | none | Override the configured storage database path. | Equivalent to overriding the storage database path in config. |
| `EE_DEMO_EVIDENCE_ROOT` | paths | path | none | Override the demo evidence storage root. | Used by demo evidence capture surfaces. |
| `EE_DIAG_FORCE_CAPABILITY_GAP` | diagnostics | comma-separated tokens | none | Force selected capability probes to report build-gap diagnostics. | Diagnostics-only fixture control; accepts `runtime`, `storage`, `search`, `graph`, `science`, or `all`. |
| `EE_DISABLE_ADAPTIVE` | tuning | boolean flag | `false` | Disable SRR5 swarm adaptive prefetch and scheduling without editing config. | Set to `true` or `1` to opt out of `[swarm.adaptive].enabled`; disabled adaptive mode is a config/capability state, not a per-response degradation. |
| `EE_DISABLE_TOON` | output | boolean flag | none | Disable TOON output capability reporting and auto-selection. | Forces TOON capability diagnostics to report unavailable and makes renderer auto-detection fall back to JSON. |
| `EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS` | tuning | boolean flag | none | Disable Frankensearch neighbors during remember-time proposal. | Forces remember-time curation proposal to use deterministic tag-overlap neighbors only. |
| `EE_E2E_RETENTION_MANIFEST` | paths | path | none | Override the retained-artifact manifest path used by diagnostics. | Used by disk-pressure artifact-retention diagnostics when `EPIC_RETENTION_MANIFEST` is unset. |
| `EE_EMBED_DEDUP_COSINE_FLOOR` | embeddings | float `0.0..=1.0` | `0.97` | Set the cosine-similarity floor for insert-time embedding dedup confirmation. | Parsed before write-path use; invalid or non-finite values must return structured repair text instead of silently enabling reuse. |
| `EE_EMBED_DEDUP_ENABLED` | embeddings | boolean flag | `false` | Enable insert-time embedding deduplication after storage and write-path gates are wired. | Disabled by default so `ee remember` remains byte-compatible until the storage, dedup-link, and e2e beads land. |
| `EE_EMBED_DEDUP_HAMMING_K` | embeddings | integer `0..=128` | `12` | Set the maximum SimHash Hamming distance admitted to dedup cosine confirmation. | Affects only the cheap SimHash candidate gate; cosine confirmation is still mandatory before embedding reuse. |
| `EE_EMBED_DOWNLOAD` | embeddings | enum (`auto`, `off`) | `auto` | Control bundled embedding model download behavior with auto or off. | `auto` permits ee to fetch `potion-multilingual-128M` on the first embedding operation and cache it locally; `off` stays on deterministic hash fallback for offline or air-gapped runs. Progress and notices go to stderr only. |
| `EE_EMBED_MODEL_DIR` | embeddings | path | none | Override the bundled embedding model cache directory used by ee. | Pre-populate this directory with `potion-multilingual-128M/` for air-gapped machines, or let `EE_EMBED_DOWNLOAD=auto` fill it once on a networked host. Defaults to ee's data directory under `models/`. |
| `EE_EMBED_MODEL_PATH` | embeddings | path | none | Fault-injection path used to simulate an unavailable search embedder; this does not load alternate models. | Not a user-facing model loader or model-selection knob. Missing paths force `embed_model_unavailable` when lexical fallback remains available; use `EE_EMBED_MODEL_DIR` with `EE_EMBED_DOWNLOAD=auto` or a pre-populated `potion-multilingual-128M/` cache for real bundled-model control. |
| `EE_EXPERIMENTAL_TRIAD` | output | boolean flag | none | Compatibility no-op for the promoted ee pack/note/why aliases. | Retained so spike-era scripts continue to run; it no longer gates behavior. |
| `EE_FLIGHT_RECORDER` | diagnostics | boolean flag | `false` | Enable the redacted command flight recorder for ee subcommands. | Disabled by default; when enabled, command traces must stay redacted and retention-bounded. |
| `EE_FLIGHT_RECORDER_DIR` | paths | path | none | Override the directory where flight recorder traces are written. | Machine-local override for command trace storage; do not commit absolute local paths. |
| `EE_FLIGHT_RECORDER_RETENTION_DAYS` | diagnostics | integer days | `7` | Override the flight recorder trace retention window in days. | Applies to flight-recorder trace pruning once the recorder is enabled. |
| `EE_FORMAT` | output | output format | none | Select the default output renderer. | Lower-priority compatibility alias for output format selection. |
| `EE_GRAPH_MEMORY_DEGRADED_BELOW_PCT` | tuning | integer percent | `80` | Override the graph snapshot advisory threshold as a percent of the snapshot cap. | Maps to `[graph.memory].degraded_below_pct`; values above `100` clamp to `100` for deterministic admission checks. |
| `EE_GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS` | tuning | integer basis points | `15000` | Override the graph snapshot in-build growth tripwire ratio in basis points. | Maps to `[graph.memory].growth_multiplier_basis_points`; `15000` means `1.5x`. |
| `EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB` | tuning | integer MiB | `100` | Override the per-algorithm graph working-set cap in MiB. | Maps to `[graph.memory].per_algorithm_cap_mb`; lower this on memory-constrained hosts. |
| `EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB` | tuning | integer MiB | `250` | Override the graph snapshot admission cap in MiB. | Maps to `[graph.memory].snapshot_cap_mb`; F1 builders refuse oversized snapshots before allocation. |
| `EE_GRAPH_NUMA_PIN_DISABLE` | tuning | boolean flag | `false` | Disable graph snapshot NUMA pinning without editing config. | Inverts `[graph.numa_pin].enabled`; set to `true` to force the `numa_pin_disabled` degraded path even on supported Linux hosts. |
| `EE_GRAPH_NUMA_PIN_NODE` | tuning | enum or non-negative integer | `auto` | Select auto NUMA placement or an explicit non-negative NUMA node for graph snapshot pinning. | Maps to `[graph.numa_pin].preferred_node`; invalid values keep the default and are reported through the NUMA pin diagnostics surface. |
| `EE_GRAPH_NUMA_PIN_POPULATE` | tuning | boolean flag | `true` | Control whether graph snapshot loading pre-faults pages during NUMA pinning. | Maps to `[graph.numa_pin].populate_on_load`; unsupported platforms still degrade without claiming pages were pinned. |
| `EE_GRAPH_WITNESSES_RETENTION_DAYS` | tuning | integer days | `30` | Override the default graph algorithm witness retention window in days. | Maps to `[graph.witnesses].retention_days`; per-algorithm config overrides still come from config files or CLI flags. |
| `EE_HARMFUL_BURST_WINDOW_SECONDS` | tuning | integer seconds | none | Override the harmful feedback burst window in seconds. | Overrides feedback policy timing from config. |
| `EE_HARMFUL_PER_SOURCE_PER_HOUR` | tuning | integer count | none | Override the harmful feedback rate limit per source. | Overrides feedback rate limits from config. |
| `EE_HOOK_MODE` | output | boolean flag | none | Use hook-oriented machine output defaults. | Optimizes renderer auto-detection for hook protocols. |
| `EE_INDEX_DIR` | paths | path | none | Override the configured search index directory. | Equivalent to overriding the storage index directory in config. |
| `EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS` | tuning | integer count | `200` | Override index publish advisory-lock retry attempts. | Used by Frankensearch writers. |
| `EE_JSON` | output | boolean flag | none | Request JSON output from renderer auto-detection. | Prefer explicit `--json` for scripts when possible. |
| `EE_JOURNAL_ENABLED` | memory | boolean flag | `true` | Enable or disable append-only agent journal capture. | Mirrors `[journal].enabled`; false makes journal append, list, show, and distill report `journal_disabled` instead of mutating capture state. |
| `EE_JOURNAL_RETENTION_DAYS` | tuning | integer days | `14` | Override the append-only journal retention window in days. | Mirrors `[journal].retention_days`; pruning is performed only by the explicit `journal-retention` steward job and is audited. |
| `EE_L2_PACK_CACHE_BYTES` | tuning | integer bytes | none | Override the L2 pack cache byte cap per workspace. | Maps to `[cache.pack_l2].max_bytes`; default is 1 GiB. |
| `EE_L2_PACK_CACHE_DIR` | paths | path | none | Override the L2 pack cache root directory. | Maps to `[cache.pack_l2].directory`; entries are stored below a workspace-specific subdirectory. |
| `EE_L2_PACK_CACHE_DISABLE` | tuning | boolean flag | none | Disable L2 pack cache lookup and writes. | Inverts `[cache.pack_l2].enabled` for `ee pack` and the `ee context` alias once L2 runtime wiring lands. |
| `EE_LEGACY_SELECTION_CERTIFICATE` | output | boolean flag | none | Include the legacy selectionCertificate field in context JSON. | Transitional compatibility switch for consumers migrating from the old field name. |
| `EE_LEXICAL_INDEX_HUGEPAGES` | tuning | boolean flag | `false` | Request transparent hugepage hints for opt-in lexical index RAM-tier pinning. | Accepts `true`/`false`, `1`/`0`, `yes`/`no`, or `on`/`off`; only meaningful when `EE_LEXICAL_INDEX_PIN_RAM` enables the tier, and unsupported platforms degrade without changing search results. |
| `EE_LEXICAL_INDEX_PIN_RAM` | tuning | boolean flag | `false` | Opt in to lexical index RAM-tier page-cache population. | Accepts `true`/`false`, `1`/`0`, `yes`/`no`, or `on`/`off`; disabled by default so large indexes do not pressure RAM unexpectedly, and status/search/doctor read it through the central env registry. |
| `EE_LOG_FORMAT` | diagnostics | enum | none | Select structured log format. | `json` selects structured command-start logs on stderr. |
| `EE_LOG_JSON` | diagnostics | boolean flag | none | Enable JSON command-start logs on stderr. | Shortcut for JSON command logging. |
| `EE_MAX_OUTPUT_TOKENS` | output | integer tokens | none | Cap estimated response tokens for machine output (output governor ceiling). | Env mirror of the global `--max-output-tokens` flag (ADR 0063); the flag wins when both are set. Unset or unparseable values leave output ungoverned. |
| `EE_MAX_TOKENS` | tuning | integer tokens | none | Override the default context pack token budget. | Applies when a command does not pass an explicit token budget. |
| `EE_MCP_MAX_REQUEST_BYTES` | tuning | integer bytes | `16777216` | Override the MCP stdio JSON-RPC request and response byte cap. | Defaults to 16 MiB. Values below 1024 bytes are clamped upward so the adapter can still emit a structured `size_limit_exceeded` JSON-RPC error. |
| `EE_MESH_DISCOVERY_CACHE_TTL_SECONDS` | mesh | integer seconds | `30` | Override the mesh autodiscovery cache TTL in seconds. | Used by SRR6.46 discovery-cache decisions; cache rows are derived state and may be refreshed early on workspace, tailnet, explicit-refresh, or auto-enroll invalidation. |
| `EE_MESH_DRIFT_SOFT_STALE_AFTER` | mesh | integer count | `1` | Override missed mesh hello probes before soft-stale drift grace. | A soft-stale peer is reported as transient unreachable, not removal-class stale drift. |
| `EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS` | mesh | integer seconds | `300` | Override seconds since last successful mesh probe before soft-stale drift grace. | Defaults to five minutes to tolerate laptop sleep and transient tailnet blips. |
| `EE_MESH_DRIFT_HARD_STALE_AFTER` | mesh | integer count | `3` | Override missed mesh hello probes before hard-stale drift. | Hard-stale peers may be reported in `stalePeersInConfig` with reason `consecutive_probes_missed`. |
| `EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS` | mesh | integer seconds | `3600` | Override seconds since last successful mesh probe before hard-stale drift. | Defaults to one hour so transient reachability failures can self-resolve first. |
| `EE_MESH_ENABLED` | mesh | boolean flag | `false` | Enable optional mesh-memory surfaces. | Disabled by default; ordinary local-first commands must not open network listeners or require peer configuration when unset. |
| `EE_MESH_HELLO_PORT` | mesh | integer port | `41888` | Override the mesh hello responder bind port on the local Tailscale address. | The responder lifecycle job binds only when mesh is enabled and `EE_MESH_HELLO_RESPONDER_DISABLED` is false. |
| `EE_MESH_HELLO_RESPONDER_DISABLED` | mesh | boolean flag | `false` | Disable the mesh hello responder lifecycle job while leaving other mesh surfaces enabled. | Use this off-switch when discovery should remain caller-only or the daemon lane is being repaired. |
| `EE_MESH_MODE` | mesh | enum | `off` | Select the default mesh command mode. | Accepted values are `off`, `cache`, `revisable`, and `blocking`; explicit `--mesh` command flags take precedence. |
| `EE_NO_COLOR` | output | boolean flag | none | Disable colored diagnostics. | Mirrors the behavior of `NO_COLOR` for ee-specific control. |
| `EE_OUTPUT_FORMAT` | output | output format | none | Select the default output renderer. | Highest-priority environment output format selector. |
| `EE_PREFLIGHT_BYPASS_SECRET` | policy | secret string | none | Supply preflight bypass secret material. | Capabilities must never expose this current value. |
| `EE_PROFILE` | tuning | profile name | none | Override the default context pack profile. | Applies when pack/context profile is not specified explicitly. |
| `EE_PPR_CACHE_ENTRIES` | tuning | integer count | `4096` | Override the in-process PPR prefetch cache entry cap. | Set to `0` to disable prefetch entries while keeping the algorithm result cache intact. |
| `EE_QUERY_PLAN_CACHE_ENTRIES` | tuning | integer count | `1024` | Override the in-process EQL query plan cache entry cap. | Set to `0` to disable plan caching. Plan-cache hits skip parse + bind + index-selection cost; see [`docs/search/plan-cache.md`](search/plan-cache.md). Tracks bead `bd-2mey5`. |
| `EE_QUERY_MISS_RETENTION_DAYS` | tuning | integer days | `30` | Override the query-miss ledger retention window used by ee learn gaps. | Mirrors `[search].query_miss_retention_days`; longer windows preserve hash-only miss demand signals for weekly or slower review cadences. |
| `EE_READ_POOL_DISABLE_PIN` | tuning | boolean flag | none | Disable read-side snapshot pinning. | Inverts `[storage.read_pool].pin_snapshot` for read-heavy status/context paths. |
| `EE_READ_POOL_ACQUIRE_TIMEOUT_MS` | tuning | integer milliseconds | `5000` | Override the read-side connection pool acquire timeout in milliseconds. | Maps to `[storage.read_pool].acquire_timeout_ms`; when all pooled reads are active, context waits this long before opening a one-shot ad-hoc read connection. |
| `EE_READ_POOL_IDLE_TIMEOUT_S` | tuning | integer seconds | none | Override the read-side connection pool idle timeout in seconds. | Maps to `[storage.read_pool].idle_timeout_seconds`; idle pooled handles are closed after the configured age. |
| `EE_READ_POOL_MAX_PIN_SECONDS` | tuning | integer seconds | `30` | Override the read-side snapshot pin maximum lifetime in seconds. | Maps to `[storage.read_pool].max_pin_duration_seconds`; expired pins are reported through the snapshot-pin degraded-code family. |
| `EE_READ_POOL_SIZE` | tuning | integer count | none | Override the read-side connection pool size. | Maps to `[storage.read_pool].size`; pool construction normalizes zero to one connection. |
| `EE_REFLECTION_CONSUMED_RETENTION_DAYS` | reflection | integer days | `30` | Override retention for consumed reflection requests in days. | Reserved for reflection request retention planning; support bundles and status output must not include request bodies. |
| `EE_REFLECTION_EXPIRED_RETENTION_DAYS` | reflection | integer days | `7` | Override retention for expired reflection requests in days. | Distinguishes expired request cleanup from consumed request retention. |
| `EE_REFLECTION_HMAC_KEY_ID` | reflection | string | none | Select the reflection request HMAC key identifier. | Key identifiers may be reported; key material must never be logged or emitted. |
| `EE_REFLECTION_HMAC_KEY_PATH` | paths | path | none | Select the reflection request HMAC key file path without exposing key material. | Capabilities report presence only and must not expose the current value. |
| `EE_REFLECTION_HMAC_ROTATION_GRACE_SECONDS` | reflection | integer seconds | `86400` | Override reflection HMAC key rotation grace in seconds. | Lets old request signatures validate during bounded rotation windows. |
| `EE_REFLECTION_REQUEST_LIST_LIMIT` | reflection | integer count | `50` | Override the default reflection request list limit. | Used by request list-style surfaces once wired; explicit command limits take precedence. |
| `EE_REFLECTION_REQUEST_SHOW_SOURCE_LIMIT` | reflection | integer count | `20` | Override how many source-package entries reflection request show may include. | Bounds metadata-only source listings; raw source text remains governed by redaction policy. |
| `EE_REFLECTION_REQUEST_TTL_SECONDS` | reflection | integer seconds | `86400` | Override the default reflection request TTL in seconds. | Missing, expired, and malformed-key cases should surface distinct repair actions. |
| `EE_REFLECTION_SOURCE_BUDGET_BYTES` | reflection | integer bytes | `65536` | Override the reflection source-package byte budget. | Downstream propose/ingest paths should report the effective budget hash or value without source text. |
| `EE_REMEMBER_CURATION_SYNC_BUDGET_MS` | curation | integer milliseconds | `50` | Override remember-time curation sync budget in milliseconds. | Registry-defined default is used when unset. |
| `EE_SECURITY_PROFILE` | policy | profile name | none | Select security profile. | Controls policy posture for security-sensitive operations. |
| `EE_SERVE_TOKEN` | policy | secret string | none | Configure the bearer token required by the localhost serve adapter. | Must contain at least 32 random bytes before `ee serve --foreground` can accept HTTP requests; capabilities report only presence, never token material. |
| `EE_SCIENCE_BACKEND_PATH` | integration | path | none | Configure an optional science analytics backend path; missing paths report backend-unavailable. | Used by science-status and analytics commands to surface configured backend outages. |
| `EE_SHARD_FANOUT_ENABLED` | storage | boolean flag | `false` | Enable read-only shard fan-out planning and, after migration, per-workspace shard routing. | Disabled keeps the legacy `<workspace>/.ee/ee.db` path authoritative. When enabled before migration, status/doctor report migration-required rather than creating files. |
| `EE_SHARDS_DIR` | paths | path | none | Override the per-workspace shard directory used by shard fan-out planning. | Must be an absolute, non-symlinked directory. The catalog is planned as the sibling `catalog.db` next to this shard directory. |
| `EE_TEST_LOG_LEVEL` | diagnostics | enum | none | Control structured test-log verbosity. | Used by the J1 structured E2E logging harness. |
| `EE_TEST_LOG_PATH` | diagnostics | path | none | Enable structured test logging at this JSONL path. | Used by Rust and shell E2E logging helpers. |
| `EE_TEST_LOG_TEST_ID` | diagnostics | string | none | Name the active structured test-log scenario. | Identifies events emitted by the test logging harness. |
| `EE_TAILSCALE_BINARY_OVERRIDE` | mesh | absolute path | none | Test-only override for the tailscale binary used by fake-tailnet harnesses. | Reserved for deterministic fake Tailscale tests; production mesh code must default to normal discovery when unset. |
| `EE_TAILSCALE_PROBE_TIMEOUT_MS` | mesh | integer milliseconds | `1500` | Override the local Tailscale probe timeout budget. | Applies to optional mesh-local Tailscale CLI/socket probes; ignored when mesh is disabled. |
| `EE_TAILSCALE_PROBE_SOCKET_OVERRIDE` | mesh | path | none | Test-only override for fake mesh hello responder socket discovery. | Reserved for deterministic fake Tailscale tests; production mesh code must default to normal Tailscale peer probing when unset. |
| `EE_TAILSCALE_DISCOVERY_MODE` | mesh | enum (`service_tag`, `auto_admit`, `allowlist`) | `service_tag` | Select the caller-side mesh peer discovery policy (service_tag, auto_admit, allowlist). | SRR6.46.7 — controls who `ee mesh` probes on the tailnet; `service_tag` (default) restricts probes to peers advertising `tag:ee-mesh`. Precedence: CLI flag > env var > workspace config > built-in default. |
| `EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS` | mesh | integer milliseconds | `750` | Override the per-peer Tailscale hello probe timeout budget. | SRR6.46.2 — each autodiscovery candidate gets this much time to answer the ee hello handshake before it is recorded as `probe_timeout`. |
| `EE_TAILSCALE_DISCOVERY_BUDGET_MS` | mesh | integer milliseconds | `5000` | Override the total Tailscale peer autodiscovery wall-clock budget. | SRR6.46.2 — discovery stops once the aggregate budget is exhausted and emits `peer_discovery_budget_exhausted`. |
| `EE_TAILSCALE_RESPOND_MODE` | mesh | enum (`service_tag`, `auto_admit`, `allowlist`) | `service_tag` | Select the responder-side mesh discovery consent policy (service_tag, auto_admit, allowlist). | SRR6.46.7 — controls whether this host admits inbound `ee.mesh.hello.v1` probes; symmetric to `EE_TAILSCALE_DISCOVERY_MODE`. Misconfiguration (e.g. `service_tag` without advertising `tag:ee-mesh`) surfaces as a degraded code rather than silent decline. |
| `EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS` | policy | comma-separated path matchers | none | Add local workspace-hygiene patterns that force matching paths into human review. | Matcher syntax is `exact:<path>`, `prefix:<path>`, `suffix:<path>`, or `contains:<text>`; secret-risk evidence still wins. |
| `EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS` | policy | comma-separated path matchers | none | Add local workspace-hygiene generated-artifact path patterns. | Matching paths classify as `generated` / `do_not_commit` unless secret-risk evidence overrides first. |
| `EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS` | policy | comma-separated path matchers | none | Add local workspace-hygiene machine-local artifact path patterns. | Matching paths classify as `local_machine` / `do_not_commit`; use for host-specific files only. |
| `EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS` | policy | comma-separated path matchers | none | Add local workspace-hygiene scratch-artifact path patterns. | Matching paths classify as `scratch` / `do_not_commit`; keep patterns narrow and repo-relative. |
| `EE_WAL_CHECKPOINT_BYTES_THRESHOLD` | tuning | integer bytes | `67108864` | Override the WAL checkpoint warning threshold in bytes. | `ee status --json` reports `wal_growth_exceeds_threshold` once `data.wal.bytes` is above this value. |
| `EE_WRITE_GROUP_COMMIT_ENABLED` | tuning | boolean flag | `false` | Enable bounded group-commit coalescing for durable writes. | Maps to `[write].group_commit_enabled`; the shipped config default remains false until the RCH soak/perf proof flips it. Daemon-routed writes use the daemon actor's bounded internal coalescing path without changing this global default. |
| `EE_WRITE_GROUP_COMMIT_BATCH_WINDOW_MS` | tuning | integer milliseconds | `2` | Override the maximum group-commit batch dwell time in milliseconds. | Maps to `[write].batch_window_ms`; zero or invalid merged bounds fail safe to the per-write path when group commit is requested. |
| `EE_WRITE_GROUP_COMMIT_MAX_BATCH_SIZE` | tuning | integer count | `64` | Override the maximum durable writes coalesced into one group commit. | Maps to `[write].max_batch_size`; each coalesced write still receives its own result and audit row. |
| `EE_WRITE_GROUP_COMMIT_MAX_INFLIGHT_BYTES` | tuning | integer bytes | `4194304` | Override the pending payload byte ceiling for group-commit intake. | Maps to `[write].max_inflight_bytes`; oversized or degraded intake falls back to a single per-write commit. |
| `EE_WORKSPACE` | paths | path | none | Override workspace root discovery. | Used after explicit `--workspace` and before cwd walk-up. |
| `EE_WORKSPACE_CLOSE_DRAIN_TIMEOUT_S` | tuning | integer seconds | `5` | Override workspace-close wait time for read snapshot pins in seconds. | Bounds how long workspace-close lifecycle waits for active SnapshotPins before force-poisoning remaining read snapshots. |
| `EE_WORKSPACE_REGISTRY` | paths | path | none | Override the workspace alias registry database path. | Controls where workspace aliases are stored. |

## Build-time variables

These names are read by compile-time Rust macros such as `option_env!` or
`env!`. They are not runtime registry entries and do not appear in
`data.envOverrides[]`.

| Name | Consumer | Controls | Notes |
|---|---|---|---|
| `EE_BUILD_TARGET` | `src/core/mod.rs` | Target triple reported in build/version provenance. | Missing or invalid values emit the `target_triple_unavailable` build-provenance degradation. |
| `EE_RELEASE_CHANNEL` | `src/core/mod.rs` | Release channel label. | Accepted values are `stable`, `beta`, `nightly`, and `dev`; invalid values fall back to `dev` for debug builds and `stable` for release builds. |
| `EE_TRACE_BEAD_ID` | tracing checkpoints | Compile-time bead id embedded in selected tracing events. | Set this before compilation for debug or verification builds that need a different static trace bead id. |
