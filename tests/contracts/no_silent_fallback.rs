use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    MustFix,
    Allowed,
}

#[derive(Clone, Copy, Debug)]
struct InventoryRule {
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    disposition: Disposition,
    follow_up: Option<&'static str>,
    reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct ManualFinding {
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug)]
struct SourceFinding {
    file: String,
    line: usize,
    text: String,
    context: String,
}

const FOLLOW_UP_BEADS: &[&str] = &[
    "eidetic_engine_cli-sos5.2",
    "eidetic_engine_cli-sos5.3",
    "eidetic_engine_cli-sos5.4",
    "eidetic_engine_cli-sos5.7",
    "eidetic_engine_cli-ogy9",
];

const INVENTORY_RULES: &[InventoryRule] = &[
    must_fix(
        "NSF-CASS-PIPE-READ",
        "src/cass/process.rs",
        "read_to_end",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess pipe read errors must become CassError or explicit degradations.",
    ),
    must_fix(
        "NSF-CASS-PIPE-JOIN",
        "src/cass/process.rs",
        "join().unwrap_or_default()",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess reader thread failures must not become empty stdout/stderr.",
    ),
    must_fix(
        "NSF-CASS-PIPE-TAKE",
        "src/cass/process.rs",
        "stdout_bytes.take().unwrap_or_default()",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess pipe capture should not convert a missing reader result into empty stdout/stderr.",
    ),
    must_fix(
        "NSF-HOOK-INSTALLER-JSON",
        "src/hooks/installer.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Hook installer JSON is machine-facing output and must not serialize to an empty string on failure.",
    ),
    must_fix(
        "NSF-OUTPUT-RENDERER-JSON",
        "src/output/mod.rs",
        "serde_json::to_string(report).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Machine-facing renderers must return a stable error/degradation instead of empty JSON.",
    ),
    must_fix(
        "NSF-OUTPUT-SHADOW-INCUMBENT",
        "src/output/mod.rs",
        "incumbent_outcome.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Shadow decision output should distinguish missing incumbent evidence from an empty incumbent outcome.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-JSON",
        "src/cli/mod.rs",
        "serde_json::to_string_pretty",
        "eidetic_engine_cli-sos5.3",
        "Certificate JSON handlers bypass the shared renderer and silently erase serialization failures.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-ERROR",
        "src/cli/mod.rs",
        "report.error.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Certificate error reports should not convert a missing error message into an empty machine string.",
    ),
    must_fix(
        "NSF-CLI-DEMO-AUDIT",
        "src/cli/mod.rs",
        "latest_demo_audit_by_id",
        "eidetic_engine_cli-sos5.4",
        "Demo status output should distinguish missing audit storage from an empty run map.",
    ),
    must_fix(
        "NSF-CLI-DEMO-FILE",
        "src/cli/mod.rs",
        "fn read_text_lossy",
        "eidetic_engine_cli-sos5.3",
        "Demo file reads should not render missing or unreadable expected output as empty content.",
    ),
    allowed(
        "NSF-MODELS-JSONL-BUILDERS",
        "src/models/jsonl.rs",
        "ExportRecordBuildError",
        "JSONL export builders reject missing required IDs, timestamps, content, and schema fields with ExportRecordBuildError.",
    ),
    must_fix(
        "NSF-CURATE-CERTIFICATE-BUILDER",
        "src/curate/mod.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Curation risk certificate builders default machine-facing IDs/timestamps to empty values.",
    ),
    must_fix(
        "NSF-MODELS-DECISION-BUILDER",
        "src/models/decision.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Decision records should distinguish a missing outcome from an empty outcome string.",
    ),
    must_fix(
        "NSF-MODELS-MUTATION-JSON",
        "src/models/mutation.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Mutation reports are machine-facing and must not serialize to empty strings on failure.",
    ),
    must_fix(
        "NSF-MODELS-PROGRESS-BUILDER",
        "src/models/progress.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Progress records default required operation/message/timestamp fields to empty values.",
    ),
    must_fix(
        "NSF-CORE-AUDIT-JSON",
        "src/core/audit.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Audit timeline JSON is machine-facing output and must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-BACKUP-IMPORT",
        "src/core/backup.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Backup import/export records should distinguish absent message, next action, and audit target fields.",
    ),
    must_fix(
        "NSF-CORE-CLAIMS-INPUT",
        "src/core/claims.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Claim parsing defaults optional statement/artifact collections into machine-facing records and needs an explicit contract.",
    ),
    must_fix(
        "NSF-CORE-FEEDBACK-JSON",
        "src/core/feedback.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Feedback reports are machine-facing and must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-JSON",
        "src/core/handoff.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Handoff JSON render helpers must not hide serialization failures.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-CAPSULE",
        "src/core/handoff.rs",
        "capsule_content).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Handoff capsule serialization failure should not become an empty capsule hash input.",
    ),
    must_fix(
        "NSF-CORE-LAB-JSON",
        "src/core/lab.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Lab report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-LEARN-JSON",
        "src/core/learn.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Learning report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-LEGACY-JSON",
        "src/core/legacy_import.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Legacy import TOON rendering must not treat failed JSON serialization as empty JSON.",
    ),
    allowed(
        "NSF-CORE-LEGACY-SKIP-DIR",
        "src/core/legacy_import.rs",
        "fn should_skip_directory",
        "A path without a UTF-8 file name cannot match a skipped legacy directory name.",
    ),
    must_fix(
        "NSF-CORE-OUTCOME-WORKSPACE",
        "src/core/outcome.rs",
        "workspace_id.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Outcome recording should not turn a missing workspace ID into an empty persisted field.",
    ),
    must_fix(
        "NSF-CORE-PREFLIGHT-JSON",
        "src/core/preflight.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Preflight report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-PROCEDURE-JSON",
        "src/core/procedure.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Procedure report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-REHEARSE-JSON",
        "src/core/rehearse.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Rehearsal report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-REPRO-JSON",
        "src/core/repro.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Repro artifact JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-TRIPWIRE-JSON",
        "src/core/tripwire.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Tripwire report JSON helpers must not silently serialize to empty.",
    ),
    allowed(
        "NSF-CASS-IMPORT-OPTIONAL-FIELDS",
        "src/cass/import.rs",
        "unwrap_or_default()",
        "CASS importer defaults here are parser policy for empty spans, unknown line types, optional counts, or fallback content hashes; malformed required JSON still errors.",
    ),
    allowed(
        "NSF-CLI-WORKSPACE-CWD",
        "src/cli/mod.rs",
        "std::env::current_dir().unwrap_or_default()",
        "CLI workspace fallback is the existing documented relative-workspace behavior; it does not convert parsed machine data to success.",
    ),
    allowed(
        "NSF-CLI-EVAL-NO-EXPECTATIONS",
        "src/cli/mod.rs",
        "query_expectations.is_empty()",
        "An eval fixture with no expected query matches has no retrieval queries to run; index and search failures still propagate once queries exist.",
    ),
    allowed(
        "NSF-CLI-EVAL-FIRST-FAILURE-NO-QUERY",
        "src/cli/mod.rs",
        "\"expectedIds\": query.map",
        "Eval first-failure output uses empty ID arrays only when no failing per-query metric exists; fixture status and reason codes still report the failure.",
    ),
    allowed(
        "NSF-CLI-RESPONSE-FIELD-COUNT",
        "src/cli/mod.rs",
        "map(count_json_object_fields)",
        "A response without a data object has zero selectable data fields for field-selector telemetry.",
    ),
    must_fix(
        "NSF-CLI-ENVELOPE-JSON-SERIALIZE",
        "src/cli/mod.rs",
        "serde_json::to_string(&envelope).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Machine-facing envelope output must not silently serialize to an empty line.",
    ),
    must_fix(
        "NSF-CLI-MACHINE-JSON-SERIALIZE",
        "src/cli/mod.rs",
        "serde_json::to_string(&json).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Machine-facing CLI JSON output must return a contextual error instead of an empty line on serialization failure.",
    ),
    allowed(
        "NSF-CLI-PACK-DEFAULT-PROFILES",
        "src/cli/mod.rs",
        "pack_profile: args.pack_profile.unwrap_or_default()",
        "Omitted pack/resource profiles intentionally use the default ContextOutputOptions profile.",
    ),
    allowed(
        "NSF-CLI-CONTEXT-OUTPUT-DEFAULT-PROFILES",
        "src/cli/mod.rs",
        "args.pack_profile.unwrap_or_default()",
        "Omitted context output profile arguments intentionally select default pack/resource output profiles.",
    ),
    allowed(
        "NSF-CLI-PACK-DIFF-OPTIONAL-STRING-ARRAYS",
        "src/cli/mod.rs",
        "strings.sort()",
        "Pack diff redaction-class arrays are optional ledger details; absent arrays mean no classes to compare.",
    ),
    must_fix(
        "NSF-CLI-PACK-DIFF-RANK-DEFAULT",
        "src/cli/mod.rs",
        "let old_rank = old_item.rank.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Pack diff should distinguish a missing ledger rank from rank zero when reporting rank deltas.",
    ),
    allowed(
        "NSF-CLI-PACK-REPLAY-SELECTED-ITEMS",
        "src/cli/mod.rs",
        "ledger_core_array(value, \"selectedItems\")",
        "Pack replay with a missing selectedItems ledger array reports an empty replay section while ledger status/degradations remain available.",
    ),
    allowed(
        "NSF-CLI-PACK-REPLAY-OMITTED-ITEMS",
        "src/cli/mod.rs",
        "ledger_core_array(value, \"omittedItems\")",
        "Pack replay with a missing omittedItems ledger array reports no omitted items while ledger status/degradations remain available.",
    ),
    allowed(
        "NSF-CLI-QUERY-PAGINATION-DEFAULT",
        "src/cli/mod.rs",
        "parse_pagination",
        "Missing query-file pagination intentionally means default pagination bounds.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-SEEDS-DEFAULT",
        "src/cli/mod.rs",
        "let seed_memories = graph",
        "Missing graph seedMemories in ee.query.v1 intentionally means no explicit graph seeds.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-TRAVERSAL-DEFAULT",
        "src/cli/mod.rs",
        "let traversal = graph",
        "Missing graph traversal intentionally uses the QueryGraphTraversal default after validation handles malformed values.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-LINK-TYPES-DEFAULT",
        "src/cli/mod.rs",
        "let include_orphans = graph",
        "Missing graph linkTypes intentionally means no relation filter after validation handles malformed values.",
    ),
    allowed(
        "NSF-CLI-REHEARSE-NO-COMMANDS",
        "src/cli/mod.rs",
        "(None, None) => return Ok(Vec::new())",
        "Omitting both rehearsal command sources intentionally plans no commands; unreadable files and malformed JSON still return DomainError.",
    ),
    allowed(
        "NSF-CLI-MAINTENANCE-NO-HISTORY",
        "src/cli/mod.rs",
        "if !path.exists()",
        "A missing maintenance history JSONL file means no recorded jobs yet; read and parse errors on an existing file still fail.",
    ),
    allowed(
        "NSF-CLI-QUERY-FILTERS",
        "src/cli/mod.rs",
        "parse_filters",
        "Missing query filters are an explicit empty-filter case; malformed recognized fields are validated separately.",
    ),
    allowed(
        "NSF-CORE-BUDGET-SATURATION",
        "src/core/budget.rs",
        "unwrap_or_default()",
        "Budget clock math intentionally saturates reversed or expired durations to zero and documents that behavior.",
    ),
    allowed(
        "NSF-CORE-CAUSAL-OPTIONAL-FILTERS",
        "src/core/causal.rs",
        "unwrap_or_default()",
        "Optional memory IDs are query filters and do not represent parsed storage failure.",
    ),
    allowed(
        "NSF-CORE-CONTEXT-TAGS",
        "src/core/context.rs",
        "tags_map.get",
        "A memory with no tag rows has an explicit empty tag set.",
    ),
    must_fix(
        "NSF-CORE-CONTEXT-COORDINATION-HASH",
        "src/core/context.rs",
        "serde_json::to_string(coordination).unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Context pack hashes should not silently drop coordination snapshot bytes when serialization fails.",
    ),
    allowed(
        "NSF-CORE-CURATE-PROPOSED-CONTENT-TAGS",
        "src/core/curate.rs",
        "stored.proposed_content.as_deref().unwrap_or_default()",
        "A curation candidate without proposed content can still derive tags from its reason and cluster membership.",
    ),
    allowed(
        "NSF-CORE-CLAIMS-NO-EVIDENCE",
        "src/core/claims.rs",
        "let Some(raw_evidence) = raw_evidence else",
        "Claims without an evidence field deliberately have an empty evidence list; malformed evidence entries still return ClaimParseError.",
    ),
    allowed(
        "NSF-CORE-CLAIMS-NULL-EVIDENCE",
        "src/core/claims.rs",
        "YamlValue::Null => Ok(Vec::new())",
        "A YAML null evidence field is treated as explicitly empty evidence, while non-null malformed evidence is rejected.",
    ),
    allowed(
        "NSF-CORE-DOCTOR-OPTIONAL-REPAIR",
        "src/core/doctor.rs",
        "check.repair.unwrap_or_default()",
        "Doctor command text may be absent; the surrounding check still carries severity and message.",
    ),
    allowed(
        "NSF-CORE-ECONOMY-BASELINE",
        "src/core/economy.rs",
        "unwrap_or_default()",
        "No matching baseline scenario means there are no baseline artifact scores to compare.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINKS",
        "src/core/handoff.rs",
        "get(\"kind\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINK-IDS",
        "src/core/handoff.rs",
        "get(\"id\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-ADDED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"memories_added\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable added-memory counts from zero.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-EXPIRED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"any_expired_in_pack\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable expired-memory counts from zero.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-DRIFT-DEFAULT",
        "src/core/handoff.rs",
        "content_drift_score.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable content drift from zero drift.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-REVISED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"memories_revised\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable revised-memory counts from zero.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-TAG-LOOKUP",
        "src/core/handoff.rs",
        "conn.get_memory_tags(&memory.id).unwrap_or_default()",
        "eidetic_engine_cli-sos5.7",
        "Handoff snapshot hashes should not silently treat failed tag lookups as untagged memories.",
    ),
    allowed(
        "NSF-CORE-INDEX-HUMAN-DIMENSION",
        "src/core/index.rs",
        "quality_dimension.unwrap_or_default()",
        "Quality embedder dimension is optional human display text and is gated by quality model presence.",
    ),
    allowed(
        "NSF-CORE-INDEX-VACUUM-NO-PARENT",
        "src/core/index.rs",
        "return Ok(Vec::new());",
        "If the index parent directory does not exist, there are no stale index directories to vacuum.",
    ),
    allowed(
        "NSF-CORE-INIT-CWD",
        "src/core/init.rs",
        "std::env::current_dir",
        "Relative init paths retain the existing workspace fallback and still render the selected path.",
    ),
    allowed(
        "NSF-CORE-INSTALL-OPTIONALS",
        "src/core/install.rs",
        "unwrap_or_default()",
        "Installer planning treats missing artifacts and PATH as empty collections without reporting a successful install.",
    ),
    allowed(
        "NSF-CORE-JSONL-IMPORT-TAGS",
        "src/core/jsonl_import.rs",
        "tags_by_memory",
        "Imported memories without tag records have an explicit empty tag set.",
    ),
    allowed(
        "NSF-CORE-MEMORY-AUTO-LINK-DISABLED",
        "src/core/memory.rs",
        "if !enabled",
        "Disabled remember auto-linking intentionally creates no links before any repository query is attempted.",
    ),
    allowed(
        "NSF-CORE-MEMORY-AUTO-LINK-NO-WORKFLOW",
        "src/core/memory.rs",
        "let Some(workflow_id) = workflow_id else",
        "Remember auto-linking without a workflow ID has no workflow neighborhood to query; repository errors after a workflow is present still propagate.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SUGGEST-LINKS-NO-TAGS",
        "src/core/memory.rs",
        "if tags.is_empty()",
        "Tag-based link suggestions require at least one tag; missing tags are an explicit no-input case.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SUGGEST-LINKS-NO-MATCHES",
        "src/core/memory.rs",
        "if matches.is_empty()",
        "A successful tag lookup with no candidate memories is an explicit empty suggestion set; lookup failures still return DomainError.",
    ),
    allowed(
        "NSF-CORE-LAB-OPTIONAL-FIELDS",
        "src/core/lab.rs",
        "as_deref().unwrap_or_default()",
        "Lab hash input includes optional intervention fields as empty components while retaining the surrounding structured record.",
    ),
    allowed(
        "NSF-CORE-LEARN-CWD",
        "src/core/learn.rs",
        "current_dir().unwrap_or_default()",
        "Learning path resolution keeps the existing relative path fallback and does not manufacture learned evidence.",
    ),
    allowed(
        "NSF-CORE-LEARN-CLUSTER-NO-TAGS",
        "src/core/learn.rs",
        "snapshot\n.memory_tags",
        "Learn-cluster embedding text represents untagged memories with an empty tags line.",
    ),
    allowed(
        "NSF-CORE-LEGACY-NONUTF8-FILENAME",
        "src/core/legacy_import.rs",
        "name.starts_with(\"._\")",
        "A non-UTF-8 legacy filename cannot match macOS metadata filenames and is still sorted by the path wire string.",
    ),
    must_fix(
        "NSF-CORE-MEMORY-LINE-SPAN",
        "src/core/memory.rs",
        "extract_line_span(&contents, *span).unwrap_or_default()",
        "eidetic_engine_cli-sos5.7",
        "Evidence freshness should report an invalid provenance span instead of hashing an empty source excerpt.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SECRET-ALLOWLISTS",
        "src/core/memory.rs",
        "allow_phrases: config",
        "Missing secret-detector allowlist arrays intentionally mean no configured bypass phrases or regexes.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SECRET-ALLOWREGEX",
        "src/core/memory.rs",
        "allow_regex: config",
        "Missing secret-detector allow_regex config intentionally means no configured bypass regexes.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SCOPE-TEAM-MEMBERS",
        "src/core/memory_scope.rs",
        "team_members\n.unwrap_or_default()",
        "Missing trust.team_members config intentionally produces an empty verified-agent set.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SCOPE-AGENT-URI",
        "src/core/memory_scope.rs",
        ".split(['/', '#', '?'])",
        "An agent provenance URI with no name segment is normalized away rather than emitted as an empty agent.",
    ),
    allowed(
        "NSF-CORE-PLAN-RAND-ID",
        "src/core/plan.rs",
        "duration_since(SystemTime::UNIX_EPOCH)",
        "Pseudo-random fallback only handles a clock before UNIX_EPOCH and does not feed persisted evidence.",
    ),
    allowed(
        "NSF-CORE-RECORDER-CASS-CLASSIFIER",
        "src/core/recorder.rs",
        "unwrap_or_default()",
        "Recorder CASS line classification maps missing type/role to a conservative message event.",
    ),
    allowed(
        "NSF-CORE-RECORDER-IMPORT-NO-INPUT",
        "src/core/recorder.rs",
        "let Some(input) = options.input_json.as_deref() else",
        "Recorder import with no inline CASS view input is an explicit empty future-connector plan; invalid provided JSON returns recorder_import_invalid_json.",
    ),
    allowed(
        "NSF-CORE-REPRO-MISSING-HASH",
        "src/core/repro.rs",
        "expected_artifacts",
        "A missing expected hash is paired with a failed verification result, not a successful empty hash.",
    ),
    allowed(
        "NSF-CORE-PREFLIGHT-GUARD-NO-RULES",
        "src/core/preflight_guard.rs",
        "let Some(rules_item) = document.get(\"rules\") else",
        "A workspace guard file without a rules table has no rules to enforce; malformed rules tables still return DomainError.",
    ),
    allowed(
        "NSF-CORE-PROCEDURE-NO-STORE",
        "src/core/procedure.rs",
        "let Some(store) = open_procedure_store(workspace)? else",
        "A workspace without a procedure store has no procedures yet; store open errors still propagate through DomainError.",
    ),
    allowed(
        "NSF-CORE-SEARCH-OPTIONAL-DETAIL",
        "src/core/search.rs",
        "last_check_error",
        "Absent index-check detail appends no extra sentence while preserving the high-severity corruption signal.",
    ),
    allowed(
        "NSF-CORE-SEARCH-NO-RELEVANT-TOP-SCORE",
        "src/core/search.rs",
        "let top_note = top_score",
        "A no-relevant-results degradation may omit the optional top-score sentence while keeping the main degradation.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-TAGS",
        "src/core/search.rs",
        "metadata_string(metadata, \"tags\")",
        "Search hits without tag metadata are valid untagged memories.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-TOKEN-CONTENT",
        "src/core/search.rs",
        "estimate_tokens_default",
        "Search hit token estimates fall back to already-required content metadata when the analysis content key is absent.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-SECTION",
        "src/core/search.rs",
        "match (level.unwrap_or_default(), kind.unwrap_or_default())",
        "Missing optional search metadata classifies the pack item into the generic artifacts section.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-PROVENANCE",
        "src/core/search.rs",
        "PackProvenance::new(uri",
        "If derived provenance construction rejects the fallback URI, the hit can still be represented without provenance details.",
    ),
    must_fix(
        "NSF-CORE-STATUS-AUDIT-ACCESS",
        "src/core/status.rs",
        "list_audit_entries",
        "eidetic_engine_cli-sos5.7",
        "Status memory health should surface audit-log read failures instead of treating all memories as never accessed.",
    ),
    allowed(
        "NSF-CORE-SUPPORT-BUNDLE-PACK-QUERY",
        "src/core/support_bundle.rs",
        "let query = row_text(row, 1).unwrap_or_default()",
        "Support-bundle pack summaries may represent a missing query column as an empty diagnostic field.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-BV-TOP-PICKS",
        "src/core/swarm_brief.rs",
        "\"topPickIds\"",
        "A swarm brief without BV top picks intentionally reports an empty top-pick list.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-RECOMMENDATIONS",
        "src/core/swarm_brief.rs",
        "Swarm brief summary",
        "A swarm brief summary without recommendation IDs intentionally renders no recommendation examples.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-CYCLE-EXAMPLES",
        "src/core/swarm_brief.rs",
        "examples.sort()",
        "A Beads dependency-cycle payload without example cycles intentionally reports an empty examples list.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-BV-PICKS",
        "src/core/swarm_brief.rs",
        "let picks_value = quick_ref",
        "BV robot JSON may omit top_picks while still reporting aggregate counts.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-RESERVATIONS",
        "src/core/swarm_brief.rs",
        "let inbox = value",
        "Agent Mail snapshots may omit reservations; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-INBOX",
        "src/core/swarm_brief.rs",
        "let threads = value",
        "Agent Mail snapshots may omit inbox entries; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-THREADS",
        "src/core/swarm_brief.rs",
        "let mut reservations = reservations",
        "Agent Mail snapshots may omit thread entries; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-RCH-OPTIONAL-WORKER",
        "src/core/swarm_brief.rs",
        "summarize_rch_topology_blocked_message",
        "An RCH-E327 degradation may omit the selected worker; the topology-blocked code and redacted root summary remain explicit.",
    ),
    allowed(
        "NSF-CORE-PERF-FORENSICS-SOURCE-SCHEMA",
        "src/core/perf_forensics.rs",
        "source_schema: normalized.source_schema.unwrap_or_default()",
        "Perf artifacts treat source schema as optional metadata; missing values do not hide metric ingestion failure.",
    ),
    allowed(
        "NSF-CORE-PERF-FORENSICS-UNIT",
        "src/core/perf_forensics.rs",
        "unit.unwrap_or_default().to_lowercase()",
        "Perf metric unit is optional metadata; missing units simply skip unit-based volatility inference.",
    ),
    allowed(
        "NSF-MODELS-QUERY-MISSING-ARRAY-FILTER",
        "src/models/query.rs",
        "Result<Vec<String>, EqlQueryError>",
        "Missing optional EQL array filters are deliberate empty filter sets; present non-array or empty-string values still return EqlQueryError.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-FILTERS",
        "src/models/query.rs",
        "let require_any = object",
        "Missing tag filter arrays in ee.query.v1 intentionally mean no tag filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-REQUIRE-ANY-FILTERS",
        "src/models/query.rs",
        "let exclude = object",
        "Missing tag requireAny arrays in ee.query.v1 intentionally mean no alternate tag filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-EXCLUDE-FILTERS",
        "src/models/query.rs",
        "TagFilters {",
        "Missing tag exclude arrays in ee.query.v1 intentionally mean no tag exclusion filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TRUST-FILTERS",
        "src/models/query.rs",
        "let require_posture = object",
        "Missing trust excludeClasses in ee.query.v1 intentionally means no trust-class exclusions.",
    ),
    allowed(
        "NSF-MODELS-QUERY-REDACTION-FILTERS",
        "src/models/query.rs",
        "RedactionFilters {",
        "Missing redaction allowCategories in ee.query.v1 intentionally means the default redaction policy.",
    ),
    allowed(
        "NSF-DB-FEEDBACK-SIGNAL",
        "src/db/mod.rs",
        "optional_text(row, 0)?.unwrap_or_default()",
        "Missing feedback signal maps to no positive/negative bucket and does not create a successful signal.",
    ),
    allowed(
        "NSF-DB-LATEST-SCHEMA-EMPTY",
        "src/db/mod.rs",
        "MIGRATIONS\n.last()",
        "A build with no compiled migrations would report schema version zero rather than hiding a database operation failure.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-NO-DEGRADATIONS",
        "src/db/mod.rs",
        "return Ok(Vec::new());",
        "A pack ledger with no degraded JSON has an explicit empty degradation list.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-ARRAY",
        "src/db/mod.rs",
        "pack_ledger_core_array(ledger, \"degraded\")",
        "A parsed pack ledger without a degraded array has no ledger-local degradations.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-SORT",
        "src/db/mod.rs",
        "let severity = value",
        "Missing degradation sort-key fields are used only to produce a deterministic order for malformed diagnostic values.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-MESSAGE-SORT",
        "src/db/mod.rs",
        "let message = value",
        "Missing degradation messages are used only to produce a deterministic order for malformed diagnostic values.",
    ),
    allowed(
        "NSF-GRAPH-PPR-NO-NEIGHBORS",
        "src/graph/ppr.rs",
        "edges.sort_unstable_by_key",
        "A graph node with no outgoing neighbors intentionally contributes an empty normalized edge list.",
    ),
    allowed(
        "NSF-GRAPH-PACK-DNA-NO-PPR-SEEDS",
        "src/graph/pack_dna.rs",
        "query_seed_weights.is_empty() || limit == 0",
        "Pack DNA PPR neighbors are explicitly empty when there are no valid query seeds or the caller requested a zero-neighbor limit.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-CLOSURE-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "closure\n.successors(failure_id)\n.unwrap_or_default()",
        "A failure node with no transitive causal successors intentionally has an empty ancestor list.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-TERMINAL-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "graph\n.successors(&ancestor.memory_id)\n.unwrap_or_default()",
        "A reachable ancestor with no outgoing causal successors is intentionally treated as terminal.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-NO-NODE-ATTRS",
        "src/graph/causal.rs",
        "graph.node_attrs(node).cloned().unwrap_or_default()",
        "Causal flow projection permits nodes without optional attributes while adding required demand metadata explicitly.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-BFS-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "graph.successors(&current).unwrap_or_default()",
        "Causal shortest-path traversal uses an empty successor list as the explicit leaf-node case.",
    ),
    allowed(
        "NSF-OUTPUT-FIELD-SELECTOR-COMMAND",
        "src/output/mod.rs",
        "requested_fields_for_selector(command, selector)",
        "A response without a command name cannot match command-specific field selectors and is returned unchanged.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-SCHEMA",
        "src/pack/mod.rs",
        "coordination_string_field(value, &[\"schema\"])",
        "A coordination snapshot without an explicit schema is treated as the current schema after the required sources array is validated.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-ENTRIES",
        "src/pack/mod.rs",
        "entries.sort()",
        "A coordination source without entries intentionally contributes an empty entry list.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-DEGRADATIONS",
        "src/pack/mod.rs",
        "coordination_string_field(item, &[\"repair\"])",
        "A coordination snapshot without degradation entries intentionally has no source degradations.",
    ),
    allowed(
        "NSF-CURATE-CLUSTER-DIMENSION",
        "src/curate/cluster_coherence.rs",
        "points\n.first()",
        "Cluster coherence converts an empty or zero-dimensional input into an explicit ClusterCoherenceError.",
    ),
    allowed(
        "NSF-CURATE-CLUSTER-REPRESENTATIVE",
        "src/curate/cluster_coherence.rs",
        "representative_memory_id",
        "Cluster representatives are derived after cluster membership validation and sorting.",
    ),
    allowed(
        "NSF-SERVE-DAEMON-DRY-RUN-ROWS",
        "src/serve.rs",
        "report.dry_run || run_id == \"dry-run\"",
        "A dry-run foreground daemon report intentionally produces no durable daemon job rows.",
    ),
    allowed(
        "NSF-SERVE-DAEMON-MISSING-TABLE",
        "src/serve.rs",
        "if !table_path.exists()",
        "A missing daemon job JSONL table means no daemon jobs have been recorded; existing-table read and parse errors still fail.",
    ),
    allowed(
        "NSF-MODELS-DEMO-OPTIONALS",
        "src/models/demo.rs",
        "unwrap_or_default()",
        "Demo fixtures use empty optional descriptions and values for human demonstration metadata only.",
    ),
    allowed(
        "NSF-POLICY-ENV-PROFILE",
        "src/policy/security_profile.rs",
        "read(EnvVar::SecurityProfile)",
        "Absent or invalid environment profile intentionally falls back to the default security profile.",
    ),
    allowed(
        "NSF-STEWARD-RESOURCE-SUMMARY",
        "src/steward/mod.rs",
        "consumption",
        "No recorded consumption for a budgeted resource means zero consumed, not hidden failed I/O.",
    ),
];

const MANUAL_FINDINGS: &[ManualFinding] = &[];

const REQUIRED_SURFACE_FILES: &[&str] = &[
    "src/cass/process.rs",
    "src/db/mod.rs",
    "src/output/mod.rs",
    "src/hooks/installer.rs",
    "src/models/jsonl.rs",
];

const fn must_fix(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        fragment,
        disposition: Disposition::MustFix,
        follow_up: Some(follow_up),
        reason,
    }
}

const fn allowed(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        fragment,
        disposition: Disposition::Allowed,
        follow_up: None,
        reason,
    }
}

#[test]
fn no_silent_fallback_inventory_covers_current_source_findings() -> TestResult {
    let findings = scan_source_findings()?;
    let mut uncovered = Vec::new();

    for finding in &findings {
        if classify_finding(finding).is_none() {
            uncovered.push(format!(
                "{}:{} `{}`\ncontext:\n{}",
                finding.file, finding.line, finding.text, finding.context
            ));
        }
    }

    if uncovered.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unclassified production fallback(s):\n{}\n\nRepair: return a contextual error/degradation or add a justified inventory entry with a follow-up bead.",
            uncovered.join("\n\n")
        ))
    }
}

#[test]
fn no_silent_fallback_must_fix_entries_have_follow_up_beads() -> TestResult {
    let mut missing = Vec::new();

    for rule in INVENTORY_RULES {
        if rule.disposition == Disposition::MustFix {
            match rule.follow_up {
                Some(bead) if FOLLOW_UP_BEADS.contains(&bead) => {}
                Some(bead) => missing.push(format!(
                    "{} references unknown follow-up `{bead}`: {}",
                    rule.id, rule.reason
                )),
                None => missing.push(format!(
                    "{} has no follow-up bead: {}",
                    rule.id, rule.reason
                )),
            }
        }
    }

    for finding in MANUAL_FINDINGS {
        if !FOLLOW_UP_BEADS.contains(&finding.follow_up) {
            missing.push(format!(
                "{} references unknown follow-up `{}`: {}",
                finding.id, finding.follow_up, finding.reason
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[test]
fn no_silent_fallback_inventory_covers_required_surfaces() -> TestResult {
    let mut missing = Vec::new();
    for required in REQUIRED_SURFACE_FILES {
        if !INVENTORY_RULES.iter().any(|rule| rule.file == *required) {
            missing.push(*required);
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required no-silent-fallback inventory surface(s) missing: {}",
            missing.join(", ")
        ))
    }
}

#[test]
fn no_silent_fallback_manual_findings_still_point_at_real_code() -> TestResult {
    let mut missing = Vec::new();
    for finding in MANUAL_FINDINGS {
        let path = repo_path(finding.file);
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if !source.contains(finding.fragment) {
            missing.push(format!(
                "{} missing `{}` in {}",
                finding.id, finding.fragment, finding.file
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[test]
fn no_silent_fallback_guard_rejects_new_unclassified_renderer_default() -> TestResult {
    let synthetic = SourceFinding {
        file: "src/output/new_renderer.rs".to_owned(),
        line: 1,
        text: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
        context: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
    };

    if classify_finding(&synthetic).is_none() {
        Ok(())
    } else {
        Err("synthetic unclassified renderer fallback was unexpectedly allowlisted".to_owned())
    }
}

#[test]
fn no_silent_fallback_guard_rejects_new_unclassified_empty_vec() -> TestResult {
    let synthetic = SourceFinding {
        file: "src/db/new_repository.rs".to_owned(),
        line: 42,
        text: "return Ok(Vec::new());".to_owned(),
        context: "return Ok(Vec::new());".to_owned(),
    };

    if classify_finding(&synthetic).is_none() {
        Ok(())
    } else {
        Err("synthetic unclassified empty-vector fallback was unexpectedly allowlisted".to_owned())
    }
}

fn classify_finding(finding: &SourceFinding) -> Option<&'static InventoryRule> {
    INVENTORY_RULES
        .iter()
        .find(|rule| rule.file == finding.file && finding.context.contains(rule.fragment))
}

fn scan_source_findings() -> Result<Vec<SourceFinding>, String> {
    let mut files = Vec::new();
    collect_rust_files(&repo_path("src"), &mut files)?;
    files.sort();

    let mut findings = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let relative = relative_path(&path)?;
        let ignored = ignored_test_module_lines(&source);
        let lines = source.lines().collect::<Vec<_>>();

        for (index, line) in lines.iter().enumerate() {
            if ignored[index] || !is_high_risk_line(line) {
                continue;
            }
            findings.push(SourceFinding {
                file: relative.clone(),
                line: index + 1,
                text: line.trim().to_owned(),
                context: context_window(&lines, index),
            });
        }
    }

    Ok(findings)
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read dir entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            if is_temp_sync_dir(&path) {
                continue;
            }
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn is_temp_sync_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".tmp-sync"))
}

fn ignored_test_module_lines(source: &str) -> Vec<bool> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut ignored = vec![false; lines.len()];
    let mut pending_cfg_test = false;
    let mut in_test_module = false;
    let mut brace_depth = 0_i32;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if in_test_module {
            ignored[index] = true;
            brace_depth += brace_delta(line);
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if pending_cfg_test && trimmed.starts_with("mod tests") && trimmed.contains('{') {
            ignored[index] = true;
            in_test_module = true;
            brace_depth = brace_delta(line);
            pending_cfg_test = false;
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if trimmed == "#[cfg(test)]" {
            pending_cfg_test = true;
        } else if pending_cfg_test
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("//")
        {
            pending_cfg_test = false;
        }
    }

    ignored
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0_i32, |depth, ch| match ch {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn is_high_risk_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains(".unwrap_or_default()")
        || trimmed.contains("Ok(Vec::new())")
        || (trimmed.starts_with("let _ =") && trimmed.contains("read_to_end"))
        || trimmed.contains("join().unwrap_or_default()")
}

fn context_window(lines: &[&str], index: usize) -> String {
    let start = index.saturating_sub(4);
    let end = (index + 5).min(lines.len());
    lines[start..end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn relative_path(path: &Path) -> Result<String, String> {
    let root = repo_path("");
    let relative = path
        .strip_prefix(&root)
        .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}
