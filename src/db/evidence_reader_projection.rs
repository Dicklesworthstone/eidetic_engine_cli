//! Rebuildable reader bytes and egress proof for one exact evidence row.
//!
//! The source row and its live session remain authoritative. A cache binding
//! covers every admission input plus the projector version; an independent
//! digest catches damaged or mismatched derived bytes. These are integrity
//! checks for local derived state, not authentication against a database writer.

use super::{EVIDENCE_ADMISSION_VERDICT_REVISION, EvidenceProducerKind, StoredEvidenceSpan};
use crate::cass::transcript::{MAX_SOURCE_BYTES, TRANSCRIPT_PROJECTION_VERSION};

/// A validated, immutable reader projection. Presence does not affect source
/// equality, provenance, backup records or pack identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceReaderProjection {
    projection_version: u32,
    source_binding: String,
    reader_body: String,
    reader_text: String,
    egress_safe: bool,
    integrity_binding: String,
}

impl EvidenceReaderProjection {
    /// Derive both reader views in a single projector pass. A refusal is a
    /// materialized empty view, so it also avoids repeated decoding on reads.
    pub(super) fn derive(span: &StoredEvidenceSpan) -> Self {
        let start = span.excerpt.trim_start();
        let structured_cass = EvidenceProducerKind::parse(&span.producer_kind)
            == Some(EvidenceProducerKind::CassImport)
            && (start.starts_with('{') || start.starts_with('['));
        let (reader_body, reader_text) = if structured_cass {
            crate::cass::transcript::materialized_reader_views(&span.excerpt).unwrap_or_default()
        } else {
            // Non-CASS and plain evidence retain their exact historical bytes.
            (span.excerpt.clone(), span.excerpt.clone())
        };
        // Keep this proof exactly equal to the existing raw-source egress
        // predicate. Readability is checked separately by admission/search;
        // an otherwise safe reasoning-only source can still have empty views.
        let egress = crate::policy::screen_external_text_for_ingestion(&span.excerpt);
        let egress_safe = !egress.redacted
            && !egress.instruction_like
            && matches!(egress.instruction_risk, "none" | "low");
        let mut projection = Self {
            projection_version: TRANSCRIPT_PROJECTION_VERSION,
            source_binding: source_binding(span),
            reader_body,
            reader_text,
            egress_safe,
            integrity_binding: String::new(),
        };
        projection.integrity_binding = projection.calculate_integrity_binding();
        projection
    }

    /// Accept persisted bytes only when both their source and their content
    /// match. Invalid cache state is an ordinary read-only derivation miss.
    pub(super) fn from_storage(
        span: &StoredEvidenceSpan,
        projection_version: u32,
        source_binding: String,
        reader_body: String,
        reader_text: String,
        egress_safe: bool,
        integrity_binding: String,
    ) -> Option<Self> {
        if reader_body.len() > MAX_SOURCE_BYTES
            || reader_text.len() > MAX_SOURCE_BYTES
            || reader_body.is_empty() != reader_text.is_empty()
        {
            return None;
        }
        let projection = Self {
            projection_version,
            source_binding,
            reader_body,
            reader_text,
            egress_safe,
            integrity_binding,
        };
        (projection.is_current_for(span)
            && projection.integrity_binding == projection.calculate_integrity_binding())
        .then_some(projection)
    }

    /// Fields on StoredEvidenceSpan are public and can change after hydration.
    /// Recheck their cheap binding before borrowing privately held cache bytes.
    pub(super) fn is_current_for(&self, span: &StoredEvidenceSpan) -> bool {
        self.projection_version == TRANSCRIPT_PROJECTION_VERSION
            && self.source_binding == source_binding(span)
    }

    pub(super) fn projection_version(&self) -> u32 {
        self.projection_version
    }

    pub(super) fn source_binding(&self) -> &str {
        &self.source_binding
    }

    pub(super) fn reader_body(&self) -> &str {
        &self.reader_body
    }

    pub(super) fn reader_text(&self) -> &str {
        &self.reader_text
    }

    pub(super) fn egress_safe(&self) -> bool {
        self.egress_safe
    }

    pub(super) fn integrity_binding(&self) -> &str {
        &self.integrity_binding
    }

    fn calculate_integrity_binding(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ee.evidence.reader_projection.content.v1");
        hasher.update(&self.projection_version.to_le_bytes());
        hasher.update(&[u8::from(self.egress_safe)]);
        for text in [&self.source_binding, &self.reader_body, &self.reader_text] {
            hasher.update(&(text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

fn source_binding(span: &StoredEvidenceSpan) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.evidence.reader_projection.source.v1");
    hasher.update(&TRANSCRIPT_PROJECTION_VERSION.to_le_bytes());
    hasher.update(
        span.admission_verdict_hasher(EVIDENCE_ADMISSION_VERDICT_REVISION)
            .finalize()
            .as_bytes(),
    );
    format!("blake3:{}", hasher.finalize().to_hex())
}

impl StoredEvidenceSpan {
    /// Identify a native locator after ingestion has replaced the upstream
    /// text with its commitment. Recognition is not admission or proof; either
    /// matching field keeps a damaged card from reentering as plain evidence.
    pub(crate) fn has_screened_upstream_reference(&self, upstream: &str) -> bool {
        let expected = super::canonical_evidence_hash(upstream);
        self.cass_span_id == expected || self.upstream_ref_hash.as_deref() == Some(&expected)
    }

    /// Compare a freshly derived input with the bytes the real ingestion
    /// boundary would store. Reuse screening, metadata commitments and locator
    /// hashing without writing a row, initializing a store or granting live
    /// admission. The caller owns native identity and its source snapshot.
    pub(crate) fn matches_screened_input(
        &self,
        input: &super::CreateEvidenceSpanInput,
    ) -> super::Result<bool> {
        let prepared = super::prepare_evidence_security(input)?;
        Ok(self.workspace_id == input.workspace_id
            && self.session_id == input.session_id
            && self.memory_id == input.memory_id
            && self.producer_kind == prepared.producer_kind.as_str()
            && self.span_kind == input.span_kind
            && self.start_line == input.start_line
            && self.end_line == input.end_line
            && self.start_byte == input.start_byte
            && self.end_byte == input.end_byte
            && self.role == input.role
            && self.cass_span_id == prepared.upstream_ref_hash
            && self.upstream_ref_hash.as_deref() == Some(&prepared.upstream_ref_hash)
            && self.excerpt == prepared.excerpt
            && self.content_hash == prepared.canonical_excerpt_hash
            && self.canonical_excerpt_hash.as_deref() == Some(&prepared.canonical_excerpt_hash)
            && self.metadata_json.as_deref() == Some(&prepared.safe_metadata_json))
    }
}
