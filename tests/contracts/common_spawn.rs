//! Shared real-binary (`CARGO_BIN_EXE_ee`) spawn hygiene for contracts tests.
//!
//! bd-7vtqm: under an unfiltered `cargo test --test contracts`, libtest runs
//! each module's tests on its own worker threads, and every real-binary
//! invocation competes for CPU with every other one. The resume bridges first
//! serialized their own spawns behind a file-local mutex
//! (bd-resume-verb-v0f57, commit 7f50e5c7); this module promotes that pattern
//! to one crate-wide lock so ALL direct binary spawns serialize as contention
//! hygiene. Per-module semantics are unchanged: callers keep their argument
//! shapes, environment tweaks, current_dir, and failure text; only the
//! serialization gate is shared.

use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard};

/// Crate-wide serialization gate for every real-binary spawn below.
static REAL_EE_SERIAL: Mutex<()> = Mutex::new(());

fn lock_real_ee_serial() -> MutexGuard<'static, ()> {
    // A panicked peer poisons the gate, but the lock guards scheduling
    // hygiene only; poisoned or not, later spawns must proceed.
    REAL_EE_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Path of the real `ee` binary provided by the Cargo test harness. This is
/// the only `CARGO_BIN_EXE_ee` reference allowed in `tests/contracts`.
pub fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

/// Spawn the real binary with `args` while holding the crate-wide
/// serialization lock. Failure text matches the conventional per-module
/// helpers: `failed to run ee <args joined by spaces>: <io error>`.
pub fn serialized_real_ee<I, S>(args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments: Vec<String> = args
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let _serial_guard = lock_real_ee_serial();
    Command::new(ee_binary())
        .args(&arguments)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", arguments.join(" ")))
}

/// Caller-configured variant: holds the crate-wide serialization lock while
/// `configure` mutates a fresh command pre-seeded with the real binary path.
/// Use this for workspace prefixes, env removals or overrides, `current_dir`,
/// or custom failure text; the io error is returned unmapped so each call
/// site keeps its original message.
pub fn serialized_real_ee_with<F>(configure: F) -> std::io::Result<Output>
where
    F: FnOnce(&mut Command),
{
    let _serial_guard = lock_real_ee_serial();
    let mut command = Command::new(ee_binary());
    configure(&mut command);
    command.output()
}
