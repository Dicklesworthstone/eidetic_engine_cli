//! Cached-only model resolution for read-only retrieval.
//!
//! Search can lazily download a model or use a configured remote endpoint.
//! Scoped answers and nonpersisting packs do neither. Reuse the registry's
//! verified local identity and the default cache verifier.

use super::*;
use crate::core::remote_embed::{EmbedBackendSelection, configured_embed_backend};

/// Proof of the concrete, already-loaded local model admitted for one daemon
/// request. This is model identity, not a cache-hit or a claim that inference
/// succeeded. Runtime retrieval still owns the executed backend in its report.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CachedLocalEmbedderAttestation {
    backend: EmbedBackend,
    model_id: String,
    model_hash: String,
    dimension: u32,
}

impl CachedLocalEmbedderAttestation {
    pub(crate) fn matches_client_configuration(&self) -> bool {
        self.is_valid()
            && cached_daemon_model_allowed(
                &default_embedder_settings(),
                configured_embed_backend() == EmbedBackendSelection::Remote,
            )
    }

    pub(crate) fn is_valid(&self) -> bool {
        let descriptor = EmbedderDescriptor::potion();
        self.backend == EmbedBackend::NeuralLocal
            && self.model_id == descriptor.id
            && Some(self.dimension) == u32::try_from(descriptor.dimension).ok()
            && self.model_hash
                == descriptor_content_hash(
                    &descriptor,
                    ModelProvider::Model2Vec,
                    Some(&ModelManifest::potion_128m()),
                )
    }

    fn for_loaded(embedder: &dyn crate::search::Embedder) -> Option<Self> {
        if !embedder.is_ready()
            || !embedder.is_semantic()
            || embedder.category() != ModelCategory::StaticEmbedder
            || embedder.tier() != ModelTier::Fast
        {
            return None;
        }
        let attestation = Self {
            backend: EmbedBackend::NeuralLocal,
            model_id: embedder.id().to_owned(),
            model_hash: active_embedder_fingerprint(embedder, ModelProvider::Model2Vec)
                .content_hash,
            dimension: u32::try_from(embedder.dimension()).ok()?,
        };
        attestation.is_valid().then_some(attestation)
    }
}

pub(crate) struct CachedLocalEmbedder {
    pub(crate) embedder: Arc<dyn crate::search::Embedder>,
    pub(crate) attestation: CachedLocalEmbedderAttestation,
}

/// Unlike cached-on-disk preparation below, this never loads weights, creates
/// a lazy model, downloads, initializes a process cache, or contacts a remote
/// service. An unavailable/busy cache is a refusal, not an invitation to warm.
pub(crate) fn already_loaded_local_embedder_for_workspace(
    workspace_path: &Path,
    database_path: &Path,
) -> Result<Option<CachedLocalEmbedder>, DbError> {
    if configured_embed_backend() == EmbedBackendSelection::Remote {
        return Ok(None);
    }
    let connection = DbConnection::open_file_read_only(database_path)?;
    let Some(workspace_id) = workspace_id_for_index_status(&connection, workspace_path)? else {
        return Ok(None);
    };
    already_loaded_local_selection(
        Some((&connection, &workspace_id)),
        &default_embedder_settings(),
        false,
        &DEFAULT_SEARCH_EMBEDDER,
        &REGISTERED_MODEL2VEC_CACHE,
    )
}

fn already_loaded_local_selection(
    registry: Option<(&DbConnection, &str)>,
    settings: &EeEmbedderSettings,
    remote_selected: bool,
    default: &OnceLock<DefaultSearchEmbedder>,
    registered: &OnceLock<RegisteredModel2VecCache>,
) -> Result<Option<CachedLocalEmbedder>, DbError> {
    if remote_selected {
        return Ok(None);
    }
    if settings.local_source != EmbedModelSource::Configured
        && let Some((connection, workspace_id)) = registry
    {
        // Run the same live registry/path/hash/dimension admission as ordinary
        // retrieval, but replace its loader with an exact-identity cache probe.
        // A replaced registration must never inherit the previous model.
        match resolve_registered_model2vec(connection, workspace_id, |identity| {
            Ok(already_loaded_registered(registered, &identity))
        })? {
            RegisteredModel2VecResolution::Ready(embedder) => {
                return Ok(embedder.and_then(attest_loaded_local));
            }
            RegisteredModel2VecResolution::Rejected(_) => return Ok(None),
            RegisteredModel2VecResolution::NotRegistered
            | RegisteredModel2VecResolution::BundledDefaultDeclared => {}
        }
    }
    let Some(selection) = default.get() else {
        return Ok(None);
    };
    let source_matches = selection.model_resolution.source == settings.local_source
        || (settings.local_source == EmbedModelSource::Cache
            && selection.model_resolution.source == EmbedModelSource::Downloaded);
    if !source_matches || selection.local_model_load_failed() {
        return Ok(None);
    }
    let Some(loaded) = attest_loaded_local(selection.stack.fast_arc()) else {
        return Ok(None);
    };
    // Keep the current local artifact admissibility gate. Verification may
    // read a stale receipt's files, but it cannot load a model or write one.
    Ok(verified_default_model_dir(settings).map(|_| loaded))
}

fn attest_loaded_local(embedder: Arc<dyn crate::search::Embedder>) -> Option<CachedLocalEmbedder> {
    let attestation = CachedLocalEmbedderAttestation::for_loaded(embedder.as_ref())?;
    Some(CachedLocalEmbedder {
        embedder,
        attestation,
    })
}

fn already_loaded_registered(
    registered: &OnceLock<RegisteredModel2VecCache>,
    identity: &RegisteredModel2VecIdentity,
) -> Option<Arc<dyn crate::search::Embedder>> {
    registered
        .get()
        .and_then(|cache| cache.current.try_lock().ok())
        .and_then(|entry| {
            entry.as_ref().and_then(|entry| {
                (&entry.identity == identity).then(|| Arc::clone(&entry.embedder))
            })
        })
}

fn cached_daemon_model_allowed(settings: &EeEmbedderSettings, remote_selected: bool) -> bool {
    // A warm peer must not override the invoking process's explicit remote or
    // invalid local-model choice. This verifies files only, never loads them.
    !remote_selected
        && (settings.local_source != EmbedModelSource::Configured
            || verified_default_model_dir(settings).is_some())
}

pub(crate) fn local_read_only_embedder(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<Option<Arc<dyn crate::search::Embedder>>, DbError> {
    let selection = cached_local_selection(
        Some((connection, workspace_id)),
        &default_embedder_settings(),
        configured_embed_backend() == EmbedBackendSelection::Remote,
    )?;
    Ok(semantic_only(selection.stack.fast_arc()))
}

/// Prepare an executable local model without downloading, activating a
/// registry entry, or changing the process-default search configuration.
/// The fallback is a concrete ready hash model, never a lazy downloader.
pub(crate) fn prepare_read_only_search_embedder_for_workspace(
    cx: &asupersync::Cx,
    workspace_path: &Path,
    database_path: &Path,
) -> Result<EmbedderPreparation, SearchError> {
    prepare_read_only_search_embedder_with_settings(
        cx,
        workspace_path,
        database_path,
        &default_embedder_settings(),
        configured_embed_backend() == EmbedBackendSelection::Remote,
    )
}

fn prepare_read_only_search_embedder_with_settings(
    cx: &asupersync::Cx,
    workspace_path: &Path,
    database_path: &Path,
    settings: &EeEmbedderSettings,
    remote_selected: bool,
) -> Result<EmbedderPreparation, SearchError> {
    model_initialization_checkpoint(cx, "before read-only embedder preparation")?;
    let started = Instant::now();
    let database = database_path
        .exists()
        .then(|| DbConnection::open_file_read_only(database_path))
        .transpose()
        .map_err(|error| SearchError::SubsystemError {
            subsystem: "model registry",
            source: Box::new(error),
        })?;
    let workspace_id = database
        .as_ref()
        .map(|database| workspace_id_for_index_status(database, workspace_path))
        .transpose()
        .map_err(|error| SearchError::SubsystemError {
            subsystem: "model registry",
            source: Box::new(error),
        })?
        .flatten();
    let selection = cached_local_selection(
        database.as_ref().zip(workspace_id.as_deref()),
        settings,
        remote_selected,
    )
    .map_err(|error| SearchError::SubsystemError {
        subsystem: "model registry",
        source: Box::new(error),
    })?;
    model_initialization_checkpoint(cx, "after read-only embedder preparation")?;
    let backend = if selection.stack.fast().is_semantic() {
        EmbedBackend::NeuralLocal
    } else {
        EmbedBackend::HashFallback
    };
    Ok(EmbedderPreparation::new(
        backend,
        selection.model_resolution,
        started.elapsed(),
        selection.stack.fast_arc(),
    ))
}

pub(super) fn cached_local_selection(
    registry: Option<(&DbConnection, &str)>,
    settings: &EeEmbedderSettings,
    remote_selected: bool,
) -> Result<WorkspaceRegistryEmbedderSelection, DbError> {
    let hash_fallback = || WorkspaceRegistryEmbedderSelection {
        stack: hash_fallback_embedder_stack(),
        model_resolution: EmbedModelResolution::deterministic_hash(),
    };
    // Read-only retrieval cannot submit source content to a remote endpoint.
    // An explicit remote choice also must not select a different local model.
    if remote_selected {
        return Ok(hash_fallback());
    }
    #[cfg(test)]
    if settings.local_source != EmbedModelSource::Configured
        && let Some((_, workspace_id)) = registry
        && let Some(stack) = TEST_WORKSPACE_EMBEDDER_STACK_OVERRIDES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .ok()
            .and_then(|overrides| overrides.get(workspace_id).cloned())
    {
        let model_resolution = if stack.fast().is_semantic() {
            EmbedModelResolution::ready(EmbedModelSource::Registered)
        } else {
            EmbedModelResolution::deterministic_hash()
        };
        return Ok(WorkspaceRegistryEmbedderSelection {
            stack,
            model_resolution,
        });
    }
    if settings.local_source != EmbedModelSource::Configured
        && let Some((connection, workspace_id)) = registry
        && let Some(selection) = workspace_registry_embedder_selection(connection, workspace_id)?
    {
        // Rejected registry entries resolve to the hash tier. Do not bypass a
        // rejected explicit registration by trying another local directory.
        return Ok(selection);
    }
    Ok(match cached_default(settings) {
        Some(embedder) => WorkspaceRegistryEmbedderSelection {
            stack: EmbedderStack::from_parts(embedder, None),
            model_resolution: EmbedModelResolution::ready(settings.local_source),
        },
        None => hash_fallback(),
    })
}

fn semantic_only(
    embedder: Arc<dyn crate::search::Embedder>,
) -> Option<Arc<dyn crate::search::Embedder>> {
    (embedder.is_semantic() && embedder.dimension() > 0).then_some(embedder)
}

fn cached_default(settings: &EeEmbedderSettings) -> Option<Arc<dyn crate::search::Embedder>> {
    // Keep the configured discovery semantics, including direct model paths,
    // but deliberately omit the Auto branch that creates a lazy downloader.
    let model_dir = verified_default_model_dir(settings)?;
    match Model2VecEmbedder::load_shared_with_name(&model_dir, POTION_MODEL_NAME) {
        Ok(embedder) => semantic_only(embedder),
        Err(_) => {
            // Model paths and backend diagnostics need not become public
            // output. The existing semantic-degraded field reports fallback.
            tracing::warn!(
                target: "ee::index::embedder",
                "verified cached read-only model could not be loaded; using lexical scoring"
            );
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hash_fallback_is_not_a_semantic_model() {
        assert!(semantic_only(hash_fallback_embedder_stack().fast_arc()).is_none());
    }

    #[test]
    fn already_loaded_probe_does_not_initialize_either_cache() {
        let root = tempfile::tempdir().unwrap();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("absent"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let default = OnceLock::new();
        let registered = OnceLock::new();
        assert!(
            already_loaded_local_selection(None, &settings, false, &default, &registered)
                .unwrap()
                .is_none()
        );
        assert!(default.get().is_none());
        assert!(registered.get().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn already_loaded_registered_probe_is_exact_nonblocking_and_noninitializing() {
        let registered = OnceLock::new();
        let identity = RegisteredModel2VecIdentity {
            canonical_source: PathBuf::from("/verified/model"),
            content_hash: "blake3:cache-identity".to_owned(),
            dimension: 256,
            distance_metric: "cosine",
        };
        assert!(already_loaded_registered(&registered, &identity).is_none());
        assert!(registered.get().is_none());
        let cache = registered.get_or_init(RegisteredModel2VecCache::default);
        let expected = cache
            .get_or_try_insert_with(identity.clone(), || {
                Some(hash_fallback_embedder_stack().fast_arc())
            })
            .unwrap();
        // Exercise the real cache, not semantic admission: a ready hash model
        // is still rejected separately by attest_loaded_local.
        let cached = already_loaded_registered(&registered, &identity).unwrap();
        assert!(Arc::ptr_eq(&expected, &cached));
        let mut replaced = identity.clone();
        replaced.canonical_source = PathBuf::from("/replacement/model");
        assert!(already_loaded_registered(&registered, &replaced).is_none());
        let mut changed_dimension = identity.clone();
        changed_dimension.dimension = 128;
        assert!(already_loaded_registered(&registered, &changed_dimension).is_none());
        let _loading = cache.current.lock().unwrap();
        assert!(already_loaded_registered(&registered, &identity).is_none());
    }

    #[test]
    fn already_loaded_client_policy_preserves_explicit_model_choices() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = EeEmbedderSettings {
            model_root: root.path().join("absent"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        assert!(cached_daemon_model_allowed(&settings, false));
        assert!(!cached_daemon_model_allowed(&settings, true));
        settings.local_source = EmbedModelSource::Configured;
        assert!(!cached_daemon_model_allowed(&settings, false));
        assert!(!cached_daemon_model_allowed(&settings, true));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn already_loaded_probe_refuses_remote_before_registry_or_cache_access() {
        let database = DbConnection::open_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("absent"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let default = OnceLock::new();
        let registered = OnceLock::new();
        // This database has no schema: even reading the registry would fail.
        assert!(
            already_loaded_local_selection(
                Some((&database, "workspace")),
                &settings,
                true,
                &default,
                &registered,
            )
            .unwrap()
            .is_none()
        );
        assert!(default.get().is_none());
        assert!(registered.get().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn already_loaded_probe_refuses_ready_hash_and_pending_lazy_models() {
        let root = tempfile::tempdir().unwrap();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("absent"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let hash = hash_fallback_embedder_stack().fast_arc();
        assert!(hash.is_ready());
        assert!(attest_loaded_local(hash).is_none());
        let default = OnceLock::new();
        assert!(
            default
                .set(ee_auto_download_embedder(settings.model_root.clone()))
                .is_ok()
        );
        assert!(
            already_loaded_local_selection(None, &settings, false, &default, &OnceLock::new())
                .unwrap()
                .is_none()
        );
        let lazy = default.get().unwrap().lazy_model2vec.as_ref().unwrap();
        assert!(!lazy.is_ready());
        assert!(!lazy.failed());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn cached_local_attestation_rejects_model_identity_drift() {
        let descriptor = EmbedderDescriptor::potion();
        let value = serde_json::json!({
            "backend": "neural_local",
            "modelId": descriptor.id,
            "modelHash": descriptor_content_hash(
                &descriptor, ModelProvider::Model2Vec, Some(&ModelManifest::potion_128m()),
            ),
            "dimension": descriptor.dimension,
        });
        let attestation: CachedLocalEmbedderAttestation =
            serde_json::from_value(value.clone()).unwrap();
        assert!(attestation.is_valid());
        for (key, wrong) in [
            ("backend", serde_json::json!("hash_fallback")),
            ("modelId", serde_json::json!("other-model")),
            ("modelHash", serde_json::json!("blake3:wrong")),
            ("dimension", serde_json::json!(0)),
        ] {
            let mut changed = value.clone();
            changed[key] = wrong;
            let changed: CachedLocalEmbedderAttestation = serde_json::from_value(changed).unwrap();
            assert!(!changed.is_valid(), "accepted changed {key}");
        }
        let mut extra = value;
        extra["untrustedClaim"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CachedLocalEmbedderAttestation>(extra).is_err());
    }

    #[test]
    #[ignore = "requires the real potion-multilingual-128M fixture"]
    fn real_potion_cached_daemon_model_reuses_the_loaded_allocation() {
        let root = crate::config::env_registry::read_os(
            crate::config::env_registry::EnvVar::EmbedModelFixtureDir,
        )
        .map(PathBuf::from)
        .expect("EE_EMBED_MODEL_FIXTURE_DIR must name the real model");
        let settings = EeEmbedderSettings {
            model_root: root,
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Configured,
        };
        let directory = verified_default_model_dir(&settings).expect("verified fixture");
        let expected: Arc<dyn crate::search::Embedder> =
            Model2VecEmbedder::load_shared_with_name(&directory, POTION_MODEL_NAME).unwrap();
        let default = OnceLock::new();
        assert!(
            default
                .set(DefaultSearchEmbedder::ready(
                    EmbedderStack::from_parts(Arc::clone(&expected), None),
                    EmbedModelResolution::ready(EmbedModelSource::Configured),
                ))
                .is_ok()
        );
        let registered = OnceLock::new();
        let admitted =
            already_loaded_local_selection(None, &settings, false, &default, &registered)
                .unwrap()
                .expect("warm local model");
        assert!(Arc::ptr_eq(&expected, &admitted.embedder));
        assert!(admitted.attestation.is_valid());
        assert!(registered.get().is_none());
        let cx = asupersync::Cx::for_testing();
        let (direct, delegated) = crate::core::run_cli_future(async {
            (
                expected.embed(&cx, "durable agent memory").await.unwrap(),
                admitted
                    .embedder
                    .embed(&cx, "durable agent memory")
                    .await
                    .unwrap(),
            )
        })
        .unwrap();
        assert_eq!(direct, delegated);
    }

    #[test]
    fn missing_cache_never_creates_directories_even_when_downloads_are_auto() {
        let root = tempfile::tempdir().unwrap();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("not-installed"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        assert!(cached_default(&settings).is_none());
        assert!(!settings.model_root.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn read_only_preparation_with_auto_downloads_keeps_workspace_and_registry_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let metadata = workspace.join(".ee");
        std::fs::create_dir_all(&metadata).unwrap();
        let database_path = metadata.join("ee.db");
        let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        let source_generation = {
            let database = DbConnection::open_file(&database_path).unwrap();
            database.migrate().unwrap();
            database
                .insert_workspace(
                    &workspace_id,
                    &crate::db::CreateWorkspaceInput {
                        path: workspace.display().to_string(),
                        name: None,
                    },
                )
                .unwrap();
            assert!(
                database
                    .list_model_registry_entries(&workspace_id)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(database.count_table_rows("audit_log").unwrap(), 0);
            database.get_workspace_generation(&workspace_id).unwrap()
        };
        let snapshot = || {
            let mut files = std::fs::read_dir(&metadata)
                .unwrap()
                .filter_map(|entry| {
                    let entry = entry.unwrap();
                    // Read-only WAL readers may update shared-memory reader
                    // slots. Durable database, WAL, and registry files must
                    // still remain byte-identical.
                    if entry.file_name().to_string_lossy().ends_with("-shm") {
                        return None;
                    }
                    Some((entry.file_name(), std::fs::read(entry.path()).unwrap()))
                })
                .collect::<Vec<_>>();
            files.sort_by(|left, right| left.0.cmp(&right.0));
            files
        };
        let before = snapshot();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("not-installed"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let cx = asupersync::Cx::for_testing();
        let prepared = prepare_read_only_search_embedder_with_settings(
            &cx,
            &workspace,
            &database_path,
            &settings,
            false,
        )
        .unwrap();
        assert_eq!(prepared.backend, EmbedBackend::HashFallback);
        assert_eq!(
            prepared.model_resolution,
            EmbedModelResolution::deterministic_hash()
        );
        assert!(prepared.fast_embedder.is_ready());
        assert!(!prepared.fast_embedder.is_semantic());
        assert!(!embedder_reports_pending_model2vec_download(
            prepared.fast_embedder.as_ref()
        ));
        let embedded = crate::core::run_cli_future(async {
            prepared.fast_embedder.embed(&cx, "read-only recall").await
        })
        .unwrap()
        .unwrap();
        assert_eq!(embedded.len(), 256);
        assert!(!settings.model_root.exists());
        assert_eq!(snapshot(), before);
        let database = DbConnection::open_file_read_only(&database_path).unwrap();
        assert!(
            database
                .list_model_registry_entries(&workspace_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(database.count_table_rows("audit_log").unwrap(), 0);
        assert_eq!(
            database.get_workspace_generation(&workspace_id).unwrap(),
            source_generation
        );
    }

    #[test]
    fn read_only_preparation_does_not_initialize_a_missing_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("absent-workspace");
        let database_path = workspace.join(".ee/ee.db");
        let settings = EeEmbedderSettings {
            model_root: root.path().join("not-installed"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let prepared = prepare_read_only_search_embedder_with_settings(
            &asupersync::Cx::for_testing(),
            &workspace,
            &database_path,
            &settings,
            false,
        )
        .unwrap();
        assert_eq!(prepared.backend, EmbedBackend::HashFallback);
        assert!(prepared.fast_embedder.is_ready());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn read_only_remote_selection_never_inspects_or_activates_registry() {
        // An unmigrated registry would fail if remote refusal fell through to
        // database inspection. The missing auto cache must also remain absent.
        let database = DbConnection::open_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let settings = EeEmbedderSettings {
            model_root: root.path().join("not-installed"),
            download_mode: EeEmbedDownloadMode::Auto,
            local_source: EmbedModelSource::Cache,
        };
        let selection =
            cached_local_selection(Some((&database, "workspace")), &settings, true).unwrap();
        assert!(selection.stack.fast().is_ready());
        assert!(!selection.stack.fast().is_semantic());
        assert_eq!(
            selection.model_resolution,
            EmbedModelResolution::deterministic_hash()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn unverified_files_never_enable_semantic_scoring() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(POTION_MODEL_NAME);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("model.safetensors"), b"not verified weights").unwrap();
        std::fs::write(directory.join("tokenizer.json"), b"{}").unwrap();
        let settings = EeEmbedderSettings {
            model_root: directory.clone(),
            download_mode: EeEmbedDownloadMode::Off,
            local_source: EmbedModelSource::Configured,
        };
        assert!(cached_default(&settings).is_none());
        assert_eq!(
            std::fs::read(directory.join("model.safetensors")).unwrap(),
            b"not verified weights"
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
    }
}
