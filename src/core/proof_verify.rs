//! Proof artifact discovery and check result modeling for `ee verify proofs`.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;

pub const PROOF_CHECK_SCHEMA_V1: &str = "ee.proof_check.v1";

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
                degraded.push("degraded.proof_tool_missing".to_owned());
            } else if status == ProofCheckStatus::Violation {
                degraded.push("degraded.proof_violation_detected".to_owned());
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

fn collect_proof_artifacts(
    dir: &Path,
    kind: ProofArtifactKind,
    artifacts: &mut Vec<ProofArtifact>,
) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if !is_proof_artifact(&path, kind) {
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
    let body = fs::read_to_string(path)?;
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
            vec!["degraded.proof_tool_missing".to_owned()]
        );
        assert!(report.checks.iter().all(|check| {
            check.status == ProofCheckStatus::ToolMissing && !check.artifact.invariants.is_empty()
        }));
    }
}
