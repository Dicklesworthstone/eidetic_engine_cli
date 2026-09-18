//! Cached-only model resolution for `ask`.
//!
//! Search can lazily download a model or use a configured remote endpoint.
//! A scoped answer read does neither. Reuse the registry's verified local
//! identity and the default cache verifier, not the downloading search stack.

use super::*;
use crate::core::remote_embed::{EmbedBackendSelection, configured_embed_backend};

pub(crate) fn local_ask_embedder(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<Option<Arc<dyn crate::search::Embedder>>, DbError> {
    // Selecting a remote backend is not consent to transmit transcript spans
    // from this command. Nor should ask silently choose a different model.
    if configured_embed_backend() == EmbedBackendSelection::Remote {
        return Ok(None);
    }
    if configured_embedder_model_root().is_none()
        && let Some(selection) = workspace_registry_embedder_selection(connection, workspace_id)?
    {
        // Rejected registry entries resolve to the hash tier. Do not bypass a
        // rejected explicit registration by trying another local directory.
        return Ok(semantic_only(selection.stack.fast_arc()));
    }
    Ok(cached_default(&default_embedder_settings()))
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
            // Model paths and backend diagnostics need not become public ask
            // output. The existing semantic-degraded field reports fallback.
            tracing::warn!(
                target: "ee::core::ask::semantic",
                "verified cached ask model could not be loaded; using lexical scoring"
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
