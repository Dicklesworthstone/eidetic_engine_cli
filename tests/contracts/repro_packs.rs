//! Gate 14: repro replay and minimization contracts.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::core::repro::{
    MinimizeOptions, MinimizeReport, ReplayOptions, ReplayReport, ReplayStatus, VerificationResult,
    minimize_pack, replay_pack,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("repro")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(actual == expected, format!("golden mismatch for {name}"))
}

fn write_repro_pack(pack_dir: &PathBuf) -> TestResult {
    fs::create_dir_all(pack_dir).map_err(|error| error.to_string())?;
    fs::write(
        pack_dir.join("manifest.json"),
        r#"{"schema":"ee.repro_pack.manifest.v1","name":"release_context_demo","version":"1.0.0","artifacts":[],"created_at":"2026-05-01T00:00:00Z"}"#,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        pack_dir.join("env.json"),
        r#"{"schema":"ee.repro_pack.env.v1","os":"linux","arch":"x86_64","captured_at":"2026-05-01T00:00:00Z","env_vars":{},"tool_versions":{"ee":"0.1.0"}}"#,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        pack_dir.join("repro.lock"),
        r#"{"schema":"ee.repro_pack.lock.v1","lock_version":1,"locked_at":"2026-05-01T00:00:00Z","dependencies":[]}"#,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        pack_dir.join("provenance.json"),
        r#"{"schema":"ee.repro_pack.provenance.v1","sources":[],"events":[],"verifications":[],"updated_at":"2026-05-01T00:00:00Z"}"#,
    )
    .map_err(|error| error.to_string())?;
    fs::write(pack_dir.join("LEGAL.md"), "fixture legal note\n").map_err(|error| error.to_string())
}

#[test]
fn gate14_replay_pack_verifies_required_files() -> TestResult {
    let pack_dir = env::temp_dir().join(format!("ee_gate14_repro_pack_{}", std::process::id()));
    write_repro_pack(&pack_dir)?;

    let report = replay_pack(&ReplayOptions {
        pack_path: pack_dir.clone(),
        work_dir: pack_dir,
        verify_hashes: true,
        check_env: false,
        dry_run: false,
    })
    .map_err(|error| error.message())?;

    ensure(
        report.status == ReplayStatus::Verified,
        "replay should verify",
    )?;
    ensure(
        report.artifacts_verified == 4,
        "all required files verified",
    )?;
    ensure(report.artifacts_failed == 0, "no required file failed")
}

#[test]
fn gate14_minimize_pack_preserves_required_files_and_marks_optional_removal() -> TestResult {
    let pack_dir = env::temp_dir().join(format!("ee_gate14_minimize_pack_{}", std::process::id()));
    write_repro_pack(&pack_dir)?;

    let report = minimize_pack(&MinimizeOptions {
        pack_path: pack_dir,
        output_dir: PathBuf::from("minimized-release-context-demo"),
        remove_optional: true,
        remove_binaries: true,
        max_file_size: Some(4096),
        dry_run: true,
    })
    .map_err(|error| error.message())?;

    ensure(report.artifacts_kept == 4, "required files are kept")?;
    ensure(
        report.artifacts_removed == 1,
        "optional legal file is removable",
    )?;
    ensure(
        report
            .removed_files
            .iter()
            .any(|file| file.path == "LEGAL.md"),
        "optional file removal is explicit",
    )
}

#[test]
fn gate14_replay_success_matches_golden() -> TestResult {
    let mut report = ReplayReport::new(
        PathBuf::from("repro/release_context_demo"),
        "release_context_demo".to_string(),
        "1.0.0".to_string(),
    );
    report.status = ReplayStatus::Verified;
    report.add_verification(VerificationResult {
        path: "manifest.json".to_string(),
        expected_hash: "blake3:manifest_hash".to_string(),
        actual_hash: Some("blake3:manifest_hash".to_string()),
        passed: true,
        error: None,
    });
    report.add_verification(VerificationResult {
        path: "stdout.json".to_string(),
        expected_hash: "blake3:stdout_hash".to_string(),
        actual_hash: Some("blake3:stdout_hash".to_string()),
        passed: true,
        error: None,
    });

    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())? + "\n";
    assert_golden("replay_success", &json)
}

#[test]
fn gate14_minimized_failure_matches_golden() -> TestResult {
    let mut report = MinimizeReport::new(
        PathBuf::from("repro/failing_release_context_demo"),
        PathBuf::from("repro/minimized_release_context_demo"),
    );
    report.add_kept(1024);
    report.add_kept(512);
    report.add_removed(ee::core::repro::RemovedFile {
        path: "trace/full-session.jsonl".to_string(),
        size_bytes: 8192,
        reason: "nonessential trace detail; minimized fixture preserves failure".to_string(),
    });

    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())? + "\n";
    assert_golden("minimized_failure", &json)
}
