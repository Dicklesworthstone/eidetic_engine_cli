#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn retained_scratch_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ee-typed-fields-decide-e2e-script-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn typed_fields_decide_e2e_script_exercises_public_cli() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ee_bin = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    let scratch = retained_scratch_dir();
    let log_dir = scratch.join("logs");
    fs::create_dir_all(&log_dir).expect("create retained typed-fields decide e2e log directory");

    let output = Command::new(repo.join("scripts/e2e_typed_fields_decide.sh"))
        .current_dir(&repo)
        .env("EE_BIN", &ee_bin)
        .env("EE_E2E_KEEP", "1")
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env("EE_E2E_TMPDIR", &scratch)
        .env("LOG_DIR", &log_dir)
        .env("EE_TEST_LOG_PATH", log_dir.join("events.jsonl"))
        .output()
        .expect("run scripts/e2e_typed_fields_decide.sh");

    if !output.status.success() {
        panic!(
            "scripts/e2e_typed_fields_decide.sh failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let summary_path = log_dir.join("summary.json");
    let summary_bytes = fs::read(&summary_path).unwrap_or_else(|error| {
        panic!(
            "read typed-fields decide e2e summary at {}: {error}",
            summary_path.display()
        )
    });
    let summary: serde_json::Value =
        serde_json::from_slice(&summary_bytes).expect("parse typed-fields decide e2e summary JSON");

    assert_eq!(summary["schema"], "ee.test_event.v1.summary");
    assert_eq!(summary["test"], "typed_fields_decide");
    assert_eq!(summary["verdict"], "PASS");
    assert!(
        summary["pass"].as_u64().unwrap_or(0) >= 30,
        "typed-fields decide e2e should record broad public-CLI assertions: {summary}"
    );
    assert_eq!(summary["fail"], 0);
}
