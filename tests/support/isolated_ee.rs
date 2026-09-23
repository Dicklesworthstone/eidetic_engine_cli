//! bd-rvrj2: the one way a test should spawn the `ee` binary.
//!
//! `ee` resolves its data dir from `XDG_DATA_HOME`, else `HOME/.local/share`,
//! and keeps the embedding-model cache, the global store and the catalog there.
//! A spawn that inherits the runner's environment therefore reads whatever
//! state the worker happens to hold, and the same test can give a different
//! verdict on a different host (measured on bd-rvrj2: the same worker and
//! binary emit different degradation codes with and without host state).
//!
//! `isolated_ee_command` gives every spawn its own HOME and XDG dirs under a
//! caller-owned root, turns model downloads off, and drops the workspace
//! selectors, so the verdict depends only on what the test itself set up.
//! Tests that need the real semantic model must opt in explicitly (bd-rvrj2
//! step 3); they must not rely on a model the host happens to cache.
//!
//! The guard in `tests/ee_spawn_isolation_contract.rs` counts spawns that do
//! not isolate and holds them to a shrink-only baseline.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// Build a `Command` for the `ee` binary whose data, config, cache and state
/// dirs live under `root` (created if absent). Callers add `--workspace` and
/// arguments as usual.
pub fn isolated_ee_command(root: &Path) -> Result<Command, String> {
    let home = root.join("home");
    let data = root.join("xdg-data");
    let config = root.join("xdg-config");
    let cache = root.join("xdg-cache");
    let state = root.join("xdg-state");
    for dir in [&home, &data, &config, &cache, &state] {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("create isolated ee dir {}: {error}", dir.display()))?;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_STATE_HOME", &state)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_EMBED_MODEL_DIR");
    Ok(command)
}
