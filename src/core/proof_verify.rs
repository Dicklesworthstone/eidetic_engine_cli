//! Proof artifact discovery and check result modeling for `ee verify proofs`.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;

pub const PROOF_CHECK_SCHEMA_V1: &str = "ee.proof_check.v1";
pub const PROOF_TOOL_MISSING_CODE: &str = "proof_tool_missing";
pub const PROOF_VIOLATION_DETECTED_CODE: &str = "proof_violation_detected";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProofArtifactKind {
    Lean4,
    #[serde(rename = "tla+")]
    TlaPlus,
}

impl ProofArtifactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lean4 => "lean4",
            Self::TlaPlus => "tla+",
        }
    }

    #[must_use]
    pub const fn default_tool(self) -> &'static str {
        match self {
            Self::Lean4 => "lake",
            Self::TlaPlus => "tlc",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofCheckStatus {
    Proved,
    ModelChecked,
    Violation,
    ToolMissing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofArtifact {
    pub path: PathBuf,
    pub kind: ProofArtifactKind,
    pub invariants: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofCheck {
    pub artifact: ProofArtifact,
    pub command: Vec<String>,
    pub duration_ms: u64,
    pub status: ProofCheckStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofCheckReport {
    pub schema: &'static str,
    pub success: bool,
    pub checks: Vec<ProofCheck>,
    pub degraded: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofCommandOutcome {
    pub tool_available: bool,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait ProofCommandRunner {
    fn run(&self, artifact: &ProofArtifact) -> ProofCommandOutcome;
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
pub struct SystemProofCommandRunner;

impl ProofCommandRunner for SystemProofCommandRunner {
    fn run(&self, artifact: &ProofArtifact) -> ProofCommandOutcome {
        if !tool_is_available(artifact.kind.default_tool()) {
            return ProofCommandOutcome {
                tool_available: false,
                duration_ms: 0,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("{} not found on PATH", artifact.kind.default_tool()),
            };
        }

        let started = Instant::now();
        let output = match artifact.kind {
            ProofArtifactKind::Lean4 => Command::new("lake")
                .arg("build")
                .current_dir(artifact.path.parent().unwrap_or_else(|| Path::new(".")))
                .output(),
            ProofArtifactKind::TlaPlus => Command::new("tlc")
                .arg("-workers")
                .arg("8")
                .arg(&artifact.path)
                .output(),
        };
        match output {
            Ok(output) => ProofCommandOutcome {
                tool_available: true,
                duration_ms: started.elapsed().as_millis() as u64,
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(error) => ProofCommandOutcome {
                tool_available: false,
                duration_ms: started.elapsed().as_millis() as u64,
                exit_code: None,
                stdout: String::new(),
                stderr: error.to_string(),
            },
        }
    }
}

pub fn discover_proof_artifacts(proofs_root: &Path) -> io::Result<Vec<ProofArtifact>> {
    let mut artifacts = Vec::new();
    collect_proof_artifacts(
        &proofs_root.join("lean4"),
        ProofArtifactKind::Lean4,
        &mut artifacts,
    )?;
    collect_proof_artifacts(
        &proofs_root.join("tla"),
        ProofArtifactKind::TlaPlus,
        &mut artifacts,
    )?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(artifacts)
}

pub fn run_proof_checks(
    proofs_root: &Path,
    runner: &dyn ProofCommandRunner,
) -> io::Result<ProofCheckReport> {
    let artifacts = discover_proof_artifacts(proofs_root)?;
    let mut degraded = Vec::new();
    let checks = artifacts
        .into_iter()
        .map(|artifact| {
            let outcome = runner.run(&artifact);
            let status = classify_status(artifact.kind, &outcome);
            if status == ProofCheckStatus::ToolMissing {
                degraded.push(degraded_code(PROOF_TOOL_MISSING_CODE));
            } else if status == ProofCheckStatus::Violation {
                degraded.push(degraded_code(PROOF_VIOLATION_DETECTED_CODE));
            }
            ProofCheck {
                command: command_for_artifact(&artifact),
                artifact,
                duration_ms: outcome.duration_ms,
                status,
                exit_code: outcome.exit_code,
                stdout: outcome.stdout,
                stderr: outcome.stderr,
            }
        })
        .collect::<Vec<_>>();
    degraded.sort();
    degraded.dedup();
    Ok(ProofCheckReport {
        schema: PROOF_CHECK_SCHEMA_V1,
        success: checks.iter().all(|check| {
            matches!(
                check.status,
                ProofCheckStatus::Proved
                    | ProofCheckStatus::ModelChecked
                    | ProofCheckStatus::ToolMissing
            )
        }),
        checks,
        degraded,
    })
}

fn degraded_code(code: &str) -> String {
    format!("degraded.{code}")
}

fn collect_proof_artifacts(
    dir: &Path,
    kind: ProofArtifactKind,
    artifacts: &mut Vec<ProofArtifact>,
) -> io::Result<()> {
    ensure_no_proof_path_symlink_components(dir, "discover proof artifacts")?;
    match fs::symlink_metadata(dir) {
        Ok(metadata) if !metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        ensure_no_proof_path_symlink_components(&path, "discover proof artifact")?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || !is_proof_artifact(&path, kind) {
            continue;
        }
        artifacts.push(ProofArtifact {
            invariants: extract_invariants(&path)?,
            path,
            kind,
        });
    }
    Ok(())
}

fn is_proof_artifact(path: &Path, kind: ProofArtifactKind) -> bool {
    match kind {
        ProofArtifactKind::Lean4 => path.extension() == Some(OsStr::new("lean")),
        ProofArtifactKind::TlaPlus => path.extension() == Some(OsStr::new("tla")),
    }
}

fn extract_invariants(path: &Path) -> io::Result<Vec<String>> {
    ensure_no_proof_path_symlink_components(path, "read proof artifact")?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to read proof artifact `{}` because it is not a regular file",
                path.display()
            ),
        ));
    }
    let body = read_proof_artifact_file(path)?;
    let mut invariants = body
        .lines()
        .filter_map(|line| {
            line.split_once("invariant:")
                .map(|(_, invariant)| invariant.trim().to_owned())
        })
        .filter(|invariant| !invariant.is_empty())
        .collect::<Vec<_>>();
    invariants.sort();
    invariants.dedup();
    Ok(invariants)
}

fn read_proof_artifact_file(path: &Path) -> io::Result<String> {
    let mut file = open_proof_artifact_file_for_read(path)?;
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    Ok(body)
}

fn open_proof_artifact_file_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_proof_artifact_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_proof_artifact_open_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_proof_artifact_open_no_follow(_options: &mut OpenOptions) {}

fn ensure_no_proof_path_symlink_components(path: &Path, operation: &'static str) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to {operation} `{}` through symlinked path component `{}`",
                        path.display(),
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn classify_status(kind: ProofArtifactKind, outcome: &ProofCommandOutcome) -> ProofCheckStatus {
    if !outcome.tool_available {
        return ProofCheckStatus::ToolMissing;
    }
    if outcome.exit_code == Some(0) {
        return match kind {
            ProofArtifactKind::Lean4 => ProofCheckStatus::Proved,
            ProofArtifactKind::TlaPlus => ProofCheckStatus::ModelChecked,
        };
    }
    ProofCheckStatus::Violation
}

fn command_for_artifact(artifact: &ProofArtifact) -> Vec<String> {
    match artifact.kind {
        ProofArtifactKind::Lean4 => vec!["lake".to_owned(), "build".to_owned()],
        ProofArtifactKind::TlaPlus => vec![
            "tlc".to_owned(),
            "-workers".to_owned(),
            "8".to_owned(),
            artifact.path.to_string_lossy().into_owned(),
        ],
    }
}

#[allow(dead_code)]
fn tool_is_available(tool: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(tool).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    type TestResult = Result<(), String>;

    #[derive(Clone, Debug)]
    struct FixedRunner {
        outcome: ProofCommandOutcome,
    }

    impl ProofCommandRunner for FixedRunner {
        fn run(&self, _artifact: &ProofArtifact) -> ProofCommandOutcome {
            self.outcome.clone()
        }
    }

    #[test]
    fn status_lean_success_is_proved() {
        let status = classify_status(
            ProofArtifactKind::Lean4,
            &ProofCommandOutcome {
                tool_available: true,
                duration_ms: 7,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        assert_eq!(status, ProofCheckStatus::Proved);
    }

    #[test]
    fn status_tla_success_is_model_checked() {
        let status = classify_status(
            ProofArtifactKind::TlaPlus,
            &ProofCommandOutcome {
                tool_available: true,
                duration_ms: 7,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        assert_eq!(status, ProofCheckStatus::ModelChecked);
    }

    #[test]
    fn status_missing_tool_is_degraded_not_failure() {
        let status = classify_status(
            ProofArtifactKind::Lean4,
            &ProofCommandOutcome {
                tool_available: false,
                duration_ms: 0,
                exit_code: None,
                stdout: String::new(),
                stderr: "lake not found on PATH".to_owned(),
            },
        );
        assert_eq!(status, ProofCheckStatus::ToolMissing);
    }

    #[test]
    fn status_nonzero_exit_is_violation() {
        let status = classify_status(
            ProofArtifactKind::TlaPlus,
            &ProofCommandOutcome {
                tool_available: true,
                duration_ms: 7,
                exit_code: Some(12),
                stdout: String::new(),
                stderr: "Temporal property violated".to_owned(),
            },
        );
        assert_eq!(status, ProofCheckStatus::Violation);
    }

    #[test]
    fn report_tool_missing_keeps_success_true_with_degraded_code() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs");
        let report = run_proof_checks(
            &root,
            &FixedRunner {
                outcome: ProofCommandOutcome {
                    tool_available: false,
                    duration_ms: 0,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: "tool missing".to_owned(),
                },
            },
        )
        .expect("committed proof artifacts should be discoverable");
        assert!(report.success);
        assert_eq!(report.schema, PROOF_CHECK_SCHEMA_V1);
        assert_eq!(
            report.degraded,
            vec![degraded_code(PROOF_TOOL_MISSING_CODE)]
        );
        assert!(report.checks.iter().all(|check| {
            check.status == ProofCheckStatus::ToolMissing && !check.artifact.invariants.is_empty()
        }));
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_symlinked_proof_artifact() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let lean_dir = temp.path().join("lean4");
        fs::create_dir_all(&lean_dir).map_err(|error| error.to_string())?;
        let outside_proof = temp.path().join("outside.lean");
        fs::write(&outside_proof, "-- invariant: outside proof\n")
            .map_err(|error| error.to_string())?;
        symlink(&outside_proof, lean_dir.join("linked.lean")).map_err(|error| error.to_string())?;

        let error = discover_proof_artifacts(temp.path())
            .expect_err("symlinked proof artifact should be rejected")
            .to_string();
        if error.contains("symlinked path component") {
            Ok(())
        } else {
            Err(format!("unexpected symlink error: {error}"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_symlinked_proof_root() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_lean_dir = temp.path().join("real-lean4");
        fs::create_dir_all(&real_lean_dir).map_err(|error| error.to_string())?;
        fs::write(
            real_lean_dir.join("proof.lean"),
            "-- invariant: hidden through root symlink\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(&real_lean_dir, temp.path().join("lean4")).map_err(|error| error.to_string())?;

        let error = discover_proof_artifacts(temp.path())
            .expect_err("symlinked proof root should be rejected")
            .to_string();
        if error.contains("symlinked path component") {
            Ok(())
        } else {
            Err(format!("unexpected symlink error: {error}"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn proof_artifact_final_read_open_rejects_symlinked_path() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_proof = temp.path().join("outside.lean");
        fs::write(&outside_proof, "-- invariant: outside proof\n")
            .map_err(|error| error.to_string())?;
        let linked_proof = temp.path().join("linked.lean");
        symlink(&outside_proof, &linked_proof).map_err(|error| error.to_string())?;

        let error = open_proof_artifact_file_for_read(&linked_proof)
            .expect_err("final proof artifact read open must reject symlinks");

        assert_ne!(
            error.kind(),
            io::ErrorKind::NotFound,
            "final symlink read should fail because the path is a symlink"
        );
        assert_eq!(
            fs::read_to_string(&outside_proof).map_err(|error| error.to_string())?,
            "-- invariant: outside proof\n",
            "proof artifact read helper must not follow the symlink target"
        );
        Ok(())
    }

    #[test]
    fn invariant_extraction_rejects_non_regular_artifact_path() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_dir = temp.path().join("proof.lean");
        fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;

        let error = extract_invariants(&artifact_dir)
            .expect_err("non-regular proof artifact path should be rejected")
            .to_string();
        if error.contains("not a regular file") {
            Ok(())
        } else {
            Err(format!("unexpected non-regular artifact error: {error}"))
        }
    }
}
