use std::path::Path;
use std::process::Command;

#[test]
fn g4_health_structural_script_passes_with_cargo_built_binary() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = repo_root.join("scripts/e2e_overhaul/g4_health_structural.sh");
    let ee_binary = env!("CARGO_BIN_EXE_ee");
    let log_path = std::env::temp_dir().join(format!(
        "ee-g4-health-structural-e2e-{}.jsonl",
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
    assert!(
        output.status.success(),
        "g4 structural health e2e script failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    );

    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", log_path.display()));
    assert!(
        log.contains("g4_health_structural_schema"),
        "structured log should include the schema assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
    assert!(
        log.contains("g4_health_structural_incoherent_cluster_identified"),
        "structured log should include the contradiction-cluster assertion; log path: {}\nlog:\n{}",
        log_path.display(),
        log
    );
}
