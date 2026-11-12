//! Rebuild selected producers without re-embedding compatible native tiers.
//!
//! This is a complete-source fallback after ordinary delta planning refuses.
//! Every document identity and exact embedding input must still match a valid
//! V3 manifest. Native readers prove the old producers, membership and empty
//! WALs before any component earns reuse. Source capture and proof remain
//! corpus-sized; only changed producers and lexical metadata are rebuilt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use frankensearch::VectorIndex;
use frankensearch::index::{Quantization, wal_path_for};

use crate::search::{Embedder, EmbedderStack, IndexableDocument};

use super::super::{
    DeltaStageError, IndexDocumentCounts, IndexPublishLockOwner, IndexRebuildError,
    UnpublishedIndexStage, VECTOR_INDEX_FALLBACK_FILE, VECTOR_INDEX_FAST_FILE,
    VECTOR_INDEX_QUALITY_FILE, index_checkpoint, storage,
};
use super::{
    AdmittedVectors, DOC_DIGESTS_SCHEMA_V3, DocDigests, admit_published_vector, batch,
    copy_generation_files, document_digest, embedder_identity, embedding_input_digest,
    read_digests, verified_embedder_identity,
};

struct TierRefreshPlan {
    identity: String,
    previous_fast: String,
    previous_quality: Option<String>,
    retain_fast: bool,
    retain_quality: bool,
    rebuild_lexical: bool,
    digests: DocDigests,
}

fn refused(detail: &str) -> IndexRebuildError {
    IndexRebuildError::Index(format!("selective_tier_refresh_unavailable: {detail}"))
}

/// V3 stores the complete pair in this exact canonical form. Splitting it
/// never turns a model label, missing identity or malformed fingerprint into
/// evidence that two producers are compatible.
fn producer_pair(identity: &str) -> Option<(&str, Option<&str>)> {
    let (fast, quality) = identity.strip_prefix("fast=")?.split_once(";quality=")?;
    let fingerprint = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    };
    if !fingerprint(fast) || (!quality.is_empty() && !fingerprint(quality)) {
        return None;
    }
    Some((fast, (!quality.is_empty()).then_some(quality)))
}

impl TierRefreshPlan {
    fn read(
        cx: &asupersync::Cx,
        live_dir: &Path,
        stack: &EmbedderStack,
        documents: &[IndexableDocument],
    ) -> Result<Self, IndexRebuildError> {
        index_checkpoint(cx)?;
        if documents.is_empty() {
            return Err(refused("an empty generation needs no tier reuse"));
        }
        let identity = embedder_identity(stack)
            .ok_or_else(|| refused("selected producers have no complete identities"))?;
        let live = read_digests(live_dir).map_err(|_| refused("V3 source proof is unavailable"))?;
        index_checkpoint(cx)?;
        if live.schema != DOC_DIGESTS_SCHEMA_V3 {
            return Err(refused("only complete V3 source proof supports tier reuse"));
        }
        let previous = live
            .embedder
            .as_deref()
            .and_then(producer_pair)
            .ok_or_else(|| refused("previous producers have no complete identities"))?;
        let selected = producer_pair(&identity)
            .ok_or_else(|| refused("selected producer identities are malformed"))?;
        let retain_fast = previous.0 == selected.0;
        let retain_quality = previous.1.is_some() && previous.1 == selected.1;
        if !retain_fast && !retain_quality {
            return Err(refused("no selected vector tier has a compatible producer"));
        }
        let inputs = live
            .embedding_inputs
            .as_ref()
            .ok_or_else(|| refused("previous exact embedding-input proof is missing"))?;
        if inputs.len() != documents.len() {
            return Err(refused("source membership changed"));
        }
        let mut current_documents = BTreeMap::new();
        let mut current_inputs = BTreeMap::new();
        for document in documents {
            index_checkpoint(cx)?;
            if document.id.is_empty() || current_documents.contains_key(&document.id) {
                return Err(refused("source identities are empty or duplicated"));
            }
            let input = embedding_input_digest(document);
            index_checkpoint(cx)?;
            if inputs.get(&document.id) != Some(&input) {
                return Err(refused(
                    "source membership or exact embedding input changed",
                ));
            }
            current_inputs.insert(document.id.clone(), input);
            current_documents.insert(document.id.clone(), document_digest(document));
        }
        let rebuild_lexical = current_documents != live.documents;
        if live.embedder.as_deref() == Some(identity.as_str()) && !rebuild_lexical {
            return Err(refused("no producer or source document changed"));
        }
        index_checkpoint(cx)?;
        Ok(Self {
            previous_fast: previous.0.to_owned(),
            previous_quality: previous.1.map(str::to_owned),
            retain_fast,
            retain_quality,
            rebuild_lexical,
            digests: DocDigests {
                schema: DOC_DIGESTS_SCHEMA_V3.to_owned(),
                embedder: Some(identity.clone()),
                documents: current_documents,
                embedding_inputs: Some(current_inputs),
            },
            identity,
        })
    }

    fn verify_stack(&self, stack: &EmbedderStack) -> Result<(), IndexRebuildError> {
        if embedder_identity(stack).as_deref() != Some(self.identity.as_str()) {
            return Err(refused(
                "selected producer changed after tier-refresh planning",
            ));
        }
        Ok(())
    }

    fn admit_sources(
        &self,
        cx: &asupersync::Cx,
        live_dir: &Path,
        stack: &EmbedderStack,
    ) -> Result<(AdmittedVectors, BTreeSet<PathBuf>, u16), IndexRebuildError> {
        self.verify_stack(stack)?;
        let fast_path = live_dir.join(VECTOR_INDEX_FAST_FILE);
        let fallback_path = live_dir.join(VECTOR_INDEX_FALLBACK_FILE);
        super::ensure_index_path_has_no_symlinks(&fast_path, "admit selective fast tier")?;
        super::ensure_index_path_has_no_symlinks(&fallback_path, "admit selective fast tier")?;
        let fast_present = super::super::path_is_regular_file_no_follow(&fast_path);
        let fallback_present = super::super::path_is_regular_file_no_follow(&fallback_path);
        if fast_present == fallback_present {
            return Err(refused(
                "previous fast-tier pathname is missing or ambiguous",
            ));
        }
        let fast_name = if fast_present {
            VECTOR_INDEX_FAST_FILE
        } else {
            VECTOR_INDEX_FALLBACK_FILE
        };
        let (fast, quality) = super::super::stack_descriptors(stack);
        let mut files = BTreeMap::new();
        let mut retained_roots = BTreeSet::new();
        let mut nonce = None;
        for (name, producer, retained, selected) in [
            Some((
                fast_name,
                self.previous_fast.as_str(),
                self.retain_fast,
                Some(&fast),
            )),
            self.previous_quality.as_deref().map(|producer| {
                (
                    VECTOR_INDEX_QUALITY_FILE,
                    producer,
                    self.retain_quality,
                    quality.as_ref(),
                )
            }),
        ]
        .into_iter()
        .flatten()
        {
            index_checkpoint(cx)?;
            let (metadata, reader) = admit_published_vector(&live_dir.join(name))?;
            index_checkpoint(cx)?;
            if reader.embedder_revision() != producer {
                return Err(refused("native producer differs from the source proof"));
            }
            verify_native_membership(cx, &reader, &self.digests.documents)?;
            if nonce.is_some_and(|nonce| nonce != reader.publication_nonce()) {
                return Err(refused(
                    "native fast and quality publication identities differ",
                ));
            }
            nonce = Some(reader.publication_nonce());
            if retained {
                let selected =
                    selected.ok_or_else(|| refused("retained tier has no selected producer"))?;
                super::super::verify_vector_producer(&reader, selected, name)
                    .map_err(IndexRebuildError::Index)?;
                retained_roots.insert(PathBuf::from(name));
                retained_roots.insert(wal_path_for(Path::new(name)));
            }
            // Keep every native reader alive through staging. Identity-checked
            // source copies hold the same map lock that admitted the file.
            files.insert(PathBuf::from(name), (metadata, reader));
        }
        #[cfg(feature = "lexical-bm25")]
        if !self.rebuild_lexical {
            retained_roots.insert(PathBuf::from(super::super::LEXICAL_INDEX_SUBDIR));
        }
        let expected = u32::try_from(self.digests.documents.len())
            .map_err(|_| refused("source count exceeds generation metadata capacity"))?;
        // Also reject unexpected/missing quality and incomplete lexical tiers.
        // A malformed old component must take the existing complete rebuild.
        super::super::verify_published_tier_counts(
            live_dir,
            expected,
            self.previous_quality.is_some(),
        )
        .map_err(IndexRebuildError::Index)?;
        verify_native_pair(live_dir, self.previous_quality.is_some())?;
        self.verify_stack(stack)?;
        index_checkpoint(cx)?;
        Ok((
            AdmittedVectors {
                share: cfg!(unix),
                files,
            },
            retained_roots,
            nonce.ok_or_else(|| refused("no native tier was admitted"))?,
        ))
    }
}

fn verify_native_membership(
    cx: &asupersync::Cx,
    reader: &VectorIndex,
    documents: &BTreeMap<String, String>,
) -> Result<(), IndexRebuildError> {
    if reader.record_count() != documents.len()
        || reader.wal_record_count() != 0
        || reader.tombstone_count() != 0
    {
        return Err(refused(
            "native tier does not contain exactly the proved source set",
        ));
    }
    let mut seen = BTreeSet::new();
    for row in 0..reader.record_count() {
        index_checkpoint(cx)?;
        let id = reader
            .doc_id_at(row)
            .map_err(|_| refused("native document identity is malformed"))?;
        if !documents.contains_key(id) || !seen.insert(id) {
            return Err(refused(
                "native document identities differ from the source proof",
            ));
        }
    }
    Ok(())
}

fn verify_native_pair(directory: &Path, expect_quality: bool) -> Result<(), IndexRebuildError> {
    let fast = super::super::open_fast_vector_index_read_only(directory)
        .map_err(|_| refused("native fast tier cannot be reopened"))?;
    let mut paths = frankensearch::index::TwoTierIndexPaths::new(fast.path());
    if expect_quality {
        paths = paths.with_quality_index(directory.join(VECTOR_INDEX_QUALITY_FILE));
    }
    // Explicit paths omit every ANN sidecar, including when that feature is
    // enabled. Pair admission is read-only and delegates role-alias checks to
    // the native owner instead of recreating its filesystem identity rules.
    let pair = frankensearch::TwoTierIndex::open_with_paths(
        &paths,
        frankensearch::TwoTierConfig::default(),
    )
    .map_err(|_| refused("native tier roles cannot be admitted together"))?;
    if pair.quality_index_path().is_some() != expect_quality {
        return Err(refused(
            "native pair admission would discard the quality tier",
        ));
    }
    Ok(())
}

pub(in super::super) async fn stage(
    publish_lock: &IndexPublishLockOwner<'_>,
    live_dir: &Path,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
    generation: u64,
    document_counts: IndexDocumentCounts,
) -> Result<UnpublishedIndexStage, DeltaStageError> {
    stage_with_capacity(
        publish_lock,
        live_dir,
        stack,
        documents,
        generation,
        document_counts,
        storage::filesystem_capacity,
    )
    .await
}

async fn stage_with_capacity(
    publish_lock: &IndexPublishLockOwner<'_>,
    live_dir: &Path,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
    generation: u64,
    document_counts: IndexDocumentCounts,
    mut probe: impl FnMut(&Path) -> Result<storage::Capacity, IndexRebuildError>,
) -> Result<UnpublishedIndexStage, DeltaStageError> {
    let result = stage_inner(
        publish_lock,
        live_dir,
        stack,
        documents,
        generation,
        document_counts,
        &mut probe,
    )
    .await;
    // Native/manifest failures may race cancellation. Preserve the request or
    // operation-control reason before a caller can try an ordinary full build.
    publish_lock.checkpoint().map_err(DeltaStageError::from)?;
    result.map_err(DeltaStageError::from)
}

async fn stage_inner(
    publish_lock: &IndexPublishLockOwner<'_>,
    live_dir: &Path,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
    generation: u64,
    document_counts: IndexDocumentCounts,
    probe: &mut impl FnMut(&Path) -> Result<storage::Capacity, IndexRebuildError>,
) -> Result<UnpublishedIndexStage, IndexRebuildError> {
    publish_lock.checkpoint()?;
    super::ensure_index_path_has_no_symlinks(live_dir, "admit selective tier refresh")?;
    // The selected fast producer may be the tier being replaced. Admit corpus,
    // security epoch and native counts independently of the process-wide model;
    // the V3/native checks below prove every retained producer exactly.
    let previous_generation =
        super::super::validated_index_generation_with_identity(live_dir, None)
            .map_err(|_| refused("live generation metadata is incompatible"))?;
    if previous_generation > generation {
        return Err(refused("live generation is ahead of the source snapshot"));
    }
    let cx = publish_lock.cx;
    let plan = TierRefreshPlan::read(cx, live_dir, stack, documents)?;
    if u32::try_from(documents.len()).ok() != Some(document_counts.total()) {
        return Err(refused("source and publication counts disagree"));
    }
    let (admitted, retained_roots, publication_nonce) = plan.admit_sources(cx, live_dir, stack)?;
    let allocation = storage::TierRefreshAllocation {
        fast: !plan.retain_fast,
        quality: stack.quality().is_some() && !plan.retain_quality,
        lexical: cfg!(feature = "lexical-bm25") && plan.rebuild_lexical,
        digest_bytes: storage::serialized_bytes(&plan.digests)?,
    };
    publish_lock.checkpoint()?;
    storage::admit_tier_refresh(cx, live_dir, stack, documents, allocation, &mut *probe)?;
    let staging = UnpublishedIndexStage::create(live_dir)?;
    copy_generation_files(
        live_dir,
        staging.path(),
        Some(&admitted),
        Some(&retained_roots),
    )?;
    publish_lock.checkpoint()?;
    storage::admit_tier_refresh(
        cx,
        staging.path(),
        stack,
        documents,
        allocation,
        &mut *probe,
    )?;
    plan.verify_stack(stack)?;
    if allocation.fast {
        build_tier(
            publish_lock,
            &staging.path().join(VECTOR_INDEX_FAST_FILE),
            stack.fast(),
            documents,
            publication_nonce,
            "fast",
        )
        .await?;
    }
    if allocation.quality {
        let embedder = stack
            .quality()
            .ok_or_else(|| refused("selected quality tier disappeared"))?;
        build_tier(
            publish_lock,
            &staging.path().join(VECTOR_INDEX_QUALITY_FILE),
            embedder,
            documents,
            publication_nonce,
            "quality",
        )
        .await?;
    }
    #[cfg(feature = "lexical-bm25")]
    if plan.rebuild_lexical {
        let lexical_allocation = storage::TierRefreshAllocation {
            fast: false,
            quality: false,
            ..allocation
        };
        storage::admit_tier_refresh(
            cx,
            staging.path(),
            stack,
            documents,
            lexical_allocation,
            &mut *probe,
        )?;
        super::super::build_lexical_tier(cx, staging.path(), documents).await?;
    }
    publish_lock.checkpoint()?;
    plan.verify_stack(stack)?;
    super::super::verify_index_vector_producers(
        staging.path(),
        &super::super::stack_descriptors(stack),
    )
    .map_err(IndexRebuildError::Index)?;
    super::super::verify_published_tier_counts(
        staging.path(),
        document_counts.total(),
        stack.quality().is_some(),
    )
    .map_err(IndexRebuildError::Index)?;
    verify_native_pair(staging.path(), stack.quality().is_some())?;
    storage::admit_manifest(cx, staging.path(), allocation.digest_bytes, &mut *probe)?;
    plan.digests.write(staging.path())?;
    super::super::sync_index_generation(staging.path(), || publish_lock.checkpoint())?;
    storage::confirm_reserve(cx, staging.path(), &mut *probe)?;
    plan.verify_stack(stack)?;
    publish_lock.checkpoint()?;
    tracing::info!(
        target: "ee::index",
        fast_reused = plan.retain_fast,
        quality_reused = plan.retain_quality,
        lexical_rebuilt = allocation.lexical,
        documents = documents.len(),
        "staged index generation with compatible native tiers retained"
    );
    Ok(staging)
}

async fn build_tier(
    publish_lock: &IndexPublishLockOwner<'_>,
    path: &Path,
    embedder: &dyn Embedder,
    documents: &[IndexableDocument],
    publication_nonce: u16,
    tier: &str,
) -> Result<(), IndexRebuildError> {
    publish_lock.checkpoint()?;
    for target in [path.to_path_buf(), wal_path_for(path)] {
        super::ensure_index_path_has_no_symlinks(&target, "build selected native tier")?;
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(refused(
                    "new native tier or WAL pathname is already occupied",
                ));
            }
        }
    }
    let producer = verified_embedder_identity(embedder)
        .ok_or_else(|| refused("changed tier has no validated producer identity"))?;
    let mut writer = VectorIndex::create_with_revision(
        path,
        embedder.id(),
        &producer,
        embedder.dimension(),
        Quantization::F16,
    ).map_err(|_| refused("native tier writer could not be created"))?
        // The retained native tier must remain byte-identical. Carry its pair
        // nonce into the new tier; EE atomically publishes the whole directory.
        // A different nonce would silently disable quality on native reopen.
        .with_publication_nonce(publication_nonce);
    let embedded = batch::embed_batches_with(
        publish_lock.cx,
        embedder,
        documents,
        &producer,
        tier,
        |entries| {
            for (id, vector) in entries {
                publish_lock.checkpoint().map_err(|_| {
                    super::incremental_fallback(
                        super::IncrementalFallbackReason::TierUnavailable,
                        "selective tier inference interrupted",
                    )
                })?;
                writer.write_record(&id, &vector).map_err(|_| {
                    super::incremental_fallback(
                        super::IncrementalFallbackReason::TierUnavailable,
                        "native selective tier writer rejected an embedding",
                    )
                })?;
            }
            Ok(())
        },
    )
    .await;
    publish_lock.checkpoint()?;
    embedded.map_err(|_| refused("identity-bound tier inference failed"))?;
    let finished = writer.finish();
    publish_lock.checkpoint()?;
    finished.map_err(|_| refused("native selected tier could not be persisted"))
}

#[cfg(test)]
#[path = "index_tier_refresh_tests.rs"]
mod tests;
