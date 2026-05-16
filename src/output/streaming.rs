use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::pack::{ContextResponse, ContextResponseDegradation, ContextResponseSeverity};

use super::{ContextJsonRenderOptions, render_context_response_json_with_options};

pub const PACK_STREAM_SCHEMA_V1: &str = "ee.pack.stream.v1";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamDegradation {
    pub code: String,
    pub severity: StreamSeverity,
    pub message: String,
    pub repair: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, JsonValue>,
}

impl StreamDegradation {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        severity: StreamSeverity,
        message: impl Into<String>,
        repair: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            repair,
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.details.insert(key.into(), value);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamSeverity {
    Info,
    Low,
    Warning,
    Medium,
    High,
    Critical,
}

impl StreamSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Warning => "warning",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for StreamSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<ContextResponseSeverity> for StreamSeverity {
    fn from(severity: ContextResponseSeverity) -> Self {
        match severity {
            ContextResponseSeverity::Info => Self::Info,
            ContextResponseSeverity::Low => Self::Low,
            ContextResponseSeverity::Medium => Self::Medium,
            ContextResponseSeverity::High => Self::High,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamError {
    pub code: String,
    pub message: String,
    pub severity: StreamSeverity,
    pub repair: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, JsonValue>,
}

impl StreamError {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        severity: StreamSeverity,
        repair: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity,
            repair,
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.details.insert(key.into(), value);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderFrame {
    pub schema: &'static str,
    pub kind: &'static str,
    pub pack_id: String,
    pub query: String,
    pub workspace_id: String,
    pub request_id: String,
    pub profile: String,
    pub max_tokens: u32,
    pub candidate_pool: u32,
    pub memory_scope: String,
    pub strict_scope: bool,
    pub started_at: String,
    pub feature_flags_hash: Option<String>,
    pub canonical_key_hash: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<StreamDegradation>,
}

impl HeaderFrame {
    #[must_use]
    pub fn new(input: HeaderFrameInput) -> Self {
        Self {
            schema: PACK_STREAM_SCHEMA_V1,
            kind: "header",
            pack_id: input.pack_id,
            query: input.query,
            workspace_id: input.workspace_id,
            request_id: input.request_id,
            profile: input.profile,
            max_tokens: input.max_tokens,
            candidate_pool: input.candidate_pool,
            memory_scope: input.memory_scope,
            strict_scope: input.strict_scope,
            started_at: input.started_at,
            feature_flags_hash: None,
            canonical_key_hash: None,
            degraded: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderFrameInput {
    pub pack_id: String,
    pub query: String,
    pub workspace_id: String,
    pub request_id: String,
    pub profile: String,
    pub max_tokens: u32,
    pub candidate_pool: u32,
    pub memory_scope: String,
    pub strict_scope: bool,
    pub started_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemFrame {
    pub schema: &'static str,
    pub kind: &'static str,
    pub pack_id: String,
    pub seq: u32,
    pub rank: u32,
    pub memory_id: String,
    pub section: String,
    pub content: String,
    pub estimated_tokens: u32,
    pub scores: BTreeMap<String, JsonValue>,
    pub why: String,
    pub provenance: Vec<BTreeMap<String, JsonValue>>,
    pub trust: BTreeMap<String, JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diversity_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_in: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lifecycle: BTreeMap<String, JsonValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<BTreeMap<String, JsonValue>>,
}

impl ItemFrame {
    #[must_use]
    pub fn new(input: ItemFrameInput) -> Self {
        Self {
            schema: PACK_STREAM_SCHEMA_V1,
            kind: "item",
            pack_id: input.pack_id,
            seq: input.seq,
            rank: input.rank,
            memory_id: input.memory_id,
            section: input.section,
            content: input.content,
            estimated_tokens: input.estimated_tokens,
            scores: BTreeMap::new(),
            why: input.why,
            provenance: Vec::new(),
            trust: BTreeMap::new(),
            diversity_key: None,
            selected_in: None,
            lifecycle: BTreeMap::new(),
            redactions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemFrameInput {
    pub pack_id: String,
    pub seq: u32,
    pub rank: u32,
    pub memory_id: String,
    pub section: String,
    pub content: String,
    pub estimated_tokens: u32,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrailerFrame {
    pub schema: &'static str,
    pub kind: &'static str,
    pub pack_id: String,
    pub pack_hash: String,
    pub total_items: u32,
    pub used_tokens: u32,
    pub selection_audit: BTreeMap<String, JsonValue>,
    pub quality: BTreeMap<String, JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_total: Option<u32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance_footer: BTreeMap<String, JsonValue>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub coordination: BTreeMap<String, JsonValue>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pack_dna: BTreeMap<String, JsonValue>,
    pub degraded: Vec<StreamDegradation>,
    pub completed_at: String,
}

impl TrailerFrame {
    #[must_use]
    pub fn new(
        pack_id: impl Into<String>,
        pack_hash: impl Into<String>,
        total_items: u32,
        used_tokens: u32,
        completed_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: PACK_STREAM_SCHEMA_V1,
            kind: "trailer",
            pack_id: pack_id.into(),
            pack_hash: pack_hash.into(),
            total_items,
            used_tokens,
            selection_audit: BTreeMap::new(),
            quality: BTreeMap::new(),
            skipped_total: None,
            provenance_footer: BTreeMap::new(),
            coordination: BTreeMap::new(),
            pack_dna: BTreeMap::new(),
            degraded: Vec::new(),
            completed_at: completed_at.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrame {
    pub schema: &'static str,
    pub kind: TerminalKind,
    pub pack_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitted_items: Option<u32>,
    pub error: StreamError,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<StreamDegradation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl TerminalFrame {
    #[must_use]
    pub fn error(pack_id: Option<String>, error: StreamError) -> Self {
        Self {
            schema: PACK_STREAM_SCHEMA_V1,
            kind: TerminalKind::Error,
            pack_id,
            emitted_items: None,
            error,
            degraded: Vec::new(),
            completed_at: None,
        }
    }

    #[must_use]
    pub fn cancelled(pack_id: Option<String>, error: StreamError) -> Self {
        Self {
            schema: PACK_STREAM_SCHEMA_V1,
            kind: TerminalKind::Cancelled,
            pack_id,
            emitted_items: None,
            error,
            degraded: Vec::new(),
            completed_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PackStreamFrame {
    Header(HeaderFrame),
    Item(ItemFrame),
    Trailer(TrailerFrame),
    Terminal(TerminalFrame),
}

impl PackStreamFrame {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Header(_) => "header",
            Self::Item(_) => "item",
            Self::Trailer(_) => "trailer",
            Self::Terminal(frame) => match frame.kind {
                TerminalKind::Error => "error",
                TerminalKind::Cancelled => "cancelled",
            },
        }
    }
}

pub struct PackStreamWriter<W: Write> {
    writer: W,
    frames_written: u32,
}

impl<W: Write> PackStreamWriter<W> {
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self {
            writer,
            frames_written: 0,
        }
    }

    /// Write one frame as one NDJSON line and flush immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization, writing, or flushing fails.
    pub fn write_frame(&mut self, frame: &PackStreamFrame) -> io::Result<()> {
        let bytes = serde_json::to_vec(frame)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.writer.write_all(&bytes)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.frames_written += 1;
        Ok(())
    }

    #[must_use]
    pub const fn frames_written(&self) -> u32 {
        self.frames_written
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamValidationError {
    MissingHeader,
    MissingTerminal,
    DuplicateHeader,
    ItemBeforeHeader,
    TerminalBeforeHeader,
    FrameAfterTerminal,
    DuplicateTerminal,
    UnexpectedItemSeq { expected: u32, actual: u32 },
    UnexpectedItemRank { expected: u32, actual: u32 },
    TrailerItemCountMismatch { expected: u32, actual: u32 },
}

impl fmt::Display for StreamValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => formatter.write_str("stream ended without a header frame"),
            Self::MissingTerminal => formatter.write_str("stream ended without a terminal frame"),
            Self::DuplicateHeader => formatter.write_str("stream contains more than one header"),
            Self::ItemBeforeHeader => formatter.write_str("item frame appeared before header"),
            Self::TerminalBeforeHeader => {
                formatter.write_str("terminal frame appeared before header")
            }
            Self::FrameAfterTerminal => formatter.write_str("frame appeared after terminal frame"),
            Self::DuplicateTerminal => {
                formatter.write_str("stream contains more than one terminal frame")
            }
            Self::UnexpectedItemSeq { expected, actual } => {
                write!(formatter, "expected item seq {expected}, got {actual}")
            }
            Self::UnexpectedItemRank { expected, actual } => {
                write!(formatter, "expected item rank {expected}, got {actual}")
            }
            Self::TrailerItemCountMismatch { expected, actual } => {
                write!(
                    formatter,
                    "trailer totalItems {actual} did not match emitted item count {expected}"
                )
            }
        }
    }
}

impl std::error::Error for StreamValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextStreamFrameOptions {
    pub pack_id: String,
    pub workspace_id: String,
    pub request_id: String,
    pub started_at: String,
    pub completed_at: String,
}

impl ContextStreamFrameOptions {
    #[must_use]
    pub fn new(
        pack_id: impl Into<String>,
        workspace_id: impl Into<String>,
        request_id: impl Into<String>,
        started_at: impl Into<String>,
        completed_at: impl Into<String>,
    ) -> Self {
        Self {
            pack_id: pack_id.into(),
            workspace_id: workspace_id.into(),
            request_id: request_id.into(),
            started_at: started_at.into(),
            completed_at: completed_at.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextStreamAdapterError {
    BatchJsonParse(String),
    MissingField(&'static str),
    InvalidField(&'static str),
    NumericOverflow(&'static str),
    Validation(StreamValidationError),
}

impl fmt::Display for ContextStreamAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchJsonParse(error) => {
                write!(formatter, "batch context JSON could not be parsed: {error}")
            }
            Self::MissingField(field) => write!(formatter, "batch context JSON is missing {field}"),
            Self::InvalidField(field) => {
                write!(
                    formatter,
                    "batch context JSON field {field} has an invalid shape"
                )
            }
            Self::NumericOverflow(field) => {
                write!(
                    formatter,
                    "batch context JSON field {field} does not fit in u32"
                )
            }
            Self::Validation(error) => {
                write!(formatter, "stream frame sequence is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for ContextStreamAdapterError {}

/// Convert an already-assembled batch context response into typed stream
/// frames without changing selection order or pack hash.
///
/// The adapter reads item/trailer material from the canonical batch JSON
/// renderer, so stream output stays aligned with the stable batch envelope
/// until the CLI streaming path is wired directly into context assembly.
///
/// # Errors
///
/// Returns [`ContextStreamAdapterError`] if the canonical batch JSON renderer
/// emits an unexpected shape or the generated frame sequence violates the
/// stream ordering contract.
pub fn context_response_stream_frames(
    response: &ContextResponse,
    options: ContextStreamFrameOptions,
) -> Result<Vec<PackStreamFrame>, ContextStreamAdapterError> {
    let batch_json = render_context_response_json_with_options(
        response,
        ContextJsonRenderOptions {
            include_rendered_text: false,
            include_skipped: true,
            include_meta: true,
            include_verbose_meta: false,
            include_non_affecting_degradations: false,
            include_legacy_selection_certificate: false,
        },
    );
    let root: JsonValue = serde_json::from_str(&batch_json)
        .map_err(|error| ContextStreamAdapterError::BatchJsonParse(error.to_string()))?;
    let pack = required_object(&root, "data.pack")?;
    let item_values = pack
        .get("items")
        .ok_or(ContextStreamAdapterError::MissingField("data.pack.items"))?
        .as_array()
        .cloned()
        .ok_or(ContextStreamAdapterError::InvalidField("data.pack.items"))?;
    let pack_hash = pack_hash_from_batch(&pack);

    let memory_scope = response
        .data
        .scope_stats
        .as_ref()
        .map_or("swarm", |stats| stats.scope_applied.as_str())
        .to_string();
    let strict_scope = response
        .data
        .scope_stats
        .as_ref()
        .is_some_and(|stats| stats.strict_scope);

    let mut header = HeaderFrame::new(HeaderFrameInput {
        pack_id: options.pack_id.clone(),
        query: response.data.request.query.clone(),
        workspace_id: options.workspace_id.clone(),
        request_id: options.request_id,
        profile: response.data.request.profile.as_str().to_string(),
        max_tokens: response.data.request.budget.max_tokens(),
        candidate_pool: response.data.request.candidate_pool,
        memory_scope,
        strict_scope,
        started_at: options.started_at,
    });
    if pack_hash != "absent" {
        header.canonical_key_hash = Some(pack_hash.clone());
    }

    let mut frames = Vec::with_capacity(item_values.len().saturating_add(2));
    frames.push(PackStreamFrame::Header(header));

    for (seq, value) in item_values.iter().enumerate() {
        frames.push(PackStreamFrame::Item(item_frame_from_batch_item(
            &options.pack_id,
            u32_from_usize(seq, "data.pack.items[].seq")?,
            value,
        )?));
    }

    let mut trailer = TrailerFrame::new(
        options.pack_id,
        pack_hash,
        u32_from_usize(item_values.len(), "data.pack.items.length")?,
        response.data.pack.used_tokens,
        options.completed_at,
    );
    trailer.selection_audit = optional_object(&pack, "selectionAudit")?.unwrap_or_default();
    trailer.quality = optional_object(&pack, "quality")?.unwrap_or_default();
    trailer.skipped_total = optional_u32(&pack, "skippedTotal")?;
    trailer.provenance_footer = optional_object(&pack, "provenanceFooter")?.unwrap_or_default();
    trailer.coordination = optional_object(&pack, "coordination")?.unwrap_or_default();
    trailer.pack_dna = optional_object(&pack, "packDna")?.unwrap_or_default();
    trailer.degraded = response
        .data
        .degraded
        .iter()
        .map(stream_degradation_from_context)
        .collect();
    frames.push(PackStreamFrame::Trailer(trailer));

    validate_generated_frames(&frames)?;
    Ok(frames)
}

fn item_frame_from_batch_item(
    pack_id: &str,
    seq: u32,
    value: &JsonValue,
) -> Result<ItemFrame, ContextStreamAdapterError> {
    let item = value
        .as_object()
        .ok_or(ContextStreamAdapterError::InvalidField("data.pack.items[]"))?;
    let mut frame = ItemFrame::new(ItemFrameInput {
        pack_id: pack_id.to_string(),
        seq,
        rank: required_u32(item, "rank")?,
        memory_id: required_string(item, "memoryId")?.to_string(),
        section: required_string(item, "section")?.to_string(),
        content: required_string(item, "content")?.to_string(),
        estimated_tokens: required_u32(item, "estimatedTokens")?,
        why: required_string(item, "why")?.to_string(),
    });
    frame.scores = optional_object(item, "scores")?.unwrap_or_default();
    frame.provenance = optional_array_of_objects(item, "provenance")?.unwrap_or_default();
    frame.trust = optional_object(item, "trust")?.unwrap_or_default();
    frame.diversity_key = optional_string(item, "diversityKey").map(str::to_string);
    frame.selected_in = optional_string(item, "selectedIn").map(str::to_string);
    frame.lifecycle = optional_object(item, "lifecycle")?.unwrap_or_default();
    frame.redactions = optional_array_of_objects(item, "redactions")?.unwrap_or_default();
    Ok(frame)
}

fn stream_degradation_from_context(degradation: &ContextResponseDegradation) -> StreamDegradation {
    StreamDegradation::new(
        degradation.code.clone(),
        StreamSeverity::from(degradation.severity),
        degradation.message.clone(),
        degradation.repair.clone(),
    )
}

fn validate_generated_frames(frames: &[PackStreamFrame]) -> Result<(), ContextStreamAdapterError> {
    let mut validator = StreamSequenceValidator::new();
    for frame in frames {
        validator
            .observe(frame)
            .map_err(ContextStreamAdapterError::Validation)?;
    }
    validator
        .finish()
        .map_err(ContextStreamAdapterError::Validation)
}

fn pack_hash_from_batch(pack: &serde_json::Map<String, JsonValue>) -> String {
    pack.get("hash")
        .and_then(JsonValue::as_str)
        .unwrap_or("absent")
        .to_string()
}

fn required_object(
    value: &JsonValue,
    path: &'static str,
) -> Result<serde_json::Map<String, JsonValue>, ContextStreamAdapterError> {
    let mut current = value;
    for segment in path.split('.') {
        current = current
            .get(segment)
            .ok_or(ContextStreamAdapterError::MissingField(path))?;
    }
    current
        .as_object()
        .cloned()
        .ok_or(ContextStreamAdapterError::InvalidField(path))
}

fn required_string<'a>(
    item: &'a serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<&'a str, ContextStreamAdapterError> {
    item.get(field)
        .ok_or(ContextStreamAdapterError::MissingField(field))?
        .as_str()
        .ok_or(ContextStreamAdapterError::InvalidField(field))
}

fn optional_string<'a>(
    item: &'a serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Option<&'a str> {
    item.get(field).and_then(JsonValue::as_str)
}

fn required_u32(
    item: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<u32, ContextStreamAdapterError> {
    let value = item
        .get(field)
        .ok_or(ContextStreamAdapterError::MissingField(field))?
        .as_u64()
        .ok_or(ContextStreamAdapterError::InvalidField(field))?;
    u32::try_from(value).map_err(|_| ContextStreamAdapterError::NumericOverflow(field))
}

fn optional_u32(
    item: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<Option<u32>, ContextStreamAdapterError> {
    match item.get(field) {
        Some(value) => {
            let Some(number) = value.as_u64() else {
                return Err(ContextStreamAdapterError::InvalidField(field));
            };
            Ok(Some(u32::try_from(number).map_err(|_| {
                ContextStreamAdapterError::NumericOverflow(field)
            })?))
        }
        None => Ok(None),
    }
}

fn optional_object(
    item: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<Option<BTreeMap<String, JsonValue>>, ContextStreamAdapterError> {
    item.get(field).map(object_to_btree).transpose()
}

fn optional_array_of_objects(
    item: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<Option<Vec<BTreeMap<String, JsonValue>>>, ContextStreamAdapterError> {
    let Some(value) = item.get(field) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or(ContextStreamAdapterError::InvalidField(field))?;
    array
        .iter()
        .map(object_to_btree)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn object_to_btree(
    value: &JsonValue,
) -> Result<BTreeMap<String, JsonValue>, ContextStreamAdapterError> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .ok_or(ContextStreamAdapterError::InvalidField("object"))
}

fn u32_from_usize(value: usize, field: &'static str) -> Result<u32, ContextStreamAdapterError> {
    u32::try_from(value).map_err(|_| ContextStreamAdapterError::NumericOverflow(field))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamSequenceValidator {
    saw_header: bool,
    saw_terminal: bool,
    next_seq: u32,
    next_rank: u32,
}

impl StreamSequenceValidator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            saw_header: false,
            saw_terminal: false,
            next_seq: 0,
            next_rank: 1,
        }
    }

    /// Observe one frame and enforce the stream order and item counters.
    ///
    /// # Errors
    ///
    /// Returns an error when frame order, `seq`, `rank`, or trailer totals
    /// violate the `ee.pack.stream.v1` contract.
    pub fn observe(&mut self, frame: &PackStreamFrame) -> Result<(), StreamValidationError> {
        if self.saw_terminal {
            return match frame {
                PackStreamFrame::Trailer(_) | PackStreamFrame::Terminal(_) => {
                    Err(StreamValidationError::DuplicateTerminal)
                }
                PackStreamFrame::Header(_) | PackStreamFrame::Item(_) => {
                    Err(StreamValidationError::FrameAfterTerminal)
                }
            };
        }

        match frame {
            PackStreamFrame::Header(_) => self.observe_header(),
            PackStreamFrame::Item(item) => self.observe_item(item),
            PackStreamFrame::Trailer(trailer) => self.observe_trailer(trailer),
            PackStreamFrame::Terminal(_) => self.observe_terminal(),
        }
    }

    /// Finish validation after the last frame.
    ///
    /// # Errors
    ///
    /// Returns an error when no header or terminal frame was observed.
    pub const fn finish(&self) -> Result<(), StreamValidationError> {
        if !self.saw_header {
            Err(StreamValidationError::MissingHeader)
        } else if !self.saw_terminal {
            Err(StreamValidationError::MissingTerminal)
        } else {
            Ok(())
        }
    }

    fn observe_header(&mut self) -> Result<(), StreamValidationError> {
        if self.saw_header {
            return Err(StreamValidationError::DuplicateHeader);
        }
        self.saw_header = true;
        Ok(())
    }

    fn observe_item(&mut self, item: &ItemFrame) -> Result<(), StreamValidationError> {
        if !self.saw_header {
            return Err(StreamValidationError::ItemBeforeHeader);
        }
        if item.seq != self.next_seq {
            return Err(StreamValidationError::UnexpectedItemSeq {
                expected: self.next_seq,
                actual: item.seq,
            });
        }
        if item.rank != self.next_rank {
            return Err(StreamValidationError::UnexpectedItemRank {
                expected: self.next_rank,
                actual: item.rank,
            });
        }
        self.next_seq += 1;
        self.next_rank += 1;
        Ok(())
    }

    fn observe_trailer(&mut self, trailer: &TrailerFrame) -> Result<(), StreamValidationError> {
        if !self.saw_header {
            return Err(StreamValidationError::TerminalBeforeHeader);
        }
        if self.saw_terminal {
            return Err(StreamValidationError::DuplicateTerminal);
        }
        if trailer.total_items != self.next_seq {
            return Err(StreamValidationError::TrailerItemCountMismatch {
                expected: self.next_seq,
                actual: trailer.total_items,
            });
        }
        self.saw_terminal = true;
        Ok(())
    }

    fn observe_terminal(&mut self) -> Result<(), StreamValidationError> {
        if !self.saw_header {
            return Err(StreamValidationError::TerminalBeforeHeader);
        }
        if self.saw_terminal {
            return Err(StreamValidationError::DuplicateTerminal);
        }
        self.saw_terminal = true;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn header() -> PackStreamFrame {
        PackStreamFrame::Header(HeaderFrame::new(HeaderFrameInput {
            pack_id: "pack_1".to_string(),
            query: "prepare release".to_string(),
            workspace_id: "workspace_1".to_string(),
            request_id: "request_1".to_string(),
            profile: "compact".to_string(),
            max_tokens: 512,
            candidate_pool: 20,
            memory_scope: "workspace".to_string(),
            strict_scope: true,
            started_at: "2026-05-16T00:00:00Z".to_string(),
        }))
    }

    fn item(seq: u32, rank: u32) -> PackStreamFrame {
        let mut frame = ItemFrame::new(ItemFrameInput {
            pack_id: "pack_1".to_string(),
            seq,
            rank,
            memory_id: format!("mem_{rank}"),
            section: "procedural_rules".to_string(),
            content: "Run cargo fmt --check.".to_string(),
            estimated_tokens: 6,
            why: "matched release procedure".to_string(),
        });
        frame
            .scores
            .insert("relevance".to_string(), JsonValue::from(0.91));
        frame
            .trust
            .insert("posture".to_string(), JsonValue::from("authoritative"));
        PackStreamFrame::Item(frame)
    }

    fn trailer(total_items: u32) -> PackStreamFrame {
        PackStreamFrame::Trailer(TrailerFrame::new(
            "pack_1",
            "blake3:abc",
            total_items,
            12,
            "2026-05-16T00:00:01Z",
        ))
    }

    fn error_frame() -> PackStreamFrame {
        PackStreamFrame::Terminal(TerminalFrame::error(
            Some("pack_1".to_string()),
            StreamError::new(
                "stream_failed",
                "stream failed",
                StreamSeverity::Medium,
                Some("retry the command".to_string()),
            ),
        ))
    }

    fn memory_id(value: u128) -> crate::models::MemoryId {
        crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(value))
    }

    fn pack_candidate(id_value: u128, content: &str, relevance: f32) -> crate::pack::PackCandidate {
        let id = memory_id(id_value);
        crate::pack::PackCandidate::new(crate::pack::PackCandidateInput {
            memory_id: id,
            section: crate::pack::PackSection::ProceduralRules,
            content: content.to_string(),
            estimated_tokens: 8,
            relevance: crate::models::UnitScore::parse(relevance).unwrap(),
            utility: crate::models::UnitScore::parse(0.8).unwrap(),
            provenance: vec![
                crate::pack::PackProvenance::new(
                    crate::models::ProvenanceUri::EeMemory(id),
                    "stream adapter test",
                )
                .unwrap(),
            ],
            why: format!("matched stream adapter fixture {id}"),
        })
        .unwrap()
    }

    fn context_response_fixture() -> crate::pack::ContextResponse {
        let request = crate::pack::ContextRequest::from_query("prepare release stream").unwrap();
        let mut draft = crate::pack::assemble_draft(
            "prepare release stream",
            crate::pack::TokenBudget::new(64).unwrap(),
            vec![
                pack_candidate(0x9001, "Run cargo fmt --check.", 0.93),
                pack_candidate(0x9002, "Run RCH-only focused tests.", 0.81),
            ],
        )
        .unwrap();
        draft.hash = Some("blake3:stream-fixture-pack".to_string());
        crate::pack::ContextResponse::new(
            request,
            draft,
            vec![
                crate::pack::ContextResponseDegradation::new(
                    "stream_fixture_degraded",
                    crate::pack::ContextResponseSeverity::Medium,
                    "fixture degradation",
                    Some("inspect fixture".to_string()),
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    fn stream_options() -> ContextStreamFrameOptions {
        ContextStreamFrameOptions::new(
            "pack_stream_fixture",
            "workspace_fixture",
            "request_fixture",
            "2026-05-16T00:00:00Z",
            "2026-05-16T00:00:01Z",
        )
    }

    #[test]
    fn happy_path_context_response_stream_frames_preserve_batch_order_and_hash() {
        let response = context_response_fixture();
        let batch: JsonValue =
            serde_json::from_str(&crate::output::render_context_response_json(&response)).unwrap();
        let frames = context_response_stream_frames(&response, stream_options()).unwrap();

        let mut validator = StreamSequenceValidator::new();
        for frame in &frames {
            validator.observe(frame).unwrap();
        }
        validator.finish().unwrap();

        assert_eq!(frames.len(), response.data.pack.items.len() + 2);
        let PackStreamFrame::Header(header) = &frames[0] else {
            panic!("first frame should be header");
        };
        assert_eq!(header.query, response.data.request.query);
        assert_eq!(
            header.canonical_key_hash.as_deref(),
            Some("blake3:stream-fixture-pack")
        );

        for (index, batch_item) in response.data.pack.items.iter().enumerate() {
            let PackStreamFrame::Item(frame) = &frames[index + 1] else {
                panic!("middle frame should be item");
            };
            assert_eq!(frame.seq, u32::try_from(index).unwrap());
            assert_eq!(frame.rank, batch_item.rank);
            assert_eq!(frame.memory_id, batch_item.memory_id.to_string());
            assert_eq!(frame.content, batch_item.content);
            assert_eq!(
                frame.selected_in.as_deref(),
                Some(batch_item.selected_in.as_str())
            );
        }

        let PackStreamFrame::Trailer(trailer) = frames.last().unwrap() else {
            panic!("last frame should be trailer");
        };
        assert_eq!(
            Some(trailer.pack_hash.as_str()),
            batch["data"]["pack"]["hash"].as_str()
        );
        assert_eq!(
            trailer.total_items,
            u32::try_from(response.data.pack.items.len()).unwrap()
        );
    }

    #[test]
    fn happy_path_context_response_stream_trailer_carries_delayed_batch_sections() {
        let response = context_response_fixture();
        let frames = context_response_stream_frames(&response, stream_options()).unwrap();
        let PackStreamFrame::Trailer(trailer) = frames.last().unwrap() else {
            panic!("last frame should be trailer");
        };

        assert_eq!(
            trailer.selection_audit["algorithmId"],
            "mmr_with_coverage_fill_v1"
        );
        assert_eq!(trailer.quality["itemCount"], JsonValue::from(2));
        assert_eq!(trailer.provenance_footer["memoryCount"], JsonValue::from(2));
        assert_eq!(trailer.skipped_total, Some(0));
        assert_eq!(trailer.degraded.len(), 1);
        assert_eq!(trailer.degraded[0].code, "stream_fixture_degraded");
        assert_eq!(trailer.degraded[0].severity, StreamSeverity::Medium);
    }

    #[test]
    fn happy_path_writer_serializes_one_json_object_per_line_and_flushes() {
        let sink = FlushCountingWriter::default();
        let mut writer = PackStreamWriter::new(sink);

        writer.write_frame(&header()).unwrap();
        writer.write_frame(&item(0, 1)).unwrap();
        writer.write_frame(&trailer(1)).unwrap();

        assert_eq!(writer.frames_written(), 3);
        let sink = writer.into_inner();
        assert_eq!(sink.flush_count, 3);
        let output = String::from_utf8(sink.bytes).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            serde_json::from_str::<JsonValue>(lines[0]).unwrap()["kind"],
            JsonValue::from("header")
        );
        assert_eq!(
            serde_json::from_str::<JsonValue>(lines[1]).unwrap()["kind"],
            JsonValue::from("item")
        );
        assert_eq!(
            serde_json::from_str::<JsonValue>(lines[2]).unwrap()["kind"],
            JsonValue::from("trailer")
        );
    }

    #[test]
    fn happy_path_sequence_validator_accepts_header_items_trailer() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&item(0, 1)).unwrap();
        validator.observe(&item(1, 2)).unwrap();
        validator.observe(&trailer(2)).unwrap();

        assert_eq!(validator.finish(), Ok(()));
    }

    #[test]
    fn happy_path_optional_fields_are_omitted_when_absent() {
        let trailer = trailer(0);
        let terminal = error_frame();

        let trailer_json = serde_json::to_value(&trailer).unwrap();
        let terminal_json = serde_json::to_value(&terminal).unwrap();

        assert!(trailer_json.get("skippedTotal").is_none());
        assert!(terminal_json.get("emittedItems").is_none());
        assert!(terminal_json.get("completedAt").is_none());
    }

    #[test]
    fn happy_path_error_and_cancelled_terminal_frames_serialize_expected_kind() {
        let mut cancelled = TerminalFrame::cancelled(
            Some("pack_1".to_string()),
            StreamError::new("cancelled", "cancelled", StreamSeverity::Low, None),
        );
        cancelled.emitted_items = Some(2);
        cancelled.completed_at = Some("2026-05-16T00:00:02Z".to_string());

        let error_json = serde_json::to_value(error_frame()).unwrap();
        let cancelled_json = serde_json::to_value(PackStreamFrame::Terminal(cancelled)).unwrap();

        assert_eq!(error_json["kind"], JsonValue::from("error"));
        assert_eq!(error_json["packId"], JsonValue::from("pack_1"));
        assert_eq!(error_json["error"]["severity"], JsonValue::from("medium"));
        assert_eq!(cancelled_json["kind"], JsonValue::from("cancelled"));
        assert_eq!(cancelled_json["emittedItems"], JsonValue::from(2));
        assert_eq!(
            cancelled_json["completedAt"],
            JsonValue::from("2026-05-16T00:00:02Z")
        );
    }

    #[test]
    fn happy_path_cancelled_terminal_is_valid_after_partial_items() {
        let mut terminal = TerminalFrame::cancelled(
            Some("pack_1".to_string()),
            StreamError::new("cancelled", "cancelled", StreamSeverity::Low, None),
        );
        terminal.emitted_items = Some(1);
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&item(0, 1)).unwrap();
        validator
            .observe(&PackStreamFrame::Terminal(terminal))
            .unwrap();

        assert_eq!(validator.finish(), Ok(()));
    }

    #[test]
    fn empty_or_boundary_header_without_items_then_empty_trailer_is_valid() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&trailer(0)).unwrap();

        assert_eq!(validator.finish(), Ok(()));
    }

    #[test]
    fn error_or_invalid_missing_terminal_is_rejected_at_finish() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();

        assert_eq!(
            validator.finish(),
            Err(StreamValidationError::MissingTerminal)
        );
    }

    #[test]
    fn error_or_invalid_item_before_header_is_rejected() {
        let mut validator = StreamSequenceValidator::new();

        assert_eq!(
            validator.observe(&item(0, 1)),
            Err(StreamValidationError::ItemBeforeHeader)
        );
    }

    #[test]
    fn error_or_invalid_unexpected_seq_and_rank_are_rejected() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        assert_eq!(
            validator.observe(&item(1, 1)),
            Err(StreamValidationError::UnexpectedItemSeq {
                expected: 0,
                actual: 1
            })
        );

        let mut validator = StreamSequenceValidator::new();
        validator.observe(&header()).unwrap();
        assert_eq!(
            validator.observe(&item(0, 2)),
            Err(StreamValidationError::UnexpectedItemRank {
                expected: 1,
                actual: 2
            })
        );
    }

    #[test]
    fn error_or_invalid_trailer_total_must_match_emitted_items() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&item(0, 1)).unwrap();

        assert_eq!(
            validator.observe(&trailer(2)),
            Err(StreamValidationError::TrailerItemCountMismatch {
                expected: 1,
                actual: 2
            })
        );
    }

    #[test]
    fn error_or_invalid_frame_after_terminal_is_rejected() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&error_frame()).unwrap();

        assert_eq!(
            validator.observe(&item(0, 1)),
            Err(StreamValidationError::FrameAfterTerminal)
        );
    }

    #[test]
    fn error_or_invalid_duplicate_terminal_is_rejected() {
        let mut validator = StreamSequenceValidator::new();

        validator.observe(&header()).unwrap();
        validator.observe(&trailer(0)).unwrap();

        assert_eq!(
            validator.observe(&error_frame()),
            Err(StreamValidationError::DuplicateTerminal)
        );
    }

    #[test]
    fn error_or_invalid_write_and_flush_errors_propagate() {
        let mut writer = PackStreamWriter::new(FailingWriter {
            fail_write: true,
            fail_flush: false,
            bytes: Vec::new(),
        });

        let write_error = writer.write_frame(&header()).unwrap_err();
        assert_eq!(write_error.kind(), io::ErrorKind::BrokenPipe);

        let mut writer = PackStreamWriter::new(FailingWriter {
            fail_write: false,
            fail_flush: true,
            bytes: Vec::new(),
        });

        let flush_error = writer.write_frame(&header()).unwrap_err();
        assert_eq!(flush_error.kind(), io::ErrorKind::Other);
    }

    #[derive(Default)]
    struct FlushCountingWriter {
        bytes: Vec<u8>,
        flush_count: u32,
    }

    impl Write for FlushCountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_count += 1;
            Ok(())
        }
    }

    struct FailingWriter {
        fail_write: bool,
        fail_flush: bool,
        bytes: Vec<u8>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }
}
