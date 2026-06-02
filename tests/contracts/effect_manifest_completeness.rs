//! Contract coverage for the `EffectManifest` daemon-subcommand entries
//! (bd-30i43).
//!
//! `src/core/effect.rs::EffectManifest` is the agent-readable catalog of
//! blast radius for every CLI subcommand. Trauma-guard, agent harnesses,
//! and policy layers consult the manifest before invoking a command. The
//! `daemon start` and `daemon stop` subcommands (bd-oja31, dispatched at
//! `src/cli/mod.rs:41843-41847`) both mutate the filesystem — `start`
//! binds a UDS file at `$XDG_RUNTIME_DIR/ee/daemon.sock` (or
//! `$TMPDIR/ee-daemon.sock`) and `stop` `fs::remove_file()`s it — so
//! both belong in the manifest under the `durable_write` effect class
//! with an empty workspace-DB write surface (the socket lives outside
//! the workspace).
//!
//! This test pins both entries' existence and effect class so future
//! manifest reshuffles do not silently drop them.

use ee::core::effect::{EffectClass, EffectManifest};

type TestResult = Result<(), String>;

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn daemon_start_is_declared_durable_write() -> TestResult {
    let manifest = EffectManifest::build();
    let effect = manifest
        .get("daemon start")
        .ok_or_else(|| "`daemon start` must appear in EffectManifest".to_string())?;
    ensure_equal(
        &effect.default_effect,
        &EffectClass::DurableMemoryWrite,
        "`daemon start` is durable_write (binds UDS file outside the workspace)",
    )?;
    if !effect.write_surfaces.db_tables.is_empty() {
        return Err(format!(
            "`daemon start` writes a UDS socket, not workspace DB tables; \
             got db_tables = {:?}",
            effect.write_surfaces.db_tables
        ));
    }
    if !effect.description.contains("same-uid auth-required")
        || !effect.description.contains("workspace-bound methods")
    {
        return Err(format!(
            "`daemon start` manifest must document daemon RPC authorization; got {:?}",
            effect.description
        ));
    }
    Ok(())
}

#[test]
fn daemon_stop_is_declared_durable_write() -> TestResult {
    let manifest = EffectManifest::build();
    let effect = manifest
        .get("daemon stop")
        .ok_or_else(|| "`daemon stop` must appear in EffectManifest".to_string())?;
    ensure_equal(
        &effect.default_effect,
        &EffectClass::DurableMemoryWrite,
        "`daemon stop` is durable_write (removes UDS file)",
    )?;
    if !effect.write_surfaces.db_tables.is_empty() {
        return Err(format!(
            "`daemon stop` removes a UDS socket, not workspace DB tables; \
             got db_tables = {:?}",
            effect.write_surfaces.db_tables
        ));
    }
    Ok(())
}

#[test]
fn daemon_start_and_stop_entries_are_distinct() -> TestResult {
    let manifest = EffectManifest::build();
    let start = manifest
        .get("daemon start")
        .ok_or_else(|| "`daemon start` missing from manifest".to_string())?;
    let stop = manifest
        .get("daemon stop")
        .ok_or_else(|| "`daemon stop` missing from manifest".to_string())?;
    if start.command_path == stop.command_path {
        return Err(format!(
            "`daemon start` and `daemon stop` collapsed to the same command_path: {}",
            start.command_path
        ));
    }
    Ok(())
}
