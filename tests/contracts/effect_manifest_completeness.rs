//! Contract coverage for the daemon command's `EffectManifest` entry
//! (bd-30i43, corrected by bd-d67os.21).
//!
//! `src/core/effect.rs::EffectManifest` is the agent-readable catalog of blast
//! radius for every CLI subcommand. Trauma-guard, agent harnesses, and policy
//! layers consult it via the **normalized** command path. `src/cli/mod.rs`
//! collapses *every* `Command::Daemon(_)` invocation (status, start, stop, …) to
//! the single path `"daemon"` (`NormalizedInvocation::extract_command_path`), so
//! the entry production actually looks up is `EffectManifest::get("daemon")`.
//!
//! The earlier revision of this test asserted `EffectManifest::get("daemon
//! start")` / `get("daemon stop")` — manifest-only documentation rows that CLI
//! normalization never produces — so it pinned paths the policy layers never
//! consult (and against a stale `durable_write`/empty-surface framing). These
//! tests instead pin (a) that daemon subcommands really normalize to `"daemon"`,
//! and (b) the live `"daemon"` entry's classification, keeping the contract
//! coupled to the path production uses.

use clap::Parser;
use ee::cli::{Cli, NormalizedInvocation};
use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};
use std::ffi::OsString;

type TestResult = Result<(), String>;

fn normalized_command_path(argv: &[&str]) -> Result<String, String> {
    let cli = Cli::try_parse_from(argv.iter().copied())
        .map_err(|error| format!("`{}` must parse: {error}", argv.join(" ")))?;
    let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
    Ok(NormalizedInvocation::from_cli(&cli, &args).command_path)
}

#[test]
fn daemon_subcommands_normalize_to_the_daemon_manifest_path() -> TestResult {
    // Every daemon subcommand collapses to the single normalized path "daemon"
    // (src/cli/mod.rs `Command::Daemon(_) => "daemon"`). This is the key the
    // policy / trauma-guard layers actually look up, so the manifest assertions
    // below must target it — not "daemon start" / "daemon stop".
    for argv in [
        ["ee", "daemon", "status"],
        ["ee", "daemon", "start"],
        ["ee", "daemon", "stop"],
    ] {
        let path = normalized_command_path(&argv)?;
        if path != "daemon" {
            return Err(format!(
                "`{}` must normalize to command path \"daemon\" (the manifest key policy \
                 layers consult); got {path:?}",
                argv.join(" ")
            ));
        }
    }
    Ok(())
}

#[test]
fn normalized_daemon_entry_is_external_io_mixed_with_real_write_surface() -> TestResult {
    // The entry production consults for any daemon invocation: external_io (UDS
    // lifecycle) with a Mixed side-effect class (status is read-only; some
    // foreground steward jobs mutate handler-owned DB state) and a NON-empty DB
    // write surface — the opposite of the empty-surface, durable_write framing
    // the previous test asserted against unreachable per-subcommand rows.
    let manifest = EffectManifest::build();
    let effect = manifest.get("daemon").ok_or_else(|| {
        "the normalized `daemon` command path must appear in EffectManifest".to_string()
    })?;
    if effect.default_effect != EffectClass::ExternalIo {
        return Err(format!(
            "`daemon` default effect must be external_io (UDS lifecycle); got {:?}",
            effect.default_effect
        ));
    }
    if effect.mutation_contract.side_effect_class != SideEffectClass::Mixed {
        return Err(format!(
            "`daemon` side-effect class must be Mixed (subcommand-specific); got {:?}",
            effect.mutation_contract.side_effect_class
        ));
    }
    if effect.write_surfaces.db_tables.is_empty() {
        return Err(
            "`daemon` must declare the DB write surface a foreground steward job can touch; \
             got empty db_tables"
                .to_string(),
        );
    }
    Ok(())
}

#[test]
fn daemon_start_stop_are_manifest_only_documentation_rows() -> TestResult {
    // `daemon start` / `daemon stop` exist in the manifest to document the UDS
    // socket lifecycle, but CLI normalization never produces these paths (proven
    // by `daemon_subcommands_normalize_to_the_daemon_manifest_path`), so policy
    // layers never look them up. Pin their actual `workspace_file_write` class so
    // the documentation rows can't silently drift, while making explicit that
    // they are NOT the consulted entry.
    let manifest = EffectManifest::build();
    for path in ["daemon start", "daemon stop"] {
        let effect = manifest
            .get(path)
            .ok_or_else(|| format!("manifest-only documentation row `{path}` must exist"))?;
        if effect.default_effect != EffectClass::WorkspaceFileWrite {
            return Err(format!(
                "`{path}` documents a UDS socket file write; expected workspace_file_write, \
                 got {:?}",
                effect.default_effect
            ));
        }
    }
    Ok(())
}
