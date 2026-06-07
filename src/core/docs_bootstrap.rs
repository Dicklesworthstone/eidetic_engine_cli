//! Deterministic docs-to-memory bootstrap compiler substrate.
//!
//! This module intentionally stops at safe source discovery and run/candidate
//! modeling. Later bootstrap leaves add structural extraction and curation
//! persistence on top of this no-mutation foundation.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde::Serialize;

pub const DOCS_BOOTSTRAP_RUN_SCHEMA_V1: &str = "ee.bootstrap.docs.run.v1";
pub const DOCS_BOOTSTRAP_PARSER_VERSION: &str = "docs-bootstrap-v1";
pub const DOCS_BOOTSTRAP_DEFAULT_MAX_SOURCE_BYTES: u64 = 512 * 1024;
pub const DOCS_BOOTSTRAP_DEFAULT_MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapSourceKind {
    RootPolicy,
    Readme,
    Adr,
    Schema,
    EnvVars,
    FailureModeFixture,
}

impl BootstrapSourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RootPolicy => "root_policy",
            Self::Readme => "readme",
            Self::Adr => "adr",
            Self::Schema => "schema",
            Self::EnvVars => "env_vars",
            Self::FailureModeFixture => "failure_mode_fixture",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapTrustClass {
    HumanExplicit,
    AgentAssertion,
}

impl BootstrapTrustClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HumanExplicit => "human_explicit",
            Self::AgentAssertion => "agent_assertion",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapRun {
    pub schema: &'static str,
    pub parser_version: &'static str,
    pub run_id: String,
    pub workspace_path: String,
    pub source_count: usize,
    pub source_bytes: u64,
    pub max_source_bytes: u64,
    pub max_total_bytes: u64,
    pub sources: Vec<BootstrapSourceDocument>,
    pub candidates: Vec<BootstrapCandidate>,
    pub degraded: Vec<BootstrapDegradation>,
    pub durable_mutation: bool,
}

impl BootstrapRun {
    #[must_use]
    pub fn data_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|error| {
            serde_json::json!({
                "schema": "ee.error.v2",
                "error": {
                    "code": "serialization_failed",
                    "message": "Failed to serialize docs bootstrap run.",
                    "severity": "high",
                    "repair": "Fix the docs bootstrap serializer before exposing this command.",
                    "details": {
                        "serializerError": error.to_string(),
                    },
                },
            })
            .to_string()
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSourceDocument {
    pub relative_path: String,
    pub source_kind: &'static str,
    pub content_hash: String,
    pub byte_count: u64,
    pub line_count: usize,
    #[serde(skip_serializing)]
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapCandidate {
    pub candidate_id: String,
    pub source_path: String,
    pub source_hash: String,
    pub source_span: BootstrapSourceSpan,
    pub proposed_content: String,
    pub level: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub anchors: Vec<BootstrapAnchor>,
    pub specificity: u32,
    pub trust_class: &'static str,
    pub rationale: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSourceSpan {
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapAnchor {
    pub anchor_type: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CompileDocsBootstrapOptions<'a> {
    pub workspace_path: &'a Path,
    pub max_source_bytes: u64,
    pub max_total_bytes: u64,
}

impl<'a> CompileDocsBootstrapOptions<'a> {
    #[must_use]
    pub const fn for_workspace(workspace_path: &'a Path) -> Self {
        Self {
            workspace_path,
            max_source_bytes: DOCS_BOOTSTRAP_DEFAULT_MAX_SOURCE_BYTES,
            max_total_bytes: DOCS_BOOTSTRAP_DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AllowedSource {
    relative_path: String,
    kind: BootstrapSourceKind,
}

#[must_use]
pub fn compile_docs_bootstrap(options: &CompileDocsBootstrapOptions<'_>) -> BootstrapRun {
    let mut degraded = Vec::new();
    let mut sources = Vec::new();
    let mut total_bytes = 0_u64;

    for allowed in discover_allowed_sources(options.workspace_path, &mut degraded) {
        match read_allowed_source(options, &allowed, total_bytes) {
            SourceReadOutcome::Read(document) => {
                total_bytes = total_bytes.saturating_add(document.byte_count);
                sources.push(document);
            }
            SourceReadOutcome::Rejected(degradation) => degraded.push(degradation),
            SourceReadOutcome::TotalLimitReached(degradation) => {
                degraded.push(degradation);
                break;
            }
        }
    }

    let run_id = bootstrap_run_id(options.workspace_path, &sources, &degraded);
    BootstrapRun {
        schema: DOCS_BOOTSTRAP_RUN_SCHEMA_V1,
        parser_version: DOCS_BOOTSTRAP_PARSER_VERSION,
        run_id,
        workspace_path: options.workspace_path.display().to_string(),
        source_count: sources.len(),
        source_bytes: total_bytes,
        max_source_bytes: options.max_source_bytes,
        max_total_bytes: options.max_total_bytes,
        sources,
        candidates: Vec::new(),
        degraded,
        durable_mutation: false,
    }
}

enum SourceReadOutcome {
    Read(BootstrapSourceDocument),
    Rejected(BootstrapDegradation),
    TotalLimitReached(BootstrapDegradation),
}

fn discover_allowed_sources(
    workspace_path: &Path,
    degraded: &mut Vec<BootstrapDegradation>,
) -> Vec<AllowedSource> {
    let mut sources = vec![
        AllowedSource {
            relative_path: "AGENTS.md".to_owned(),
            kind: BootstrapSourceKind::RootPolicy,
        },
        AllowedSource {
            relative_path: "README.md".to_owned(),
            kind: BootstrapSourceKind::Readme,
        },
        AllowedSource {
            relative_path: "docs/env_vars.md".to_owned(),
            kind: BootstrapSourceKind::EnvVars,
        },
    ];

    extend_allowlisted_dir(
        workspace_path,
        "docs/adr",
        "md",
        BootstrapSourceKind::Adr,
        &mut sources,
        degraded,
    );
    extend_allowlisted_dir(
        workspace_path,
        "docs/schemas",
        "json",
        BootstrapSourceKind::Schema,
        &mut sources,
        degraded,
    );
    extend_allowlisted_dir(
        workspace_path,
        "tests/fixtures/failure_modes",
        "json",
        BootstrapSourceKind::FailureModeFixture,
        &mut sources,
        degraded,
    );

    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    sources.dedup_by(|left, right| left.relative_path == right.relative_path);
    sources
}

fn extend_allowlisted_dir(
    workspace_path: &Path,
    relative_dir: &str,
    extension: &str,
    kind: BootstrapSourceKind,
    sources: &mut Vec<AllowedSource>,
    degraded: &mut Vec<BootstrapDegradation>,
) {
    let dir_path = workspace_path.join(relative_dir);
    let Ok(metadata) = fs::symlink_metadata(&dir_path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        degraded.push(degradation(
            "docs_bootstrap_symlink_rejected",
            "medium",
            format!("Rejected allowlisted docs directory symlink `{relative_dir}`."),
            "Replace the symlink with real files inside the workspace before bootstrapping docs.",
            Some(relative_dir),
        ));
        return;
    }
    if !metadata.is_dir() {
        degraded.push(degradation(
            "docs_bootstrap_source_not_file",
            "low",
            format!("Allowlisted docs path `{relative_dir}` is not a directory."),
            "Use the documented docs bootstrap allowlist paths.",
            Some(relative_dir),
        ));
        return;
    }

    let entries = match fs::read_dir(&dir_path) {
        Ok(entries) => entries,
        Err(error) => {
            degraded.push(degradation(
                "docs_bootstrap_read_dir_failed",
                "low",
                format!("Could not list allowlisted docs directory `{relative_dir}`: {error}."),
                "Fix directory permissions and retry `ee bootstrap docs --dry-run`.",
                Some(relative_dir),
            ));
            return;
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            degraded.push(degradation(
                "docs_bootstrap_read_dir_entry_failed",
                "low",
                format!("Could not inspect an entry under `{relative_dir}`."),
                "Fix directory permissions and retry `ee bootstrap docs --dry-run`.",
                Some(relative_dir),
            ));
            continue;
        };
        let path = entry.path();
        if !path_has_extension(&path, extension) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        sources.push(AllowedSource {
            relative_path: format!("{relative_dir}/{file_name}"),
            kind,
        });
    }
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|actual| actual.to_str())
        .is_some_and(|actual| actual == extension)
}

fn read_allowed_source(
    options: &CompileDocsBootstrapOptions<'_>,
    allowed: &AllowedSource,
    current_total_bytes: u64,
) -> SourceReadOutcome {
    let path = options.workspace_path.join(&allowed.relative_path);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SourceReadOutcome::Rejected(degradation(
                "docs_bootstrap_source_missing",
                "low",
                format!(
                    "Allowlisted docs source `{}` is missing.",
                    allowed.relative_path
                ),
                "Create the expected docs file or ignore this degraded bootstrap input.",
                Some(&allowed.relative_path),
            ));
        }
        Err(error) => {
            return SourceReadOutcome::Rejected(degradation(
                "docs_bootstrap_metadata_failed",
                "low",
                format!(
                    "Could not inspect allowlisted docs source `{}`: {error}.",
                    allowed.relative_path
                ),
                "Fix file permissions and retry `ee bootstrap docs --dry-run`.",
                Some(&allowed.relative_path),
            ));
        }
    };

    if metadata.file_type().is_symlink() {
        return SourceReadOutcome::Rejected(degradation(
            "docs_bootstrap_symlink_rejected",
            "medium",
            format!(
                "Rejected allowlisted docs source symlink `{}`.",
                allowed.relative_path
            ),
            "Replace the symlink with a real file inside the workspace before bootstrapping docs.",
            Some(&allowed.relative_path),
        ));
    }
    if !metadata.is_file() {
        return SourceReadOutcome::Rejected(degradation(
            "docs_bootstrap_source_not_file",
            "low",
            format!(
                "Allowlisted docs source `{}` is not a regular file.",
                allowed.relative_path
            ),
            "Use regular files for docs bootstrap inputs.",
            Some(&allowed.relative_path),
        ));
    }

    let byte_count = metadata.len();
    if byte_count > options.max_source_bytes {
        return SourceReadOutcome::Rejected(degradation(
            "docs_bootstrap_source_oversized",
            "medium",
            format!(
                "Rejected allowlisted docs source `{}` because it is {byte_count} bytes, above the {} byte per-source limit.",
                allowed.relative_path, options.max_source_bytes
            ),
            "Reduce the file size or raise the docs bootstrap source limit explicitly.",
            Some(&allowed.relative_path),
        ));
    }
    if current_total_bytes.saturating_add(byte_count) > options.max_total_bytes {
        return SourceReadOutcome::TotalLimitReached(degradation(
            "docs_bootstrap_total_limit_reached",
            "medium",
            format!(
                "Stopped docs bootstrap reads before `{}` because the run would exceed the {} byte total limit.",
                allowed.relative_path, options.max_total_bytes
            ),
            "Raise the docs bootstrap total limit or reduce allowlisted docs size.",
            Some(&allowed.relative_path),
        ));
    }

    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            return SourceReadOutcome::Rejected(degradation(
                "docs_bootstrap_open_failed",
                "low",
                format!(
                    "Could not open allowlisted docs source `{}`: {error}.",
                    allowed.relative_path
                ),
                "Fix file permissions and retry `ee bootstrap docs --dry-run`.",
                Some(&allowed.relative_path),
            ));
        }
    };
    let mut content = String::new();
    if let Err(error) = file.read_to_string(&mut content) {
        return SourceReadOutcome::Rejected(degradation(
            "docs_bootstrap_non_utf8",
            "medium",
            format!(
                "Rejected allowlisted docs source `{}` because it is not readable UTF-8: {error}.",
                allowed.relative_path
            ),
            "Convert the docs file to UTF-8 before bootstrapping.",
            Some(&allowed.relative_path),
        ));
    }

    SourceReadOutcome::Read(BootstrapSourceDocument {
        relative_path: allowed.relative_path.clone(),
        source_kind: allowed.kind.as_str(),
        content_hash: content_hash(content.as_bytes()),
        byte_count,
        line_count: content.lines().count(),
        content,
    })
}

fn bootstrap_run_id(
    workspace_path: &Path,
    sources: &[BootstrapSourceDocument],
    degraded: &[BootstrapDegradation],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOCS_BOOTSTRAP_PARSER_VERSION.as_bytes());
    hasher.update(b"\0workspace\0");
    hasher.update(workspace_path.display().to_string().as_bytes());
    for source in sources {
        hasher.update(b"\0source\0");
        hasher.update(source.relative_path.as_bytes());
        hasher.update(b"\0");
        hasher.update(source.content_hash.as_bytes());
        hasher.update(b"\0");
        hasher.update(source.byte_count.to_string().as_bytes());
    }
    for degradation in degraded {
        hasher.update(b"\0degraded\0");
        hasher.update(degradation.code.as_bytes());
        if let Some(path) = degradation.path.as_deref() {
            hasher.update(b"\0");
            hasher.update(path.as_bytes());
        }
    }
    let digest = hasher.finalize().to_hex().to_string();
    format!("bootrun_{}", &digest[..26])
}

fn content_hash(content: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(content).to_hex())
}

fn degradation(
    code: &str,
    severity: &'static str,
    message: String,
    repair: &str,
    path: Option<&str>,
) -> BootstrapDegradation {
    BootstrapDegradation {
        code: code.to_owned(),
        severity,
        message,
        repair: repair.to_owned(),
        path: path.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn write_file(root: &Path, relative_path: &str, content: &str) -> TestResult {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(path, content).map_err(|error| error.to_string())
    }

    fn fixture_workspace() -> Result<tempfile::TempDir, String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = tempdir.path();
        write_file(root, "AGENTS.md", "# Agent rules\n\nNever delete files.\n")?;
        write_file(root, "README.md", "# Project\n\nUse `ee pack`.\n")?;
        write_file(root, "docs/env_vars.md", "# Env\n\n`EE_TEST=1`\n")?;
        write_file(root, "docs/adr/0058-test.md", "# ADR 0058\n")?;
        write_file(
            root,
            "docs/schemas/ee.example.v1.json",
            r#"{"schema":"ee.example.v1"}"#,
        )?;
        write_file(
            root,
            "tests/fixtures/failure_modes/no_relevant_results.json",
            r#"{"code":"no_relevant_results"}"#,
        )?;
        write_file(root, "docs/private.md", "not allowlisted")?;
        Ok(tempdir)
    }

    #[test]
    fn docs_bootstrap_reads_only_allowlisted_sources_deterministically() -> TestResult {
        let tempdir = fixture_workspace()?;
        let options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());

        let first = compile_docs_bootstrap(&options);
        let replay = compile_docs_bootstrap(&options);

        assert_eq!(first.run_id, replay.run_id);
        assert_eq!(first.schema, DOCS_BOOTSTRAP_RUN_SCHEMA_V1);
        assert_eq!(first.parser_version, DOCS_BOOTSTRAP_PARSER_VERSION);
        assert!(!first.durable_mutation);
        assert!(first.candidates.is_empty());
        assert!(first.degraded.is_empty());
        assert_eq!(
            first
                .sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "AGENTS.md",
                "README.md",
                "docs/adr/0058-test.md",
                "docs/env_vars.md",
                "docs/schemas/ee.example.v1.json",
                "tests/fixtures/failure_modes/no_relevant_results.json",
            ]
        );
        assert!(
            first
                .sources
                .iter()
                .all(|source| source.content_hash.starts_with("blake3:"))
        );
        assert!(
            first
                .sources
                .iter()
                .all(|source| source.relative_path != "docs/private.md")
        );
        Ok(())
    }

    #[test]
    fn docs_bootstrap_rejects_oversized_sources() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_file(tempdir.path(), "AGENTS.md", "123456789")?;
        write_file(tempdir.path(), "README.md", "ok")?;
        let mut options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());
        options.max_source_bytes = 4;

        let run = compile_docs_bootstrap(&options);

        assert!(
            run.sources
                .iter()
                .all(|source| source.relative_path != "AGENTS.md")
        );
        assert!(
            run.sources
                .iter()
                .any(|source| source.relative_path == "README.md")
        );
        assert!(run.degraded.iter().any(|degradation| {
            degradation.code == "docs_bootstrap_source_oversized"
                && degradation.path.as_deref() == Some("AGENTS.md")
        }));
        Ok(())
    }

    #[test]
    fn docs_bootstrap_stops_at_total_byte_limit() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_file(tempdir.path(), "AGENTS.md", "1234")?;
        write_file(tempdir.path(), "README.md", "5678")?;
        let mut options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());
        options.max_total_bytes = 4;

        let run = compile_docs_bootstrap(&options);

        assert_eq!(run.sources.len(), 1);
        assert_eq!(run.sources[0].relative_path, "AGENTS.md");
        assert!(run.degraded.iter().any(|degradation| {
            degradation.code == "docs_bootstrap_total_limit_reached"
                && degradation.path.as_deref() == Some("README.md")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn docs_bootstrap_rejects_symlink_sources_without_following() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_file(tempdir.path(), "real_readme.md", "# real\n")?;
        std::os::unix::fs::symlink("real_readme.md", tempdir.path().join("README.md"))
            .map_err(|error| error.to_string())?;
        write_file(tempdir.path(), "AGENTS.md", "# rules\n")?;

        let run =
            compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));

        assert!(
            run.sources
                .iter()
                .all(|source| source.relative_path != "README.md")
        );
        assert!(run.degraded.iter().any(|degradation| {
            degradation.code == "docs_bootstrap_symlink_rejected"
                && degradation.path.as_deref() == Some("README.md")
        }));
        Ok(())
    }

    #[test]
    fn bootstrap_run_json_omits_raw_source_content() -> TestResult {
        let tempdir = fixture_workspace()?;
        let run =
            compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));

        let json = run.data_json();

        assert!(json.contains(DOCS_BOOTSTRAP_RUN_SCHEMA_V1));
        assert!(!json.contains("Never delete files."));
        assert!(!json.contains("not allowlisted"));
        Ok(())
    }
}
