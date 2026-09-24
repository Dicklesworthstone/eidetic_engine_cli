//! Pack request resolution must consume the setting before applying profile caps.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::profile::OperatingProfile;

fn options(workspace: &Path) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace.to_owned(),
        query: "candidate pool configuration".to_owned(),
        task_paths: Vec::new(),
        database_path: None,
        index_dir: None,
        speed: crate::search::SpeedMode::default(),
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: false,
        filters: crate::models::QueryFilters::default(),
        profile: None,
        max_tokens: Some(1000),
        candidate_pool: None,
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: crate::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: 0,
        task_lens: None,
        require_fresh_sentinels: false,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
    }
}

#[test]
fn candidate_pool_config_reaches_request_and_profile_caps_still_apply() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".ee")).unwrap();
    let config_path = temp.path().join(".ee/config.toml");
    fs::write(&config_path, "[pack]\ncandidate_pool = 7\n").unwrap();
    let mut opts = options(temp.path());
    let profile = RuntimeProfileReport::for_profile(OperatingProfile::Swarm, "pool-test");
    let run = context_request_from_options_with_runtime_profile(&opts, &profile).unwrap();
    assert_eq!(run.request.candidate_pool, 7);
    assert!(!run.candidate_pool_capped);
    opts.candidate_pool = Some(100);
    assert_eq!(context_request_from_options_with_runtime_profile(&opts, &profile).unwrap().request.candidate_pool, 100);
    opts.candidate_pool = Some(0);
    assert!(context_request_from_options_with_runtime_profile(&opts, &profile).is_err());
    opts.candidate_pool = None;
    fs::write(&config_path, format!("[pack]\ncandidate_pool = {}\n", u32::MAX)).unwrap();
    let run = context_request_from_options_with_runtime_profile(&opts, &profile).unwrap();
    assert!(run.candidate_pool_capped);
    assert_eq!(u64::from(run.request.candidate_pool), profile.budgets.pack.max_candidate_memories);
    assert_eq!(run.effective_candidate_pool, run.request.candidate_pool);
    // An edited configuration is observed by the next request, not cached as a default.
    fs::write(&config_path, "[pack]\ncandidate_pool = 11\n").unwrap();
    assert_eq!(context_request_from_options_with_runtime_profile(&opts, &profile).unwrap().request.candidate_pool, 11);
    fs::write(&config_path, "[pack]\ncandidate_pool = 4294967296\n").unwrap();
    assert!(context_request_from_options_with_runtime_profile(&opts, &profile).is_err());
}
