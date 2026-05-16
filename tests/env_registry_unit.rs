#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use ee::config::EnvVar;
use ee::core::agent_docs::ENV_VARS;

type TestResult = Result<(), String>;

const EXPECTED_ENV_VARS: &[&str] = &[
    "EE_AGENT_NAME",
    "EE_AGENT_MODE",
    "EE_CASS_BINARY",
    "EE_DATABASE_PATH",
    "EE_DEMO_EVIDENCE_ROOT",
    "EE_DIAG_FORCE_CAPABILITY_GAP",
    "EE_DISABLE_TOON",
    "EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS",
    "EE_EXPERIMENTAL_TRIAD",
    "EE_FORMAT",
    "EE_GRAPH_WITNESSES_RETENTION_DAYS",
    "EE_HARMFUL_BURST_WINDOW_SECONDS",
    "EE_HARMFUL_PER_SOURCE_PER_HOUR",
    "EE_HOOK_MODE",
    "EE_INDEX_DIR",
    "EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS",
    "EE_JSON",
    "EE_L2_PACK_CACHE_BYTES",
    "EE_L2_PACK_CACHE_DIR",
    "EE_L2_PACK_CACHE_DISABLE",
    "EE_LOG_FORMAT",
    "EE_LOG_JSON",
    "EE_MAX_TOKENS",
    "EE_MESH_ENABLED",
    "EE_MESH_MODE",
    "EE_NO_COLOR",
    "EE_OUTPUT_FORMAT",
    "EE_PREFLIGHT_BYPASS_SECRET",
    "EE_PROFILE",
    "EE_PPR_CACHE_ENTRIES",
    "EE_READ_POOL_DISABLE_PIN",
    "EE_READ_POOL_ACQUIRE_TIMEOUT_MS",
    "EE_READ_POOL_IDLE_TIMEOUT_S",
    "EE_READ_POOL_MAX_PIN_SECONDS",
    "EE_READ_POOL_SIZE",
    "EE_REMEMBER_CURATION_SYNC_BUDGET_MS",
    "EE_SECURITY_PROFILE",
    "EE_SCIENCE_BACKEND_PATH",
    "EE_TEST_LOG_LEVEL",
    "EE_TEST_LOG_PATH",
    "EE_TEST_LOG_TEST_ID",
    "EE_TAILSCALE_BINARY_OVERRIDE",
    "EE_TAILSCALE_PROBE_TIMEOUT_MS",
    "EE_TAILSCALE_PROBE_SOCKET_OVERRIDE",
    "EE_WORKSPACE",
    "EE_WORKSPACE_CLOSE_DRAIN_TIMEOUT_S",
    "EE_WORKSPACE_REGISTRY",
];

#[test]
fn registry_lists_the_current_runtime_ee_env_surface() -> TestResult {
    let actual = EnvVar::all()
        .iter()
        .map(|var| var.name())
        .collect::<Vec<_>>();
    if actual == EXPECTED_ENV_VARS {
        Ok(())
    } else {
        Err(format!(
            "registered EE_* env surface drifted\nexpected: {EXPECTED_ENV_VARS:?}\nactual:   {actual:?}"
        ))
    }
}

#[test]
fn registry_entries_are_documentable_and_unique() -> TestResult {
    let mut names = BTreeSet::new();
    for var in EnvVar::all() {
        let name = var.name();
        if !name.starts_with("EE_") {
            return Err(format!("{name} does not start with EE_"));
        }
        if !names.insert(name) {
            return Err(format!("duplicate env var registered: {name}"));
        }
        if var.description().trim().is_empty() {
            return Err(format!("{name} is missing a description"));
        }
    }
    Ok(())
}

#[test]
fn registry_exposes_known_defaults_and_sensitive_markers() -> TestResult {
    let default = EnvVar::RememberCurationSyncBudgetMs
        .default_value()
        .ok_or_else(|| "missing curation sync budget default".to_string())?;
    if default != "50" {
        return Err(format!(
            "unexpected curation sync budget default: {default}"
        ));
    }
    let drain_default = EnvVar::WorkspaceCloseDrainTimeoutSeconds
        .default_value()
        .ok_or_else(|| "missing workspace close drain timeout default".to_string())?;
    if drain_default != "5" {
        return Err(format!(
            "unexpected workspace close drain timeout default: {drain_default}"
        ));
    }
    if EnvVar::PreflightBypassSecret.exposes_value() {
        return Err("preflight bypass secret must not expose values".to_string());
    }
    Ok(())
}

#[test]
fn agent_docs_env_table_tracks_registry() -> TestResult {
    let documented = ENV_VARS
        .iter()
        .filter_map(|entry| entry.name.starts_with("EE_").then_some(entry.name))
        .collect::<Vec<_>>();
    if documented == EXPECTED_ENV_VARS {
        Ok(())
    } else {
        Err(format!(
            "agent docs EE_* env table drifted\nexpected: {EXPECTED_ENV_VARS:?}\nactual:   {documented:?}"
        ))
    }
}
