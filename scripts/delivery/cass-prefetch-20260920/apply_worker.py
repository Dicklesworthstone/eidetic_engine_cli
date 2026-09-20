#!/usr/bin/env python3
"""Wire the reviewed read-only worker without overwriting unrelated main changes."""
from pathlib import Path
import sys

assert 'MAX_PREFETCH_RESIDENT_HISTORIES' in Path('src/core/cass_prefetch.rs').read_text(), 'resident history safety must be integrated first'
installed = [
    'pub(crate) async fn try_read_for_prefetch(' in Path('src/core/index_read_lease.rs').read_text(),
    'pub(crate) use prefetch::warm_prefetch_lexical;' in Path('src/core/search.rs').read_text(),
    'fn plan_and_observe_cass_prefetch(' in Path('src/daemon/server.rs').read_text(),
]
if all(installed):
    print('worker wiring already integrated; verify current source')
    sys.exit(0)
assert not any(installed), 'partial worker wiring requires inspection'

def update(path, replacements):
    target = Path(path)
    text = target.read_text()
    for old, new in replacements:
        if new in text:
            assert text.count(new) == 1, f'duplicate replacement in {path}'
            continue
        assert text.count(old) == 1, f'changed source anchor in {path}: {old[:100]!r}'
        text = text.replace(old, new, 1)
    target.write_text(text)

update('src/core/index_read_lease.rs', [('''    /// Acquire BEFORE the database writer fence: a reader may still need DB
''', '''    /// Best-effort lexical warming never waits for a publisher, discovers a
    /// retained generation, repairs files or initializes an embedding model.
    /// Recheck the exact generation and corpus/security contract AFTER acquiring
    /// the publication lease, then reject unsafe entries before the backend opens.
    pub(crate) async fn try_read_for_prefetch(
        cx: &asupersync::Cx,
        index_dir: &Path,
        expected_generation: u64,
    ) -> Result<Option<Self>, IndexRebuildError> {
        index_checkpoint(cx)?;
        ensure_index_path_has_no_symlinks(index_dir, "prefetch lexical generation")?;
        let lease = Self::acquire(cx, index_dir, false, Duration::ZERO).await?;
        super::ensure_index_publish_target_is_directory_or_missing(
            index_dir, "prefetch lexical generation",
        )?;
        let metadata_path = index_dir.join(super::INDEX_METADATA_FILE);
        let Some(metadata) = super::parse_index_metadata(index_dir)
            .map_err(IndexRebuildError::Index)? else {
            return Ok(None);
        };
        if metadata.generation != Some(expected_generation)
            || super::index_metadata_compatibility_error(&metadata_path, &metadata).is_some()
        {
            return Ok(None);
        }
        index_checkpoint(cx)?;
        ensure_generation_entries_are_regular(cx, &index_dir.join("lexical"))?;
        Ok(Some(lease))
    }

    /// Acquire BEFORE the database writer fence: a reader may still need DB
''')])

update('src/core/search.rs', [('''pub const DEFAULT_INDEX_SUBDIR: &str = "index";''', '''#[cfg(unix)]
#[path = "search_prefetch.rs"]
mod prefetch;
#[cfg(unix)]
pub(crate) use prefetch::warm_prefetch_lexical;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";''')])

update('src/daemon/server.rs', [
('''#![cfg(unix)]
''', '''#![cfg(unix)]

#[path = "cass_prefetch_worker.rs"]
mod cass_prefetch_worker;
use cass_prefetch_worker::CassPrefetchWorker;
'''),
('''    AgentScope, CassPrefetchCoordinator, PrefetchGeneration, RecencyWeightedFrequencyPredictor,
''', '''    AgentScope, CassPrefetchCoordinator, GatedPrediction, PrefetchGeneration,
    RecencyWeightedFrequencyPredictor,
'''),
('''    cass_prefetch: Arc<Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>>,
''', '''    cass_prefetch: Arc<Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>>,
    /// One idle, bounded, read-only warm batch per server; absent for cold mode
    /// and for direct dispatches which have no supervised server lifetime.
    cass_prefetch_worker: Option<CassPrefetchWorker>,
'''),
('''    let pool_in_thread = Arc::clone(&pool);

    // Host the long-lived write-owner actor''', '''    let pool_in_thread = Arc::clone(&pool);
    // A cloned policy is configuration, not ownership of another daemon's
    // observations, shutdown flag or speculative single-flight slot.
    dispatch_policy.cass_prefetch = Arc::new(Mutex::new(CassPrefetchCoordinator::new()));
    dispatch_policy.cass_prefetch_worker = should_warm.then(|| {
        CassPrefetchWorker::new(Arc::clone(&shutdown), Arc::clone(&pool))
    });

    // Host the long-lived write-owner actor'''),
('''            policy.cass_prefetch(),
            defer_advisory_until_socket_write,
''', '''            policy.cass_prefetch(),
            policy.cass_prefetch_worker.as_ref(),
            defer_advisory_until_socket_write,
'''),
('''    cass_prefetch: &Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>,
    defer_advisory_until_socket_write: bool,
''', '''    cass_prefetch: &Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>,
    prefetch_worker: Option<&CassPrefetchWorker>,
    defer_advisory_until_socket_write: bool,
'''),
('''    let prefetch_degraded = observe_and_schedule_cass_prefetch(
        cass_prefetch,
        shutdown,
        &request.agent_id,
        &advisory_workspace_id,
        &params.query,
        context_response
            .data
            .slo
            .as_ref()
            .and_then(|slo| slo.actuals.index_generation),
    );''', '''    let prefetch_generation = context_response
        .data
        .slo
        .as_ref()
        .and_then(|slo| slo.actuals.index_generation);
    let prefetch_prediction = plan_and_observe_cass_prefetch(
        cass_prefetch,
        shutdown,
        &request.agent_id,
        &advisory_workspace_id,
        &params.query,
        prefetch_generation,
    );'''),
('''    if let Some(code) = prefetch_degraded {
''', '''    if let Some(code) = prefetch_prediction.degraded {
'''),
('''    pending_delivery.finish(response, defer_advisory_until_socket_write)
}

/// Post-success CASS prefetch observation''', '''    // Submit only after successful rendering and size/deadline checks. This
    // never waits for warming, modifies the response or awards evidence-use
    // hit/miss credit. Semantic-only requests do not opt into lexical work.
    if params.source_mode != SearchSourceMode::SemanticOnly
        && let Some((worker, generation)) = prefetch_worker.zip(prefetch_generation)
    {
        worker.submit(
            crate::config::workspace::resolve_store_index_dir(
                &options.workspace_path,
                options.database_path.as_deref(),
                options.index_dir.as_deref(),
            ),
            generation,
            prefetch_prediction.candidates,
        );
    }
    pending_delivery.finish(response, defer_advisory_until_socket_write)
}

/// Post-success CASS prefetch observation'''),
('''fn observe_and_schedule_cass_prefetch(
    coordinator: &Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>,
    shutdown: &AtomicBool,
    agent_id: &str,
    workspace_id: &str,
    topic: &str,
    index_generation: Option<u64>,
) -> Option<&'static str> {
    // Speculative work is the first thing to go when the daemon is stopping.
    if shutdown.load(Ordering::SeqCst) {
        return None;
    }
    // Fail closed rather than stamping a placeholder generation.
    let generation = PrefetchGeneration::new(0, index_generation?);''', '''fn plan_and_observe_cass_prefetch(
    coordinator: &Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>,
    shutdown: &AtomicBool,
    agent_id: &str,
    workspace_id: &str,
    topic: &str,
    index_generation: Option<u64>,
) -> GatedPrediction {
    // Speculative work is the first thing to go when the daemon is stopping.
    if shutdown.load(Ordering::SeqCst) {
        return GatedPrediction::default();
    }
    // Fail closed rather than stamping a placeholder generation.
    let Some(index_generation) = index_generation else {
        return GatedPrediction::default();
    };
    let generation = PrefetchGeneration::new(0, index_generation);'''),
('''    // `CassPrefetchHistoryStore::observe` re-stamps the WHOLE history with the
    // generation it is handed, so observing first would overwrite the
    // generation the earlier requests were actually measured against. The
    // gate would then always compare a generation against itself and the
    // stale-generation path could never fire — the invalidation this bead
    // exists to enforce would be dead code.
''', '''    // Observe restarts changed-generation/corpus windows. Scheduling first
    // still matters: report the stale-history refusal instead of silently
    // replacing it with an empty prediction against the fresh window.
'''),
('''    let degraded = prefetch
        .schedule(&agent_scope, workspace_id, generation, corpus_revision)
        .degraded;
''', '''    let prediction = prefetch
        .schedule(&agent_scope, workspace_id, generation, corpus_revision);
'''),
('''    degraded
}

fn attach_daemon_context_search_advisories_for_delivery(''', '''    prediction
}

// Keep the existing envelope/isolation regressions on their original seam.
#[cfg(test)]
fn observe_and_schedule_cass_prefetch(
    coordinator: &Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>,
    shutdown: &AtomicBool,
    agent_id: &str,
    workspace_id: &str,
    topic: &str,
    index_generation: Option<u64>,
) -> Option<&'static str> {
    plan_and_observe_cass_prefetch(
        coordinator, shutdown, agent_id, workspace_id, topic, index_generation,
    ).degraded
}

fn attach_daemon_context_search_advisories_for_delivery('''),
])
