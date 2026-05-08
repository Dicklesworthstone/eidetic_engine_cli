# Silent Fallback Inventory (sos5.1)

Generated: 2026-05-08

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

## Category 2: JSONL Import Model Defaults (FOLLOWUP)

### src/models/jsonl.rs

These defaults convert missing optional fields to empty strings during JSONL import.
Machine-facing IDs defaulting to empty string can corrupt downstream lookups.

| Line | Field | Severity | Issue |
|------|-------|----------|-------|
| 670 | `created_at: self.created_at.unwrap_or_default()` | FOLLOWUP | Timestamp defaults to epoch |
| 676 | `ee_version: self.ee_version.unwrap_or_default()` | SAFE | Version metadata, optional |
| 678 | `export_id: self.export_id.unwrap_or_default()` | FOLLOWUP | Empty export_id breaks provenance |
| 790 | `export_id: self.export_id.unwrap_or_default()` | FOLLOWUP | Same (footer) |
| 791 | `completed_at: self.completed_at.unwrap_or_default()` | FOLLOWUP | Missing completion timestamp |
| 962 | `memory_id: self.memory_id.unwrap_or_default()` | **MUST-FIX** | Empty memory_id corrupts storage |
| 963 | `workspace_id: self.workspace_id.unwrap_or_default()` | **MUST-FIX** | Empty workspace_id orphans record |
| 964 | `level: self.level.unwrap_or_default()` | FOLLOWUP | Defaults to first enum variant |
| 965 | `kind: self.kind.unwrap_or_default()` | FOLLOWUP | Defaults to first enum variant |
| 966 | `content: self.content.unwrap_or_default()` | FOLLOWUP | Empty content is semantically valid |
| 970 | `created_at: self.created_at.unwrap_or_default()` | FOLLOWUP | Same as 670 |
| 1141 | `artifact_id: self.artifact_id.unwrap_or_default()` | FOLLOWUP | Empty artifact_id |
| 1142 | `workspace_id: self.workspace_id.unwrap_or_default()` | FOLLOWUP | Same as 963 |

**Linked bead:** eidetic_engine_cli-sos5.4

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

## Summary

| Category | MUST-FIX | FOLLOWUP | SAFE/DISPLAY |
|----------|----------|----------|--------------|
| CASS subprocess | 6 | 0 | 0 |
| JSONL models | 2 | 11 | 0 |
| Hooks installer | 2 | 0 | 0 |
| Output module | 0 | 8 | 0 |
| CLI module | 0 | 2 | 14 |
| Core modules | 0 | 0 | 41+ |
| Other models | 0 | 6 | 8 |
| **Total** | **10** | **27** | **63+** |

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
