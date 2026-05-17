use std::path::Path;
use std::process::Command;

#[test]
fn g5_curate_decay_script_passes_with_cargo_built_binary() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = repo_root.join("scripts/e2e_overhaul/g5_curate_decay.sh");
    let ee_binary = env!("CARGO_BIN_EXE_ee");
    let log_path = std::env::temp_dir().join(format!(
        "ee-g5-curate-decay-e2e-{}.jsonl",
        std::process::id()
    ));

    let output = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root)
        .env("EE_BINARY", ee_binary)
        .env("EE_E2E_KEEP_WORKSPACE", "1")
        .env("EE_TEST_LOG_PATH", &log_path)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", script.display()));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|error| format!("<failed to read {}: {error}>", log_path.display()));
    assert!(
        output.status.success(),
        "g5 curate decay e2e script failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nlog path: {}\nlog:\n{}",
        output.status.code(),
        stdout,
        stderr,
        log_path.display(),
        log
    );

    assert!(
        log.contains("g5_curate_decay_data_schema"),
        "structured log should include the disposition schema assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
    assert!(
        log.contains("g5_curate_decay_structural_adjustment_present"),
        "structured log should include the structural adjustment assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
    assert!(
        log.contains("g5_curate_decay_adjusted_decay"),
        "structured log should include the adjusted decay assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
    assert!(
        log.contains("g5_curate_decay_opt_out_structural_adjustments_absent"),
        "structured log should include the --no-structural-decay opt-out assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
}
