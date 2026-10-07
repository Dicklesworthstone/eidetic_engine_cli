//! Incremental index intake as staged delta generations
//! (bd-reality-core-convergence-1azkt.57).
//!
//! Since cancellation-safe staged publication landed, every write rebuilt and
//! re-embedded the whole corpus, so write cost grew with the store. This module
//! restores O(delta) intake without giving up any publication property: the
//! staged generation is a private copy of the live one with only the changed
//! documents applied, and it is validated and published through exactly the
//! same masked, fenced, atomic path as a full build.
//!
//! The delta is not taken from job bookkeeping. Every generation records a
//! digest of each document it indexed (`doc_digests.json`). The publisher
//! diffs that map against the authoritative source snapshot it already holds:
//! a changed digest is an upsert, a missing id a removal. A change made by any
//! write path, with or without an index job, is therefore always visible, and
//! a generation without digests (or built by another embedder) simply takes
//! the full rebuild. Large deltas, accumulated lexical segments and any staging
//! failure also fall back to the full rebuild, which remains the compaction
//! step.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(feature = "lexical-bm25")]
use crate::search::LexicalWrite;

use super::{
    EmbedderStack, INDEX_METADATA_FILE, IncrementalFallback, IncrementalFallbackReason,
    IndexRebuildError, LEXICAL_INDEX_SUBDIR, ensure_index_path_has_no_symlinks,
    incremental_fallback,
};

/// Per-document digests of the documents a generation indexed.
pub(super) const DOC_DIGESTS_FILE: &str = "doc_digests.json";
const DOC_DIGESTS_SCHEMA_V1: &str = "ee.index.doc_digests.v1";
const DOC_DIGEST_DOMAIN: &[u8] = b"ee.index.doc_digest.v1\0";

/// Above this many changed documents a full rebuild is the simpler and
/// comparably priced path.
const MAX_DELTA_DOCUMENTS: usize = 256;
/// Above this share of the corpus (percent) a full rebuild is preferred.
const MAX_DELTA_PERCENT: usize = 25;
/// Each delta commit adds a lexical segment; past this many the next write
/// takes a full rebuild, which compacts them.
const MAX_LEXICAL_SEGMENTS: usize = 32;

/// Files that belong to one generation's admission state, never to its tiers.
const TANTIVY_LOCK_FILES: [&str; 2] = [".tantivy-writer.lock", ".tantivy-meta.lock"];
const RETIRED_MANIFEST_PREFIX: &str = ".rejected-meta-";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DocDigests {
    schema: String,
    embedder: String,
    documents: BTreeMap<String, String>,
}

/// The documents to apply to a copy of the live generation.
#[derive(Debug, Default)]
pub(super) struct Delta {
    pub upserts: Vec<crate::search::IndexableDocument>,
    pub removals: Vec<String>,
}

impl Delta {
    pub(super) fn len(&self) -> usize {
        self.upserts.len().saturating_add(self.removals.len())
    }
}

/// Identity of the embedders whose vectors a generation holds. Vectors from a
/// different model or dimension can never be mixed into one tier.
pub(super) fn embedder_identity(stack: &EmbedderStack) -> String {
    let fast = stack.fast();
    let quality = stack.quality().map_or_else(String::new, |quality| {
        format!("{}:{}", quality.id(), quality.dimension())
    });
    format!(
        "fast={}:{}:{};quality={quality}",
        fast.id(),
        fast.dimension(),
        fast.is_semantic()
    )
}

fn update_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Length-delimited digest over every field the index stores for a document.
pub(super) fn document_digest(document: &crate::search::IndexableDocument) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOC_DIGEST_DOMAIN);
    update_field(&mut hasher, document.id.as_bytes());
    update_field(&mut hasher, document.content.as_bytes());
    match &document.title {
        None => {
            hasher.update(&[0]);
        }
        Some(title) => {
            hasher.update(&[1]);
            update_field(&mut hasher, title.as_bytes());
        }
    }
    let mut metadata = document.metadata.iter().collect::<Vec<_>>();
    metadata.sort();
    hasher.update(&(metadata.len() as u64).to_le_bytes());
    for (key, value) in metadata {
        update_field(&mut hasher, key.as_bytes());
        update_field(&mut hasher, value.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub(super) fn digest_map(
    documents: &[crate::search::IndexableDocument],
) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|document| (document.id.clone(), document_digest(document)))
        .collect()
}

impl DocDigests {
    /// Digests of exactly the documents a generation is built from, bound to
    /// the embedders that produce its vectors.
    pub(super) fn new(
        stack: &EmbedderStack,
        documents: &[crate::search::IndexableDocument],
    ) -> Self {
        Self {
            schema: DOC_DIGESTS_SCHEMA_V1.to_owned(),
            embedder: embedder_identity(stack),
            documents: digest_map(documents),
        }
    }

    /// Record the digests in a staged generation. Written before the
    /// generation is flushed, so the same durability barrier covers them.
    pub(super) fn write(&self, generation_dir: &Path) -> Result<(), IndexRebuildError> {
        let path = generation_dir.join(DOC_DIGESTS_FILE);
        ensure_index_path_has_no_symlinks(&path, "write index document digests")?;
        let bytes = serde_json::to_vec(self).map_err(|error| {
            IndexRebuildError::Index(format!("Failed to encode index document digests: {error}"))
        })?;
        std::fs::write(&path, bytes).map_err(|error| {
            IndexRebuildError::Index(format!("Failed to write index document digests: {error}"))
        })
    }
}

fn read_digests(generation_dir: &Path) -> Result<DocDigests, IncrementalFallback> {
    let path = generation_dir.join(DOC_DIGESTS_FILE);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::IndexAbsent,
            format!("live generation has no document digests: {error}"),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            "live document digests are not a regular file",
        ));
    }
    let raw = std::fs::read(&path).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("failed to read live document digests: {error}"),
        )
    })?;
    let digests: DocDigests = serde_json::from_slice(&raw).map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!("live document digests are malformed: {error}"),
        )
    })?;
    if digests.schema != DOC_DIGESTS_SCHEMA_V1 {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!("unsupported document digest schema {}", digests.schema),
        ));
    }
    Ok(digests)
}

/// Diff the live generation's digests against the authoritative snapshot.
/// `Err` names why only a full rebuild is acceptable.
pub(super) fn plan(
    live_dir: &Path,
    stack: &EmbedderStack,
    documents: &[crate::search::IndexableDocument],
) -> Result<Delta, IncrementalFallback> {
    let live = read_digests(live_dir)?;
    if live.embedder != embedder_identity(stack) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!(
                "live generation was embedded by `{}`, the workspace now uses `{}`",
                live.embedder,
                embedder_identity(stack)
            ),
        ));
    }
    let mut delta = Delta::default();
    let mut current = std::collections::BTreeSet::new();
    for document in documents {
        current.insert(document.id.as_str());
        if live.documents.get(&document.id) != Some(&document_digest(document)) {
            delta.upserts.push(document.clone());
        }
    }
    delta.removals = live
        .documents
        .keys()
        .filter(|id| !current.contains(id.as_str()))
        .cloned()
        .collect();
    let limit = MAX_DELTA_DOCUMENTS.min(
        documents
            .len()
            .max(live.documents.len())
            .saturating_mul(MAX_DELTA_PERCENT)
            / 100,
    );
    if delta.len() > limit.max(1) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::DeltaOverThreshold,
            format!(
                "{} changed documents exceed the delta bound {}",
                delta.len(),
                limit.max(1)
            ),
        ));
    }
    let segments = lexical_segment_count(live_dir);
    if segments > MAX_LEXICAL_SEGMENTS {
        return Err(incremental_fallback(
            IncrementalFallbackReason::DeltaOverThreshold,
            format!(
                "{segments} lexical segments exceed {MAX_LEXICAL_SEGMENTS}; a full rebuild compacts them"
            ),
        ));
    }
    Ok(delta)
}

fn lexical_segment_count(generation_dir: &Path) -> usize {
    std::fs::read_dir(generation_dir.join(LEXICAL_INDEX_SUBDIR))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".idx"))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Copy the live generation's tier files into an empty private staging
/// directory. Copies, not links: tier writers mutate files in place, and the
/// vector index refuses multiply linked files by design. Admission state
/// (`meta.json`, digests, retired manifests) and writer locks are not copied;
/// the staged generation earns its own.
pub(super) fn copy_generation(
    live_dir: &Path,
    staging_dir: &Path,
) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(live_dir, "copy live index generation")?;
    ensure_index_path_has_no_symlinks(staging_dir, "copy live index generation")?;
    let mut pending = vec![(live_dir.to_path_buf(), staging_dir.to_path_buf(), true)];
    while let Some((source, destination, top_level)) = pending.pop() {
        let entries = std::fs::read_dir(&source).map_err(|error| {
            IndexRebuildError::Index(format!(
                "Failed to enumerate live index generation: {error}"
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                IndexRebuildError::Index(format!("Failed to read live index entry: {error}"))
            })?;
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                return Err(IndexRebuildError::Index(
                    "Refusing to copy a non-UTF-8 index entry".to_owned(),
                ));
            };
            if (top_level
                && (name_str == INDEX_METADATA_FILE
                    || name_str == DOC_DIGESTS_FILE
                    || name_str.starts_with(RETIRED_MANIFEST_PREFIX)))
                || TANTIVY_LOCK_FILES.contains(&name_str)
            {
                continue;
            }
            let kind = entry.file_type().map_err(|error| {
                IndexRebuildError::Index(format!("Failed to inspect live index entry: {error}"))
            })?;
            let target = destination.join(&name);
            if kind.is_dir() {
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&target).map_err(|error| {
                    IndexRebuildError::Index(format!(
                        "Failed to create staged index directory: {error}"
                    ))
                })?;
                pending.push((entry.path(), target, false));
            } else if kind.is_file() {
                std::fs::copy(entry.path(), &target).map_err(|error| {
                    IndexRebuildError::Index(format!("Failed to copy live index file: {error}"))
                })?;
            } else {
                return Err(IndexRebuildError::Index(
                    "Refusing to copy an index generation containing a symlink or special entry"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Apply a planned delta to a staged copy: removals, then upserts, one
/// compaction per vector tier and one lexical commit. Only the staging copy is
/// opened for writing; the live generation is never mutated.
pub(super) async fn apply(
    cx: &asupersync::Cx,
    staging_dir: &Path,
    stack: &EmbedderStack,
    delta: &Delta,
) -> Result<(), IncrementalFallback> {
    let tier_error =
        |detail: String| incremental_fallback(IncrementalFallbackReason::TierUnavailable, detail);

    let mut fast = super::open_fast_vector_index(staging_dir)?;
    let mut removed = false;
    for id in &delta.removals {
        removed |= fast
            .soft_delete(id)
            .map_err(|error| tier_error(format!("fast-tier vector delete failed: {error}")))?;
    }
    if removed {
        super::vacuum_incremental_vector_index(&mut fast, "fast")?;
    }
    let fast_embedder = stack.fast_arc();
    for document in &delta.upserts {
        let vector = fast_embedder
            .embed(cx, &document.content)
            .await
            .map_err(|error| tier_error(format!("fast-tier embedding failed: {error}")))?;
        fast.append(&document.id, &vector)
            .map_err(|error| tier_error(format!("fast-tier vector upsert failed: {error}")))?;
    }
    super::compact_incremental_vector_index(&mut fast, "fast")?;
    drop(fast);

    if let Some(quality_embedder) = stack.quality_arc() {
        let mut quality = super::open_quality_vector_index(staging_dir)?.ok_or_else(|| {
            tier_error(
                "quality-tier vector index is absent for a two-tier embedder stack".to_owned(),
            )
        })?;
        let mut removed = false;
        for id in &delta.removals {
            removed |= quality.soft_delete(id).map_err(|error| {
                tier_error(format!("quality-tier vector delete failed: {error}"))
            })?;
        }
        if removed {
            super::vacuum_incremental_vector_index(&mut quality, "quality")?;
        }
        for document in &delta.upserts {
            let vector = quality_embedder
                .embed(cx, &document.content)
                .await
                .map_err(|error| tier_error(format!("quality-tier embedding failed: {error}")))?;
            quality.append(&document.id, &vector).map_err(|error| {
                tier_error(format!("quality-tier vector upsert failed: {error}"))
            })?;
        }
        super::compact_incremental_vector_index(&mut quality, "quality")?;
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let lexical = super::open_lexical_index(staging_dir)?;
        for id in &delta.removals {
            lexical
                .delete_document(cx, id)
                .await
                .map_err(|error| tier_error(format!("lexical delete failed: {error}")))?;
        }
        for document in &delta.upserts {
            lexical
                .index_document(cx, document)
                .await
                .map_err(|error| tier_error(format!("lexical upsert failed: {error}")))?;
        }
        lexical
            .commit(cx)
            .await
            .map_err(|error| tier_error(format!("lexical commit failed: {error}")))?;
    }
    Ok(())
}

/// Best-effort removal of a staging directory this publisher created and
/// abandoned before publication.
pub(super) fn discard_staging(staging_dir: &Path) {
    if ensure_index_path_has_no_symlinks(staging_dir, "discard abandoned staging").is_ok()
        && let Err(error) = std::fs::remove_dir_all(staging_dir)
    {
        tracing::warn!(
            target: "ee::index",
            %error,
            staging = %staging_dir.display(),
            "abandoned delta staging could not be removed; ee index vacuum reports it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn doc(id: &str, content: &str) -> crate::search::IndexableDocument {
        crate::search::IndexableDocument::new(id, content)
    }

    fn generation_with_digests(
        label: &str,
        stack: &EmbedderStack,
        documents: &[crate::search::IndexableDocument],
    ) -> Result<tempfile::TempDir, String> {
        let root = tempfile::Builder::new()
            .prefix(&format!("ee-delta-{label}-"))
            .tempdir()
            .map_err(|error| error.to_string())?;
        DocDigests::new(stack, documents)
            .write(root.path())
            .map_err(|error| error.to_string())?;
        Ok(root)
    }

    #[test]
    fn digest_covers_every_indexed_field_and_ignores_metadata_order() {
        let base = doc("mem_a", "Pin the replay clock.").with_title("Replay");
        let mut first = base.clone();
        first.metadata.insert("kind".to_owned(), "rule".to_owned());
        first
            .metadata
            .insert("level".to_owned(), "semantic".to_owned());
        let mut second = base.clone();
        second
            .metadata
            .insert("level".to_owned(), "semantic".to_owned());
        second.metadata.insert("kind".to_owned(), "rule".to_owned());
        assert_eq!(document_digest(&first), document_digest(&second));

        let mut changed = first.clone();
        changed.content.push('!');
        assert_ne!(document_digest(&first), document_digest(&changed));
        let mut retitled = first.clone();
        retitled.title = None;
        assert_ne!(document_digest(&first), document_digest(&retitled));
        let mut tagged = first.clone();
        tagged
            .metadata
            .insert("tags".to_owned(), "release".to_owned());
        assert_ne!(document_digest(&first), document_digest(&tagged));
        // Length delimiting: moving bytes between fields changes the digest.
        assert_ne!(
            document_digest(&doc("ab", "c")),
            document_digest(&doc("a", "bc"))
        );
    }

    #[test]
    fn plan_diffs_digests_into_exact_upserts_and_removals() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let live_docs = (0..12)
            .map(|index| doc(&format!("mem_{index:02}"), &format!("lesson {index}")))
            .collect::<Vec<_>>();
        let live = generation_with_digests("plan", &stack, &live_docs)?;

        let mut current = live_docs.clone();
        current[3].content = "lesson 3, revised".to_owned();
        current.remove(7);
        current.push(doc("mem_new", "a brand new lesson"));

        let delta = plan(live.path(), &stack, &current).map_err(|error| error.detail)?;
        let upserts = delta
            .upserts
            .iter()
            .map(|document| document.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(upserts, vec!["mem_03", "mem_new"]);
        assert_eq!(delta.removals, vec!["mem_07".to_owned()]);

        let unchanged = plan(live.path(), &stack, &live_docs).map_err(|error| error.detail)?;
        assert_eq!(unchanged.len(), 0, "an unchanged corpus is an empty delta");
        Ok(())
    }

    #[test]
    fn plan_refuses_missing_digests_large_deltas_and_other_embedders() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let empty = tempfile::tempdir().map_err(|error| error.to_string())?;
        let refusal = plan(empty.path(), &stack, &[doc("mem_a", "x")])
            .err()
            .ok_or("a generation without digests must take the full rebuild")?;
        assert_eq!(refusal.reason.as_str(), "index_absent");

        let live_docs = (0..8)
            .map(|index| doc(&format!("mem_{index}"), &format!("lesson {index}")))
            .collect::<Vec<_>>();
        let live = generation_with_digests("large", &stack, &live_docs)?;
        let rewritten = live_docs
            .iter()
            .map(|document| doc(&document.id, &format!("{} rewritten", document.content)))
            .collect::<Vec<_>>();
        let refusal = plan(live.path(), &stack, &rewritten)
            .err()
            .ok_or("rewriting the whole corpus must take the full rebuild")?;
        assert_eq!(refusal.reason.as_str(), "delta_over_threshold");

        let mut foreign = DocDigests::new(&stack, &live_docs);
        foreign.embedder = "fast=other-model:256:true;quality=".to_owned();
        foreign
            .write(live.path())
            .map_err(|error| error.to_string())?;
        let refusal = plan(live.path(), &stack, &live_docs)
            .err()
            .ok_or("vectors from another embedder can never be mixed in")?;
        assert_eq!(
            refusal.reason.as_str(),
            crate::models::INDEX_INTAKE_FALLBACK_CORPUS_REVISION_MISMATCH
        );
        Ok(())
    }

    #[test]
    fn copy_generation_copies_tiers_but_never_admission_state_or_locks() -> TestResult {
        let live = tempfile::tempdir().map_err(|error| error.to_string())?;
        let staging = tempfile::tempdir().map_err(|error| error.to_string())?;
        let write = |relative: &str, body: &str| -> TestResult {
            let path = live.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(path, body).map_err(|error| error.to_string())
        };
        write("vector.fast.idx", "vectors")?;
        write(INDEX_METADATA_FILE, "{}")?;
        write(DOC_DIGESTS_FILE, "{}")?;
        write(".rejected-meta-1-000.json", "{}")?;
        write("lexical/meta.json", "tantivy meta")?;
        write("lexical/abc.idx", "segment")?;
        write("lexical/.tantivy-writer.lock", "")?;

        copy_generation(live.path(), staging.path()).map_err(|error| error.to_string())?;

        let read = |relative: &str| std::fs::read_to_string(staging.path().join(relative)).ok();
        assert_eq!(read("vector.fast.idx").as_deref(), Some("vectors"));
        assert_eq!(read("lexical/meta.json").as_deref(), Some("tantivy meta"));
        assert_eq!(read("lexical/abc.idx").as_deref(), Some("segment"));
        for excluded in [
            INDEX_METADATA_FILE,
            DOC_DIGESTS_FILE,
            ".rejected-meta-1-000.json",
            "lexical/.tantivy-writer.lock",
        ] {
            assert!(
                !staging.path().join(excluded).exists(),
                "{excluded} must not be copied into staging"
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_generation_refuses_symlinks() -> TestResult {
        let live = tempfile::tempdir().map_err(|error| error.to_string())?;
        let staging = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink("/etc/hostname", live.path().join("vector.fast.idx"))
            .map_err(|error| error.to_string())?;
        assert!(copy_generation(live.path(), staging.path()).is_err());
        Ok(())
    }
}
