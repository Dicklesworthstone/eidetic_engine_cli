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
const BOOTSTRAP_COMMAND_PREFIXES: &[&str] = &[
    "br", "bv", "cargo", "cass", "ee", "gh", "git", "jq", "rch", "rustfmt",
];

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

    let candidates = extract_bootstrap_candidates(&sources);
    let run_id = bootstrap_run_id(options.workspace_path, &sources, &candidates, &degraded);
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
        candidates,
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

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    number: usize,
    start_byte: usize,
    end_byte: usize,
    text: &'a str,
}

struct StructuralCandidateInput<'a> {
    line: SourceLine<'a>,
    discriminator: &'a str,
    proposed_content: &'a str,
    level: &'a str,
    kind: &'a str,
    tags: Vec<String>,
    anchors: Vec<BootstrapAnchor>,
}

fn extract_bootstrap_candidates(sources: &[BootstrapSourceDocument]) -> Vec<BootstrapCandidate> {
    let mut candidates = Vec::new();
    for source in sources {
        extract_line_structures(source, &mut candidates);
        if source.source_kind == BootstrapSourceKind::FailureModeFixture.as_str() {
            extract_failure_mode_fixture_code(source, &mut candidates);
        }
    }
    candidates.sort_by(|left, right| {
        (
            left.source_path.as_str(),
            left.source_span.start_line,
            left.candidate_id.as_str(),
        )
            .cmp(&(
                right.source_path.as_str(),
                right.source_span.start_line,
                right.candidate_id.as_str(),
            ))
    });
    candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
    candidates
}

fn extract_line_structures(
    source: &BootstrapSourceDocument,
    candidates: &mut Vec<BootstrapCandidate>,
) {
    let mut in_fence = false;
    for line in source_lines(source.content.as_str()) {
        let trimmed = line.text.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }

        if in_fence && looks_like_command_line(trimmed) {
            push_structural_candidate(
                candidates,
                source,
                StructuralCandidateInput {
                    line,
                    discriminator: "fenced_command",
                    proposed_content: trimmed,
                    level: "procedural",
                    kind: "rule",
                    tags: vec!["bootstrap".to_owned(), "command".to_owned()],
                    anchors: vec![BootstrapAnchor {
                        anchor_type: "command".to_owned(),
                        value: first_token(trimmed),
                    }],
                },
            );
            continue;
        }

        if let Some(heading) = markdown_heading(trimmed) {
            push_structural_candidate(
                candidates,
                source,
                StructuralCandidateInput {
                    line,
                    discriminator: "heading",
                    proposed_content: heading,
                    level: "semantic",
                    kind: "fact",
                    tags: vec!["bootstrap".to_owned(), "heading".to_owned()],
                    anchors: vec![BootstrapAnchor {
                        anchor_type: "heading".to_owned(),
                        value: heading.to_owned(),
                    }],
                },
            );
        }

        if is_structural_table_row(trimmed) {
            push_structural_candidate(
                candidates,
                source,
                StructuralCandidateInput {
                    line,
                    discriminator: "table_row",
                    proposed_content: trimmed,
                    level: "semantic",
                    kind: "fact",
                    tags: vec!["bootstrap".to_owned(), "table".to_owned()],
                    anchors: vec![BootstrapAnchor {
                        anchor_type: "table_row".to_owned(),
                        value: source.relative_path.clone(),
                    }],
                },
            );
        }

        if is_explicit_policy_line(trimmed) {
            push_structural_candidate(
                candidates,
                source,
                StructuralCandidateInput {
                    line,
                    discriminator: "explicit_policy",
                    proposed_content: trimmed,
                    level: "procedural",
                    kind: "rule",
                    tags: vec!["bootstrap".to_owned(), "policy".to_owned()],
                    anchors: vec![BootstrapAnchor {
                        anchor_type: "policy_language".to_owned(),
                        value: policy_anchor(trimmed),
                    }],
                },
            );
        }

        for schema_id in structural_tokens(trimmed).into_iter().filter(|token| {
            is_schema_id(token.as_str()) && token.as_str() != DOCS_BOOTSTRAP_RUN_SCHEMA_V1
        }) {
            push_token_candidate(
                candidates,
                source,
                line,
                "schema_id",
                &schema_id,
                "schema_id",
            );
        }
        for env_var in structural_tokens(trimmed)
            .into_iter()
            .filter(|token| is_env_var(token.as_str()))
        {
            push_token_candidate(candidates, source, line, "env_var", &env_var, "env_var");
        }
        for degraded_code in structural_tokens(trimmed)
            .into_iter()
            .filter(|token| is_degraded_code_context(trimmed, token.as_str()))
        {
            push_token_candidate(
                candidates,
                source,
                line,
                "degraded_code",
                &degraded_code,
                "degraded_code",
            );
        }
    }
}

fn extract_failure_mode_fixture_code(
    source: &BootstrapSourceDocument,
    candidates: &mut Vec<BootstrapCandidate>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source.content.as_str()) else {
        return;
    };
    let Some(code) = value.get("code").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Some(line) = source_lines(source.content.as_str()).into_iter().next() else {
        return;
    };
    push_token_candidate(
        candidates,
        source,
        line,
        "degraded_code",
        code,
        "degraded_code",
    );
}

fn push_token_candidate(
    candidates: &mut Vec<BootstrapCandidate>,
    source: &BootstrapSourceDocument,
    line: SourceLine<'_>,
    discriminator: &str,
    token: &str,
    anchor_type: &str,
) {
    push_structural_candidate(
        candidates,
        source,
        StructuralCandidateInput {
            line,
            discriminator,
            proposed_content: token,
            level: "semantic",
            kind: "fact",
            tags: vec!["bootstrap".to_owned(), anchor_type.to_owned()],
            anchors: vec![BootstrapAnchor {
                anchor_type: anchor_type.to_owned(),
                value: token.to_owned(),
            }],
        },
    );
}

fn push_structural_candidate(
    candidates: &mut Vec<BootstrapCandidate>,
    source: &BootstrapSourceDocument,
    input: StructuralCandidateInput<'_>,
) {
    let source_span = BootstrapSourceSpan {
        start_line: input.line.number,
        end_line: input.line.number,
        start_byte: input.line.start_byte,
        end_byte: input.line.end_byte,
    };
    let candidate_id = bootstrap_candidate_id(
        source,
        &source_span,
        input.discriminator,
        input.proposed_content,
    );
    let specificity = candidate_specificity(input.proposed_content, input.anchors.as_slice());
    candidates.push(BootstrapCandidate {
        candidate_id,
        source_path: source.relative_path.clone(),
        source_hash: source.content_hash.clone(),
        source_span,
        proposed_content: input.proposed_content.to_owned(),
        level: input.level.to_owned(),
        kind: input.kind.to_owned(),
        tags: input.tags,
        anchors: input.anchors,
        specificity,
        trust_class: trust_class_for(source, input.discriminator).as_str(),
        rationale: format!(
            "Extracted explicit `{}` structure from allowlisted docs.",
            input.discriminator
        ),
    });
}

fn source_lines(content: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start_byte = 0_usize;
    for (index, segment) in content.split_inclusive('\n').enumerate() {
        let end_byte = start_byte.saturating_add(segment.len());
        lines.push(SourceLine {
            number: index + 1,
            start_byte,
            end_byte,
            text: segment.trim_end_matches(|character| matches!(character, '\r' | '\n')),
        });
        start_byte = end_byte;
    }
    if content.is_empty() {
        return lines;
    }
    if !content.ends_with('\n') && lines.is_empty() {
        lines.push(SourceLine {
            number: 1,
            start_byte: 0,
            end_byte: content.len(),
            text: content,
        });
    }
    lines
}

fn markdown_heading(trimmed: &str) -> Option<&str> {
    let hash_count = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if hash_count == 0 || hash_count > 6 {
        return None;
    }
    let rest = trimmed.get(hash_count..)?.trim();
    if rest.is_empty() { None } else { Some(rest) }
}

fn looks_like_command_line(trimmed: &str) -> bool {
    let command = first_token(trimmed);
    BOOTSTRAP_COMMAND_PREFIXES
        .iter()
        .any(|prefix| command == *prefix)
}

fn first_token(input: &str) -> String {
    input
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('$')
        .to_owned()
}

fn is_structural_table_row(trimmed: &str) -> bool {
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return false;
    }
    let body = trimmed.trim_matches('|').trim();
    !body.is_empty()
        && !body
            .chars()
            .all(|character| matches!(character, '-' | ':' | '|' | ' '))
}

fn is_explicit_policy_line(trimmed: &str) -> bool {
    let upper = trimmed.to_ascii_uppercase();
    upper.contains("MUST")
        || upper.contains("NEVER")
        || upper.contains("DO NOT")
        || upper.contains("FORBIDDEN")
}

fn policy_anchor(trimmed: &str) -> String {
    let upper = trimmed.to_ascii_uppercase();
    for marker in ["MUST", "NEVER", "DO NOT", "FORBIDDEN"] {
        if upper.contains(marker) {
            return marker.to_ascii_lowercase().replace(' ', "_");
        }
    }
    "policy".to_owned()
}

fn structural_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in input.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':' | '/') {
            current.push(character);
        } else if !current.is_empty() {
            tokens.push(normalize_structural_token(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(normalize_structural_token(&current));
    }
    tokens.retain(|token| !token.is_empty());
    tokens
}

fn normalize_structural_token(token: &str) -> String {
    token
        .trim_matches(|character: char| {
            matches!(
                character,
                '.' | ',' | ';' | ':' | ')' | '(' | ']' | '[' | '}' | '{' | '"' | '\''
            )
        })
        .to_owned()
}

fn is_schema_id(token: &str) -> bool {
    let Some((prefix, version)) = token.rsplit_once(".v") else {
        return false;
    };
    prefix.starts_with("ee.")
        && !prefix.trim().is_empty()
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_env_var(token: &str) -> bool {
    token.starts_with("EE_")
        && token.len() > 3
        && token
            .bytes()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

fn is_degraded_code_context(line: &str, token: &str) -> bool {
    if !token.contains('_') {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let context_mentions_codes =
        lower.contains("degraded") || lower.contains("failure") || lower.contains("code");
    context_mentions_codes
        && token
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn candidate_specificity(content: &str, anchors: &[BootstrapAnchor]) -> u32 {
    let token_bonus = content.split_whitespace().take(8).count() as u32 * 4;
    let anchor_bonus = anchors.len() as u32 * 12;
    40_u32
        .saturating_add(token_bonus)
        .saturating_add(anchor_bonus)
        .min(100)
}

fn trust_class_for(source: &BootstrapSourceDocument, discriminator: &str) -> BootstrapTrustClass {
    if matches!(
        source.source_kind,
        "root_policy" | "readme" | "adr" | "schema" | "env_vars"
    ) && matches!(
        discriminator,
        "explicit_policy" | "schema_id" | "env_var" | "heading" | "table_row"
    ) {
        BootstrapTrustClass::HumanExplicit
    } else {
        BootstrapTrustClass::AgentAssertion
    }
}

fn bootstrap_candidate_id(
    source: &BootstrapSourceDocument,
    span: &BootstrapSourceSpan,
    discriminator: &str,
    proposed_content: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOCS_BOOTSTRAP_PARSER_VERSION.as_bytes());
    hasher.update(b"\0candidate\0");
    hasher.update(source.relative_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(source.content_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(span.start_line.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(discriminator.as_bytes());
    hasher.update(b"\0");
    hasher.update(proposed_content.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("bootcand_{}", &digest[..26])
}

fn bootstrap_run_id(
    workspace_path: &Path,
    sources: &[BootstrapSourceDocument],
    candidates: &[BootstrapCandidate],
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
    for candidate in candidates {
        hasher.update(b"\0candidate\0");
        hasher.update(candidate.candidate_id.as_bytes());
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
        assert!(!first.candidates.is_empty());
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
        assert!(first.candidates.iter().any(|candidate| {
            candidate.proposed_content == "Never delete files."
                && candidate.anchors.iter().any(|anchor| {
                    anchor.anchor_type == "policy_language" && anchor.value == "never"
                })
        }));
        assert!(first.candidates.iter().any(|candidate| {
            candidate.proposed_content == "EE_TEST"
                && candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor.anchor_type == "env_var")
        }));
        assert!(first.candidates.iter().any(|candidate| {
            candidate.proposed_content == "ee.example.v1"
                && candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor.anchor_type == "schema_id")
        }));
        assert!(first.candidates.iter().any(|candidate| {
            candidate.proposed_content == "no_relevant_results"
                && candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor.anchor_type == "degraded_code")
        }));
        assert!(first.candidates.iter().all(|candidate| {
            candidate.source_hash.starts_with("blake3:") && candidate.source_span.start_line > 0
        }));
        Ok(())
    }

    #[test]
    fn docs_bootstrap_extracts_fenced_commands_and_tables_without_summarizing() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_file(
            tempdir.path(),
            "AGENTS.md",
            "# Agent rules\n\n```bash\ncargo check --all-targets\n```\n\n| Crate | Reason |\n| --- | --- |\n| tokio | forbidden runtime |\n",
        )?;
        write_file(tempdir.path(), "README.md", "# Readme\n")?;

        let run =
            compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));

        assert!(run.candidates.iter().any(|candidate| {
            candidate.proposed_content == "cargo check --all-targets"
                && candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor.anchor_type == "command" && anchor.value == "cargo")
        }));
        assert!(run.candidates.iter().any(|candidate| {
            candidate.proposed_content == "| tokio | forbidden runtime |"
                && candidate.tags.iter().any(|tag| tag == "table")
        }));
        assert!(
            run.candidates
                .iter()
                .all(|candidate| !candidate.rationale.contains("summary"))
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
        assert!(!json.contains("not allowlisted"));
        Ok(())
    }
}
