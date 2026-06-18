#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use ee::config::EnvVar;
use ee::core::agent_docs::env_var_entries;

type TestResult = Result<(), String>;

const REPRESENTATIVE_RECENT_ENV_VARS: &[&str] = &[
    "EE_ADAPTIVE_BACKOFF_MS",
    "EE_DAEMON_MAX_INFLIGHT",
    "EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB",
    "EE_MAX_OUTPUT_TOKENS",
    "EE_MCP_MAX_REQUEST_BYTES",
    "EE_MESH_DISCOVERY_CACHE_TTL_SECONDS",
    "EE_REFLECTION_REQUEST_TTL_SECONDS",
    "EE_SERVE_TOKEN",
    "EE_TAILSCALE_DISCOVERY_BUDGET_MS",
    "EE_WRITE_GROUP_COMMIT_ENABLED",
];

#[test]
fn registry_includes_recent_runtime_ee_env_surface() -> TestResult {
    let registered = EnvVar::all()
        .iter()
        .map(|var| var.name())
        .collect::<BTreeSet<_>>();
    for &expected in REPRESENTATIVE_RECENT_ENV_VARS {
        if !registered.contains(expected) {
            return Err(format!("recent env var missing from registry: {expected}"));
        }
    }
    Ok(())
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
    let documented = env_var_entries()
        .iter()
        .filter_map(|entry| entry.name.starts_with("EE_").then_some(entry.name))
        .collect::<Vec<_>>();
    let expected = EnvVar::all()
        .iter()
        .map(|var| var.name())
        .collect::<Vec<_>>();
    if documented == expected {
        Ok(())
    } else {
        Err(format!(
            "agent docs EE_* env table drifted\nexpected: {expected:?}\nactual:   {documented:?}"
        ))
    }
}
