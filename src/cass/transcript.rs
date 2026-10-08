//! Shared, bounded reader and learning projections of screened CASS evidence.
//!
//! A projection is derived text, never a replacement for the stored envelope,
//! content hash, or source locator. Every structured record must decode and
//! pass the same authority checks. Unknown records, tools and privileged roles
//! never become raw-text fallbacks. Reader projections omit reasoning; learning
//! additionally refuses any record containing reasoning or a summary.

use std::borrow::Cow;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

use super::CassRole;

pub(crate) const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TEXT_BLOCKS: usize = 256;
pub(crate) const MAX_TRANSCRIPT_RECORDS: usize = 256;
pub(crate) const MAX_ENVELOPE_DEPTH: usize = 8;

/// Interpretation version for projections derived from stored source bytes.
pub const TRANSCRIPT_PROJECTION_VERSION: u32 = 2;

/// Kinds currently admitted into a reader projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptProjectionKind {
    /// User or assistant text, or an already-unstructured evidence excerpt.
    Text,
    /// An explicitly identified transcript summary.
    Summary,
}

impl TranscriptProjectionKind {
    /// Stable spelling for projection metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Summary => "summary",
        }
    }
}

/// One readable record, decoded exactly once from a screened evidence span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptProjection<'a> {
    /// Canonical source role; absent when the source did not declare one.
    pub role: Option<CassRole>,
    /// The admitted source interpretation, independent of the durable span kind.
    pub kind: TranscriptProjectionKind,
    /// Decoded text, with reasoning and envelope metadata excluded.
    pub text: Cow<'a, str>,
    /// UTF-8 byte count of `text`, without a reader role label.
    pub text_bytes: usize,
    /// Version of the deterministic projection rules that produced this record.
    pub projection_version: u32,
}

impl TranscriptProjection<'_> {
    /// Format this record for retrieval without exposing JSON envelope fields.
    #[must_use]
    pub fn reader_text(&self) -> Cow<'_, str> {
        match (self.role, self.kind) {
            (Some(role), _) => Cow::Owned(format!("{}: {}", role.as_str(), self.text)),
            (None, TranscriptProjectionKind::Summary) => {
                Cow::Owned(format!("summary: {}", self.text))
            }
            (None, TranscriptProjectionKind::Text) => Cow::Borrowed(self.text.as_ref()),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProjectionPurpose {
    Reader,
    Learning,
}

/// Project a complete excerpt into admitted reader records in source order.
///
/// Plain evidence retains its exact bytes. Structured input is a single JSON
/// record or a bounded JSONL window, including pretty-printed records. A reader
/// may omit an otherwise valid reasoning-only record, but no unknown, empty,
/// malformed, tool or privileged record can be skipped to salvage a window.
#[must_use]
pub fn project_transcript(excerpt: &str) -> Option<Vec<TranscriptProjection<'_>>> {
    project_with(excerpt, ProjectionPurpose::Reader)
}

pub(crate) fn message_text(excerpt: &str) -> Option<Cow<'_, str>> {
    join_projections(project_with(excerpt, ProjectionPurpose::Learning)?, false)
}

/// Reader body text without labels. Reasoning is never returned to a reader.
pub(crate) fn display_text(excerpt: &str) -> Option<Cow<'_, str>> {
    join_projections(project_transcript(excerpt)?, false)
}

/// Reader text with each record's own canonical role, preserving JSONL order.
pub(crate) fn reader_text(excerpt: &str) -> Option<Cow<'_, str>> {
    join_projections(project_transcript(excerpt)?, true)
}

fn join_projections(
    mut projections: Vec<TranscriptProjection<'_>>,
    include_labels: bool,
) -> Option<Cow<'_, str>> {
    if projections.len() == 1 {
        let projection = projections.pop()?;
        if !include_labels
            || (projection.role.is_none() && projection.kind == TranscriptProjectionKind::Text)
        {
            return Some(projection.text);
        }
        return Some(Cow::Owned(projection.reader_text().into_owned()));
    }
    let mut text = String::new();
    for projection in projections {
        let body = if include_labels {
            projection.reader_text()
        } else {
            Cow::Borrowed(projection.text.as_ref())
        };
        append_bounded(&mut text, &body)?;
    }
    (!text.is_empty()).then_some(Cow::Owned(text))
}

fn project_with(
    excerpt: &str,
    purpose: ProjectionPurpose,
) -> Option<Vec<TranscriptProjection<'_>>> {
    if excerpt.len() > MAX_SOURCE_BYTES || excerpt.trim().is_empty() {
        return None;
    }
    let start = excerpt.trim_start();
    if !start.starts_with('{') && !start.starts_with('[') {
        // Plain evidence keeps its exact historical interpretation and bytes.
        return Some(vec![TranscriptProjection {
            role: None,
            kind: TranscriptProjectionKind::Text,
            text: Cow::Borrowed(excerpt),
            text_bytes: excerpt.len(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION,
        }]);
    }
    // Stream values rather than splitting lines: pretty-printed envelopes and
    // escaped newlines inside a body are not additional conversation turns.
    let mut records = serde_json::Deserializer::from_str(excerpt).into_iter::<UniqueValue>();
    let mut consumed = 0;
    let mut count = 0;
    let mut projections = Vec::new();
    let mut visible_text = String::new();
    let mut screening_text = String::new();
    while let Some(record) = records.next() {
        let value = record.ok()?;
        if count == MAX_TRANSCRIPT_RECORDS {
            return None;
        }
        let end = records.byte_offset();
        let raw = &excerpt[consumed..end];
        if count != 0 {
            let body = raw.trim_start_matches([' ', '\t', '\r', '\n']);
            let separator = &raw[..raw.len() - body.len()];
            if !separator.contains('\n') {
                return None;
            }
        }
        let class = crate::policy::classify_transcript_record(raw);
        if !class.is_indexable() {
            return None;
        }
        let kind = match class.span_kind {
            "message" => TranscriptProjectionKind::Text,
            "summary" if purpose == ProjectionPurpose::Reader => TranscriptProjectionKind::Summary,
            _ => return None,
        };
        let role = match class.role {
            Some("user") => Some(CassRole::User),
            Some("assistant") => Some(CassRole::Assistant),
            None => None,
            _ => return None,
        };
        let channel = message_channel(&value.0, 0)?;
        if channel.is_some() && role != Some(CassRole::Assistant) {
            return None;
        }
        let analysis_channel = channel == Some("analysis");
        if analysis_channel && purpose == ProjectionPurpose::Learning {
            return None;
        }
        let mut bodies = Vec::new();
        let mut screening_bodies = Vec::new();
        let omitted_reasoning = collect_message(
            &value.0,
            0,
            purpose,
            kind,
            &mut bodies,
            &mut screening_bodies,
        )?;
        let body = if analysis_channel {
            String::new()
        } else {
            bodies.join("\n")
        };
        let decoded = screening_bodies.join("\n");
        // Omitted reasoning still participates in the final decoded security
        // screen: hiding it must not launder an instruction-risk source record.
        append_bounded(&mut screening_text, &decoded)?;
        if body.trim().is_empty() {
            if !omitted_reasoning && !analysis_channel {
                return None;
            }
        } else {
            append_bounded(&mut visible_text, &body)?;
            projections.push(TranscriptProjection {
                role,
                kind,
                text_bytes: body.len(),
                text: Cow::Owned(body),
                projection_version: TRANSCRIPT_PROJECTION_VERSION,
            });
        }
        consumed = end;
        count += 1;
    }
    if projections.is_empty()
        || !safe_decoded_window(&screening_text)
        || (visible_text != screening_text && !safe_decoded_window(&visible_text))
    {
        return None;
    }
    Some(projections)
}

fn append_bounded(text: &mut String, body: &str) -> Option<()> {
    let separator_bytes = usize::from(!text.is_empty());
    if text
        .len()
        .checked_add(separator_bytes)?
        .checked_add(body.len())?
        > MAX_SOURCE_BYTES
    {
        return None;
    }
    if separator_bytes != 0 {
        text.push('\n');
    }
    text.push_str(body);
    Some(())
}

fn safe_decoded_window(text: &str) -> bool {
    if !safe_decoded_text(text) {
        return false;
    }
    // Record/block framing must not split a dangerous instruction into
    // individually harmless fragments. This is a screening view, not source.
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    folded == text || safe_decoded_text(&folded)
}

fn safe_decoded_text(text: &str) -> bool {
    let screened = crate::policy::screen_external_text_for_ingestion(text);
    !screened.redacted
        && !screened.instruction_like
        && matches!(screened.instruction_risk, "none" | "low")
        && screened.content == text
}

/// Older or external response envelopes may explicitly label an analysis
/// channel. Only envelope fields carry that declaration; quoted body text,
/// unrelated metadata and current harness `phase` fields do not set a channel.
fn message_channel(value: &Value, depth: usize) -> Option<Option<&str>> {
    if depth >= MAX_ENVELOPE_DEPTH || !value.is_object() {
        return None;
    }
    let mut channel = match value.get("channel") {
        None => None,
        Some(Value::String(channel))
            if matches!(channel.as_str(), "analysis" | "final" | "commentary") =>
        {
            Some(channel.as_str())
        }
        _ => return None,
    };
    for field in ["message", "payload"] {
        if let Some(nested) = value.get(field).filter(|nested| nested.is_object())
            && let Some(nested_channel) = message_channel(nested, depth + 1)?
        {
            if channel.is_some_and(|outer| outer != nested_channel) {
                return None;
            }
            channel = Some(nested_channel);
        }
    }
    Some(channel)
}

fn collect_message<'a>(
    value: &'a Value,
    depth: usize,
    purpose: ProjectionPurpose,
    kind: TranscriptProjectionKind,
    bodies: &mut Vec<&'a str>,
    screening_bodies: &mut Vec<&'a str>,
) -> Option<bool> {
    if depth >= MAX_ENVELOPE_DEPTH || !value.is_object() {
        return None;
    }
    // Different body fields have no specified temporal order. Arrays do.
    let fields = ["content", "message", "payload", "summary"];
    let mut present = fields.iter().filter_map(|field| value.get(*field));
    let body = present.next()?;
    if present.next().is_some()
        || (value.get("summary").is_some() && kind != TranscriptProjectionKind::Summary)
    {
        return None;
    }
    if body.is_object() {
        if value.get("content").is_some() || value.get("summary").is_some() {
            return None;
        }
        return collect_message(body, depth + 1, purpose, kind, bodies, screening_bodies);
    }
    if value.get("payload").is_some() {
        return None;
    }
    let mut omitted_reasoning = false;
    match body {
        Value::String(text) => {
            bodies.push(text);
            screening_bodies.push(text);
        }
        Value::Array(blocks) if value.get("content").is_some() => {
            if blocks.len() > MAX_TEXT_BLOCKS {
                return None;
            }
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        let text = block.get("text")?.as_str()?;
                        bodies.push(text);
                        screening_bodies.push(text);
                    }
                    Some("thinking") if purpose == ProjectionPurpose::Reader => {
                        screening_bodies.push(block.get("thinking")?.as_str()?);
                        omitted_reasoning = true;
                    }
                    Some("redacted_thinking") if purpose == ProjectionPurpose::Reader => {
                        // This opaque payload is already redacted and has no
                        // readable body. It cannot supply reader or lesson text.
                        omitted_reasoning = true;
                    }
                    _ => return None,
                }
            }
        }
        _ => return None,
    }
    Some(omitted_reasoning)
}

/// Reject duplicate keys, including escaped-equivalent keys, at every depth.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON with unique object fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueValue, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate transcript field"));
            }
            object.insert(key, map.next_value::<UniqueValue>()?.0);
        }
        Ok(UniqueValue(Value::Object(object)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueValue, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<UniqueValue, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<UniqueValue, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<UniqueValue, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<UniqueValue, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<UniqueValue, E> {
        Number::from_f64(value)
            .map(|number| UniqueValue(Value::Number(number)))
            .ok_or_else(|| de::Error::custom("invalid transcript number"))
    }

    fn visit_unit<E: de::Error>(self) -> Result<UniqueValue, E> {
        Ok(UniqueValue(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_projection_keeps_canonical_roles_and_source_order() {
        let raw = concat!(
            r#"{"type":"user","message":{"role":"user","content":"The cache failed."}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Use stable ids."}]}}"#,
        );
        let projections = project_transcript(raw).expect("reader records");
        assert_eq!(projections.len(), 2);
        assert_eq!(projections[0].role, Some(CassRole::User));
        assert_eq!(projections[1].role, Some(CassRole::Assistant));
        assert_eq!(projections[0].kind, TranscriptProjectionKind::Text);
        assert_eq!(projections[1].text_bytes, "Use stable ids.".len());
        assert_eq!(
            projections[1].projection_version,
            TRANSCRIPT_PROJECTION_VERSION
        );
        assert_eq!(
            reader_text(raw).as_deref(),
            Some("user: The cache failed.\nassistant: Use stable ids.")
        );
    }
}
