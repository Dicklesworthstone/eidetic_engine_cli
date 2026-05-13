# Silent Fallback Inventory (sos5.1)

Generated: 2026-05-08
Last updated: 2026-05-08 (eidetic_engine_cli-08rn: tag/temporal/trust/redaction filters)

This document inventories production code locations where `unwrap_or_default`, ignored
`Result`s, or serialization fallbacks could hide data corruption or I/O failures.
Each finding is classified for triage.

## Classification Legend

| Tag | Meaning |
|-----|---------|
| **MUST-FIX** | Silent failure in machine-facing data path; requires immediate fix |
| **FOLLOWUP** | Requires separate bead; not trivial to fix |
| **SAFE** | Intentional default; acceptable behavior documented |
| **DISPLAY** | Human display only; empty string acceptable for formatting |

---

## Category 1: CASS Subprocess I/O (MUST-FIX)

### src/cass/process.rs

| Line | Pattern | Severity | Issue |
|------|---------|----------|-------|
| 262 | `let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);` | **MUST-FIX** | Pipe read error silently discarded |
| 268 | `let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);` | **MUST-FIX** | Pipe read error silently discarded |
| 274 | `stdout_thread.join().unwrap_or_default()` | **MUST-FIX** | Thread panic produces empty Vec instead of error |
| 275 | `stderr_thread.join().unwrap_or_default()` | **MUST-FIX** | Thread panic produces empty Vec instead of error |
| 290 | `stdout_thread.join().unwrap_or_default()` | **MUST-FIX** | Same as 274 (timeout branch) |
| 291 | `stderr_thread.join().unwrap_or_default()` | **MUST-FIX** | Same as 275 (timeout branch) |

**Linked bead:** eidetic_engine_cli-sos5.2

---

## Category 2: JSONL Import Model Defaults (RESOLVED)

### src/models/jsonl.rs

Resolved by `ExportRecordBuildError`: JSONL export builders now reject missing
or blank required IDs, timestamps, content, schema metadata, and size fields
instead of manufacturing empty strings or zero sizes. Optional display and
metadata fields remain optional.

**Regression:** `models::jsonl::tests::export_record_builders_reject_missing_required_fields`.
**Linked bead:** eidetic_engine_cli-sos5.4.

---

## Category 3: Hooks Installer Serialization (MUST-FIX)

### src/hooks/installer.rs

| Line | Pattern | Severity | Issue |
|------|---------|----------|-------|
| 164 | `serde_json::to_string(self).unwrap_or_default()` | **MUST-FIX** | Hook config serialization failure hidden |
| 645 | `serde_json::to_string(self).unwrap_or_default()` | **MUST-FIX** | Same pattern in different impl |

**Fix:** Return `Result<String, ...>` or log+degrade with stable error JSON.

---

## Category 4: Output Module Serialization (FOLLOWUP)

### src/output/mod.rs

These are JSON rendering functions that output to stdout. Silent failure produces
empty string instead of error JSON.

| Line | Pattern | Severity | Issue |
|------|---------|----------|-------|
| 7166 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Search report render |
| 7208 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 7258 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 7315 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 7361 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 7408 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 8147 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |
| 8198 | `serde_json::to_string(report).unwrap_or_default()` | FOLLOWUP | Same pattern |

**Linked bead:** eidetic_engine_cli-sos5.3

---

## Category 5: Mutation Model Serialization (SAFE)

### src/models/mutation.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 382 | `serde_json::to_string(self).unwrap_or_default()` | SAFE | Display impl for logging |
| 387 | `serde_json::to_string_pretty(self).unwrap_or_default()` | SAFE | Same |
| 477 | `serde_json::to_string(self).unwrap_or_default()` | SAFE | Same |

---

## Category 6: CLI Workspace Resolution (SAFE)

### src/cli/mod.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 8262 | `std::env::current_dir().unwrap_or_default()` | SAFE | Fallback for cwd; error handled elsewhere |
| 13159 | Same pattern | SAFE | Same |
| 13222 | Same pattern | SAFE | Same |
| 13293 | Same pattern | SAFE | Same |
| 13364 | Same pattern | SAFE | Same |
| 13431 | Same pattern | SAFE | Same |
| 20985 | Same pattern | SAFE | Same |
| 21053 | Same pattern | SAFE | Same |

---

## Category 7: CLI Error Message Extraction (SAFE)

### src/cli/mod.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 13187 | `report.error.clone().unwrap_or_default()` | SAFE | Optional error message for display |
| 13247 | Same pattern | SAFE | Same |
| 13318 | Same pattern | SAFE | Same |
| 23082 | `value["data"]["content"].as_str().unwrap_or_default()` | SAFE | JSON extraction for display |
| 24483 | `workspace_check["message"].as_str().unwrap_or_default()` | SAFE | Same |
| 26342 | `value["error"]["message"].as_str().unwrap_or_default()` | SAFE | Same |

---

## Category 8: CLI JSON Output (DISPLAY)

### src/cli/mod.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 16826 | `serde_json::to_string_pretty(&report.data_json()).unwrap_or_default()` | FOLLOWUP | Machine JSON output |
| 16858 | Same pattern | FOLLOWUP | Same |

---

## Category 9: Core Module Serialization (DISPLAY/SAFE)

These are `fn to_json(&self) -> String` implementations on report structs.
Most are for human display or logging. Listed for completeness.

### Allowlisted as DISPLAY (41 occurrences):

- `src/core/learn.rs`: 89, 187, 279, 1580
- `src/core/feedback.rs`: 334, 446, 498
- `src/core/outcome.rs`: 463, 506 (uses `unwrap_or_else` with explicit fallback)
- `src/core/rehearse.rs`: 166, 323, 599, 733
- `src/core/handoff.rs`: 430, 435, 517, 522, 625, 630, 729, 734, 1178
- `src/core/tripwire.rs`: 117, 122, 337, 342
- `src/core/lab.rs`: 119, 124, 203, 414, 1198, 1203
- `src/core/audit.rs`: 103, 129, 146, 172
- `src/core/procedure.rs`: 85, 90, 317, 399, 480, 793, 987, 1433, 1634, 2557
- `src/core/preflight.rs`: 430, 458, 624
- `src/core/legacy_import.rs`: 1061
- `src/core/repro.rs`: 801, 830, 842, 855
- `src/core/curate.rs`: 345, 402, 477, 555, 780 (use `unwrap_or_else` with explicit fallback)
- `src/core/rule.rs`: 183, 271, 328, 422, 475 (use `unwrap_or_else` with explicit fallback)
- `src/core/focus.rs`: 1079

---

## Category 10: Progress Model (SAFE)

### src/models/progress.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 146 | `serde_json::to_string(self).unwrap_or_else(\|_\| String::new())` | SAFE | Progress display |
| 223 | `self.operation.unwrap_or_default()` | SAFE | Optional operation name |
| 224 | `self.message.unwrap_or_default()` | SAFE | Optional message |
| 230 | `self.timestamp.unwrap_or_default()` | SAFE | Optional timestamp |

---

## Category 11: CASS Import (FOLLOWUP)

### src/cass/import.rs

| Line | Pattern | Severity | Issue |
|------|---------|----------|-------|
| 296 | `.unwrap_or_default()` | FOLLOWUP | Session metadata extraction |
| 301 | `.unwrap_or_default()` | FOLLOWUP | Same |
| 936 | `.unwrap_or_default()` | FOLLOWUP | Same |
| 1104 | `session.message_count.unwrap_or_default()` | SAFE | Optional count |
| 1193 | `.unwrap_or_default()` | FOLLOWUP | Same |
| 1197 | `.unwrap_or_default()` | FOLLOWUP | Same |

---

## Category 12: DB Module (SAFE)

### src/db/mod.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 6481 | `optional_text(row, 0)?.unwrap_or_default()` | SAFE | NULL column to empty string |
| 13327 | `.unwrap_or_default()` | SAFE | Optional metadata |

---

## Category 13: Demo Model (SAFE)

### src/models/demo.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 383 | `raw_demo.description.unwrap_or_default()` | SAFE | Optional description |
| 487 | `value.unwrap_or_default()` | SAFE | Optional value |

---

## Category 14: Decision Model (SAFE)

### src/models/decision.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 332 | `self.outcome.unwrap_or_default()` | SAFE | Optional outcome field |

---

## Category 15: Query-File Filter Arrays (SAFE)

### src/models/query.rs

Tag, trust, and redaction filter arrays default to empty Vec when absent. Empty arrays
mean "no filtering", which is the correct semantic for optional query-file controls.

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 1057 | `require` tags array `.unwrap_or_default()` | SAFE | Empty array = no require filter |
| 1068 | `requireAny` tags array `.unwrap_or_default()` | SAFE | Empty array = no requireAny filter |
| 1079 | `exclude` tags array `.unwrap_or_default()` | SAFE | Empty array = no exclude filter |
| 1176 | `excludeClasses` trust array `.unwrap_or_default()` | SAFE | Empty array = no class exclusions |
| 1233 | `allowCategories` redaction array `.unwrap_or_default()` | SAFE | Empty array = default category policy |

**Rationale:** Query-file filter arrays are explicitly optional per `docs/query-schema.md`.
Absent arrays produce no-op filters, which is correct behavior. Non-array types are
rejected earlier with `ERR_MALFORMED_JSON`.

---

## Category 16: Context Tags Lookup (SAFE)

### src/core/context.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 1368 | `tags_map.get(&memory_key).cloned().unwrap_or_default()` | SAFE | Missing tags = empty Vec |
| 1630 | `tags_map.get(&memory.id).cloned().unwrap_or_default()` | SAFE | Same pattern |

**Rationale:** Memory may have no tags. Empty Vec is correct for untagged memories.

---

## Category 17: Perf Forensics Metadata (SAFE)

### src/core/perf_forensics.rs

| Line | Pattern | Severity | Reason |
|------|---------|----------|--------|
| 910 | `normalized.source_schema.unwrap_or_default()` | SAFE | Optional schema metadata for display |
| 1059 | `unit.unwrap_or_default().to_lowercase()` | SAFE | Missing unit = no unit-based inference |

**Rationale:** Source schema and unit are optional metadata. Missing values produce
empty strings which are semantically valid (no schema specified, no unit inference).

---

## Category 18: CASS Process Take (extends Category 1)

### src/cass/process.rs

| Line | Pattern | Severity | Issue |
|------|---------|----------|-------|
| 503 | `stdout_bytes.take().unwrap_or_default()` | **MUST-FIX** | Option::take on captured bytes hides None |
| 504 | `stderr_bytes.take().unwrap_or_default()` | **MUST-FIX** | Same as 503 |

**Linked bead:** eidetic_engine_cli-sos5.2 (same as Category 1)

---

## Summary

| Category | MUST-FIX | FOLLOWUP | SAFE/DISPLAY |
|----------|----------|----------|--------------|
| CASS subprocess | 6 | 0 | 0 |
| CASS process take | 2 | 0 | 0 |
| JSONL models | 2 | 11 | 0 |
| Hooks installer | 2 | 0 | 0 |
| Output module | 0 | 8 | 0 |
| CLI module | 0 | 2 | 14 |
| Core modules | 0 | 0 | 41+ |
| Query-file filters | 0 | 0 | 5 |
| Context tags lookup | 0 | 0 | 2 |
| Perf forensics metadata | 0 | 0 | 2 |
| Other models | 0 | 6 | 8 |
| **Total** | **12** | **27** | **72+** |

---

## Linked Beads

- `eidetic_engine_cli-sos5.2`: CASS subprocess pipe read/join failures
- `eidetic_engine_cli-sos5.3`: Output renderer serialization defaults
- `eidetic_engine_cli-sos5.4`: Machine-facing builder defaults

---

## Allowlist Contract

The following patterns are **explicitly allowed** and should not trigger future audits:

1. **Display `to_json()` methods** returning empty string on serialization failure
   - Reason: Human-facing display, not machine data paths
   - Scope: `fn to_json(&self) -> String` impls in report structs

2. **Optional error message extraction** with `unwrap_or_default()`
   - Reason: Absence of error message is semantically valid
   - Scope: `report.error.clone().unwrap_or_default()` patterns

3. **Current directory fallback** with `std::env::current_dir().unwrap_or_default()`
   - Reason: Handled by workspace resolution error path
   - Scope: CLI workspace resolution only

4. **Optional metadata fields** in builder patterns
   - Reason: Semantically optional (description, version info)
   - Scope: Explicitly marked Optional<T> fields with default impls

5. **Query-file filter arrays** with `unwrap_or_default()`
   - Reason: Absent arrays produce no-op filters (correct semantic)
   - Scope: `tags.require`, `tags.requireAny`, `tags.exclude`, `trust.excludeClasses`, `redaction.allowCategories`
   - Added: 2026-05-08 (eidetic_engine_cli-08rn)

6. **Context tags map lookups** with `unwrap_or_default()`
   - Reason: Missing tags = empty Vec is semantically correct
   - Scope: `tags_map.get().cloned().unwrap_or_default()` in context assembly

7. **Perf forensics optional metadata** with `unwrap_or_default()`
   - Reason: Optional schema/unit metadata for display and inference
   - Scope: `source_schema`, `unit` fields in perf_forensics.rs

## Feature flag reservations

Some Cargo feature flags exist in `Cargo.toml` `[features]` without
cfg-gating any code today. These are *reserved* — the flag name is
held against future use rather than being silently empty. The
canonical inventory lives in [`feature_flag_registry.md`](feature_flag_registry.md).

Current reservations:

| Flag                | Status   | Notes |
| ------------------- | -------- | ----- |
| `json`              | reserved | Future "JSON-only minimal output" build profile. No current cfg-gates. |
| `serve`             | reserved | Future localhost HTTP/SSE adapter (AGENTS.md §Module Layout `src/serve/`). Not implemented in v0.1. |
| `science-analytics` | reserved | One cfg-gate at `src/science/mod.rs:1999`. The CLI surface `ee analyze science-status` is `CommandEffect::degraded_unavailable` per `src/core/effect.rs`. Reserved for EE-171 analytics subsystem. |

`tests/feature_flag_registry_in_sync.rs` enforces 1:1 correspondence
between `Cargo.toml` and the registry; a flag in either without a
matching entry in the other fails CI.

Owner: `bd-17c65.11.7` (K7).
