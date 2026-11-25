//! Source witnesses for reusing daemon retrieval across the user-global lane.
//!
//! A workspace generation cannot attest another database, its feedback, or a
//! change in global participation. Only hashes travel over IPC; the receiving
//! process obtains bodies and admission authority from its own pinned source.

use super::*;
use crate::core::global_store::{GlobalInclusionInput, GlobalStorePaths};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GlobalPackSearchWitness {
    identity: String,
}

pub(super) struct ValidatedGlobalPackSnapshot {
    source: Option<GlobalStoreSearchSnapshot>,
    pub(super) local_memories: BTreeMap<String, StoredMemory>,
}

fn hash_json(hasher: &mut blake3::Hasher, value: &serde_json::Value) -> Option<()> {
    let bytes = serde_json::to_vec(value).ok()?;
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(&bytes);
    Some(())
}

fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    // An unpublished index may not exist yet. Resolve its nearest existing
    // ancestor so the CLI's canonical workspace and the caller's symlink or
    // relative spelling still identify the same source without creating it.
    let mut ancestor = path;
    let mut suffix = Vec::new();
    let resolved = loop {
        if let Ok(mut resolved) = ancestor.canonicalize() {
            for component in suffix.iter().rev() {
                resolved.push(component);
            }
            break resolved;
        }
        let Some(name) = ancestor.file_name() else {
            break path.to_path_buf();
        };
        suffix.push(name.to_owned());
        let Some(parent) = ancestor.parent() else {
            break path.to_path_buf();
        };
        ancestor = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
    };
    let bytes = resolved.as_os_str().as_encoded_bytes();
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

pub(super) fn request_identity(options: &SearchOptions) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.pack.retrieval_request_witness.v1");
    if options.no_global {
        hasher.update(b"no_global\0");
    }
    hash_path(&mut hasher, &options.workspace_path);
    hash_path(&mut hasher, &options.resolve_database_path());
    hash_path(&mut hasher, &options.resolve_index_dir());
    hash_json(
        &mut hasher,
        &serde_json::json!({
            "query": options.query,
            "asOf": options.as_of,
            "limit": options.limit,
            "speed": options.speed.as_str(),
            "explain": options.explain,
            "includeTombstoned": options.include_tombstoned,
            "includeExpired": options.include_expired,
            "includeFuture": options.include_future,
            "includeStale": options.include_stale,
            "relevanceFloorBits": options.relevance_floor.map(f32::to_bits),
            "dedup": options.dedup_mode.as_str(),
            "sourceMode": options.source_mode.as_str(),
            "strictSourceMode": options.strict_source_mode,
            "memoryScope": options.memory_scope.as_str(),
            "strictScope": options.strict_scope,
        }),
    )?;
    Some(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub(super) fn capture(
    options: &SearchOptions,
    policy: &crate::config::MemoryConfig,
    admitted: Option<(&GlobalStorePaths, &GlobalStoreSearchSnapshot)>,
) -> Option<GlobalPackSearchWitness> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.pack.global_source_witness.v1");
    if !global_store_participates_in_scope(options.memory_scope) || options.query.trim().is_empty()
    {
        hasher.update(b"not_requested");
    } else {
        let resolved_paths;
        let paths = if let Some((paths, _)) = admitted {
            paths
        } else {
            resolved_paths =
                crate::core::global_store::default_global_store_paths_from_env().ok()?;
            &resolved_paths
        };
        let participating = policy.participate.unwrap_or(true);
        let enabled = policy.include_global.unwrap_or(true);
        hash_json(&mut hasher, &serde_json::json!([participating, enabled]))?;
        hash_path(&mut hasher, &paths.root);
        hash_path(&mut hasher, &paths.database_path);
        if let Some((_, snapshot)) = admitted {
            if options.no_global || !participating || !enabled {
                return None;
            }
            hasher.update(b"admitted");
            let connection = snapshot.source.connection();
            let workspace = crate::core::workspace::addressed_workspace_row(
                connection,
                &paths.root,
                &paths.database_path,
            )
            .ok()?;
            let generation = workspace
                .as_ref()
                .map(|workspace| connection.get_workspace_generation(&workspace.id))
                .transpose()
                .ok()?
                .flatten();
            if !snapshot.memories.is_empty() && snapshot.generation != generation {
                return None;
            }
            hash_json(
                &mut hasher,
                &serde_json::json!([workspace.as_ref().map(|row| &row.id), generation]),
            )?;
            // A count/watermark cannot detect same-count ledger edits. Read
            // the complete ledger on the body snapshot, including future rows.
            let ledger = if let Some(workspace) = &workspace {
                let events = connection.list_feedback_events(&workspace.id).ok()?;
                Some(
                    DbConnection::feedback_events_content_fingerprint_from_events(
                        &workspace.id,
                        &events,
                    )
                    .ok()?,
                )
            } else {
                None
            };
            hash_json(&mut hasher, &serde_json::json!(ledger))?;
            hash_json(
                &mut hasher,
                &serde_json::json!({
                    "referenceTime": snapshot.feedback.reference_time,
                    "workspaceId": snapshot.feedback.workspace_id,
                    "harmfulCounts": snapshot.feedback.harmful_counts,
                }),
            )?;
            let mut memories = snapshot.memories.iter().collect::<Vec<_>>();
            memories.sort_by(|left, right| left.id.cmp(&right.id));
            for memory in memories {
                // Include every source field, not just selected hits or body
                // hashes: ranking and provenance can change without new IDs.
                hash_json(
                    &mut hasher,
                    &serde_json::json!({
                        "id": memory.id,
                        "workspaceId": memory.workspace_id,
                        "level": memory.level,
                        "kind": memory.kind,
                        "content": memory.content,
                        "workflowId": memory.workflow_id,
                        "confidenceBits": memory.confidence.to_bits(),
                        "utilityBits": memory.utility.to_bits(),
                        "importanceBits": memory.importance.to_bits(),
                        "provenanceUri": memory.provenance_uri,
                        "trustClass": memory.trust_class,
                        "trustSubclass": memory.trust_subclass,
                        "provenanceChainHash": memory.provenance_chain_hash,
                        "provenanceChainHashVersion": memory.provenance_chain_hash_version,
                        "provenanceVerificationStatus": memory.provenance_verification_status,
                        "provenanceVerifiedAt": memory.provenance_verified_at,
                        "provenanceVerificationNote": memory.provenance_verification_note,
                        "createdAt": memory.created_at,
                        "updatedAt": memory.updated_at,
                        "tombstonedAt": memory.tombstoned_at,
                        "validFrom": memory.valid_from,
                        "validTo": memory.valid_to,
                    }),
                )?;
            }
        } else {
            let inclusion =
                crate::core::global_store::resolve_global_inclusion(&GlobalInclusionInput {
                    store_present: paths.database_path.try_exists().ok()?,
                    participating,
                    config_enabled: enabled,
                    no_global_flag: options.no_global,
                });
            // Failure to inspect an enabled store is not proof of an empty
            // lane. Keep the handoff unusable and let canonical retrieval retry.
            if inclusion.included {
                return None;
            }
            hash_json(&mut hasher, &serde_json::json!(inclusion.reason.as_str()))?;
        }
    }
    Some(GlobalPackSearchWitness {
        identity: format!("blake3:{}", hasher.finalize().to_hex()),
    })
}

pub(super) fn validate(
    expected: &GlobalPackSearchWitness,
    options: &SearchOptions,
    reference_time: DateTime<Utc>,
    connection: &DbConnection,
    hits: &[SearchHit],
) -> Option<ValidatedGlobalPackSnapshot> {
    let requested = global_store_participates_in_scope(options.memory_scope)
        && !options.query.trim().is_empty();
    let policy = if requested {
        crate::config::workspace_memory_policy(&options.workspace_path).ok()?
    } else {
        crate::config::MemoryConfig::default()
    };
    let mut degraded = Vec::new();
    let admitted = requested
        .then(|| {
            admitted_global_store_memories(
                options,
                &policy,
                SearchMemoryFilters::default(),
                reference_time,
                &mut degraded,
            )
        })
        .flatten();
    let actual = capture(
        options,
        &policy,
        admitted.as_ref().map(|(paths, snapshot)| (paths, snapshot)),
    )?;
    if &actual != expected {
        return None;
    }
    let source = admitted.map(|(_, snapshot)| snapshot);
    let local_memories = if source.is_some() {
        let ids = hits
            .iter()
            .filter(|hit| hit.doc_id.starts_with("mem_"))
            .map(|hit| hit.doc_id.as_str())
            .collect::<Vec<_>>();
        // A failed local read cannot grant a colliding global row authority.
        connection.get_memories_by_id_seeks(&ids).ok()?
    } else {
        BTreeMap::new()
    };
    Some(ValidatedGlobalPackSnapshot {
        source,
        local_memories,
    })
}

impl ValidatedGlobalPackSnapshot {
    pub(super) fn feedback(&self) -> Option<&GlobalMemoryFeedbackSnapshot> {
        self.source.as_ref().map(|snapshot| &snapshot.feedback)
    }

    pub(super) fn take_global_hits(
        &self,
        options: &SearchOptions,
        hits: &mut Vec<SearchHit>,
        memories: &mut BTreeMap<String, StoredMemory>,
    ) -> Vec<SearchHit> {
        let Some(source) = &self.source else {
            return Vec::new();
        };
        let global_rows = source
            .memories
            .iter()
            .map(|memory| (memory.id.as_str(), memory))
            .collect::<BTreeMap<_, _>>();
        let (global_hits, local_hits): (Vec<_>, Vec<_>) =
            std::mem::take(hits).into_iter().partition(|hit| {
                !self.local_memories.contains_key(&hit.doc_id)
                    && global_rows.contains_key(hit.doc_id.as_str())
            });
        *hits = local_hits;
        let selected = global_hits
            .iter()
            .filter_map(|hit| global_rows.get(hit.doc_id.as_str()).copied().cloned())
            .collect();
        // Scope admission and quality scoring need source-derived metadata.
        // Preserve the derived annotations certified by the request and both
        // sources: calibration intervals precede mesh adjustment, and an MI
        // survivor must retain its duplicate provenance after rehydration.
        let annotations = global_hits
            .iter()
            .map(|hit| {
                let fields = [
                    "scoreInterval",
                    "calibrated",
                    "coverageGuarantee",
                    "scoreCalibration",
                    "dedupeMode",
                    "mergedFrom",
                    "_ee_mesh_trust_adjustment",
                ]
                .into_iter()
                .filter_map(|key| {
                    hit.metadata
                        .as_ref()?
                        .get(key)
                        .map(|value| (key.to_owned(), value.clone()))
                })
                .collect::<serde_json::Map<_, _>>();
                (hit.doc_id.clone(), fields)
            })
            .collect::<BTreeMap<_, _>>();
        let mut admitted =
            admit_global_store_search_hits(options, selected, global_hits, Some(memories));
        for hit in &mut admitted {
            if let Some(memory) = global_rows.get(hit.doc_id.as_str()) {
                mark_hit_scope(hit, options.memory_scope, memory);
            }
            if let Some(metadata) = hit
                .metadata
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
                && let Some(fields) = annotations.get(&hit.doc_id)
            {
                metadata.extend(fields.clone());
            }
        }
        admitted
    }
}
