//! Cached-only model resolution for read-only retrieval.
//!
//! Search can lazily download a model or use a configured remote endpoint.
//! Scoped answers and nonpersisting packs do neither. Reuse the registry's
//! verified local identity and the default cache verifier.

use super::*;
use crate::core::remote_embed::{EmbedBackendSelection, configured_embed_backend};

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
