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
//! It also drops every inherited variable that selects an embedding model or
//! backend, so without an opt-in the verdict is the same whether or not the
//! host holds a model.
//!
//! Neural is opt-in (bd-rvrj2 A2): a test that needs the real semantic model
//! calls `isolated_ee_command_with_model` (or `model_fixture_root`, when it
//! lays the model out itself), which takes the model from the fixture named by
//! `EE_EMBED_MODEL_FIXTURE_DIR` and fails with a named reason when the runner
//! has not provisioned one. This file is the only test code that reads that
//! variable.
//!
//! The guard in `tests/ee_spawn_isolation_contract.rs` counts spawns that do
//! not isolate and holds them to a shrink-only baseline.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The opt-in to the real embedding model: a pre-provisioned cache root that
/// holds `potion-multilingual-128M/` (the layout CI builds in
/// `.ci/model-fixture`).
pub const MODEL_FIXTURE_ENV: &str = "EE_EMBED_MODEL_FIXTURE_DIR";

/// Files `isolated_ee_command_with_model` requires under the fixture, where
/// `EE_EMBED_MODEL_DIR` looks for them. `ee` verifies the full model manifest
/// itself when it loads.
pub const MODEL_FIXTURE_FILES: [&str; 2] = [
    "potion-multilingual-128M/model.safetensors",
    "potion-multilingual-128M/tokenizer.json",
];

/// Inherited variables that select an embedding model or backend. The
/// isolated command removes them; a caller may still set one explicitly
/// after building the command.
pub const MODEL_SELECTION_ENV: [&str; 9] = [
    "EE_EMBED_BACKEND",
    "EE_EMBED_MODEL_DIR",
    "EE_EMBED_MODEL_PATH",
    "EE_EMBED_REMOTE_URL",
    "EE_EMBED_REMOTE_API_KEY",
    "EE_EMBED_REMOTE_MODEL",
    "EE_EMBED_REMOTE_DIMENSION",
    MODEL_FIXTURE_ENV,
    "EE_RERANK_MODEL_FIXTURE_DIR",
];

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
        .env_remove("EE_WORKSPACE_REGISTRY");
    for key in MODEL_SELECTION_ENV {
        command.env_remove(key);
    }
    Ok(command)
}

/// The model fixture root named by `EE_EMBED_MODEL_FIXTURE_DIR`, for a test
/// that lays the model out itself. Calling this is how a test says it needs
/// the real model.
pub fn model_fixture_root() -> Result<PathBuf, String> {
    model_fixture_root_from(std::env::var_os(MODEL_FIXTURE_ENV).as_deref())
}

/// [`model_fixture_root`] with the variable's value passed in, so the opt-in
/// can be exercised without touching the process environment.
pub fn model_fixture_root_from(fixture: Option<&OsStr>) -> Result<PathBuf, String> {
    let fixture = fixture
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!(
                "this test needs the real embedding model: set {MODEL_FIXTURE_ENV} to a \
                 pre-provisioned potion-multilingual-128M fixture"
            )
        })?;
    if !fixture.is_dir() {
        return Err(format!(
            "{MODEL_FIXTURE_ENV}={} is not a directory",
            fixture.display()
        ));
    }
    Ok(fixture)
}

/// [`isolated_ee_command`] plus the real embedding model, taken from the
/// fixture named by `EE_EMBED_MODEL_FIXTURE_DIR`.
pub fn isolated_ee_command_with_model(root: &Path) -> Result<Command, String> {
    isolated_ee_command_with_model_from(root, std::env::var_os(MODEL_FIXTURE_ENV).as_deref())
}

/// [`isolated_ee_command_with_model`] with the variable's value passed in.
pub fn isolated_ee_command_with_model_from(
    root: &Path,
    fixture: Option<&OsStr>,
) -> Result<Command, String> {
    let fixture = model_fixture_root_from(fixture)?;
    for file in MODEL_FIXTURE_FILES {
        if !fixture.join(file).is_file() {
            return Err(format!(
                "{MODEL_FIXTURE_ENV}={} is missing {file}",
                fixture.display()
            ));
        }
    }
    let mut command = isolated_ee_command(root)?;
    command.env("EE_EMBED_MODEL_DIR", &fixture);
    Ok(command)
}
