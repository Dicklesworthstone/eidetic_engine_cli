//! Incremental index intake as staged delta generations
//! (bd-reality-core-convergence-1azkt.57).
//!
//! Since cancellation-safe staged publication landed, every write rebuilt and
//! re-embedded the whole corpus, so write cost grew with the store. This module
//! limits inference to changed inputs while preserving publication properties:
//! the staged generation has private mutable files and shares authoritative
//! immutable lexical components and unchanged vector tiers, with only changed
//! documents applied, validated and published through the same masked, fenced,
//! atomic path as a full build. Source reconciliation and vector copies for
//! changed embedding inputs still scale with the corpus; total write cost is
//! not O(delta).
//!
//! The delta is not taken from job bookkeeping. Every generation records a
//! digest of each document it indexed (`doc_digests.json`). The publisher
//! diffs that map against the authoritative source snapshot it already holds:
//! a changed digest is an upsert, a missing id a removal. A change made by any
//! write path, with or without an index job, is therefore always visible, and
//! a generation without digests (or built by another embedder) simply takes
//! the full rebuild. Large vector deltas and staging failures also fall back.
//! Lexical segment pressure is compacted independently on the staged copy,
//! without sending unchanged documents through either embedding model.
//!
//! V2 binds both tiers to complete immutable embedding identities. V3 also
//! records exact embedding inputs, allowing metadata-only changes to update
//! lexical documents without recomputing unchanged vectors. V2 remains usable
//! with conservative inference; V1 cannot prove compatible embedding spaces.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use frankensearch::core::traits::IdentityBoundEmbedding;
use serde::{Deserialize, Serialize};

#[cfg(feature = "lexical-bm25")]
use crate::search::LexicalWrite;

use super::{
    EmbedderStack, INDEX_METADATA_FILE, IncrementalFallback, IncrementalFallbackReason,
    IndexRebuildError, ensure_index_path_has_no_symlinks, incremental_fallback,
};

#[cfg(feature = "lexical-bm25")]
use super::LEXICAL_INDEX_SUBDIR;

#[path = "index_delta_batch.rs"]
mod batch;

#[path = "index_delta_metadata.rs"]
mod metadata;

#[path = "index_delta_stream.rs"]
mod stream;

pub(super) use stream::{ReconciledSource, SourceReconciler};

#[cfg(feature = "lexical-bm25")]
#[path = "index_delta_lexical.rs"]
mod lexical_maintenance;

#[path = "index_staging_copy.rs"]
mod staging_copy;

#[cfg(all(test, unix, feature = "lexical-bm25"))]
#[path = "index_delta_sharing_tests.rs"]
mod sharing_tests;

#[cfg(all(test, unix))]
#[path = "index_delta_vector_sharing_tests.rs"]
mod vector_sharing_tests;

/// Per-document digests of the documents a generation indexed.
pub(super) const DOC_DIGESTS_FILE: &str = "doc_digests.json";
const DOC_DIGESTS_SCHEMA_V2: &str = "ee.index.doc_digests.v2";
const DOC_DIGESTS_SCHEMA_V3: &str = "ee.index.doc_digests.v3";
const DOC_DIGEST_DOMAIN: &[u8] = b"ee.index.doc_digest.v1\0";
const EMBEDDING_INPUT_DOMAIN: &[u8] = b"ee.index.embedding_input.v1\0";
const DIGEST_READ_BUFFER_BYTES: usize = 16 * 1024;

/// Bound changed vector inputs plus removals. V2 cannot prove input reuse and
/// charges every change; v3 metadata-only updates have separate lexical limits.
const MAX_DELTA_DOCUMENTS: usize = 256;
/// Above this share of the corpus (percent) a vector rebuild is preferred.
const MAX_DELTA_PERCENT: usize = 25;
/// Past this many active lexical segments, compact the staged lexical tier
/// before applying the next delta. Vector inference keeps its own budget.
#[cfg(any(test, feature = "lexical-bm25"))]
const MAX_LEXICAL_SEGMENTS: usize = 32;

/// Files that belong to one generation's admission state, never to its tiers.
const TANTIVY_LOCK_FILES: [&str; 2] = [".tantivy-writer.lock", ".tantivy-meta.lock"];
const RETIRED_MANIFEST_PREFIX: &str = ".rejected-meta-";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DocDigests {
    schema: String,
    // None preserves full-build availability for an embedder that cannot yet
    // attest its identity, but never authorizes incremental reuse.
    embedder: Option<String>,
    #[serde(deserialize_with = "deserialize_digest_map")]
    documents: BTreeMap<String, String>,
    // Absent only in v2. Never infer input equality from a metadata-only
    // heuristic or let an incomplete v3 map silently authorize reuse.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_digest_map"
    )]
    embedding_inputs: Option<BTreeMap<String, String>>,
}

/// Duplicate JSON keys are corruption, not permission to reuse whichever
/// vector proof happens to be last. Ordinary BTreeMap deserialization erases
/// that evidence before the complete-schema checks can inspect it.
fn deserialize_digest_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueDigests;

    impl<'de> serde::de::Visitor<'de> for UniqueDigests {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique document identities to digests")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut digests = BTreeMap::new();
            while let Some(id) = map.next_key::<String>()? {
                let std::collections::btree_map::Entry::Vacant(entry) = digests.entry(id) else {
                    return Err(serde::de::Error::custom(
                        "duplicate document identity in index digest map",
                    ));
                };
                entry.insert(map.next_value::<String>()?);
            }
            Ok(digests)
        }
    }

    deserializer.deserialize_map(UniqueDigests)
}

fn deserialize_optional_digest_map<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct UniqueDigests(
        #[serde(deserialize_with = "deserialize_digest_map")] BTreeMap<String, String>,
    );

    Option::<UniqueDigests>::deserialize(deserializer).map(|digests| digests.map(|value| value.0))
}

#[derive(Debug)]
struct EmbeddingReuse {
    identity: String,
    inputs: BTreeMap<String, String>,
}

/// The documents to apply to a copy of the live generation.
#[derive(Debug, Default)]
pub(super) struct Delta {
    pub upserts: Vec<crate::search::IndexableDocument>,
    pub removals: Vec<String>,
    reuse: Option<EmbeddingReuse>,
}

impl Delta {
    // Its only caller is the MAX_DELTA_DOCUMENTS assertion in this file's `#[cfg(test)]`
    // module (`assert_eq!(delta.len(), MAX_DELTA_DOCUMENTS)`), and `cargo clippy --lib`
    // does not compile cfg(test), so the lint sees a method nobody calls. Under
    // `-D warnings` that became `error: method `len` is never used` and took the WHOLE
    // production lint gate down -- which reddened primer-admission-20260919 and
    // index-generation-admission-20260925 at their clippy step, after every one of their
    // own test arms had passed.
    //
    // Annotated rather than deleted: the test assertion is a real use, and removing a
    // bound-checking helper to satisfy a lint that cannot see its caller would be fixing
    // the wrong thing. Annotated rather than `#[cfg(test)]`-gated because `pub(super)`
    // says this was meant to be callable from production, and gating it would quietly
    // remove that option.
    #[allow(
        dead_code,
        reason = "called only from this file's cfg(test) module, which cargo clippy --lib does not compile"
    )]
    pub(super) fn len(&self) -> usize {
        self.upserts.len().saturating_add(self.removals.len())
    }

    /// Mirror apply's identity-bound decision before pricing native compaction.
    /// Metadata-only upserts reuse vectors; removals and changed inputs may
    /// rewrite each complete main tier even when the source delta is tiny.
    pub(super) fn requires_vector_rewrite(
        &self,
        stack: &EmbedderStack,
    ) -> Result<bool, IncrementalFallback> {
        let identity = embedder_identity(stack).ok_or_else(|| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                "delta storage admission requires complete validated embedding identities",
            )
        })?;
        let upserts = self.vector_upserts(&identity)?;
        Ok(!upserts.is_empty() || !self.removals.is_empty())
    }

    /// Recheck the plan's commitments at use, before either tier is mutated.
    /// Public upsert/removal lists may have changed after planning; a changed
    /// body, changed ID, or remove-then-upsert must receive a new vector.
    /// The complete lexical upserts remain unchanged and are always applied.
    fn vector_upserts(
        &self,
        identity: &str,
    ) -> Result<Cow<'_, [crate::search::IndexableDocument]>, IncrementalFallback> {
        let Some(reuse) = &self.reuse else {
            return Ok(Cow::Borrowed(&self.upserts));
        };
        if reuse.identity != identity {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "embedding producer changed after delta planning",
            ));
        }
        if reuse.inputs.is_empty() {
            return Ok(Cow::Borrowed(&self.upserts));
        }
        // V3 admits thousands of metadata updates and withdrawals together.
        // Index removal membership once, preserving the use-time remove/re-add
        // guard without comparing every upsert with every removed ID.
        let removed_ids = self
            .removals
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        Ok(Cow::Owned(
            self.upserts
                .iter()
                .filter(|document| {
                    removed_ids.contains(document.id.as_str())
                        || reuse.inputs.get(&document.id) != Some(&embedding_input_digest(document))
                })
                .cloned()
                .collect(),
        ))
    }
}

/// Complete producer, space, input and output contract of each tier. A model
/// name and dimension are diagnostics, not vector-space compatibility: two JL
/// seeds (or two revisions of neural weights) can have both in common.
///
/// Delegate canonical fingerprinting and validation to Frankensearch. Missing
/// or malformed identities never become a shared "unknown" identity.
pub(super) fn embedder_identity(stack: &EmbedderStack) -> Option<String> {
    let fast = verified_embedder_identity(stack.fast())?;
    let quality = match stack.quality() {
        Some(quality) => verified_embedder_identity(quality)?,
        None => String::new(),
    };
    Some(format!("fast={fast};quality={quality}"))
}

pub(super) fn verified_embedder_identity(embedder: &dyn crate::search::Embedder) -> Option<String> {
    let identity = embedder.identity().ok()?;
    identity.validate().ok()?;
    if usize::try_from(identity.space.dimension).ok() != Some(embedder.dimension()) {
        return None;
    }
    Some(identity.fingerprint())
}

/// Check the values and their producer together before they enter a staged
/// tier. Even an identity-aware wrapper must not return vectors from a fallback
/// or a different model while advertising the originally selected producer.
fn validate_bound_embedding(
    embedding: &IdentityBoundEmbedding,
    expected_identity: &str,
    tier: &str,
) -> Result<(), IncrementalFallback> {
    embedding.validate().map_err(|_| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("{tier}-tier embedding output has an invalid identity or dimension"),
        )
    })?;
    if embedding.identity.fingerprint() != expected_identity {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!("{tier}-tier embedding output changed the planned producer identity"),
        ));
    }
    Ok(())
}

fn update_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Both `IndexBuilder` and delta inference embed exactly `document.content`.
/// Bind the document ID too: identical text under a new ID still needs its own
/// vector record. No normalization may erase case, whitespace or Unicode.
fn embedding_input_digest(document: &crate::search::IndexableDocument) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMBEDDING_INPUT_DOMAIN);
    update_field(&mut hasher, document.id.as_bytes());
    update_field(&mut hasher, document.content.as_bytes());
    hasher.finalize().to_hex().to_string()
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
            schema: DOC_DIGESTS_SCHEMA_V3.to_owned(),
            embedder: embedder_identity(stack),
            documents: digest_map(documents),
            embedding_inputs: Some(
                documents
                    .iter()
                    .map(|document| (document.id.clone(), embedding_input_digest(document)))
                    .collect(),
            ),
        }
    }

    /// Record the digests in a staged generation. Written before the
    /// generation is flushed, so the same durability barrier covers them.
    pub(super) fn write(&self, generation_dir: &Path) -> Result<(), IndexRebuildError> {
        let path = generation_dir.join(DOC_DIGESTS_FILE);
        ensure_index_path_has_no_symlinks(&path, "write index document digests")?;
        let file = std::fs::File::create(&path).map_err(|error| {
            IndexRebuildError::Index(format!("Failed to write index document digests: {error}"))
        })?;
        self.write_to(file)
    }

    /// The maps already own the corpus's digest strings. Do not allocate a
    /// second, corpus-sized serialized buffer just to write staging metadata.
    /// This is not publication: the caller's existing durability barrier and
    /// admission checks still own the resulting private generation.
    fn write_to(&self, writer: impl std::io::Write) -> Result<(), IndexRebuildError> {
        use std::io::Write;

        let mut writer = std::io::BufWriter::with_capacity(16 * 1024, writer);
        serde_json::to_writer(&mut writer, self).map_err(|error| {
            IndexRebuildError::Index(format!("Failed to encode index document digests: {error}"))
        })?;
        // BufWriter's Drop ignores errors. A failed final flush must prevent
        // the publisher from accepting this generation, even for tiny maps.
        writer.flush().map_err(|error| {
            IndexRebuildError::Index(format!("Failed to write index document digests: {error}"))
        })
    }
}

fn read_digests(generation_dir: &Path) -> Result<DocDigests, IncrementalFallback> {
    let request = asupersync::Cx::current();
    let checkpoint = || {
        if let Some(cx) = &request {
            batch::checkpoint(cx, "document digest loading")?;
        }
        Ok(())
    };
    checkpoint()?;
    let path = generation_dir.join(DOC_DIGESTS_FILE);
    let metadata = std::fs::symlink_metadata(&path);
    checkpoint()?;
    let metadata = metadata.map_err(|error| {
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
    let file = std::fs::File::open(&path);
    checkpoint()?;
    let file = file.map_err(|error| {
        incremental_fallback(
            IncrementalFallbackReason::TierUnavailable,
            format!("failed to read live document digests: {error}"),
        )
    })?;
    read_digests_with_checkpoint(file, checkpoint)
}

/// Check the installed request around each bounded filesystem read. Keep its
/// original refusal separate from serde's I/O error so cancellation cannot be
/// mislabeled as a corrupt manifest or retried as Interrupted by the decoder.
struct DigestReader<R, C> {
    reader: R,
    checkpoint: C,
    interrupted: Option<IncrementalFallback>,
}

impl<R, C> DigestReader<R, C>
where
    C: FnMut() -> Result<(), IncrementalFallback>,
{
    fn checkpoint(&mut self) -> std::io::Result<()> {
        if self.interrupted.is_none() {
            self.interrupted = (self.checkpoint)().err();
        }
        if self.interrupted.is_some() {
            Err(std::io::Error::other("index digest loading interrupted"))
        } else {
            Ok(())
        }
    }
}

impl<R, C> std::io::Read for DigestReader<R, C>
where
    R: std::io::Read,
    C: FnMut() -> Result<(), IncrementalFallback>,
{
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.checkpoint()?;
        let result = self.reader.read(buffer);
        // Cancellation wins even when this read returned invalid bytes, EOF,
        // an ordinary failure or EINTR. No further refill may follow it.
        self.checkpoint()?;
        result
    }
}

/// Retain the required digest maps, not a second serialized copy of the whole
/// corpus. Buffering goes outside the checkpoint reader: serde reads bytes
/// individually, while filesystem I/O and cooperative checks stay bounded to
/// refills. An individual decoded field can still exceed the input buffer.
fn read_digests_with_checkpoint(
    reader: impl std::io::Read,
    checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<DocDigests, IncrementalFallback> {
    let mut reader = std::io::BufReader::with_capacity(
        DIGEST_READ_BUFFER_BYTES,
        DigestReader {
            reader,
            checkpoint,
            interrupted: None,
        },
    );
    let parsed = serde_json::from_reader::<_, DocDigests>(&mut reader);
    let mut reader = reader.into_inner();
    if let Some(error) = reader.interrupted.take() {
        return Err(error);
    }
    // Parsing can finish (or fail) within the last buffer. Observe any request
    // change before returning a schema/I/O error or accepting the manifest.
    (reader.checkpoint)()?;
    let digests = parsed.map_err(|error| {
        if error.is_io() {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("failed to read live document digests: {error}"),
            )
        } else {
            incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                format!("live document digests are malformed: {error}"),
            )
        }
    })?;
    let valid_schema = match digests.schema.as_str() {
        DOC_DIGESTS_SCHEMA_V2 => digests.embedding_inputs.is_none(),
        DOC_DIGESTS_SCHEMA_V3 => digests
            .embedding_inputs
            .as_ref()
            .is_some_and(|inputs| inputs.len() == digests.documents.len()),
        _ => false,
    };
    if !valid_schema {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            format!(
                "unsupported or incomplete document digest schema {}",
                digests.schema
            ),
        ));
    }
    let valid_digest = |digest: &str| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    };
    for (id, digest) in &digests.documents {
        (reader.checkpoint)()?;
        if id.is_empty() || !valid_digest(digest) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "live document digest map is malformed",
            ));
        }
    }
    if let Some(inputs) = &digests.embedding_inputs {
        for ((id, _), (input_id, input)) in digests.documents.iter().zip(inputs) {
            (reader.checkpoint)()?;
            if id != input_id || !valid_digest(input) {
                return Err(incremental_fallback(
                    IncrementalFallbackReason::CorpusRevisionMismatch,
                    "live embedding-input digest map is malformed",
                ));
            }
        }
    }
    (reader.checkpoint)()?;
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
    let identity = embedder_identity(stack).ok_or_else(|| {
        incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            "workspace embedders do not supply complete validated identities",
        )
    })?;
    if live.embedder.as_deref() != Some(identity.as_str()) {
        return Err(incremental_fallback(
            IncrementalFallbackReason::CorpusRevisionMismatch,
            "live generation has no matching complete embedding identity; a full rebuild is required",
        ));
    }
    if let Some(inputs) = live.embedding_inputs.as_ref() {
        metadata::diff(&live.documents, inputs, documents, identity)
    } else {
        let mut delta = diff_documents(&live.documents, documents, document_digest)?;
        // V2 cannot prove input reuse, but its validated producer must still
        // hold at apply time. An empty input map re-embeds every changed row
        // while retaining the same producer fence as a v3 plan.
        delta.reuse = Some(EmbeddingReuse {
            identity,
            inputs: BTreeMap::new(),
        });
        Ok(delta)
    }
}

/// Decide eligibility before taking ownership of any changed document body.
/// A rejected full-corpus rewrite used to clone the whole corpus first, even
/// though at most 256 changes can be applied incrementally. Keep only bounded
/// borrowed upserts, and stop as soon as the shared upsert/removal budget is
/// exhausted. The source snapshot and live digest map remain caller-owned.
fn diff_documents(
    live: &BTreeMap<String, String>,
    documents: &[crate::search::IndexableDocument],
    mut digest: impl FnMut(&crate::search::IndexableDocument) -> String,
) -> Result<Delta, IncrementalFallback> {
    let limit = MAX_DELTA_DOCUMENTS
        .min(
            documents
                .len()
                .max(live.len())
                .saturating_mul(MAX_DELTA_PERCENT)
                / 100,
        )
        .max(1);
    let over_limit = || {
        incremental_fallback(
            IncrementalFallbackReason::DeltaOverThreshold,
            format!("more than {limit} changed documents; a full rebuild is required"),
        )
    };
    let mut current = std::collections::BTreeSet::new();
    let mut upserts = Vec::new();
    for document in documents {
        if !current.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        if live.get(&document.id) != Some(&digest(document)) {
            if upserts.len() == limit {
                return Err(over_limit());
            }
            upserts.push(document);
        }
    }
    let mut removals = Vec::new();
    for id in live.keys().filter(|id| !current.contains(id.as_str())) {
        if upserts.len() + removals.len() == limit {
            return Err(over_limit());
        }
        removals.push(id.clone());
    }
    Ok(Delta {
        upserts: upserts.into_iter().cloned().collect(),
        removals,
        ..Delta::default()
    })
}

/// Native reader locks keep admitted main files read-only while staging shares
/// or copies them. Their exact identities also bind the later filesystem copy
/// to the files whose empty WAL and producer contract were checked here.
struct AdmittedVectors {
    share: bool,
    files: BTreeMap<PathBuf, (std::fs::Metadata, frankensearch::VectorIndex)>,
}

impl AdmittedVectors {
    fn open(
        live_dir: &Path,
        stack: &EmbedderStack,
        delta: &Delta,
    ) -> Result<Self, IndexRebuildError> {
        let rewrite = delta.requires_vector_rewrite(stack).map_err(|fallback| {
            IndexRebuildError::Index(format!("{}: {}", fallback.reason.as_str(), fallback.detail))
        })?;
        let (fast, quality) = super::stack_descriptors(stack);
        let fast_name =
            if super::path_is_regular_file_no_follow(&live_dir.join(super::VECTOR_INDEX_FAST_FILE))
            {
                super::VECTOR_INDEX_FAST_FILE
            } else {
                super::VECTOR_INDEX_FALLBACK_FILE
            };
        let mut files = BTreeMap::new();
        for (name, descriptor) in [
            Some((fast_name, &fast)),
            quality
                .as_ref()
                .map(|descriptor| (super::VECTOR_INDEX_QUALITY_FILE, descriptor)),
        ]
        .into_iter()
        .flatten()
        {
            let path = live_dir.join(name);
            ensure_index_path_has_no_symlinks(&path, "admit published vector staging source")?;
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                IndexRebuildError::Index(format!(
                    "Failed to inspect published vector staging source: {error}"
                ))
            })?;
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(IndexRebuildError::Index(
                    "Published vector staging source is not a regular file".to_owned(),
                ));
            }
            // This native compatibility API admits only legacy FSVI v1. V2's
            // single-link immutable admission contract is deliberately separate.
            // EE's strict adapter also refuses any pending/recoverable WAL.
            let reader = super::vector_admission::open_read_only(&path).map_err(|error| {
                IndexRebuildError::Index(format!(
                    "Failed to admit published vector staging source: {error}"
                ))
            })?;
            staging_copy::verify_published_vector_identity(&path, &metadata).map_err(|error| {
                IndexRebuildError::Index(format!(
                    "Published vector staging source changed during native admission: {error}"
                ))
            })?;
            super::verify_vector_producer(&reader, descriptor, name)
                .map_err(IndexRebuildError::Index)?;
            files.insert(PathBuf::from(name), (metadata, reader));
        }
        Ok(Self {
            // The writer guard can prove an independent inode on Unix. Other
            // platforms retain private copies until they have that same proof.
            share: cfg!(unix) && !rewrite,
            files,
        })
    }
}

/// Stage a delta with private metadata/WAL and authoritative immutable lexical
/// segments. Complete producer/input proof permits unchanged vector main files
/// to be shared; any vector mutation first copies all selected main tiers into
/// independent inodes, including sources shared by older generations.
pub(super) fn copy_generation_for_delta(
    live_dir: &Path,
    staging_dir: &Path,
    stack: &EmbedderStack,
    delta: &Delta,
) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(live_dir, "copy live index generation")?;
    ensure_index_path_has_no_symlinks(staging_dir, "copy live index generation")?;
    let vectors = AdmittedVectors::open(live_dir, stack, delta)?;
    copy_generation_files(live_dir, staging_dir, Some(&vectors))
}

/// Low-level fixtures without native vector admission always copy vectors.
#[cfg(test)]
fn copy_generation(live_dir: &Path, staging_dir: &Path) -> Result<(), IndexRebuildError> {
    copy_generation_files(live_dir, staging_dir, None)
}

/// Unsupported sharing/cloning falls back to independent byte copies.
/// Admission state (`meta.json`, digests, retired manifests) and writer locks
/// are not copied; staging earns its own. Only the admitted vector allowlist
/// may copy a multiply linked vector source; arbitrary aliases stay refused.
fn copy_generation_files(
    live_dir: &Path,
    staging_dir: &Path,
    vectors: Option<&AdmittedVectors>,
) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(live_dir, "copy live index generation")?;
    ensure_index_path_has_no_symlinks(staging_dir, "copy live index generation")?;
    let immutable_segments = immutable_lexical_segments(live_dir)?;
    let mut copier = staging_copy::StagingCopier::default();
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
                let source_path = entry.path();
                let relative = source_path.strip_prefix(live_dir).map_err(|_| {
                    IndexRebuildError::Index(
                        "Index staging source escaped its generation".to_owned(),
                    )
                })?;
                let copied = if let Some(vectors) = vectors
                    && let Some((admitted, _reader)) = vectors.files.get(relative)
                {
                    if vectors.share {
                        copier.share_unchanged_vector(&source_path, &target, admitted)
                    } else {
                        copier.copy_published_vector(&source_path, &target, admitted)
                    }
                } else if immutable_segments.contains(relative) {
                    copier.share_immutable_segment(&source_path, &target)
                } else {
                    copier.copy_file(&source_path, &target)
                };
                copied.map_err(|error| {
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
    let stats = copier.stats();
    tracing::debug!(
        target: "ee::index",
        copied_files = stats.copied_files,
        copied_bytes = stats.copied_bytes,
        cloned_files = stats.cloned_files,
        cloned_bytes = stats.cloned_bytes,
        clone_refusals = stats.clone_refusals,
        linked_files = stats.linked_files,
        linked_bytes = stats.linked_bytes,
        link_refusals = stats.link_refusals,
        shared_vector_tiers = vectors.is_some_and(|vectors| vectors.share),
        "staged mutable index files and shared immutable components"
    );
    Ok(())
}

/// Tantivy's Directory contract is write-once for segment components: their
/// writers use create_new, merges get new segment IDs, and deletion bitsets get
/// new opstamp-qualified paths. Never infer immutability from a file extension
/// or the managed-file inventory, which can also contain obsolete files.
fn immutable_lexical_segments(live_dir: &Path) -> Result<BTreeSet<PathBuf>, IndexRebuildError> {
    #[cfg(not(feature = "lexical-bm25"))]
    {
        let _ = live_dir;
        Ok(BTreeSet::new())
    }
    #[cfg(feature = "lexical-bm25")]
    {
        let request = asupersync::Cx::current();
        let checkpoint = || -> Result<(), IndexRebuildError> {
            if let Some(cx) = &request {
                cx.checkpoint().map_err(|error| {
                    IndexRebuildError::Index(format!("Index staging request interrupted: {error}"))
                })?;
            }
            Ok(())
        };
        checkpoint()?;
        let lexical_path = live_dir.join(LEXICAL_INDEX_SUBDIR);
        // The copier also serves empty/vector-only fixtures. Without an
        // authoritative lexical manifest, no file earns permission to share.
        let meta_path = lexical_path.join("meta.json");
        ensure_index_path_has_no_symlinks(&meta_path, "read live lexical segment metadata")?;
        let metadata = std::fs::symlink_metadata(&meta_path);
        checkpoint()?;
        match metadata {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new());
            }
            Err(error) => {
                return Err(IndexRebuildError::Index(format!(
                    "Failed to inspect live lexical segment metadata: {error}"
                )));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(IndexRebuildError::Index(
                    "Live lexical segment metadata is not a regular file".to_owned(),
                ));
            }
            Ok(_) => {}
        }
        let managed_path = lexical_path.join(".managed.json");
        ensure_index_path_has_no_symlinks(&managed_path, "read live lexical managed metadata")?;
        let managed_metadata = std::fs::symlink_metadata(&managed_path);
        checkpoint()?;
        match managed_metadata {
            Ok(metadata) if metadata.is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(IndexRebuildError::Index(
                    "Live lexical managed metadata is not a regular file".to_owned(),
                ));
            }
            Err(error) => {
                return Err(IndexRebuildError::Index(format!(
                    "Failed to inspect live lexical managed metadata: {error}"
                )));
            }
        }
        checkpoint()?;
        let index = frankensearch::lexical_tantivy::Index::open_in_dir(&lexical_path);
        checkpoint()?;
        let index = index.map_err(|error| {
            IndexRebuildError::Index(format!(
                "Failed to open live lexical segment metadata: {error}"
            ))
        })?;
        let segments = index.searchable_segment_metas();
        checkpoint()?;
        let segments = segments.map_err(|error| {
            IndexRebuildError::Index(format!(
                "Failed to enumerate live lexical segments: {error}"
            ))
        })?;
        let mut files = BTreeSet::new();
        for segment in segments {
            checkpoint()?;
            for path in segment.list_files() {
                let mut components = path.components();
                if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                    || components.next().is_some()
                {
                    return Err(IndexRebuildError::Index(
                        "Live lexical segment metadata names a non-local file".to_owned(),
                    ));
                }
                // Some components (notably a segment's initial .0.del) are
                // absent. Only existing files encountered by the copier use
                // this allowlist; absence never creates a link or a payload.
                files.insert(Path::new(LEXICAL_INDEX_SUBDIR).join(path));
            }
        }
        checkpoint()?;
        Ok(files)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VectorFinalization {
    Unchanged,
    Compacted,
    Vacuumed,
}

/// Finish the complete delta once, not once for deletions and again for
/// upserts. Frankensearch compaction already omits every tombstoned main
/// record while merging the WAL. Vacuuming first would rewrite the same
/// corpus twice. A deletion-only delta still needs vacuum because compact()
/// is a no-op when the WAL is empty. Published generations remain fully
/// compacted; this does not introduce WAL-bearing reader or recovery state.
async fn apply_vector_delta(
    cx: &asupersync::Cx,
    index: &mut frankensearch::VectorIndex,
    embedder: &dyn crate::search::Embedder,
    delta: &Delta,
    vector_upserts: &[crate::search::IndexableDocument],
    expected_identity: &str,
    tier: &str,
) -> Result<VectorFinalization, IncrementalFallback> {
    batch::checkpoint(cx, tier)?;
    let removals: Vec<_> = delta.removals.iter().map(String::as_str).collect();
    let removed = if removals.is_empty() {
        0
    } else {
        index.soft_delete_batch(&removals).map_err(|error| {
            incremental_fallback(
                IncrementalFallbackReason::TierUnavailable,
                format!("{tier}-tier vector delete failed: {error}"),
            )
        })?
    };
    batch::upsert(cx, index, embedder, vector_upserts, expected_identity, tier).await?;
    batch::checkpoint(cx, tier)?;
    let finalization = if index.wal_record_count() > 0 {
        super::compact_incremental_vector_index(index, tier)?;
        VectorFinalization::Compacted
    } else if removed > 0 {
        super::vacuum_incremental_vector_index(index, tier)?;
        VectorFinalization::Vacuumed
    } else {
        VectorFinalization::Unchanged
    };
    batch::checkpoint(cx, tier)?;
    Ok(finalization)
}

/// Apply a planned delta to a staged copy: removals, then upserts, at most one
/// corpus rewrite per vector tier and at most two explicit lexical commits
/// (one removal batch and one upsert batch). Only the staging copy is opened
/// for writing; the live generation is never mutated.
pub(super) async fn apply(
    cx: &asupersync::Cx,
    staging_dir: &Path,
    stack: &EmbedderStack,
    delta: &Delta,
) -> Result<(), IncrementalFallback> {
    batch::checkpoint(cx, "delta")?;
    let tier_error =
        |detail: String| incremental_fallback(IncrementalFallbackReason::TierUnavailable, detail);

    // Resolve both identities before any staging mutation, including removals.
    let fast_identity = verified_embedder_identity(stack.fast()).ok_or_else(|| {
        tier_error("fast-tier embedder has no complete validated identity".to_owned())
    })?;
    let quality_identity = stack
        .quality()
        .map(|quality| {
            verified_embedder_identity(quality).ok_or_else(|| {
                tier_error("quality-tier embedder has no complete validated identity".to_owned())
            })
        })
        .transpose()?;
    let identity = format!(
        "fast={fast_identity};quality={}",
        quality_identity.as_deref().unwrap_or("")
    );
    let vector_upserts = delta.vector_upserts(&identity)?;
    tracing::debug!(
        target: "ee::index",
        lexical_upserts = delta.upserts.len(),
        vector_upserts = vector_upserts.len(),
        "selected identity-bound delta embedding inputs"
    );

    // A metadata-only change must not acquire vector writer locks or touch
    // vector bytes. The staging caller still validates every tier's counts
    // and compatibility before flushing and publishing the generation.
    if !vector_upserts.is_empty() || !delta.removals.is_empty() {
        batch::checkpoint(cx, "fast")?;
        let mut fast = super::open_fast_vector_index(staging_dir)?;
        apply_vector_delta(
            cx,
            &mut fast,
            stack.fast(),
            delta,
            &vector_upserts,
            &fast_identity,
            "fast",
        )
        .await?;
        drop(fast);

        if let Some(quality_embedder) = stack.quality_arc() {
            let expected_identity = quality_identity.as_deref().ok_or_else(|| {
                tier_error("quality-tier embedder appeared after identity validation".to_owned())
            })?;
            batch::checkpoint(cx, "quality")?;
            let mut quality = super::open_quality_vector_index(staging_dir)?.ok_or_else(|| {
                tier_error(
                    "quality-tier vector index is absent for a two-tier embedder stack".to_owned(),
                )
            })?;
            apply_vector_delta(
                cx,
                &mut quality,
                quality_embedder.as_ref(),
                delta,
                &vector_upserts,
                expected_identity,
                "quality",
            )
            .await?;
        }
    }

    #[cfg(feature = "lexical-bm25")]
    {
        // Finish the already-admitted vector allocations before lexical
        // writers measure the remaining capacity. Withdraw lexical IDs before
        // merging, so compaction never copies documents being removed. Every
        // writer still belongs exclusively to this unpublished staging copy.
        lexical_maintenance::delete_documents(cx, staging_dir, &delta.removals)?;
        lexical_maintenance::compact_if_needed(cx, staging_dir, MAX_LEXICAL_SEGMENTS)?;
        batch::checkpoint(cx, "lexical")?;
        // Removals already committed before maintenance. A deletion-only or
        // empty delta needs neither another writer nor an empty commit here.
        if !delta.upserts.is_empty() {
            lexical_maintenance::admit_upserts(cx, staging_dir, &delta.upserts)?;
            let lexical = super::open_lexical_index(staging_dir)?;
            for documents in lexical_maintenance::upsert_batches(&delta.upserts) {
                batch::checkpoint(cx, "lexical")?;
                let result = lexical.index_documents(cx, documents).await;
                // Cancellation wins even if a backend finishes a batch without
                // observing the caller's Cx. Never proceed to another batch or
                // a commit after interruption; the private stage is discarded.
                batch::checkpoint(cx, "lexical")?;
                result.map_err(|error| tier_error(format!("lexical upsert failed: {error}")))?;
            }
            batch::checkpoint(cx, "lexical")?;
            let result = lexical.commit(cx).await;
            batch::checkpoint(cx, "lexical")?;
            result.map_err(|error| tier_error(format!("lexical commit failed: {error}")))?;
        }
        batch::checkpoint(cx, "lexical")?;
    }
    Ok(())
}

/// Best-effort removal of a staging directory this publisher created and
/// abandoned before publication.
#[cfg(unix)]
pub(super) fn discard_staging(staging_dir: &Path) {
    if ensure_index_path_has_no_symlinks(staging_dir, "discard abandoned staging").is_ok()
        && let Err(error) = std::fs::remove_dir_all(staging_dir)
    {
        tracing::warn!(
            target: "ee::index",
            %error,
            staging = %staging_dir.display(),
            "abandoned index staging could not be removed; ee index vacuum reports it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Embedder as _, HashEmbedder};
    use frankensearch::core::generation::EmbeddingIdentityBundleV1;
    use frankensearch::core::traits::{ModelCategory, SearchFuture};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            .tempdir_in(
                std::env::temp_dir()
                    .canonicalize()
                    .map_err(|error| error.to_string())?,
            )
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
    fn a_large_rewrite_stops_hashing_after_the_first_over_budget_change() {
        let documents: Vec<_> = (0..2000)
            .map(|index| doc(&format!("mem_{index:04}"), "original body"))
            .collect();
        let live = digest_map(&documents);
        let changed: Vec<_> = documents
            .iter()
            .map(|document| doc(&document.id, "changed body"))
            .collect();
        let mut hashed = 0;
        let error = diff_documents(&live, &changed, |document| {
            hashed += 1;
            document_digest(document)
        })
        .expect_err("large rewrites must not stage an incremental generation");
        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
        assert_eq!(hashed, MAX_DELTA_DOCUMENTS + 1);
        assert!(hashed < changed.len());
    }

    #[test]
    fn upserts_and_removals_share_the_exact_inclusive_delta_bound() -> TestResult {
        let original: Vec<_> = (0..1024)
            .map(|index| doc(&format!("mem_{index:04}"), "original body"))
            .collect();
        let live = digest_map(&original);
        let mut changed = original[..896].to_vec();
        for document in &mut changed[..128] {
            document.content = "changed body".to_owned();
        }
        let delta =
            diff_documents(&live, &changed, document_digest).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 128);
        assert_eq!(delta.removals.len(), 128);
        assert_eq!(delta.len(), MAX_DELTA_DOCUMENTS);
        changed[128].content = "one change too many".to_owned();
        assert_eq!(
            diff_documents(&live, &changed, document_digest)
                .expect_err("shared budget")
                .reason,
            IncrementalFallbackReason::DeltaOverThreshold,
        );
        let deleted = diff_documents(&live, &original[..768], document_digest)
            .map_err(|error| error.detail)?;
        assert_eq!(deleted.removals.len(), MAX_DELTA_DOCUMENTS);
        assert!(deleted.upserts.is_empty());
        assert!(diff_documents(&live, &original[..767], document_digest).is_err());
        Ok(())
    }

    #[test]
    fn bounded_diff_matches_full_reference_across_insert_update_delete_combinations() {
        let original: Vec<_> = (0..8)
            .map(|index| doc(&format!("mem_{index}"), "original body"))
            .collect();
        let live = digest_map(&original);
        let mut accepted = 0;
        let mut refused = 0;
        for removed_mask in 0_u8..16 {
            for changed_mask in 0_u8..16 {
                let mut current: Vec<_> = original
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index >= 4 || removed_mask & (1_u8 << *index) == 0)
                    .map(|(index, document)| {
                        let mut document = document.clone();
                        if index < 4 && changed_mask & (1_u8 << index) != 0 {
                            document.content = "updated body".to_owned();
                        }
                        document
                    })
                    .collect();
                if removed_mask & 1 != 0 {
                    current.push(doc("mem_new", "newly inserted body"));
                }
                let expected_upserts: Vec<_> = current
                    .iter()
                    .filter(|document| live.get(&document.id) != Some(&document_digest(document)))
                    .map(|document| (document.id.clone(), document.content.clone()))
                    .collect();
                let expected_removals: Vec<_> = live
                    .keys()
                    .filter(|id| !current.iter().any(|document| &document.id == *id))
                    .cloned()
                    .collect();
                match diff_documents(&live, &current, document_digest) {
                    Ok(delta) => {
                        accepted += 1;
                        assert!(expected_upserts.len() + expected_removals.len() <= 2);
                        assert_eq!(
                            delta
                                .upserts
                                .iter()
                                .map(|document| { (document.id.clone(), document.content.clone()) })
                                .collect::<Vec<_>>(),
                            expected_upserts
                        );
                        assert_eq!(delta.removals, expected_removals);
                    }
                    Err(error) => {
                        refused += 1;
                        assert!(expected_upserts.len() + expected_removals.len() > 2);
                        assert_eq!(error.reason, IncrementalFallbackReason::DeltaOverThreshold);
                    }
                }
            }
        }
        assert!(accepted > 0 && refused > 0);
    }

    #[test]
    fn empty_and_single_document_deltas_keep_the_minimum_one_change_budget() -> TestResult {
        let empty = BTreeMap::new();
        assert_eq!(
            diff_documents(&empty, &[], document_digest)
                .map_err(|error| error.detail)?
                .len(),
            0
        );
        let single = [doc("mem_one", "one document")];
        let inserted =
            diff_documents(&empty, &single, document_digest).map_err(|error| error.detail)?;
        assert_eq!(inserted.upserts.len(), 1);
        let deleted = diff_documents(&digest_map(&single), &[], document_digest)
            .map_err(|error| error.detail)?;
        assert_eq!(deleted.removals, vec!["mem_one"]);
        Ok(())
    }

    #[test]
    fn duplicate_source_identities_never_become_a_partial_delta() {
        let first = doc("mem_same", "first body");
        let live = digest_map(std::slice::from_ref(&first));
        for second in [first.clone(), doc("mem_same", "another body")] {
            let error = diff_documents(&live, &[first.clone(), second], document_digest)
                .expect_err("ambiguous source snapshot");
            assert_eq!(
                error.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
        }
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
        foreign.embedder = Some("fast=other-model:256:true;quality=".to_owned());
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

    fn jl_stack(seed: u64) -> EmbedderStack {
        EmbedderStack::from_parts(Arc::new(HashEmbedder::jl_384(seed)), None)
    }

    #[test]
    fn equal_model_names_and_dimensions_do_not_authorize_fast_tier_reuse() -> TestResult {
        let first = jl_stack(11);
        let changed = jl_stack(29);
        assert_eq!(first.fast().id(), changed.fast().id());
        assert_eq!(first.fast().dimension(), changed.fast().dimension());
        assert_eq!(first.fast().is_semantic(), changed.fast().is_semantic());
        assert_ne!(
            HashEmbedder::jl_384(11).embed_sync("persistent memory identity"),
            HashEmbedder::jl_384(29).embed_sync("persistent memory identity")
        );
        assert_ne!(embedder_identity(&first), embedder_identity(&changed));

        let documents = [doc("mem_same", "persistent memory identity")];
        let live = generation_with_digests("fast-identity", &first, &documents)?;
        assert_eq!(
            plan(live.path(), &jl_stack(11), &documents)
                .map_err(|error| error.detail)?
                .len(),
            0,
            "independently constructed identical producers remain reusable"
        );
        let refusal = plan(live.path(), &changed, &documents)
            .err()
            .ok_or("same diagnostic id must not hide a changed embedding space")?;
        assert_eq!(
            refusal.reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        Ok(())
    }

    #[test]
    fn quality_identity_and_tier_presence_are_part_of_reuse_compatibility() -> TestResult {
        let stack = |quality_seed: Option<u64>| {
            EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()),
                quality_seed.map(|seed| {
                    Arc::new(HashEmbedder::jl_384(seed)) as Arc<dyn crate::search::Embedder>
                }),
            )
        };
        let documents = [doc("mem_same", "same input to both tiers")];
        let live = generation_with_digests("quality-identity", &stack(Some(11)), &documents)?;
        for changed in [stack(Some(29)), stack(None)] {
            let refusal = plan(live.path(), &changed, &documents)
                .err()
                .ok_or("a changed or removed quality tier must force a rebuild")?;
            assert_eq!(
                refusal.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
        }
        let fast_only = generation_with_digests("no-quality", &stack(None), &documents)?;
        assert!(plan(fast_only.path(), &stack(Some(11)), &documents).is_err());
        Ok(())
    }

    // Deliberately implements the raw legacy interface only: the default
    // identity() returns an error rather than synthesizing an identity from id.
    struct LegacyEmbedder(HashEmbedder);

    impl crate::search::Embedder for LegacyEmbedder {
        fn embed<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> frankensearch::core::traits::SearchFuture<'a, Vec<f32>> {
            self.0.embed(cx, text)
        }

        fn dimension(&self) -> usize {
            self.0.dimension()
        }

        fn id(&self) -> &str {
            self.0.id()
        }

        fn model_name(&self) -> &str {
            self.0.model_name()
        }

        fn is_semantic(&self) -> bool {
            self.0.is_semantic()
        }

        fn category(&self) -> frankensearch::core::traits::ModelCategory {
            self.0.category()
        }
    }

    #[test]
    fn missing_identities_never_match_each_other_or_verified_producers() -> TestResult {
        let legacy =
            EmbedderStack::from_parts(Arc::new(LegacyEmbedder(HashEmbedder::default_256())), None);
        let verified = super::super::hash_fallback_embedder_stack();
        assert_eq!(legacy.fast().id(), verified.fast().id());
        assert_eq!(embedder_identity(&legacy), None);
        let documents = [doc("mem_same", "identity is not an optional proof")];
        for source in [&legacy, &verified] {
            let live = generation_with_digests("missing-identity", source, &documents)?;
            assert!(plan(live.path(), &legacy, &documents).is_err());
            if embedder_identity(source).is_none() {
                assert!(plan(live.path(), &verified, &documents).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn v1_digests_require_a_full_rebuild_before_identity_bound_reuse() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let documents = [doc("mem_same", "unchanged text still needs identity proof")];
        let live = generation_with_digests("legacy-schema", &stack, &documents)?;
        let legacy = serde_json::json!({
            "schema": "ee.index.doc_digests.v1",
            "embedder": format!(
                "fast={}:{}:{};quality=",
                stack.fast().id(),
                stack.fast().dimension(),
                stack.fast().is_semantic(),
            ),
            "documents": digest_map(&documents),
        });
        std::fs::write(live.path().join(DOC_DIGESTS_FILE), legacy.to_string())
            .map_err(|error| error.to_string())?;
        let refusal = plan(live.path(), &stack, &documents)
            .err()
            .ok_or("v1 metadata cannot establish vector-space compatibility")?;
        assert_eq!(
            refusal.reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        DocDigests::new(&stack, &documents)
            .write(live.path())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            plan(live.path(), &stack, &documents)
                .map_err(|error| error.detail)?
                .len(),
            0
        );
        Ok(())
    }

    #[test]
    fn bound_output_checks_space_producer_and_vector_shape() -> TestResult {
        let producer = HashEmbedder::jl_384(11);
        let identity = producer
            .identity()
            .map_err(|error| error.to_string())?
            .clone();
        let expected = identity.fingerprint();
        let good = IdentityBoundEmbedding {
            values: producer.embed_sync("bound output"),
            identity,
        };
        validate_bound_embedding(&good, &expected, "fast").map_err(|error| error.detail)?;

        let other = HashEmbedder::jl_384(29);
        let foreign = IdentityBoundEmbedding {
            values: other.embed_sync("bound output"),
            identity: other.identity().map_err(|error| error.to_string())?.clone(),
        };
        assert!(validate_bound_embedding(&foreign, &expected, "fast").is_err());

        let mut producer_changed = good.clone();
        producer_changed.identity.producer.protocol_revision = "changed-protocol-v2".to_owned();
        producer_changed
            .validate()
            .map_err(|error| error.to_string())?;
        assert!(validate_bound_embedding(&producer_changed, &expected, "quality").is_err());

        let mut malformed = good.clone();
        let _ = malformed.values.pop();
        assert!(validate_bound_embedding(&malformed, &expected, "fast").is_err());
        let mut malformed_identity = good;
        malformed_identity.identity.storage.dimension = 0;
        assert!(validate_bound_embedding(&malformed_identity, &expected, "fast").is_err());
        Ok(())
    }

    #[cfg(not(feature = "lexical-bm25"))]
    #[test]
    fn vector_only_intake_does_not_depend_on_lexical_segments() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let documents = [doc("mem_same", "vector only")];
        let live = generation_with_digests("vector-only", &stack, &documents)?;
        let lexical = live.path().join("lexical");
        std::fs::create_dir(&lexical).map_err(|error| error.to_string())?;
        for segment in 0..=MAX_LEXICAL_SEGMENTS {
            std::fs::write(lexical.join(format!("{segment}.idx")), b"unused")
                .map_err(|error| error.to_string())?;
        }
        assert!(plan(live.path(), &stack, &documents).is_ok());
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
        write("lexical/abc.idx", "segment")?;
        #[cfg(feature = "lexical-bm25")]
        {
            let schema =
                frankensearch::lexical_tantivy::tantivy_crate::schema::Schema::builder().build();
            frankensearch::lexical_tantivy::Index::create_in_dir(
                live.path().join("lexical"),
                schema,
            )
            .map_err(|error| error.to_string())?;
        }
        #[cfg(not(feature = "lexical-bm25"))]
        write("lexical/meta.json", "tantivy meta")?;
        let lexical_metadata = std::fs::read_to_string(live.path().join("lexical/meta.json"))
            .map_err(|error| error.to_string())?;
        write("lexical/.tantivy-writer.lock", "")?;

        copy_generation(live.path(), staging.path()).map_err(|error| error.to_string())?;

        let read = |relative: &str| std::fs::read_to_string(staging.path().join(relative)).ok();
        assert_eq!(read("vector.fast.idx").as_deref(), Some("vectors"));
        assert_eq!(
            read("lexical/meta.json").as_deref(),
            Some(lexical_metadata.as_str())
        );
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

    fn read_finalized_vectors(path: &Path) -> Result<BTreeMap<String, Vec<f32>>, String> {
        let index =
            super::super::open_fast_vector_index_read_only(path).map_err(|error| error.detail)?;
        assert_eq!(index.wal_record_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
        (0..index.record_count())
            .map(|position| {
                Ok((
                    index
                        .doc_id_at(position)
                        .map_err(|error| error.to_string())?
                        .to_owned(),
                    index
                        .vector_at_f32(position)
                        .map_err(|error| error.to_string())?,
                ))
            })
            .collect()
    }

    fn vector_finalization_case(
        updates: &[usize],
        removals: &[usize],
        expected_finalization: VectorFinalization,
    ) -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let updates = updates.to_vec();
        let removals = removals.to_vec();
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
            let live = parent.join("live");
            let staged = parent.join("staged");
            let rebuilt = parent.join("rebuilt");
            let stack = super::super::hash_fallback_embedder_stack();
            let original: Vec<_> = (0..8)
                .map(|number| {
                    doc(
                        &format!("mem_{number}"),
                        &format!("original lesson {number}"),
                    )
                })
                .collect();
            super::super::IndexBuilder::new(&live)
                .with_embedder_stack(stack.clone())
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            let before =
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?;
            assert_eq!(read_finalized_vectors(&live)?.len(), original.len());
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            let delta = Delta {
                upserts: updates
                    .iter()
                    .map(|&number| doc(&original[number].id, &format!("revised lesson {number}")))
                    .collect(),
                removals: removals
                    .iter()
                    .map(|&number| original[number].id.clone())
                    .collect(),
                ..Delta::default()
            };
            let expected: Vec<_> = original
                .iter()
                .filter_map(|document| {
                    delta
                        .upserts
                        .iter()
                        .find(|update| update.id == document.id)
                        .cloned()
                        .or_else(|| {
                            (!delta.removals.contains(&document.id)).then(|| document.clone())
                        })
                })
                .collect();
            let identity = verified_embedder_identity(stack.fast()).ok_or("verified identity")?;
            let mut index =
                super::super::open_fast_vector_index(&staged).map_err(|error| error.detail)?;
            let finalization = apply_vector_delta(
                &cx,
                &mut index,
                stack.fast(),
                &delta,
                &delta.upserts,
                &identity,
                "fast",
            )
            .await
            .map_err(|error| error.detail)?;
            assert_eq!(finalization, expected_finalization);
            drop(index);
            let actual = read_finalized_vectors(&staged)?;
            assert_eq!(actual.len(), expected.len());
            if expected.is_empty() {
                assert!(actual.is_empty());
            } else {
                super::super::IndexBuilder::new(&rebuilt)
                    .with_embedder_stack(stack)
                    .add_documents(expected)
                    .build(&cx)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(actual, read_finalized_vectors(&rebuilt)?);
            }
            assert_eq!(
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?,
                before
            );
            assert_eq!(read_finalized_vectors(&live)?.len(), original.len());
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn mixed_vector_delta_compacts_once_without_a_preceding_vacuum() -> TestResult {
        vector_finalization_case(&[0, 1], &[6, 7], VectorFinalization::Compacted)
    }

    #[test]
    fn update_only_vector_delta_finishes_without_live_tombstones() -> TestResult {
        vector_finalization_case(&[0, 1], &[], VectorFinalization::Compacted)
    }

    #[test]
    fn deletion_only_vector_delta_vacuums_without_leaving_old_records() -> TestResult {
        vector_finalization_case(&[], &[6, 7], VectorFinalization::Vacuumed)
    }

    #[test]
    fn unchanged_vector_delta_needs_no_corpus_rewrite() -> TestResult {
        vector_finalization_case(&[], &[], VectorFinalization::Unchanged)
    }

    #[test]
    fn removing_every_vector_still_publishes_an_empty_physical_tier() -> TestResult {
        vector_finalization_case(&[], &[0, 1, 2, 3, 4, 5, 6, 7], VectorFinalization::Vacuumed)
    }

    #[test]
    fn embedding_inputs_ignore_metadata_but_preserve_exact_text_and_identity() {
        let original = doc("mem_a", "Exact input 資料\n");
        let mut metadata = original.clone().with_title("New title");
        metadata
            .metadata
            .insert("confidence".to_owned(), "0.8".to_owned());
        assert_ne!(document_digest(&original), document_digest(&metadata));
        assert_eq!(
            embedding_input_digest(&original),
            embedding_input_digest(&metadata)
        );
        for changed in [
            doc("mem_b", &original.content),
            doc("mem_a", "Exact input 資料"),
            doc("mem_a", "exact input 資料\n"),
            doc("mem_a", "Exact input 資料\r\n"),
        ] {
            assert_ne!(
                embedding_input_digest(&original),
                embedding_input_digest(&changed)
            );
        }
        assert_ne!(
            embedding_input_digest(&doc("ab", "c")),
            embedding_input_digest(&doc("a", "bc"))
        );
    }

    #[test]
    fn metadata_upserts_stay_in_the_lexical_delta_without_embedding_again() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let original: Vec<_> = (0..20)
            .map(|number| doc(&format!("mem_{number:02}"), &format!("lesson {number}")))
            .collect();
        let live = generation_with_digests("split-inputs", &stack, &original)?;
        let mut current = original.clone();
        current[0].title = Some("Updated title".to_owned());
        current[1]
            .metadata
            .insert("tags".to_owned(), "new-tag".to_owned());
        current[2].content.push_str(" repaired");
        current.pop();
        current.push(doc("mem_new", &original[0].content));
        let delta = plan(live.path(), &stack, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 4);
        assert_eq!(delta.removals, ["mem_19"]);
        let identity = embedder_identity(&stack).ok_or("identity")?;
        let vectors = delta
            .vector_upserts(&identity)
            .map_err(|error| error.detail)?;
        assert_eq!(
            vectors
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["mem_02", "mem_new"]
        );
        assert_eq!(delta.upserts[0].title, current[0].title);
        assert_eq!(delta.upserts[1].metadata, current[1].metadata);
        Ok(())
    }

    #[test]
    fn v2_generations_reembed_conservatively_then_upgrade_to_input_reuse() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let original = [doc("mem_one", "unchanged embedding input")];
        let live = generation_with_digests("v2-upgrade", &stack, &original)?;
        let mut old = DocDigests::new(&stack, &original);
        old.schema = DOC_DIGESTS_SCHEMA_V2.to_owned();
        old.embedding_inputs = None;
        old.write(live.path()).map_err(|error| error.to_string())?;
        let changed = [original[0].clone().with_title("Metadata changed")];
        let identity = embedder_identity(&stack).ok_or("identity")?;
        let legacy = plan(live.path(), &stack, &changed).map_err(|error| error.detail)?;
        assert_eq!(
            legacy
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .len(),
            1
        );
        DocDigests::new(&stack, &changed)
            .write(live.path())
            .map_err(|error| error.to_string())?;
        let next = [changed[0].clone().with_title("Metadata changed again")];
        let upgraded = plan(live.path(), &stack, &next).map_err(|error| error.detail)?;
        assert_eq!(upgraded.upserts.len(), 1);
        assert!(
            upgraded
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn v2_delta_rechecks_planned_producer_before_native_tier_mutation() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
            let _root = root;
            let live = parent.join("live");
            let staged = parent.join("staged");
            let stack = jl_stack(11);
            let original: Vec<_> = (0..8)
                .map(|number| doc(&format!("mem_{number:02}"), &format!("lesson {number}")))
                .collect();
            let builder = super::super::IndexBuilder::new(&live).with_embedder_stack(stack.clone());
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            let mut old = DocDigests::new(&stack, &original);
            old.schema = DOC_DIGESTS_SCHEMA_V2.to_owned();
            old.embedding_inputs = None;
            old.write(&live).map_err(|error| error.to_string())?;
            let mut current = original[..7].to_vec();
            current[0].title = Some("Changed metadata still requires v2 inference".to_owned());
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            let identity = embedder_identity(&stack).ok_or("planned producer identity")?;
            assert_eq!(delta.removals, ["mem_07"]);
            assert_eq!(
                delta
                    .vector_upserts(&identity)
                    .map_err(|error| error.detail)?
                    .iter()
                    .map(|document| document.id.as_str())
                    .collect::<Vec<_>>(),
                ["mem_00"],
                "the producer fence must not grant v2 metadata-only vector reuse",
            );
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            let before =
                std::fs::read(staged.join("vector.fast.idx")).map_err(|error| error.to_string())?;
            let refusal = apply(&cx, &staged, &jl_stack(29), &delta)
                .await
                .expect_err("producer changes after planning must refuse even a v2 delta");
            assert_eq!(
                refusal.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch,
            );
            assert!(
                refusal
                    .detail
                    .contains("producer changed after delta planning")
            );
            assert_eq!(
                std::fs::read(staged.join("vector.fast.idx")).map_err(|error| error.to_string())?,
                before,
            );
            assert_eq!(
                read_finalized_vectors(&staged)?,
                read_finalized_vectors(&live)?
            );
            super::super::verify_published_tier_counts(&staged, 8, false)?;
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn incomplete_or_malformed_v3_input_maps_never_authorize_reuse() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let original = [doc("mem_one", "source text")];
        let live = generation_with_digests("bad-inputs", &stack, &original)?;
        for case in 0..6 {
            let mut broken = DocDigests::new(&stack, &original);
            match case {
                0 => broken.embedding_inputs = None,
                1 => {
                    broken.embedding_inputs.as_mut().unwrap().clear();
                }
                2 => {
                    broken
                        .embedding_inputs
                        .as_mut()
                        .unwrap()
                        .insert("extra".to_owned(), "0".repeat(64));
                }
                3 => {
                    broken
                        .embedding_inputs
                        .as_mut()
                        .unwrap()
                        .insert("mem_one".to_owned(), "not-a-digest".to_owned());
                }
                4 => broken.schema = DOC_DIGESTS_SCHEMA_V2.to_owned(),
                _ => {
                    // Structurally valid maps can still disagree about the
                    // embedding input of an otherwise unchanged document.
                    broken.embedding_inputs.as_mut().unwrap().insert(
                        "mem_one".to_owned(),
                        embedding_input_digest(&doc("mem_one", "different source text")),
                    );
                }
            }
            broken
                .write(live.path())
                .map_err(|error| error.to_string())?;
            assert_eq!(
                plan(live.path(), &stack, &original)
                    .expect_err("invalid input proof")
                    .reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
        }
        Ok(())
    }

    #[test]
    fn input_reuse_is_rechecked_after_plan_mutation_or_producer_drift() -> TestResult {
        let stack = jl_stack(11);
        let original = [doc("mem_one", "stable input")];
        let changed = [original[0].clone().with_title("new metadata")];
        let live = generation_with_digests("recheck-inputs", &stack, &original)?;
        let identity = embedder_identity(&stack).ok_or("identity")?;
        for case in 0..3 {
            let mut delta = plan(live.path(), &stack, &changed).map_err(|error| error.detail)?;
            assert!(
                delta
                    .vector_upserts(&identity)
                    .map_err(|error| error.detail)?
                    .is_empty()
            );
            match case {
                0 => delta.upserts[0].content.push_str(" actually changed"),
                1 => delta.upserts[0].id = "mem_new".to_owned(),
                _ => delta.removals.push(original[0].id.clone()),
            }
            assert_eq!(
                delta
                    .vector_upserts(&identity)
                    .map_err(|error| error.detail)?
                    .len(),
                1
            );
        }
        let delta = plan(live.path(), &stack, &changed).map_err(|error| error.detail)?;
        let other = embedder_identity(&jl_stack(29)).ok_or("other identity")?;
        assert_eq!(
            delta
                .vector_upserts(&other)
                .expect_err("changed producer")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch
        );
        Ok(())
    }

    struct CountingHash {
        inner: HashEmbedder,
        inputs: AtomicUsize,
    }

    impl crate::search::Embedder for CountingHash {
        fn embed<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            text: &'a str,
        ) -> SearchFuture<'a, Vec<f32>> {
            self.inputs.fetch_add(1, Ordering::SeqCst);
            self.inner.embed(cx, text)
        }

        fn embed_batch<'a>(
            &'a self,
            cx: &'a asupersync::Cx,
            texts: &'a [&'a str],
        ) -> SearchFuture<'a, Vec<Vec<f32>>> {
            self.inputs.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed_batch(cx, texts)
        }

        fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
            self.inner.identity()
        }

        fn dimension(&self) -> usize {
            self.inner.dimension()
        }

        fn id(&self) -> &str {
            self.inner.id()
        }

        fn model_name(&self) -> &str {
            self.inner.model_name()
        }

        fn is_semantic(&self) -> bool {
            self.inner.is_semantic()
        }

        fn category(&self) -> ModelCategory {
            self.inner.category()
        }
    }

    #[cfg(feature = "lexical-bm25")]
    async fn assert_reuse_lexical_projection(
        cx: &asupersync::Cx,
        directory: &Path,
        updated: bool,
    ) -> TestResult {
        use crate::search::LexicalRead;

        let lexical = crate::search::open_lexical_index_read_only(&directory.join("lexical"))
            .map_err(|error| error.to_string())?;
        assert_eq!(lexical.doc_count().map_err(|error| error.to_string())?, 20);
        let (present, absent) = if updated {
            ("updatedquartzmarker", "priorquartzmarker")
        } else {
            ("priorquartzmarker", "updatedquartzmarker")
        };
        let hits = lexical
            .search(cx, present, 20)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits.iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["mem_00", "mem_01", "mem_02"])
        );
        if updated {
            for hit in &hits {
                assert_eq!(
                    hit.metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("revision"))
                        .and_then(serde_json::Value::as_str),
                    Some("2")
                );
            }
        }
        assert!(
            lexical
                .search(cx, absent, 20)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    fn native_input_reuse_case(change_body: bool) -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
            let live = parent.join("live");
            let staged = parent.join("staged");
            let rebuilt = parent.join("rebuilt");
            let observed = Arc::new(CountingHash {
                inner: HashEmbedder::default_256(),
                inputs: AtomicUsize::new(0),
            });
            let stack = EmbedderStack::from_parts(observed.clone(), None);
            let original: Vec<_> = (0..20)
                .map(|number| {
                    let document = doc(
                        &format!("mem_{number:02}"),
                        &format!("original lesson {number}"),
                    );
                    if number < 3 {
                        document.with_title("priorquartzmarker")
                    } else {
                        document
                    }
                })
                .collect();
            let builder = super::super::IndexBuilder::new(&live).with_embedder_stack(stack.clone());
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            DocDigests::new(&stack, &original)
                .write(&live)
                .map_err(|error| error.to_string())?;
            assert_eq!(read_finalized_vectors(&live)?.len(), 20);
            assert!(observed.inputs.load(Ordering::SeqCst) >= 20);
            #[cfg(feature = "lexical-bm25")]
            assert_reuse_lexical_projection(&cx, &live, false).await?;
            let before =
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?;
            let mut current = original.clone();
            for document in &mut current[..3] {
                document.title = Some("updatedquartzmarker".to_owned());
                document
                    .metadata
                    .insert("revision".to_owned(), "2".to_owned());
            }
            if change_body {
                current[7].content.push_str(" changed");
            }
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            assert_eq!(delta.upserts.len(), 3 + usize::from(change_body));
            let identity = embedder_identity(&stack).ok_or("complete identity")?;
            let vectors = delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?;
            assert_eq!(vectors.len(), usize::from(change_body));
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            observed.inputs.store(0, Ordering::SeqCst);
            apply(&cx, &staged, &stack, &delta)
                .await
                .map_err(|error| error.detail)?;
            assert_eq!(
                observed.inputs.load(Ordering::SeqCst),
                usize::from(change_body)
            );
            if !change_body {
                assert_eq!(
                    std::fs::read(staged.join("vector.fast.idx"))
                        .map_err(|error| error.to_string())?,
                    before
                );
            }
            #[cfg(feature = "lexical-bm25")]
            {
                assert_reuse_lexical_projection(&cx, &staged, true).await?;
                assert_reuse_lexical_projection(&cx, &live, false).await?;
            }
            let builder =
                super::super::IndexBuilder::new(&rebuilt).with_embedder_stack(stack.clone());
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(current.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                read_finalized_vectors(&staged)?,
                read_finalized_vectors(&rebuilt)?
            );
            #[cfg(feature = "lexical-bm25")]
            assert_reuse_lexical_projection(&cx, &rebuilt, true).await?;
            assert_eq!(
                std::fs::read(live.join("vector.fast.idx")).map_err(|error| error.to_string())?,
                before
            );
            DocDigests::new(&stack, &current)
                .write(&staged)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                plan(&staged, &stack, &current)
                    .map_err(|error| error.detail)?
                    .len(),
                0
            );
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn metadata_only_intake_calls_no_embedder_and_preserves_vector_bytes() -> TestResult {
        native_input_reuse_case(false)
    }

    #[test]
    fn mixed_metadata_and_body_intake_embeds_only_the_changed_body() -> TestResult {
        native_input_reuse_case(true)
    }

    #[test]
    fn bulk_reuse_selection_matches_the_linear_reference_after_plan_mutations() -> TestResult {
        let identity = "selection-test-identity";
        let original = (0..4096)
            .map(|number| doc(&format!("mem_{number:04}"), "unchanged input"))
            .collect::<Vec<_>>();
        let mut delta = Delta {
            upserts: original[..2048].to_vec(),
            removals: original[2048..]
                .iter()
                .map(|document| document.id.clone())
                .collect(),
            reuse: Some(EmbeddingReuse {
                identity: identity.to_owned(),
                inputs: original
                    .iter()
                    .map(|document| (document.id.clone(), embedding_input_digest(document)))
                    .collect(),
            }),
        };
        assert!(
            delta
                .vector_upserts(identity)
                .map_err(|error| error.detail)?
                .is_empty()
        );
        delta.upserts[0].content.push_str(" changed after planning");
        delta.upserts[1].id = "new-identity".to_owned();
        delta.upserts[2].title = Some("metadata only".to_owned());
        delta
            .removals
            .extend([original[3].id.clone(), original[3].id.clone()]);
        delta.upserts.reverse();
        let reuse = delta.reuse.as_ref().ok_or("reuse commitments")?;
        let expected = delta
            .upserts
            .iter()
            .filter(|document| {
                delta.removals.contains(&document.id)
                    || reuse.inputs.get(&document.id) != Some(&embedding_input_digest(document))
            })
            .map(document_digest)
            .collect::<Vec<_>>();
        let selected = delta
            .vector_upserts(identity)
            .map_err(|error| error.detail)?;
        assert_eq!(selected.len(), 3);
        assert_eq!(
            selected.iter().map(document_digest).collect::<Vec<_>>(),
            expected
        );
        assert!(delta.vector_upserts("changed-producer").is_err());
        Ok(())
    }

    #[test]
    fn streamed_digest_files_preserve_wire_bytes_for_empty_small_and_large_maps() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        for count in [0, 1, 4096] {
            let documents = (0..count)
                .map(|number| doc(&format!("mem_{number:04}_資料"), "exact source text\n"))
                .collect::<Vec<_>>();
            let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
            let digests = DocDigests::new(&stack, &documents);
            digests
                .write(directory.path())
                .map_err(|error| error.to_string())?;
            let actual = std::fs::read(directory.path().join(DOC_DIGESTS_FILE))
                .map_err(|error| error.to_string())?;
            assert_eq!(
                actual,
                serde_json::to_vec(&digests).map_err(|error| error.to_string())?
            );
            let decoded = read_digests(directory.path()).map_err(|error| error.detail)?;
            assert_eq!(decoded.documents, digests.documents);
            assert_eq!(decoded.embedding_inputs, digests.embedding_inputs);
            assert_eq!(decoded.embedder, digests.embedder);
            assert_eq!(decoded.schema, digests.schema);
        }
        Ok(())
    }

    #[test]
    fn digest_serialization_propagates_buffer_drain_and_underlying_flush_failures() {
        struct FailingWriter {
            fail_write: bool,
        }
        impl std::io::Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.fail_write {
                    Err(std::io::Error::other("injected digest write failure"))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("injected digest flush failure"))
            }
        }
        let stack = super::super::hash_fallback_embedder_stack();
        for count in [0, 4096] {
            let documents = (0..count)
                .map(|number| doc(&format!("mem_{number:04}"), "source"))
                .collect::<Vec<_>>();
            let digests = DocDigests::new(&stack, &documents);
            for fail_write in [false, true] {
                let error = digests
                    .write_to(FailingWriter { fail_write })
                    .expect_err("every I/O failure must reject staging metadata");
                assert!(error.to_string().contains(if fail_write {
                    "injected digest write failure"
                } else {
                    "injected digest flush failure"
                }));
            }
        }
    }

    struct ObservedDigestReads<R> {
        reader: R,
        chunk_bytes: usize,
        calls: usize,
        requested_bytes: usize,
        consumed_bytes: usize,
    }

    impl<R: std::io::Read> std::io::Read for ObservedDigestReads<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.requested_bytes = self.requested_bytes.max(buffer.len());
            let available = buffer.len().min(self.chunk_bytes);
            let count = self.reader.read(&mut buffer[..available])?;
            self.consumed_bytes += count;
            Ok(count)
        }
    }

    fn observed_digest_reads<R>(reader: R, chunk_bytes: usize) -> ObservedDigestReads<R> {
        ObservedDigestReads {
            reader,
            chunk_bytes,
            calls: 0,
            requested_bytes: 0,
            consumed_bytes: 0,
        }
    }

    #[test]
    fn streamed_digest_reads_preserve_v2_and_v3_across_short_utf8_reads() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let documents = [
            doc("mem_資料", "first source"),
            doc("mem_é", "second source"),
        ];
        for schema in [DOC_DIGESTS_SCHEMA_V2, DOC_DIGESTS_SCHEMA_V3] {
            let mut expected = DocDigests::new(&stack, &documents);
            expected.schema = schema.to_owned();
            if schema == DOC_DIGESTS_SCHEMA_V2 {
                expected.embedding_inputs = None;
            }
            let mut bytes = serde_json::to_vec(&expected).map_err(|error| error.to_string())?;
            bytes.extend_from_slice(b" \n\t");
            for chunk_bytes in [1, 7, DIGEST_READ_BUFFER_BYTES] {
                let mut reader = observed_digest_reads(bytes.as_slice(), chunk_bytes);
                let actual = read_digests_with_checkpoint(&mut reader, || Ok(()))
                    .map_err(|error| error.detail)?;
                assert_eq!(actual.schema, expected.schema);
                assert_eq!(actual.embedder, expected.embedder);
                assert_eq!(actual.documents, expected.documents);
                assert_eq!(actual.embedding_inputs, expected.embedding_inputs);
                assert_eq!(reader.consumed_bytes, bytes.len());
                assert!(reader.requested_bytes <= DIGEST_READ_BUFFER_BYTES);
            }
        }
        Ok(())
    }

    #[test]
    fn large_digest_manifest_uses_fixed_input_buffers_and_keeps_every_identity() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let documents = (0..8192)
            .map(|number| doc(&format!("mem_{number:05}_資料"), "exact input"))
            .collect::<Vec<_>>();
        let expected = DocDigests::new(&stack, &documents);
        let directory = generation_with_digests("bounded-read", &stack, &documents)?;
        let file = std::fs::File::open(directory.path().join(DOC_DIGESTS_FILE))
            .map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        assert!(length > 64 * DIGEST_READ_BUFFER_BYTES as u64);
        let mut reader = observed_digest_reads(file, usize::MAX);
        let actual =
            read_digests_with_checkpoint(&mut reader, || Ok(())).map_err(|error| error.detail)?;
        assert_eq!(actual.documents, expected.documents);
        assert_eq!(actual.embedding_inputs, expected.embedding_inputs);
        assert_eq!(reader.consumed_bytes as u64, length);
        assert!(reader.calls > 64);
        assert_eq!(reader.requested_bytes, DIGEST_READ_BUFFER_BYTES);
        Ok(())
    }

    struct FailedDigestRead<'a> {
        bytes: &'a [u8],
        remaining: usize,
        kind: std::io::ErrorKind,
        calls: usize,
    }

    impl std::io::Read for FailedDigestRead<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.remaining == 0 {
                return Err(std::io::Error::new(
                    self.kind,
                    "injected digest read failure",
                ));
            }
            let count = buffer.len().min(self.remaining).min(self.bytes.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn streamed_digest_reads_refuse_truncation_trailing_data_and_io_failures() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let digests = DocDigests::new(&stack, &[doc("mem_one", "source")]);
        let bytes = serde_json::to_vec(&digests).map_err(|error| error.to_string())?;
        for length in [0, 1, bytes.len() / 2, bytes.len() - 1] {
            let error = read_digests_with_checkpoint(&bytes[..length], || Ok(()))
                .expect_err("an incomplete prefix must not authorize reuse");
            assert_eq!(
                error.reason,
                IncrementalFallbackReason::CorpusRevisionMismatch
            );
        }
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(b" {}");
        assert_eq!(
            read_digests_with_checkpoint(trailing.as_slice(), || Ok(()))
                .expect_err("extra JSON after a complete manifest")
                .reason,
            IncrementalFallbackReason::CorpusRevisionMismatch,
        );
        // Failure while confirming EOF is still a read failure: accepting the
        // first object without checking the suffix would miss trailing data.
        for remaining in [0, bytes.len() / 2, bytes.len()] {
            let reader = FailedDigestRead {
                bytes: &bytes,
                remaining,
                kind: std::io::ErrorKind::Other,
                calls: 0,
            };
            let error = read_digests_with_checkpoint(reader, || Ok(()))
                .expect_err("underlying read failure cannot yield a usable manifest");
            assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
            assert!(error.detail.contains("injected digest read failure"));
        }
        Ok(())
    }

    #[test]
    fn duplicate_digest_ids_are_rejected_before_json_can_collapse_them() {
        let hash = "a".repeat(64);
        let unique = format!(r#"{{"mem_one":"{hash}"}}"#);
        // The escaped spelling decodes to the same key. Even equal duplicate
        // values must be refused; a last-wins map would otherwise hide both.
        let duplicate = format!(r#"{{"mem_one":"{hash}","mem_\u006fne":"{hash}"}}"#);
        for schema in [DOC_DIGESTS_SCHEMA_V2, DOC_DIGESTS_SCHEMA_V3] {
            for duplicate_inputs in [false, true] {
                if schema == DOC_DIGESTS_SCHEMA_V2 && duplicate_inputs {
                    continue;
                }
                let documents = if duplicate_inputs {
                    &unique
                } else {
                    &duplicate
                };
                let inputs = if duplicate_inputs {
                    &duplicate
                } else {
                    &unique
                };
                let suffix = if schema == DOC_DIGESTS_SCHEMA_V3 {
                    format!(r#", "embeddingInputs":{inputs}"#)
                } else {
                    String::new()
                };
                let bytes = format!(
                    r#"{{"schema":"{schema}","embedder":null,"documents":{documents}{suffix}}}"#
                );
                let error = read_digests_with_checkpoint(bytes.as_bytes(), || Ok(()))
                    .expect_err("duplicate identities must not authorize vector reuse");
                assert_eq!(
                    error.reason,
                    IncrementalFallbackReason::CorpusRevisionMismatch
                );
                assert!(error.detail.contains("duplicate document identity"));
            }
        }
    }

    #[test]
    fn malformed_document_digests_and_mismatched_input_ids_require_rebuild() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        for schema in [DOC_DIGESTS_SCHEMA_V2, DOC_DIGESTS_SCHEMA_V3] {
            for case in 0..4 {
                let mut broken = DocDigests::new(&stack, &[doc("mem_one", "source")]);
                broken.schema = schema.to_owned();
                match case {
                    0 => {
                        broken
                            .documents
                            .insert("mem_one".to_owned(), "short".to_owned());
                    }
                    1 => {
                        broken
                            .documents
                            .insert("mem_one".to_owned(), "A".repeat(64));
                    }
                    2 => {
                        let digest = broken
                            .documents
                            .remove("mem_one")
                            .ok_or("document digest")?;
                        broken.documents.insert(String::new(), digest);
                    }
                    _ => {
                        let digest = broken
                            .documents
                            .remove("mem_one")
                            .ok_or("document digest")?;
                        broken.documents.insert("different_id".to_owned(), digest);
                    }
                }
                if schema == DOC_DIGESTS_SCHEMA_V2 {
                    if case == 3 {
                        continue;
                    }
                    broken.embedding_inputs = None;
                }
                let bytes = serde_json::to_vec(&broken).map_err(|error| error.to_string())?;
                assert_eq!(
                    read_digests_with_checkpoint(bytes.as_slice(), || Ok(()))
                        .expect_err("malformed proof cannot authorize incremental reuse")
                        .reason,
                    IncrementalFallbackReason::CorpusRevisionMismatch,
                );
            }
        }
        Ok(())
    }

    #[test]
    fn digest_read_and_validation_checkpoints_never_return_a_partial_manifest() -> TestResult {
        let stack = super::super::hash_fallback_embedder_stack();
        let documents = (0..4)
            .map(|number| doc(&format!("mem_{number}"), "source"))
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&DocDigests::new(&stack, &documents))
            .map_err(|error| error.to_string())?;
        let mut total = 0;
        read_digests_with_checkpoint(observed_digest_reads(bytes.as_slice(), 97), || {
            total += 1;
            Ok(())
        })
        .map_err(|error| error.detail)?;
        for stop_at in 1..=total {
            let mut calls = 0;
            let mut reader = observed_digest_reads(bytes.as_slice(), 97);
            let error = read_digests_with_checkpoint(&mut reader, || {
                calls += 1;
                if calls == stop_at {
                    Err(incremental_fallback(
                        IncrementalFallbackReason::TierUnavailable,
                        "stop digest loading at this checkpoint",
                    ))
                } else {
                    Ok(())
                }
            })
            .expect_err("cancelled read or schema scan must not return partial maps");
            assert_eq!(calls, stop_at);
            assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
            assert_eq!(error.detail, "stop digest loading at this checkpoint");
            if stop_at <= 2 {
                assert_eq!(reader.calls, usize::from(stop_at == 2));
            }
        }
        Ok(())
    }

    #[test]
    fn digest_read_cancellation_wins_over_io_failure_and_is_never_retried() {
        for kind in [std::io::ErrorKind::Other, std::io::ErrorKind::Interrupted] {
            let mut reader = FailedDigestRead {
                bytes: &[],
                remaining: 0,
                kind,
                calls: 0,
            };
            let mut checks = 0;
            let error = read_digests_with_checkpoint(&mut reader, || {
                checks += 1;
                if checks == 2 {
                    Err(incremental_fallback(
                        IncrementalFallbackReason::TierUnavailable,
                        "caller cancelled during the failed read",
                    ))
                } else {
                    Ok(())
                }
            })
            .expect_err("cancellation must precede both retry and ordinary I/O fallback");
            assert_eq!(reader.calls, 1);
            assert_eq!(checks, 2);
            assert_eq!(error.detail, "caller cancelled during the failed read");
        }
    }

    #[test]
    fn installed_digest_read_cancellation_preserves_the_terminal_index_reason() -> TestResult {
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
            struct CancellingRead<'a> {
                cx: &'a asupersync::Cx,
                reason: asupersync::CancelReason,
                calls: usize,
            }
            impl std::io::Read for CancellingRead<'_> {
                fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                    self.calls += 1;
                    self.cx.set_cancel_reason(self.reason.clone());
                    buffer[0] = b'{';
                    Ok(1)
                }
            }
            let request = asupersync::Cx::current().ok_or("installed request")?;
            let reason = asupersync::CancelReason::user("stop large digest intake");
            let mut reader = CancellingRead {
                cx: &cx,
                reason: reason.clone(),
                calls: 0,
            };
            let error = read_digests_with_checkpoint(&mut reader, || {
                batch::checkpoint(&request, "document digest loading")
            })
            .expect_err("cancellation during the first real request read");
            assert_eq!(reader.calls, 1);
            assert_eq!(error.reason, IncrementalFallbackReason::TierUnavailable);
            // Both publication paths check this same request before preparing
            // a full rebuild. The terminal result retains the exact reason.
            let terminal = super::super::index_operation_checkpoint(&cx, None)
                .expect_err("a rebuild must not renew the cancelled request");
            let IndexRebuildError::Cancelled(actual) = terminal else {
                return Err(format!("expected cancellation, got {terminal}"));
            };
            assert_eq!(actual, reason);
            let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
            assert_eq!(
                read_digests(directory.path())
                    .expect_err("cancellation precedes missing-file fallback")
                    .reason,
                IncrementalFallbackReason::TierUnavailable,
            );
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }
}
