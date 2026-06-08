use std::path::Path;
use std::process::Command;

#[test]
fn sentinels_task_lens_and_typed_kinds_scripts_pass_with_cargo_built_binary() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ee_binary = env!("CARGO_BIN_EXE_ee");
    let tmp_root =
        std::env::temp_dir().join(format!("ee-e16-e17-e12-domain-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_root)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", tmp_root.display()));

    for script_name in [
        "scripts/e2e_sentinels.sh",
        "scripts/e2e_task_lens.sh",
        "scripts/e2e_typed_kinds.sh",
    ] {
        let script = repo_root.join(script_name);
        let log_dir = tmp_root.join(script_name.replace(['/', '.'], "_"));

        let output = Command::new("bash")
            .arg(&script)
            .current_dir(repo_root)
            .env("EE_BIN", ee_binary)
            .env("EE_BINARY", ee_binary)
            .env("EE_E2E_KEEP", "1")
            .env("EE_E2E_TMPDIR", &tmp_root)
            .env("LOG_DIR", &log_dir)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {}: {error}", script.display()));

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{} failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nlog_dir: {}",
            script_name,
            output.status.code(),
            stdout,
            stderr,
            log_dir.display()
        );
    }
}
