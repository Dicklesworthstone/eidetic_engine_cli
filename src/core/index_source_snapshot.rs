//! Read-only capture of the canonical index corpus.
//!
//! Dry-run corpus capture must not backfill source tables.
//! Missing precision anchors use the same pure extractor as a writable rebuild,
//! but remain request-local. The owner releases only a snapshot it began.

use super::{DbConnection, IndexRebuildError};
use crate::db::DatabaseOpenMode;
use crate::models::{MemoryAnchorSource, StoredMemoryAnchor, extract_precision_memory_anchors};

/// Project the same complete corpus as publication, while borrowing the
/// retrieval caller's snapshot. No nested transaction, anchor backfill, job
/// transition, model load or on-disk index is permitted on this path.
#[cfg(feature = "lexical-bm25")]
pub(crate) fn read_only_documents_in_current_snapshot(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    max_documents: u32,
    max_source_bytes: u64,
) -> Result<Option<Vec<crate::search::IndexableDocument>>, IndexRebuildError> {
    super::index_checkpoint(cx)?;
    if db.mode() != DatabaseOpenMode::ReadOnly {
        return Err(IndexRebuildError::Index(
            "live retrieval projection requires a read-only source connection".to_owned(),
        ));
    }
    if !super::workspace_index_source_rows_fit(db, workspace_id, max_documents)? {
        return Ok(None);
    }
    if !live_source_allocation_fits(cx, db, workspace_id, max_source_bytes)? {
        return Ok(None);
    }
    super::index_checkpoint(cx)?;
    let memories = db.list_memories_for_retrieval_with_global(workspace_id, None, false)?;
    let mut documents = Vec::new();
    for memories in memories.chunks(16) {
        super::index_checkpoint(cx)?;
        documents.extend(super::memory_documents_with_anchors(db, memories)?);
    }
    let visitor_checkpoint = || {
        cx.checkpoint()
            .map_err(|_| crate::db::DbError::MalformedRow {
                operation: crate::db::DbOperation::Query,
                message: "live retrieval source projection cancelled".to_owned(),
            })
    };
    db.visit_sessions_for_workspace_in_current_snapshot(workspace_id, |session| {
        visitor_checkpoint()?;
        documents.push(super::session_to_document(&session));
        Ok(())
    })?;
    super::index_checkpoint(cx)?;
    for artifact in db.list_artifacts(workspace_id, None)? {
        super::index_checkpoint(cx)?;
        documents.push(super::artifact_to_document(&artifact));
    }
    super::index_checkpoint(cx)?;
    documents.extend(super::rule_documents(db, workspace_id)?);
    super::index_checkpoint(cx)?;
    db.visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
        visitor_checkpoint()?;
        documents.push(super::evidence_span_to_document(&span));
        Ok(())
    })?;
    super::index_checkpoint(cx)?;
    let documents: Vec<_> = documents
        .into_iter()
        .map(super::CanonicalSearchDocument::into_indexable)
        .collect();
    // Projection adds metadata and labels. Refuse the entire replacement if
    // those exceed the bound; a truncated corpus must never claim freshness.
    let projected_bytes = documents.iter().fold(0_u64, |total, document| {
        total
            .saturating_add(document.content.len() as u64)
            .saturating_add(
                serde_json::to_vec(&document.metadata)
                    .map_or(u64::MAX, |metadata| metadata.len() as u64),
            )
    });
    if documents.len() > max_documents as usize || projected_bytes > max_source_bytes {
        return Ok(None);
    }
    Ok(Some(documents))
}

/// Preflight the complete source read before any row body, metadata sidecar,
/// or projection is hydrated. Each aggregate returns integers only; summing
/// individual UTF-8 lengths avoids first concatenating large SQL values.
///
/// This is a conservative source-allocation estimate, not an RSS limit: allow
/// eight text copies for owned SQL values, domain rows and projection buffers,
/// plus 1 KiB per row. JSON structure has a separate charge for objects, arrays
/// and members; many tiny values must not hide behind a small byte length.
/// Related rows consume the same budget, so a million tiny tags cannot pass
/// merely because their strings are short. The final projected-byte check is
/// independent and still refuses a partial corpus.
#[cfg(feature = "lexical-bm25")]
fn live_source_allocation_fits(
    cx: &asupersync::Cx,
    db: &DbConnection,
    workspace_id: &str,
    max_source_bytes: u64,
) -> Result<bool, IndexRebuildError> {
    const MEMORY_SCOPE: &str = "(m.workspace_id = ?1 OR EXISTS (SELECT 1 FROM memory_tags mt WHERE mt.memory_id = m.id AND lower(replace(trim(mt.tag), '-', '_')) IN ('global', 'house_rule'))) AND m.tombstoned_at IS NULL";
    const SESSION_COLUMNS: &str = "id workspace_id cass_session_id source_path agent_name model started_at ended_at content_hash metadata_json imported_at updated_at";
    let evidence_scope = format!(
        "e.workspace_id = ?1 AND {}",
        crate::db::EVIDENCE_SEARCH_CANDIDATE_PREDICATE
    );
    let memory_ids = format!("SELECT m.id FROM memories m WHERE {MEMORY_SCOPE}");
    let seal_placeholder = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.replace('\'', "''");
    let evidence_ids = format!("SELECT e.id FROM evidence_spans e WHERE {evidence_scope}");
    let rule_ids = "SELECT id FROM procedural_rules WHERE workspace_id = ?1";
    let mut budget = SourceReadBudget::new(max_source_bytes);
    for (table, columns, scope) in [
        (
            "memories m",
            "id workspace_id level kind content workflow_id provenance_uri trust_class trust_subclass provenance_chain_hash provenance_chain_hash_version provenance_verification_status provenance_verified_at provenance_verification_note created_at updated_at tombstoned_at valid_from valid_to typed_fields_json",
            MEMORY_SCOPE.to_owned(),
        ),
        (
            "memory_anchors",
            "memory_id anchor_kind anchor_value_hash redacted_anchor_value source provenance captured_span_hash freshness_state created_at updated_at",
            format!("memory_id IN ({memory_ids})"),
        ),
        (
            "memory_seals",
            "memory_id content_commitment sealed_at revealed_at",
            format!("memory_id IN ({memory_ids} AND m.content = '{seal_placeholder}')"),
        ),
        (
            "sessions",
            SESSION_COLUMNS,
            // Admission can encounter a mismatched session before rejecting
            // its evidence. Charge that read too. The scanner shares each
            // session with Arc, rather than cloning its metadata per span.
            format!(
                "workspace_id = ?1 OR id IN (SELECT e.session_id FROM evidence_spans e WHERE {evidence_scope})"
            ),
        ),
        (
            "artifacts",
            "id workspace_id source_kind artifact_type original_path canonical_path external_ref content_hash media_type redaction_status snippet snippet_hash provenance_uri metadata_json created_at updated_at",
            "workspace_id = ?1".to_owned(),
        ),
        (
            "procedural_rules",
            "id workspace_id content trust_class scope scope_pattern maturity last_applied_at last_validated_at superseded_by created_at updated_at tombstoned_at",
            "workspace_id = ?1 AND tombstoned_at IS NULL".to_owned(),
        ),
        (
            "rule_tags",
            "rule_id tag",
            // The existing bulk reader also loads associations of retired
            // rules before projection filters them; charge exactly that set.
            format!("rule_id IN ({rule_ids})"),
        ),
        (
            "rule_source_memories",
            "rule_id memory_id",
            format!("rule_id IN ({rule_ids})"),
        ),
        (
            "evidence_spans e",
            "id workspace_id session_id memory_id cass_span_id span_kind role excerpt content_hash metadata_json producer_kind secret_redaction_status redaction_classes_json instruction_risk search_eligibility pack_eligibility canonical_excerpt_hash upstream_ref_hash created_at updated_at",
            evidence_scope,
        ),
    ] {
        super::index_checkpoint(cx)?;
        let (rows, bytes, structure) = source_row_sizes(db, workspace_id, table, columns, &scope)?;
        if !budget.charge(rows, bytes, structure) {
            return Ok(false);
        }
    }

    // Revisions predating anchor extraction have no association rows to count.
    // Bound their request-local extraction before its map, hashes and projected
    // copies are allocated. A stored anchor set is used as-is by the reader.
    super::index_checkpoint(cx)?;
    if !budget.charge(
        0,
        0,
        missing_anchor_allocation_size(
            db,
            workspace_id,
            &format!("{MEMORY_SCOPE} AND NOT EXISTS (SELECT 1 FROM memory_anchors ma WHERE ma.memory_id = m.id) AND NOT (m.content = '{seal_placeholder}' AND EXISTS (SELECT 1 FROM memory_seals ms WHERE ms.memory_id = m.id))"),
        )?,
    ) {
        return Ok(false);
    }

    // Rules retain the addressed workspace path in each projected document.
    // Account for those copies without reading the path itself into Rust.
    super::index_checkpoint(cx)?;
    let (rows, bytes, structure) = source_row_sizes(
        db,
        workspace_id,
        "workspaces",
        "id path name scope_kind repository_root repository_fingerprint subproject_path created_at updated_at",
        "id = ?1",
    )?;
    if !budget.charge(rows, bytes, structure) {
        return Ok(false);
    }
    let (_, path_bytes, _) = source_row_sizes(db, workspace_id, "workspaces", "path", "id = ?1")?;
    let rule_count = db.query(
        "SELECT COUNT(*) FROM procedural_rules WHERE workspace_id = ?1 AND tombstoned_at IS NULL",
        &[sqlmodel_core::Value::Text(workspace_id.to_owned())],
    )?;
    let Some(projected_path_bytes) = path_bytes.checked_mul(source_size_value(&rule_count, 0)?)
    else {
        return Ok(false);
    };
    if !budget.charge(0, projected_path_bytes, 0) {
        return Ok(false);
    }

    // These derived sidecars are optional on older supported stores. Invalid
    // cached text is still read before its binding is checked, so it must fit
    // even when admission will subsequently discard it.
    for (table, columns) in [
        (
            "evidence_admission_verdicts",
            "evidence_span_id verdict_binding",
        ),
        (
            "evidence_reader_projections",
            "evidence_span_id source_binding reader_body reader_text integrity_binding",
        ),
    ] {
        super::index_checkpoint(cx)?;
        let exists = db.query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            &[sqlmodel_core::Value::Text(table.to_owned())],
        )?;
        if source_size_value(&exists, 0)? == 0 {
            continue;
        }
        let (rows, bytes, structure) = source_row_sizes(
            db,
            workspace_id,
            table,
            columns,
            &format!("evidence_span_id IN ({evidence_ids})"),
        )?;
        if !budget.charge(rows, bytes, structure) {
            return Ok(false);
        }
    }
    super::index_checkpoint(cx)?;
    Ok(true)
}

#[cfg(feature = "lexical-bm25")]
fn missing_anchor_allocation_size(
    db: &DbConnection,
    workspace_id: &str,
    memory_scope: &str,
) -> Result<u64, IndexRebuildError> {
    // Match the recognizers in extract_memory_anchor_surfaces conservatively,
    // without loading content or allocating an anchor. Every explicit anchor
    // requires "anchor:"; every schema requires "ee.". Code-only recognizers
    // require the markers below or one of the extractor's finite literal words.
    // Counting matches outside code, repeated markers within a token, and
    // substrings of other words deliberately overestimates the candidate set.
    let whole_text = ["anchor:", "ee."]
        .map(|marker| source_text_occurrences("m.content", marker))
        .join(" + ");
    let code_tokens = [
        // Path, symbol, environment, schema, degraded code and config key.
        "/",
        "AGENTS.md",
        "README.md",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "::",
        "EE_",
        "ee.",
        "_",
        ".",
        // A command contributes at most one anchor per line.
        "ee",
        "cargo",
        "rustfmt",
        "br",
        "bv",
        "git",
        "jq",
        "shellcheck",
        "scripts/",
        // Dependency anchors use this finite vocabulary in the extractor.
        "asupersync",
        "fsqlite",
        "fsqlite-core",
        "fsqlite-types",
        "fsqlite-error",
        "sqlmodel",
        "frankensearch",
        "fnx-runtime",
        "fnx-classes",
        "fnx-algorithms",
        "tokio",
        "tokio-util",
        "rusqlite",
        "sqlx",
        "diesel",
        "sea-orm",
        "petgraph",
        "hyper",
        "axum",
        "tower",
        "reqwest",
    ]
    .map(|marker| source_text_occurrences("m.content", marker))
    .join(" + ");
    let backticks = source_text_occurrences("m.content", "`");
    // Allow the extractor's tree nodes, owned hashes and redacted strings,
    // StoredMemoryAnchor values, and the builder/metadata copies per candidate.
    // Charge repeated memory identities separately; legacy IDs need not have a
    // bounded byte length. Long normalized values are covered by source text.
    // Every fragment requires two backticks; 32 bytes per marker also covers
    // the fragment Vec's fat pointers and spare capacity before deduplication.
    let sizes = db.query(
        &format!("SELECT coalesce(SUM((4096 + {} * length(CAST(m.id AS BLOB))) * ({whole_text} + CASE WHEN {backticks} > 0 THEN {code_tokens} ELSE 0 END) + 32 * ({backticks})), 0) FROM memories m WHERE {memory_scope}", SourceReadBudget::TEXT_COPIES),
        &[sqlmodel_core::Value::Text(workspace_id.to_owned())],
    )?;
    source_size_value(&sizes, 0)
}

#[cfg(feature = "lexical-bm25")]
fn source_text_occurrences(column: &str, marker: &str) -> String {
    // All callers supply static identifiers and nonempty ASCII literals. BLOB
    // lengths count the full UTF-8 buffer, including after embedded NUL bytes;
    // SQLite's TEXT length could otherwise hide anchors from this preflight.
    format!(
        "((length(CAST(coalesce({column}, '') AS BLOB)) - length(CAST(replace(coalesce({column}, ''), '{marker}', '') AS BLOB))) / {})",
        marker.len(),
    )
}

#[cfg(feature = "lexical-bm25")]
struct SourceReadBudget {
    maximum: u64,
    charged: Option<u64>,
}

#[cfg(feature = "lexical-bm25")]
impl SourceReadBudget {
    const TEXT_COPIES: u64 = 8;
    const ROW_OVERHEAD: u64 = 1024;

    const fn new(maximum: u64) -> Self {
        Self {
            maximum,
            charged: Some(0),
        }
    }

    fn charge(&mut self, rows: u64, bytes: u64, structure: u64) -> bool {
        self.charged = self
            .charged
            .and_then(|charged| rows.checked_mul(Self::ROW_OVERHEAD)?.checked_add(charged))
            .and_then(|charged| bytes.checked_mul(Self::TEXT_COPIES)?.checked_add(charged))
            .and_then(|charged| charged.checked_add(structure));
        self.charged.is_some_and(|charged| charged <= self.maximum)
    }
}

#[cfg(all(test, feature = "lexical-bm25"))]
mod source_allocation_tests {
    use super::SourceReadBudget;

    #[test]
    fn accepts_exact_budget_and_rejects_the_next_byte() {
        let one = SourceReadBudget::ROW_OVERHEAD + 7 * SourceReadBudget::TEXT_COPIES + 256;
        let mut exact = SourceReadBudget::new(one);
        assert!(exact.charge(1, 7, 256));
        assert!(!exact.charge(0, 1, 0));
        let mut empty = SourceReadBudget::new(0);
        assert!(empty.charge(0, 0, 0));
        assert!(!empty.charge(1, 0, 0));
    }

    #[test]
    fn refuses_overflow_even_when_the_limit_is_u64_max() {
        for (rows, bytes, structure) in [(u64::MAX, 0, 0), (0, u64::MAX, 0), (1, 0, u64::MAX)] {
            let mut overflow = SourceReadBudget::new(u64::MAX);
            assert!(!overflow.charge(rows, bytes, structure));
            assert!(!overflow.charge(0, 0, 0), "overflow must remain refused");
        }
        let mut cumulative = SourceReadBudget::new(u64::MAX);
        assert!(cumulative.charge(0, 0, u64::MAX));
        assert!(!cumulative.charge(0, 0, 1));
    }
}

#[cfg(feature = "lexical-bm25")]
fn source_row_sizes(
    db: &DbConnection,
    workspace_id: &str,
    table: &str,
    columns: &str,
    scope: &str,
) -> Result<(u64, u64, u64), IndexRebuildError> {
    // All identifiers and predicates come from the fixed collector inventory
    // above. The only caller-provided value is bound as a SQL parameter.
    let lengths = columns
        .split_whitespace()
        .map(|column| format!("length(CAST(coalesce({column}, '') AS BLOB))"))
        .collect::<Vec<_>>()
        .join(" + ");
    // These are conservative lexical counts, not a second JSON parser. A
    // symbol inside a quoted string also consumes the allowance. An object
    // can allocate a whole BTreeMap node for one member; arrays reserve spare
    // Value slots. The byte multiplier alone cannot account for either shape.
    let structure = columns
        .split_whitespace()
        .filter(|column| {
            column.ends_with("_json") || matches!(*column, "content" | "excerpt" | "snippet")
        })
        .flat_map(|column| {
            [("{", 1024), ("[", 256), (",", 128), (":", 128)]
                .into_iter()
                .map(move |(symbol, bytes)| {
                    format!("{bytes} * {}", source_text_occurrences(column, symbol))
                })
        })
        .collect::<Vec<_>>();
    let structure = if structure.is_empty() {
        "0".to_owned()
    } else {
        structure.join(" + ")
    };
    let sizes = db.query(
        &format!("SELECT COUNT(*), coalesce(SUM({lengths}), 0), coalesce(SUM({structure}), 0) FROM {table} WHERE {scope}"),
        &[sqlmodel_core::Value::Text(workspace_id.to_owned())],
    )?;
    Ok((
        source_size_value(&sizes, 0)?,
        source_size_value(&sizes, 1)?,
        source_size_value(&sizes, 2)?,
    ))
}

#[cfg(feature = "lexical-bm25")]
fn source_size_value(rows: &[sqlmodel_core::Row], column: usize) -> Result<u64, IndexRebuildError> {
    rows.first()
        .and_then(|row| row.get(column))
        .and_then(sqlmodel_core::Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| IndexRebuildError::Index("invalid live retrieval source size".to_owned()))
}

pub(super) fn capture<T>(
    db: &DbConnection,
    collect: impl FnOnce() -> Result<T, IndexRebuildError>,
) -> Result<T, IndexRebuildError> {
    if db.mode() != DatabaseOpenMode::ReadOnly {
        return db.with_transaction_error(collect);
    }
    let snapshot = ReadSnapshot::begin(db)?;
    let result = collect()?;
    snapshot.finish()?;
    Ok(result)
}

struct ReadSnapshot<'a> {
    db: &'a DbConnection,
    active: bool,
}

impl<'a> ReadSnapshot<'a> {
    fn begin(db: &'a DbConnection) -> Result<Self, IndexRebuildError> {
        db.begin_read_snapshot()?;
        Ok(Self { db, active: true })
    }

    fn finish(mut self) -> Result<(), IndexRebuildError> {
        self.db.commit_read_snapshot()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.db.rollback_read_snapshot().is_err() {
            tracing::error!(target: "ee::index", "failed to release index source read snapshot");
        }
    }
}

pub(super) fn projected_anchors(memory_id: &str, content: &str) -> Vec<StoredMemoryAnchor> {
    let mut anchors: Vec<_> = extract_precision_memory_anchors(
        memory_id,
        content,
        MemoryAnchorSource::IndexRebuild,
        Some("index_rebuild"),
    )
    .into_iter()
    .map(|anchor| StoredMemoryAnchor {
        memory_id: anchor.memory_id,
        anchor_kind: anchor.anchor_kind,
        anchor_value_hash: anchor.anchor_value_hash,
        redacted_anchor_value: anchor.redacted_anchor_value,
        confidence: anchor.confidence,
        source: anchor.source,
        provenance: anchor.provenance,
        captured_span_hash: anchor.captured_span_hash,
        freshness_state: anchor.freshness_state,
        generation: anchor.generation,
        // These are projections, not stored anchor rows. The search projector
        // consumes neither timestamp; do not invent capture-time evidence.
        created_at: String::new(),
        updated_at: String::new(),
    })
    .collect();
    // Match list_memory_anchors ordering, not the extractor's traversal order.
    anchors.sort_by(|left, right| {
        left.anchor_kind
            .as_str()
            .cmp(right.anchor_kind.as_str())
            .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
    });
    anchors
}

pub(super) fn reembed_dry_run(
    cx: &asupersync::Cx,
    options: &super::IndexReembedOptions,
) -> Result<super::IndexReembedReport, IndexRebuildError> {
    super::index_checkpoint(cx)?;
    let start = std::time::Instant::now();
    let index_dir = options.resolve_index_dir();
    let db = DbConnection::open_file_read_only(&options.resolve_database_path())?;
    let workspace_id = super::resolve_index_workspace_id(&db, &options.workspace_path)?;
    let snapshot = super::collect_workspace_index_source_snapshot(&db, &workspace_id)?;
    super::index_checkpoint(cx)?;
    let embedding =
        super::ReembedEmbeddingSummary::from_posture(super::embedding_posture_for_document_count(
            &db,
            &workspace_id,
            &index_dir,
            snapshot.documents_total,
        )?);
    let idempotency_key = super::reembed_idempotency_key(
        &workspace_id,
        &embedding.fast_model_id,
        embedding.quality_model_id.as_deref(),
        snapshot.document_counts,
    );
    let documents_embedded = embedding.documents_embedded();
    Ok(super::IndexReembedReport {
        status: super::IndexReembedStatus::DryRun,
        job_id: None,
        job_status: "dry_run_not_queued".to_owned(),
        job_type: super::SearchIndexJobType::FullRebuild.as_str().to_owned(),
        document_source: None,
        embedding_scope: "all_documents".to_owned(),
        embedding,
        memories_indexed: snapshot.memories_indexed,
        sessions_indexed: snapshot.sessions_indexed,
        artifacts_indexed: snapshot.artifacts_indexed,
        rules_indexed: snapshot.rules_indexed,
        evidence_indexed: snapshot.evidence_indexed,
        documents_embedded,
        documents_total: snapshot.documents_total,
        index_dir,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        dry_run: true,
        idempotency_key,
        evidence_admission: snapshot.evidence_admission,
        errors: Vec::new(),
        runtime_profile: super::runtime_profile_for_workspace(&options.workspace_path),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::core::index::{IndexRebuildOptions, rebuild_index};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    const WORKSPACE: &str = "wsp_00000000000000000000000071";
    const MEMORY: &str = "mem_00000000000000000000000071";

    fn fixture() -> (tempfile::TempDir, DbConnection, std::path::PathBuf) {
        fixture_with_content("Run `cargo fmt --check` before touching `src/db/mod.rs`.")
    }

    fn fixture_with_content(
        content: &str,
    ) -> (tempfile::TempDir, DbConnection, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().canonicalize().expect("canonical workspace");
        std::fs::create_dir(workspace.join(".ee")).expect("store directory");
        let path = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&path).expect("database");
        db.migrate().expect("schema");
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("source snapshot".to_owned()),
            },
        )
        .expect("workspace");
        db.insert_memory_revision(
            MEMORY,
            MEMORY,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("unanchored revision");
        (root, db, path)
    }

    #[cfg(feature = "lexical-bm25")]
    fn source_session(db: &DbConnection) -> String {
        let id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x7100)).to_string();
        db.insert_session(
            &id,
            &crate::db::CreateSessionInput {
                workspace_id: WORKSPACE.to_owned(),
                cass_session_id: "bounded-source-session".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 32,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"bounded source session")),
                metadata_json: None,
            },
        )
        .expect("source session");
        id
    }

    #[cfg(feature = "lexical-bm25")]
    fn source_evidence(
        db: &DbConnection,
        session: &str,
        ordinal: u32,
        text: &str,
        redacted: bool,
    ) -> String {
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x7100 + u128::from(ordinal),
        ))
        .to_string();
        db.insert_evidence_span(
            &id,
            &crate::db::CreateEvidenceSpanInput {
                workspace_id: WORKSPACE.to_owned(),
                session_id: session.to_owned(),
                memory_id: None,
                producer_kind: crate::db::EvidenceProducerKind::CassImport,
                cass_span_id: format!("bounded-source-span-{ordinal}"),
                span_kind: if redacted { "tool_result" } else { "message" }.to_owned(),
                start_line: ordinal,
                end_line: ordinal,
                start_byte: None,
                end_byte: None,
                role: Some(if redacted { "tool" } else { "assistant" }.to_owned()),
                excerpt: text.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(text.as_bytes())),
                metadata_json: None,
                inherited_redaction_classes: if redacted {
                    vec!["history".to_owned()]
                } else {
                    Vec::new()
                },
            },
        )
        .expect("screened source evidence");
        id
    }

    #[cfg(feature = "lexical-bm25")]
    fn assert_source_refusal_is_read_only(db: &DbConnection, path: &std::path::Path, budget: u64) {
        let before = std::fs::read(path).expect("source bytes");
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        let anchors = db.list_memory_anchors(MEMORY).expect("anchors");
        let audits = db.count_table_rows("audit_log").expect("audits");
        let jobs = db.count_table_rows("search_index_jobs").expect("jobs");
        let read = DbConnection::open_file_read_only(path).expect("read-only store");
        read.begin_read_snapshot().expect("caller snapshot");
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            assert!(
                !live_source_allocation_fits(&cx, &read, WORKSPACE, budget).expect("preflight")
            );
            assert!(
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 64, budget)
                    .expect("source refusal")
                    .is_none()
            );
            read.commit_read_snapshot()
                .expect("caller still owns snapshot");
        })
        .expect("runtime");
        assert_eq!(
            db.get_workspace_generation(WORKSPACE).expect("generation"),
            generation
        );
        assert_eq!(db.list_memory_anchors(MEMORY).expect("anchors"), anchors);
        assert_eq!(db.count_table_rows("audit_log").expect("audits"), audits);
        assert_eq!(
            db.count_table_rows("search_index_jobs").expect("jobs"),
            jobs
        );
        assert_eq!(std::fs::read(path).expect("unchanged source"), before);
    }

    #[cfg(feature = "lexical-bm25")]
    fn set_test_text(
        db: &DbConnection,
        table: &str,
        column: &str,
        id_column: &str,
        id: &str,
        text: Option<&str>,
    ) {
        let value = text.map_or_else(
            || "NULL".to_owned(),
            |value| format!("'{}'", value.replace('\'', "''")),
        );
        db.execute_raw(&format!(
            "UPDATE {table} SET {column} = {value} WHERE {id_column} = '{}'",
            id.replace('\'', "''")
        ))
        .expect("fixture text update");
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn source_allocation_counts_utf8_and_json_containers_before_hydration() {
        let (_root, db, _path) = fixture();
        let session = source_session(&db);
        let metadata = serde_json::json!({"value": ["é🦀", 0, 1]}).to_string();
        set_test_text(
            &db,
            "sessions",
            "metadata_json",
            "id",
            &session,
            Some(&metadata),
        );
        let (rows, bytes, structure) = source_row_sizes(
            &db,
            WORKSPACE,
            "sessions",
            "metadata_json",
            "workspace_id = ?1",
        )
        .expect("size-only SQL");
        assert_eq!(rows, 1);
        assert_eq!(bytes, metadata.len() as u64);
        assert!(bytes > metadata.chars().count() as u64);
        assert_eq!(structure, 1024 + 256 + 3 * 128);
        let charge =
            SourceReadBudget::ROW_OVERHEAD + bytes * SourceReadBudget::TEXT_COPIES + structure;
        assert!(SourceReadBudget::new(charge).charge(rows, bytes, structure));
        assert!(!SourceReadBudget::new(charge - 1).charge(rows, bytes, structure));
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_source_projection_ignores_quarantined_bodies_without_omitting_safe_evidence() {
        let (_root, db, path) = fixture();
        let session = source_session(&db);
        let evidence = source_evidence(
            &db,
            &session,
            1,
            "Fresh source sentinel confirms the repair succeeded.",
            false,
        );
        let quarantined = format!(
            "[REDACTED:history] {}",
            "withheldhistorycanary ".repeat(2500)
        );
        for ordinal in 2..=13 {
            let id = source_evidence(&db, &session, ordinal, &quarantined, true);
            assert_eq!(
                db.get_evidence_span(&id)
                    .expect("quarantine")
                    .expect("row")
                    .search_eligibility,
                "quarantined"
            );
        }
        const BUDGET: u64 = 128 * 1024;
        assert!(quarantined.len() as u64 * 12 > BUDGET);
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        read.begin_read_snapshot().expect("caller snapshot");
        let before = std::fs::read(&path).expect("source bytes");
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            assert!(
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 2, BUDGET)
                    .expect("row ceiling")
                    .is_none()
            );
            let documents =
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 3, BUDGET)
                    .expect("complete projection")
                    .expect("safe corpus fits");
            assert_eq!(documents.len(), 3);
            assert!(
                documents
                    .iter()
                    .any(|doc| doc.id == evidence && doc.content.contains("Fresh source sentinel"))
            );
            assert!(
                documents
                    .iter()
                    .all(|doc| !doc.content.contains("withheldhistorycanary"))
            );
            read.commit_read_snapshot()
                .expect("caller snapshot retained");
        })
        .expect("runtime");
        assert_eq!(
            db.get_workspace_generation(WORKSPACE).expect("generation"),
            generation
        );
        assert_eq!(std::fs::read(&path).expect("unchanged database"), before);
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_source_preflight_refuses_unprojected_evidence_metadata_and_cached_projection() {
        let (_root, db, path) = fixture();
        let session = source_session(&db);
        let evidence = source_evidence(
            &db,
            &session,
            1,
            "A bounded evidence record remains readable.",
            false,
        );
        let original = db
            .get_evidence_span(&evidence)
            .expect("evidence")
            .expect("row");
        let mut metadata: serde_json::Value =
            serde_json::from_str(original.metadata_json.as_deref().expect("metadata"))
                .expect("canonical metadata");
        metadata["padding"] = serde_json::json!("x".repeat(32_768));
        set_test_text(
            &db,
            "evidence_spans",
            "metadata_json",
            "id",
            &evidence,
            Some(&metadata.to_string()),
        );
        assert_source_refusal_is_read_only(&db, &path, 128 * 1024);
        set_test_text(
            &db,
            "evidence_spans",
            "metadata_json",
            "id",
            &evidence,
            original.metadata_json.as_deref(),
        );
        db.backfill_evidence_reader_projections(Some(WORKSPACE))
            .expect("restore derived projection");
        set_test_text(
            &db,
            "evidence_reader_projections",
            "reader_body",
            "evidence_span_id",
            &evidence,
            Some(&"x".repeat(32_768)),
        );
        assert_source_refusal_is_read_only(&db, &path, 128 * 1024);
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_source_preflight_refuses_session_locators_and_many_small_json_values() {
        let (_root, db, path) = fixture();
        let session = source_session(&db);
        source_evidence(
            &db,
            &session,
            1,
            "A bounded evidence record remains readable.",
            false,
        );
        set_test_text(
            &db,
            "sessions",
            "source_path",
            "id",
            &session,
            Some(&"x".repeat(32_768)),
        );
        assert_source_refusal_is_read_only(&db, &path, 128 * 1024);
        let metadata = serde_json::json!({"values": vec![0; 1200]}).to_string();
        assert!(
            metadata.len() < 4096,
            "small bytes alone must not admit a large JSON tree"
        );
        set_test_text(&db, "sessions", "source_path", "id", &session, None);
        set_test_text(
            &db,
            "sessions",
            "metadata_json",
            "id",
            &session,
            Some(&metadata),
        );
        assert_source_refusal_is_read_only(&db, &path, 128 * 1024);
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_source_preflight_bounds_missing_anchor_projection_before_extraction() {
        let (_root, db, path) = fixture();
        const BUDGET: u64 = 16 * 1024 * 1024;
        // A valid-sized legacy revision can produce thousands of rich anchor
        // objects even when its source text is far below the live byte ceiling.
        let content = (0..4096)
            .map(|index| format!("`src/a{index:04}.rs`"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(content.len() <= 64 * 1024);
        set_test_text(&db, "memories", "content", "id", MEMORY, Some(&content));
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("unanchored revision")
                .is_empty()
        );
        let (rows, bytes, structure) = source_row_sizes(
            &db,
            WORKSPACE,
            "memories",
            "id content",
            "workspace_id = ?1",
        )
        .expect("raw source sizes");
        assert!(SourceReadBudget::new(BUDGET).charge(rows, bytes, structure));
        assert_source_refusal_is_read_only(&db, &path, BUDGET);

        // SQL TEXT length ends at a NUL, but Rust extraction continues. Bind
        // the legacy content through the real revision write path so this
        // cannot depend on the SQL parser accepting NUL in a literal.
        let (_nul_root, nul_db, nul_path) = fixture_with_content(&format!("legacy\0 {content}"));
        assert_source_refusal_is_read_only(&nul_db, &nul_path, BUDGET);

        // Existing anchors are authoritative. Their presence prevents the
        // extractor from running; the same body therefore needs no derived
        // anchor charge and must not cause a spurious refusal or backfill.
        let anchor = crate::models::CreateMemoryAnchorInput::from_raw(
            MEMORY,
            crate::models::MemoryAnchorKind::Path,
            "src/a0000.rs",
            0.9,
            crate::models::MemoryAnchorSource::Explicit,
            "original capture",
            0,
        )
        .expect("stored anchor");
        db.upsert_memory_anchors(&[anchor])
            .expect("original anchors");
        let original = db.list_memory_anchors(MEMORY).expect("stored anchors");
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        read.begin_read_snapshot().expect("caller snapshot");
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            let documents =
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 1, BUDGET)
                    .expect("bounded stored-anchor projection")
                    .expect("existing anchors fit");
            assert_eq!(documents.len(), 1);
            assert_eq!(
                read.list_memory_anchors(MEMORY)
                    .expect("original anchor provenance"),
                original
            );
            read.commit_read_snapshot()
                .expect("caller retains snapshot");
        })
        .expect("runtime");
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn missing_anchor_preflight_covers_every_extractor_kind_and_overlapping_recognizers() {
        let (_root, db, _path) = fixture();
        let content = "anchor:path:src/explicit.rs ee.fixture.v1\n`src/code.rs Type::method EE_TEST ee.code.v2 search_stale tokio config.key rust-toolchain.toml`\n`cargo fmt`\n`scripts/check.sh`";
        set_test_text(&db, "memories", "content", "id", MEMORY, Some(content));
        let anchors = projected_anchors(MEMORY, content);
        let kinds: std::collections::BTreeSet<_> =
            anchors.iter().map(|anchor| anchor.anchor_kind).collect();
        assert_eq!(kinds.len(), 8, "exercise every current extraction route");
        let allocation = missing_anchor_allocation_size(&db, WORKSPACE, "m.workspace_id = ?1")
            .expect("integer-only anchor allocation");
        assert!(
            allocation
                >= anchors.len() as u64
                    * (4096 + SourceReadBudget::TEXT_COPIES * MEMORY.len() as u64)
        );
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_source_preflight_counts_rule_tags_anchors_typed_fields_and_artifact_locators() {
        for source in [
            "rule_tags",
            "memory_anchors",
            "typed_fields_json",
            "artifacts",
        ] {
            let (_root, db, path) = fixture();
            let read = DbConnection::open_file_read_only(&path).expect("read-only control");
            read.begin_read_snapshot().expect("control snapshot");
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                assert!(
                    live_source_allocation_fits(&cx, &read, WORKSPACE, 128 * 1024)
                        .expect("small fixture fits")
                );
                read.commit_read_snapshot().expect("control snapshot ends");
            })
            .expect("control runtime");
            match source {
                "rule_tags" => {
                    const RULE: &str = "rule_00000000000000000000000071";
                    db.insert_procedural_rule(
                        RULE,
                        &crate::db::CreateProceduralRuleInput {
                            workspace_id: WORKSPACE.to_owned(),
                            content: "Use the existing source allocation budget.".to_owned(),
                            confidence: 0.9,
                            utility: 0.8,
                            importance: 0.8,
                            trust_class: "human_explicit".to_owned(),
                            scope: "workspace".to_owned(),
                            scope_pattern: None,
                            maturity: "validated".to_owned(),
                            protected: false,
                            source_memory_ids: vec![MEMORY.to_owned()],
                            tags: (0..140).map(|number| format!("tag{number:03}")).collect(),
                        },
                    )
                    .expect("many short rule tags");
                    let (rows, bytes, _) = source_row_sizes(
                        &db,
                        WORKSPACE,
                        "rule_source_memories",
                        "rule_id memory_id",
                        "rule_id IN (SELECT id FROM procedural_rules WHERE workspace_id = ?1)",
                    )
                    .expect("source association sizes");
                    assert_eq!(rows, 1);
                    assert_eq!(bytes, (RULE.len() + MEMORY.len()) as u64);
                }
                "memory_anchors" => {
                    let anchor = crate::models::CreateMemoryAnchorInput::from_raw(
                        MEMORY,
                        crate::models::MemoryAnchorKind::Command,
                        "cargo fmt --check",
                        0.9,
                        crate::models::MemoryAnchorSource::Explicit,
                        "x".repeat(32_768),
                        0,
                    )
                    .expect("anchor");
                    db.upsert_memory_anchors(&[anchor])
                        .expect("oversized anchor provenance");
                }
                "typed_fields_json" => {
                    let metadata = serde_json::json!({"unused": "x".repeat(32_768)}).to_string();
                    set_test_text(
                        &db,
                        "memories",
                        "typed_fields_json",
                        "id",
                        MEMORY,
                        Some(&metadata),
                    );
                }
                "artifacts" => {
                    db.upsert_artifact(
                        "art_00000000000000000000000071",
                        &crate::db::CreateArtifactInput {
                            workspace_id: WORKSPACE.to_owned(),
                            source_kind: "file".to_owned(),
                            artifact_type: "text".to_owned(),
                            original_path: Some("fixture.txt".to_owned()),
                            canonical_path: Some("x".repeat(32_768)),
                            external_ref: None,
                            content_hash: format!("blake3:{}", blake3::hash(b"fixture")),
                            media_type: "text/plain".to_owned(),
                            size_bytes: 7,
                            redaction_status: "checked".to_owned(),
                            snippet: Some("fixture".to_owned()),
                            snippet_hash: None,
                            provenance_uri: None,
                            metadata_json: None,
                        },
                    )
                    .expect("oversized unprojected artifact locator");
                }
                _ => unreachable!(),
            }
            assert_source_refusal_is_read_only(&db, &path, 128 * 1024);
        }
    }

    #[test]
    fn read_only_projection_matches_writable_backfill_without_mutating_anchors() {
        let (_root, db, path) = fixture();
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        let projected = capture(&read, || {
            let memories = read.list_memories_for_retrieval_with_global(WORKSPACE, None, false)?;
            super::super::memory_documents_with_anchors(&read, &memories)
        })
        .expect("read snapshot");
        assert_eq!(projected.len(), 1);
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("unchanged anchors")
                .is_empty()
        );
        let memories = db
            .list_memories_for_retrieval_with_global(WORKSPACE, None, false)
            .expect("memories");
        let persisted =
            super::super::memory_documents_with_anchors(&db, &memories).expect("backfill");
        assert!(
            !db.list_memory_anchors(MEMORY)
                .expect("persisted anchors")
                .is_empty()
        );
        for (projected, persisted) in projected.into_iter().zip(persisted) {
            assert_eq!(projected.id(), persisted.id());
            assert_eq!(projected.content(), persisted.content());
            assert_eq!(
                projected.into_indexable().metadata,
                persisted.into_indexable().metadata
            );
        }
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn live_retrieval_projection_borrows_snapshot_and_refuses_partial_corpora() {
        let (_root, db, path) = fixture();
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        read.begin_read_snapshot().expect("caller snapshot");
        let before = std::fs::read(&path).expect("source bytes");
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            assert!(
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 0, 4096)
                    .expect("row ceiling")
                    .is_none()
            );
            assert!(
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 16, 8)
                    .expect("body ceiling")
                    .is_none()
            );
            let documents =
                read_only_documents_in_current_snapshot(&cx, &read, WORKSPACE, 16, 128 * 1024)
                    .expect("complete projection")
                    .expect("admitted corpus");
            assert_eq!(documents.len(), 1);
            assert_eq!(documents[0].id, MEMORY);
            assert!(
                read.list_memory_anchors(MEMORY)
                    .expect("anchors")
                    .is_empty()
            );
            // A nested BEGIN or COMMIT in the collector would either fail the
            // calls above or release this caller-owned transaction.
            read.commit_read_snapshot()
                .expect("caller still owns snapshot");
            assert_eq!(std::fs::read(&path).expect("unchanged source"), before);
            assert!(
                read_only_documents_in_current_snapshot(&cx, &db, WORKSPACE, 16, 128 * 1024)
                    .is_err()
            );
        })
        .expect("runtime");
    }

    #[test]
    fn dry_run_counts_the_real_corpus_without_backfilling_or_creating_an_index() {
        let (root, db, path) = fixture();
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: root.path().canonicalize().expect("root"),
            database_path: Some(path),
            index_dir: None,
            dry_run: true,
        })
        .expect("dry run");
        assert_eq!(report.memories_indexed, 1);
        assert!(report.dry_run);
        assert!(!report.index_dir.exists());
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        assert_eq!(
            generation,
            db.get_workspace_generation(WORKSPACE).expect("generation")
        );
    }

    #[test]
    fn errors_release_owned_snapshots_but_failed_nested_begin_preserves_the_callers() {
        let (_root, _db, path) = fixture();
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        let result = capture::<()>(&read, || {
            Err(IndexRebuildError::Index("fixture".to_owned()))
        });
        assert!(result.is_err());
        read.begin_read_snapshot()
            .expect("previous snapshot released");
        assert!(capture(&read, || Ok(())).is_err());
        // A nested begin may not roll back the transaction it did not own.
        read.commit_read_snapshot()
            .expect("caller still owns its snapshot");
        capture(&read, || Ok(())).expect("subsequent independent snapshot");
    }

    #[test]
    fn reembed_dry_run_does_not_mutate_sources_or_initialize_backends() {
        const CHILD_WORKSPACE: &str = "EE_TEST_REEMBED_PREVIEW_WORKSPACE";
        if let Some(workspace) = std::env::var_os(CHILD_WORKSPACE) {
            let workspace = std::path::PathBuf::from(workspace);
            assert!(super::super::ACTIVE_REMOTE_EMBEDDER.get().is_none());
            assert!(super::super::DEFAULT_SEARCH_EMBEDDER.get().is_none());
            let report = super::super::reembed_index(&super::super::IndexReembedOptions {
                workspace_path: workspace.clone(),
                database_path: Some(workspace.join(".ee/ee.db")),
                index_dir: Some(workspace.join(".ee/index")),
                dry_run: true,
            })
            .expect("read-only reembed preview");
            assert_eq!(report.status, super::super::IndexReembedStatus::DryRun);
            assert_eq!(report.memories_indexed, 1);
            assert_eq!(report.documents_total, 1);
            assert_eq!(report.documents_embedded, 0);
            assert!(report.job_id.is_none());
            assert_eq!(report.job_status, "dry_run_not_queued");
            assert_eq!(report.embedding.source, "remote_dimension_unprobed");
            assert!(super::super::ACTIVE_REMOTE_EMBEDDER.get().is_none());
            assert!(super::super::DEFAULT_SEARCH_EMBEDDER.get().is_none());
            return;
        }

        let (root, db, _path) = fixture();
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("initial anchors")
                .is_empty()
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("endpoint");
        listener
            .set_nonblocking(true)
            .expect("nonblocking endpoint");
        let endpoint = format!(
            "http://{}/v1",
            listener.local_addr().expect("endpoint address")
        );
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "core::index::source_snapshot::tests::reembed_dry_run_does_not_mutate_sources_or_initialize_backends",
                "--nocapture",
            ])
            .env(
                CHILD_WORKSPACE,
                root.path().canonicalize().expect("workspace"),
            )
            .env("EE_EMBED_BACKEND", "remote")
            .env("EE_EMBED_REMOTE_URL", endpoint)
            .env("EE_EMBED_REMOTE_MODEL", "passive-reembed-fixture")
            .env_remove("EE_EMBED_REMOTE_DIMENSION")
            .env_remove("EE_EMBED_REMOTE_API_KEY")
            .output()
            .expect("isolated reembed preview");
        assert!(
            output.status.success(),
            "isolated preview failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("unchanged anchors")
                .is_empty()
        );
        assert_eq!(
            generation,
            db.get_workspace_generation(WORKSPACE).expect("generation")
        );
        assert!(
            db.list_search_index_jobs(WORKSPACE, None)
                .expect("jobs")
                .is_empty()
        );
        assert!(
            db.list_embedding_metadata_records(WORKSPACE)
                .expect("registry")
                .is_empty()
        );
        assert!(!root.path().join(".ee/index").exists());
    }
}
