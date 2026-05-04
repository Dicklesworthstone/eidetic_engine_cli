//! Passive focus state domain model (EE-FOCUS-001).
//!
//! Focus state records the explicit active-memory set for resumed work. It is
//! passive state only: it stores and explains context, but never executes a
//! plan or mutates workspace files.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value as JsonValue, json};

use super::{MemoryId, WorkspaceId};

/// Schema identifier for a passive active-memory focus state.
pub const FOCUS_STATE_SCHEMA_V1: &str = "ee.focus.state.v1";

/// Schema identifier for one memory entry inside a focus state.
pub const FOCUS_ITEM_SCHEMA_V1: &str = "ee.focus.item.v1";

/// Schema identifier for the focus schema catalog.
pub const FOCUS_SCHEMA_CATALOG_V1: &str = "ee.focus.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// One memory in a passive focus set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusItem {
    pub schema: &'static str,
    pub memory_id: MemoryId,
    pub pinned: bool,
    pub reason: String,
    pub provenance: Vec<String>,
    pub added_at: String,
}

impl FocusItem {
    /// Create a focus item with a visible inclusion reason.
    ///
    /// # Errors
    ///
    /// Returns [`FocusValidationError`] when the reason or timestamp is empty.
    pub fn new(
        memory_id: MemoryId,
        reason: impl Into<String>,
        added_at: impl Into<String>,
    ) -> Result<Self, FocusValidationError> {
        let item = Self {
            schema: FOCUS_ITEM_SCHEMA_V1,
            memory_id,
            pinned: false,
            reason: reason.into(),
            provenance: Vec::new(),
            added_at: added_at.into(),
        };
        item.validate()?;
        Ok(item)
    }

    #[must_use]
    pub const fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    #[must_use]
    pub fn with_provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance.push(provenance.into());
        self.canonicalize();
        self
    }

    fn canonicalize(&mut self) {
        self.provenance.sort();
        self.provenance.dedup();
    }

    /// Validate item-level focus invariants.
    ///
    /// # Errors
    ///
    /// Returns [`FocusValidationError`] when required visible fields are empty.
    pub fn validate(&self) -> Result<(), FocusValidationError> {
        if self.reason.trim().is_empty() {
            return Err(FocusValidationError::EmptyReason {
                memory_id: self.memory_id.to_string(),
            });
        }
        if self.added_at.trim().is_empty() {
            return Err(FocusValidationError::EmptyTimestamp { field: "addedAt" });
        }
        Ok(())
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "memoryId": self.memory_id.to_string(),
            "pinned": self.pinned,
            "reason": &self.reason,
            "provenance": &self.provenance,
            "addedAt": &self.added_at,
        })
    }
}

/// Passive active-memory set for one workspace and optional task scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusState {
    pub schema: &'static str,
    pub workspace_id: WorkspaceId,
    pub task_frame_id: Option<String>,
    pub recorder_run_id: Option<String>,
    pub handoff_id: Option<String>,
    pub profile: Option<String>,
    pub capacity: usize,
    pub focal_memory_id: Option<MemoryId>,
    pub items: Vec<FocusItem>,
    pub updated_at: String,
    pub provenance: Vec<String>,
}

impl FocusState {
    /// Create an empty passive focus state.
    ///
    /// # Errors
    ///
    /// Returns [`FocusValidationError`] when capacity is zero or timestamp is empty.
    pub fn new(
        workspace_id: WorkspaceId,
        capacity: usize,
        updated_at: impl Into<String>,
    ) -> Result<Self, FocusValidationError> {
        let state = Self {
            schema: FOCUS_STATE_SCHEMA_V1,
            workspace_id,
            task_frame_id: None,
            recorder_run_id: None,
            handoff_id: None,
            profile: None,
            capacity,
            focal_memory_id: None,
            items: Vec::new(),
            updated_at: updated_at.into(),
            provenance: Vec::new(),
        };
        state.validate()?;
        Ok(state)
    }

    #[must_use]
    pub fn with_task_frame_id(mut self, task_frame_id: impl Into<String>) -> Self {
        self.task_frame_id = Some(task_frame_id.into());
        self
    }

    #[must_use]
    pub fn with_recorder_run_id(mut self, recorder_run_id: impl Into<String>) -> Self {
        self.recorder_run_id = Some(recorder_run_id.into());
        self
    }

    #[must_use]
    pub fn with_handoff_id(mut self, handoff_id: impl Into<String>) -> Self {
        self.handoff_id = Some(handoff_id.into());
        self
    }

    #[must_use]
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    #[must_use]
    pub fn with_provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance.push(provenance.into());
        self.canonicalize();
        self
    }

    /// Set the focal memory. The focal memory must also be present in `items`.
    #[must_use]
    pub const fn with_focal_memory_id(mut self, memory_id: MemoryId) -> Self {
        self.focal_memory_id = Some(memory_id);
        self
    }

    /// Add a memory to the passive focus set.
    ///
    /// # Errors
    ///
    /// Returns [`FocusValidationError`] if the item is invalid, duplicated, or
    /// causes the set to exceed capacity.
    pub fn with_item(mut self, item: FocusItem) -> Result<Self, FocusValidationError> {
        item.validate()?;
        if self
            .items
            .iter()
            .any(|existing| existing.memory_id == item.memory_id)
        {
            return Err(FocusValidationError::DuplicateMemoryId {
                memory_id: item.memory_id.to_string(),
            });
        }
        self.items.push(item);
        self.canonicalize();
        self.validate()?;
        Ok(self)
    }

    fn canonicalize(&mut self) {
        for item in &mut self.items {
            item.canonicalize();
        }
        self.items.sort_by_key(|item| item.memory_id);
        self.provenance.sort();
        self.provenance.dedup();
    }

    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn pinned_count(&self) -> usize {
        self.items.iter().filter(|item| item.pinned).count()
    }

    #[must_use]
    pub fn contains_memory(&self, memory_id: MemoryId) -> bool {
        self.items.iter().any(|item| item.memory_id == memory_id)
    }

    #[must_use]
    pub fn capacity_status(&self) -> FocusCapacityStatus {
        if self.items.len() <= self.capacity {
            FocusCapacityStatus::WithinCapacity
        } else {
            FocusCapacityStatus::Exceeded
        }
    }

    /// Validate passive focus state invariants.
    ///
    /// # Errors
    ///
    /// Returns [`FocusValidationError`] when the state cannot be used
    /// deterministically by context or handoff flows.
    pub fn validate(&self) -> Result<(), FocusValidationError> {
        if self.capacity == 0 {
            return Err(FocusValidationError::ZeroCapacity);
        }
        if self.updated_at.trim().is_empty() {
            return Err(FocusValidationError::EmptyTimestamp { field: "updatedAt" });
        }
        if self.items.len() > self.capacity {
            return Err(FocusValidationError::CapacityExceeded {
                capacity: self.capacity,
                item_count: self.items.len(),
            });
        }

        let mut seen = BTreeSet::new();
        for item in &self.items {
            item.validate()?;
            if !seen.insert(item.memory_id) {
                return Err(FocusValidationError::DuplicateMemoryId {
                    memory_id: item.memory_id.to_string(),
                });
            }
        }

        if let Some(focal_memory_id) = self.focal_memory_id {
            if !seen.contains(&focal_memory_id) {
                return Err(FocusValidationError::FocalMemoryMissing {
                    memory_id: focal_memory_id.to_string(),
                });
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "workspaceId": self.workspace_id.to_string(),
            "taskFrameId": self.task_frame_id.as_deref(),
            "recorderRunId": self.recorder_run_id.as_deref(),
            "handoffId": self.handoff_id.as_deref(),
            "profile": self.profile.as_deref(),
            "capacity": self.capacity,
            "itemCount": self.item_count(),
            "pinnedCount": self.pinned_count(),
            "capacityStatus": self.capacity_status().as_str(),
            "focalMemoryId": self.focal_memory_id.map(|id| id.to_string()),
            "items": self
                .items
                .iter()
                .map(FocusItem::data_json)
                .collect::<Vec<_>>(),
            "updatedAt": &self.updated_at,
            "provenance": &self.provenance,
            "mutationPosture": "passive_state_only",
        })
    }
}

/// Deterministic focus capacity status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusCapacityStatus {
    WithinCapacity,
    Exceeded,
}

impl FocusCapacityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WithinCapacity => "within_capacity",
            Self::Exceeded => "capacity_exceeded",
        }
    }
}

impl fmt::Display for FocusCapacityStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Validation failure for passive focus state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FocusValidationError {
    ZeroCapacity,
    CapacityExceeded { capacity: usize, item_count: usize },
    DuplicateMemoryId { memory_id: String },
    FocalMemoryMissing { memory_id: String },
    EmptyReason { memory_id: String },
    EmptyTimestamp { field: &'static str },
}

impl FocusValidationError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ZeroCapacity => "focus_zero_capacity",
            Self::CapacityExceeded { .. } => "focus_capacity_exceeded",
            Self::DuplicateMemoryId { .. } => "focus_duplicate_memory_id",
            Self::FocalMemoryMissing { .. } => "focus_focal_memory_missing",
            Self::EmptyReason { .. } => "focus_empty_reason",
            Self::EmptyTimestamp { .. } => "focus_empty_timestamp",
        }
    }
}

impl fmt::Display for FocusValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("focus capacity must be greater than zero"),
            Self::CapacityExceeded {
                capacity,
                item_count,
            } => write!(
                formatter,
                "focus contains {item_count} items but capacity is {capacity}"
            ),
            Self::DuplicateMemoryId { memory_id } => {
                write!(formatter, "focus memory id is duplicated: {memory_id}")
            }
            Self::FocalMemoryMissing { memory_id } => {
                write!(
                    formatter,
                    "focal memory is not present in focus items: {memory_id}"
                )
            }
            Self::EmptyReason { memory_id } => {
                write!(formatter, "focus item reason is empty for {memory_id}")
            }
            Self::EmptyTimestamp { field } => {
                write!(formatter, "focus timestamp field `{field}` is empty")
            }
        }
    }
}

impl std::error::Error for FocusValidationError {}

/// Field descriptor used by the focus schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FocusFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl FocusFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

/// Stable JSON-schema-like catalog entry for focus records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FocusObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [FocusFieldSchema],
}

impl FocusObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const FOCUS_ITEM_FIELDS: &[FocusFieldSchema] = &[
    FocusFieldSchema::new("schema", "string", true, "Schema identifier."),
    FocusFieldSchema::new("memoryId", "string", true, "Focused memory identifier."),
    FocusFieldSchema::new(
        "pinned",
        "boolean",
        true,
        "Whether capacity handling must retain it.",
    ),
    FocusFieldSchema::new(
        "reason",
        "string",
        true,
        "Visible reason for including this memory.",
    ),
    FocusFieldSchema::new(
        "provenance",
        "array<string>",
        true,
        "Explicit command, handoff, recorder, or user provenance.",
    ),
    FocusFieldSchema::new("addedAt", "string", true, "RFC 3339 inclusion timestamp."),
];

const FOCUS_STATE_FIELDS: &[FocusFieldSchema] = &[
    FocusFieldSchema::new("schema", "string", true, "Schema identifier."),
    FocusFieldSchema::new("workspaceId", "string", true, "Workspace scope identifier."),
    FocusFieldSchema::new(
        "taskFrameId",
        "string|null",
        false,
        "Optional task-frame scope identifier.",
    ),
    FocusFieldSchema::new(
        "recorderRunId",
        "string|null",
        false,
        "Optional recorder-run scope identifier.",
    ),
    FocusFieldSchema::new(
        "handoffId",
        "string|null",
        false,
        "Optional handoff/resume capsule scope identifier.",
    ),
    FocusFieldSchema::new(
        "profile",
        "string|null",
        false,
        "Optional profile or task label for parallel work in one workspace.",
    ),
    FocusFieldSchema::new(
        "capacity",
        "integer",
        true,
        "Maximum active-memory set size.",
    ),
    FocusFieldSchema::new("itemCount", "integer", true, "Number of focused memories."),
    FocusFieldSchema::new(
        "pinnedCount",
        "integer",
        true,
        "Number of pinned focused memories.",
    ),
    FocusFieldSchema::new(
        "capacityStatus",
        "string",
        true,
        "Deterministic capacity posture.",
    ),
    FocusFieldSchema::new(
        "focalMemoryId",
        "string|null",
        false,
        "Optional focal memory that must also appear in items.",
    ),
    FocusFieldSchema::new(
        "items",
        "array<focus_item>",
        true,
        "Stable active-memory entries sorted by memory ID.",
    ),
    FocusFieldSchema::new("updatedAt", "string", true, "RFC 3339 update timestamp."),
    FocusFieldSchema::new(
        "provenance",
        "array<string>",
        true,
        "Explicit state-level provenance.",
    ),
    FocusFieldSchema::new(
        "mutationPosture",
        "string",
        true,
        "Always passive_state_only; focus state never executes plans.",
    ),
];

#[must_use]
pub const fn focus_schemas() -> [FocusObjectSchema; 2] {
    [
        FocusObjectSchema {
            schema_name: FOCUS_ITEM_SCHEMA_V1,
            schema_uri: "urn:ee:schema:focus-item:v1",
            kind: "focus_item",
            title: "FocusItem",
            description: "One explicit memory entry in a passive active-memory set.",
            fields: FOCUS_ITEM_FIELDS,
        },
        FocusObjectSchema {
            schema_name: FOCUS_STATE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:focus-state:v1",
            kind: "focus_state",
            title: "FocusState",
            description: "Passive active-memory set scoped to a workspace and optional task context.",
            fields: FOCUS_STATE_FIELDS,
        },
    ]
}

#[must_use]
pub fn focus_schema_catalog_json() -> String {
    let schemas = focus_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!("  \"schema\": \"{FOCUS_SCHEMA_CATALOG_V1}\",\n"));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    const FOCUS_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/focus_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn workspace_id(seed: u128) -> WorkspaceId {
        WorkspaceId::from_uuid(Uuid::from_u128(seed))
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn item(seed: u128) -> Result<FocusItem, FocusValidationError> {
        FocusItem::new(
            memory_id(seed),
            "Relevant explicit memory.",
            "2026-05-03T12:00:00Z",
        )
    }

    #[test]
    fn focus_state_accepts_empty_state_and_items() -> TestResult {
        let first = memory_id(10);
        let state = FocusState::new(workspace_id(1), 2, "2026-05-03T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_task_frame_id("task_frame_001")
            .with_recorder_run_id("rrun_001")
            .with_profile("release-work")
            .with_provenance("ee focus set --workspace .")
            .with_focal_memory_id(first)
            .with_item(
                FocusItem::new(first, "Pinned release procedure.", "2026-05-03T12:00:01Z")
                    .map_err(|error| error.to_string())?
                    .pinned(true)
                    .with_provenance("human_explicit"),
            )
            .map_err(|error| error.to_string())?
            .with_item(item(11).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

        state.validate().map_err(|error| error.to_string())?;
        ensure(state.item_count(), 2, "item count")?;
        ensure(state.pinned_count(), 1, "pinned count")?;
        ensure(
            state.capacity_status(),
            FocusCapacityStatus::WithinCapacity,
            "capacity",
        )?;
        ensure(state.contains_memory(first), true, "contains focal")?;
        let state_json = state.data_json();
        ensure(
            state_json
                .get("mutationPosture")
                .and_then(serde_json::Value::as_str),
            Some("passive_state_only"),
            "passive posture",
        )
    }

    #[test]
    fn focus_state_rejects_duplicate_and_missing_focal_memory() -> TestResult {
        let duplicate = memory_id(22);
        let state = FocusState::new(workspace_id(2), 3, "2026-05-03T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_item(
                FocusItem::new(duplicate, "First explicit mention.", "2026-05-03T12:00:00Z")
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;

        let duplicate_result = state.clone().with_item(
            FocusItem::new(
                duplicate,
                "Duplicate should be refused.",
                "2026-05-03T12:00:01Z",
            )
            .map_err(|error| error.to_string())?,
        );
        ensure(
            duplicate_result.map_err(|error| error.code().to_owned()),
            Err("focus_duplicate_memory_id".to_owned()),
            "duplicate memory",
        )?;

        let missing_focal = state.with_focal_memory_id(memory_id(23)).validate();
        ensure(
            missing_focal.map_err(|error| error.code().to_owned()),
            Err("focus_focal_memory_missing".to_owned()),
            "missing focal",
        )
    }

    #[test]
    fn focus_state_rejects_capacity_overflow_and_empty_reason() -> TestResult {
        let overflow = FocusState::new(workspace_id(3), 1, "2026-05-03T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_item(item(30).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?
            .with_item(item(31).map_err(|error| error.to_string())?);
        ensure(
            overflow.map_err(|error| error.code().to_owned()),
            Err("focus_capacity_exceeded".to_owned()),
            "capacity overflow",
        )?;

        let empty_reason = FocusItem::new(memory_id(32), " ", "2026-05-03T12:00:00Z");
        ensure(
            empty_reason.map_err(|error| error.code().to_owned()),
            Err("focus_empty_reason".to_owned()),
            "empty reason",
        )
    }

    #[test]
    fn focus_schema_catalog_matches_golden() {
        assert_eq!(focus_schema_catalog_json(), FOCUS_SCHEMA_GOLDEN);
    }

    #[test]
    fn focus_schemas_are_registered_in_stable_order() -> TestResult {
        let schemas = focus_schemas();
        ensure(schemas.len(), 2, "schema count")?;
        ensure(
            schemas[0].schema_name,
            FOCUS_ITEM_SCHEMA_V1,
            "focus item schema",
        )?;
        ensure(
            schemas[1].schema_name,
            FOCUS_STATE_SCHEMA_V1,
            "focus state schema",
        )
    }
}
