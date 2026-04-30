//! Repro pack schema types (EE-368).
//!
//! A repro pack is a self-contained reproducibility bundle that captures
//! the environment, artifacts, and provenance needed to replay or verify
//! an executable claim. The pack structure is:
//!
//! ```text
//! repro-pack/
//!   env.json          # Environment configuration
//!   manifest.json     # Package manifest with artifact hashes
//!   repro.lock        # Deterministic lockfile for dependencies
//!   provenance.json   # Chain of custody and source attestations
//!   LEGAL.md          # Optional legal notice / license
//! ```
//!
//! All files except `LEGAL.md` are required for a valid repro pack.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Schema version for repro pack envelope.
pub const REPRO_PACK_SCHEMA_V1: &str = "ee.repro_pack.v1";

/// Schema version for repro pack env.json.
pub const REPRO_ENV_SCHEMA_V1: &str = "ee.repro_pack.env.v1";

/// Schema version for repro pack manifest.json.
pub const REPRO_MANIFEST_SCHEMA_V1: &str = "ee.repro_pack.manifest.v1";

/// Schema version for repro pack repro.lock.
pub const REPRO_LOCK_SCHEMA_V1: &str = "ee.repro_pack.lock.v1";

/// Schema version for repro pack provenance.json.
pub const REPRO_PROVENANCE_SCHEMA_V1: &str = "ee.repro_pack.provenance.v1";

/// Environment configuration for a repro pack (`env.json`).
///
/// Captures the runtime environment needed to reproduce a result,
/// including OS, toolchain versions, and relevant env vars.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReproEnv {
    /// Schema identifier.
    pub schema: &'static str,
    /// Operating system name and version.
    pub os: String,
    /// CPU architecture (e.g., "x86_64", "aarch64").
    pub arch: String,
    /// Rust toolchain version.
    pub rust_version: Option<String>,
    /// Git commit hash of the codebase.
    pub git_commit: Option<String>,
    /// Git branch name.
    pub git_branch: Option<String>,
    /// Whether the working directory was clean.
    pub git_clean: Option<bool>,
    /// Timestamp when the environment was captured (RFC 3339).
    pub captured_at: String,
    /// Sanitized environment variables relevant to reproduction.
    pub env_vars: HashMap<String, String>,
    /// Additional tool versions (e.g., "cargo", "ee").
    pub tool_versions: HashMap<String, String>,
}

impl ReproEnv {
    /// Create a new environment with required fields.
    #[must_use]
    pub fn new(
        os: impl Into<String>,
        arch: impl Into<String>,
        captured_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: REPRO_ENV_SCHEMA_V1,
            os: os.into(),
            arch: arch.into(),
            captured_at: captured_at.into(),
            ..Default::default()
        }
    }

    /// Set the Rust version.
    #[must_use]
    pub fn with_rust_version(mut self, version: impl Into<String>) -> Self {
        self.rust_version = Some(version.into());
        self
    }

    /// Set the git commit.
    #[must_use]
    pub fn with_git_commit(mut self, commit: impl Into<String>) -> Self {
        self.git_commit = Some(commit.into());
        self
    }

    /// Set the git branch.
    #[must_use]
    pub fn with_git_branch(mut self, branch: impl Into<String>) -> Self {
        self.git_branch = Some(branch.into());
        self
    }

    /// Set whether git working directory was clean.
    #[must_use]
    pub fn with_git_clean(mut self, clean: bool) -> Self {
        self.git_clean = Some(clean);
        self
    }

    /// Add an environment variable.
    #[must_use]
    pub fn with_env_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Add a tool version.
    #[must_use]
    pub fn with_tool_version(
        mut self,
        tool: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        self.tool_versions.insert(tool.into(), version.into());
        self
    }
}

/// Package manifest for a repro pack (`manifest.json`).
///
/// Lists all artifacts in the pack with their content hashes
/// for integrity verification.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReproManifest {
    /// Schema identifier.
    pub schema: &'static str,
    /// Human-readable pack name.
    pub name: String,
    /// Pack version (semver or date-based).
    pub version: String,
    /// Brief description of what this pack reproduces.
    pub description: Option<String>,
    /// Claim ID this pack is evidence for.
    pub claim_id: Option<String>,
    /// Demo ID this pack supports.
    pub demo_id: Option<String>,
    /// List of artifact entries.
    pub artifacts: Vec<ReproArtifact>,
    /// Timestamp when manifest was created (RFC 3339).
    pub created_at: String,
    /// Blake3 hash of the entire pack (excluding this field).
    pub pack_hash: Option<String>,
}

impl ReproManifest {
    /// Create a new manifest.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: REPRO_MANIFEST_SCHEMA_V1,
            name: name.into(),
            version: version.into(),
            created_at: created_at.into(),
            ..Default::default()
        }
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the claim ID.
    #[must_use]
    pub fn with_claim_id(mut self, id: impl Into<String>) -> Self {
        self.claim_id = Some(id.into());
        self
    }

    /// Set the demo ID.
    #[must_use]
    pub fn with_demo_id(mut self, id: impl Into<String>) -> Self {
        self.demo_id = Some(id.into());
        self
    }

    /// Add an artifact.
    pub fn add_artifact(&mut self, artifact: ReproArtifact) {
        self.artifacts.push(artifact);
    }

    /// Set the pack hash.
    #[must_use]
    pub fn with_pack_hash(mut self, hash: impl Into<String>) -> Self {
        self.pack_hash = Some(hash.into());
        self
    }
}

/// An artifact entry in a repro pack manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReproArtifact {
    /// Relative path within the pack.
    pub path: String,
    /// Content hash (blake3:... or sha256:...).
    pub hash: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// MIME type if known.
    pub content_type: Option<String>,
    /// Whether this artifact is required for reproduction.
    pub required: bool,
}

impl ReproArtifact {
    /// Create a new artifact entry.
    #[must_use]
    pub fn new(path: impl Into<String>, hash: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            path: path.into(),
            hash: hash.into(),
            size_bytes,
            content_type: None,
            required: true,
        }
    }

    /// Set the content type.
    #[must_use]
    pub fn with_content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }

    /// Set whether this artifact is required.
    #[must_use]
    pub fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

/// Deterministic lockfile for a repro pack (`repro.lock`).
///
/// Pins exact versions of all dependencies to ensure
/// byte-for-byte reproducibility.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReproLock {
    /// Schema identifier.
    pub schema: &'static str,
    /// Lock format version.
    pub lock_version: u32,
    /// Timestamp when lock was created (RFC 3339).
    pub locked_at: String,
    /// Pinned dependency entries.
    pub dependencies: Vec<ReproDependency>,
    /// Hash of the lockfile content (for integrity).
    pub content_hash: Option<String>,
}

impl ReproLock {
    /// Create a new lockfile.
    #[must_use]
    pub fn new(locked_at: impl Into<String>) -> Self {
        Self {
            schema: REPRO_LOCK_SCHEMA_V1,
            lock_version: 1,
            locked_at: locked_at.into(),
            ..Default::default()
        }
    }

    /// Add a dependency.
    pub fn add_dependency(&mut self, dep: ReproDependency) {
        self.dependencies.push(dep);
    }

    /// Set the content hash.
    #[must_use]
    pub fn with_content_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }
}

/// A pinned dependency in a repro lockfile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReproDependency {
    /// Dependency name (e.g., crate name, package name).
    pub name: String,
    /// Exact pinned version.
    pub version: String,
    /// Source (registry, git URL, path).
    pub source: String,
    /// Content hash of the dependency.
    pub hash: Option<String>,
    /// Dependency category.
    pub category: DependencyCategory,
}

impl ReproDependency {
    /// Create a new dependency.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        source: impl Into<String>,
        category: DependencyCategory,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            source: source.into(),
            hash: None,
            category,
        }
    }

    /// Set the content hash.
    #[must_use]
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.hash = Some(hash.into());
        self
    }
}

/// Category of a repro dependency.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum DependencyCategory {
    /// Rust crate from crates.io or git.
    #[default]
    Crate,
    /// System package (apt, brew, etc.).
    System,
    /// Tool binary (cargo, rustc, etc.).
    Tool,
    /// Data file or model.
    Data,
    /// Custom/other dependency.
    Other,
}

impl DependencyCategory {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Crate => "crate",
            Self::System => "system",
            Self::Tool => "tool",
            Self::Data => "data",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for DependencyCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DependencyCategory {
    type Err = ParseDependencyCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "crate" => Ok(Self::Crate),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            "data" => Ok(Self::Data),
            "other" => Ok(Self::Other),
            _ => Err(ParseDependencyCategoryError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a dependency category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseDependencyCategoryError {
    input: String,
}

impl fmt::Display for ParseDependencyCategoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown dependency category `{}`; expected crate, system, tool, data, or other",
            self.input
        )
    }
}

impl std::error::Error for ParseDependencyCategoryError {}

/// Provenance record for a repro pack (`provenance.json`).
///
/// Tracks the chain of custody, source attestations, and
/// verification history for the pack.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReproProvenance {
    /// Schema identifier.
    pub schema: &'static str,
    /// Trace ID for this provenance chain.
    pub trace_id: Option<String>,
    /// Evidence ID if this is attached to evidence.
    pub evidence_id: Option<String>,
    /// Source attestations.
    pub sources: Vec<ProvenanceSource>,
    /// Build/capture events.
    pub events: Vec<ProvenanceEvent>,
    /// Verification records.
    pub verifications: Vec<ProvenanceVerification>,
    /// Timestamp of last update (RFC 3339).
    pub updated_at: String,
}

impl ReproProvenance {
    /// Create a new provenance record.
    #[must_use]
    pub fn new(updated_at: impl Into<String>) -> Self {
        Self {
            schema: REPRO_PROVENANCE_SCHEMA_V1,
            updated_at: updated_at.into(),
            ..Default::default()
        }
    }

    /// Set the trace ID.
    #[must_use]
    pub fn with_trace_id(mut self, id: impl Into<String>) -> Self {
        self.trace_id = Some(id.into());
        self
    }

    /// Set the evidence ID.
    #[must_use]
    pub fn with_evidence_id(mut self, id: impl Into<String>) -> Self {
        self.evidence_id = Some(id.into());
        self
    }

    /// Add a source attestation.
    pub fn add_source(&mut self, source: ProvenanceSource) {
        self.sources.push(source);
    }

    /// Add an event.
    pub fn add_event(&mut self, event: ProvenanceEvent) {
        self.events.push(event);
    }

    /// Add a verification record.
    pub fn add_verification(&mut self, verification: ProvenanceVerification) {
        self.verifications.push(verification);
    }
}

/// A source attestation in provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceSource {
    /// Source URI (file://, git://, https://).
    pub uri: String,
    /// Content hash at the time of capture.
    pub hash: String,
    /// Timestamp of capture (RFC 3339).
    pub captured_at: String,
    /// Who or what captured this source.
    pub captured_by: Option<String>,
}

impl ProvenanceSource {
    /// Create a new source attestation.
    #[must_use]
    pub fn new(
        uri: impl Into<String>,
        hash: impl Into<String>,
        captured_at: impl Into<String>,
    ) -> Self {
        Self {
            uri: uri.into(),
            hash: hash.into(),
            captured_at: captured_at.into(),
            captured_by: None,
        }
    }

    /// Set who captured this source.
    #[must_use]
    pub fn with_captured_by(mut self, by: impl Into<String>) -> Self {
        self.captured_by = Some(by.into());
        self
    }
}

/// An event in the provenance chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceEvent {
    /// Event type (build, test, capture, etc.).
    pub event_type: ProvenanceEventType,
    /// Timestamp (RFC 3339).
    pub timestamp: String,
    /// Actor (human, agent, CI system).
    pub actor: Option<String>,
    /// Brief description.
    pub description: Option<String>,
    /// Input hashes consumed.
    pub inputs: Vec<String>,
    /// Output hashes produced.
    pub outputs: Vec<String>,
}

impl ProvenanceEvent {
    /// Create a new event.
    #[must_use]
    pub fn new(event_type: ProvenanceEventType, timestamp: impl Into<String>) -> Self {
        Self {
            event_type,
            timestamp: timestamp.into(),
            actor: None,
            description: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Set the actor.
    #[must_use]
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Add an input hash.
    pub fn add_input(&mut self, hash: impl Into<String>) {
        self.inputs.push(hash.into());
    }

    /// Add an output hash.
    pub fn add_output(&mut self, hash: impl Into<String>) {
        self.outputs.push(hash.into());
    }
}

/// Type of provenance event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProvenanceEventType {
    /// Source code was captured.
    Capture,
    /// Build was performed.
    Build,
    /// Tests were executed.
    Test,
    /// Pack was created.
    Pack,
    /// Pack was signed.
    Sign,
    /// Pack was published.
    Publish,
    /// Custom event type.
    Custom,
}

impl ProvenanceEventType {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Build => "build",
            Self::Test => "test",
            Self::Pack => "pack",
            Self::Sign => "sign",
            Self::Publish => "publish",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for ProvenanceEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProvenanceEventType {
    type Err = ParseProvenanceEventTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "capture" => Ok(Self::Capture),
            "build" => Ok(Self::Build),
            "test" => Ok(Self::Test),
            "pack" => Ok(Self::Pack),
            "sign" => Ok(Self::Sign),
            "publish" => Ok(Self::Publish),
            "custom" => Ok(Self::Custom),
            _ => Err(ParseProvenanceEventTypeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error parsing a provenance event type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseProvenanceEventTypeError {
    input: String,
}

impl fmt::Display for ParseProvenanceEventTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown provenance event type `{}`; expected capture, build, test, pack, sign, publish, or custom",
            self.input
        )
    }
}

impl std::error::Error for ParseProvenanceEventTypeError {}

/// A verification record in provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceVerification {
    /// Who verified (human, agent, CI).
    pub verifier: String,
    /// Timestamp of verification (RFC 3339).
    pub verified_at: String,
    /// Whether verification passed.
    pub passed: bool,
    /// Verification method used.
    pub method: Option<String>,
    /// Notes or failure reason.
    pub notes: Option<String>,
}

impl ProvenanceVerification {
    /// Create a new verification record.
    #[must_use]
    pub fn new(verifier: impl Into<String>, verified_at: impl Into<String>, passed: bool) -> Self {
        Self {
            verifier: verifier.into(),
            verified_at: verified_at.into(),
            passed,
            method: None,
            notes: None,
        }
    }

    /// Set the verification method.
    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    /// Set notes.
    #[must_use]
    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn repro_schema_versions_are_stable() -> TestResult {
        ensure(REPRO_PACK_SCHEMA_V1, "ee.repro_pack.v1", "pack")?;
        ensure(REPRO_ENV_SCHEMA_V1, "ee.repro_pack.env.v1", "env")?;
        ensure(
            REPRO_MANIFEST_SCHEMA_V1,
            "ee.repro_pack.manifest.v1",
            "manifest",
        )?;
        ensure(REPRO_LOCK_SCHEMA_V1, "ee.repro_pack.lock.v1", "lock")?;
        ensure(
            REPRO_PROVENANCE_SCHEMA_V1,
            "ee.repro_pack.provenance.v1",
            "provenance",
        )
    }

    #[test]
    fn repro_env_builder() -> TestResult {
        let env = ReproEnv::new("Linux 6.1", "x86_64", "2026-04-30T12:00:00Z")
            .with_rust_version("1.80.0")
            .with_git_commit("abc123")
            .with_git_branch("main")
            .with_git_clean(true)
            .with_env_var("CARGO_HOME", "/home/user/.cargo")
            .with_tool_version("cargo", "1.80.0");

        ensure(env.schema, REPRO_ENV_SCHEMA_V1, "schema")?;
        ensure(env.os, "Linux 6.1".to_string(), "os")?;
        ensure(env.arch, "x86_64".to_string(), "arch")?;
        ensure(env.rust_version, Some("1.80.0".to_string()), "rust")?;
        ensure(env.git_commit, Some("abc123".to_string()), "commit")?;
        ensure(env.git_branch, Some("main".to_string()), "branch")?;
        ensure(env.git_clean, Some(true), "clean")?;
        ensure(
            env.env_vars.get("CARGO_HOME"),
            Some(&"/home/user/.cargo".to_string()),
            "env_var",
        )?;
        ensure(
            env.tool_versions.get("cargo"),
            Some(&"1.80.0".to_string()),
            "tool",
        )
    }

    #[test]
    fn repro_manifest_builder() -> TestResult {
        let mut manifest = ReproManifest::new("test-pack", "1.0.0", "2026-04-30T12:00:00Z")
            .with_description("Test repro pack")
            .with_claim_id("claim_00000000000000000000000001")
            .with_demo_id("demo_00000000000000000000000001")
            .with_pack_hash("blake3:abc123");

        manifest.add_artifact(ReproArtifact::new("env.json", "blake3:env123", 256));

        ensure(manifest.schema, REPRO_MANIFEST_SCHEMA_V1, "schema")?;
        ensure(manifest.name, "test-pack".to_string(), "name")?;
        ensure(manifest.version, "1.0.0".to_string(), "version")?;
        ensure(manifest.artifacts.len(), 1, "artifact count")
    }

    #[test]
    fn repro_artifact_builder() -> TestResult {
        let artifact = ReproArtifact::new("data.json", "sha256:abc", 1024)
            .with_content_type("application/json")
            .with_required(false);

        ensure(artifact.path, "data.json".to_string(), "path")?;
        ensure(artifact.hash, "sha256:abc".to_string(), "hash")?;
        ensure(artifact.size_bytes, 1024, "size")?;
        ensure(
            artifact.content_type,
            Some("application/json".to_string()),
            "content_type",
        )?;
        ensure(artifact.required, false, "required")
    }

    #[test]
    fn repro_lock_builder() -> TestResult {
        let mut lock = ReproLock::new("2026-04-30T12:00:00Z").with_content_hash("blake3:lock123");

        lock.add_dependency(ReproDependency::new(
            "serde",
            "1.0.200",
            "crates.io",
            DependencyCategory::Crate,
        ));

        ensure(lock.schema, REPRO_LOCK_SCHEMA_V1, "schema")?;
        ensure(lock.lock_version, 1, "version")?;
        ensure(lock.dependencies.len(), 1, "dep count")
    }

    #[test]
    fn dependency_category_strings_are_stable() -> TestResult {
        ensure(DependencyCategory::Crate.as_str(), "crate", "crate")?;
        ensure(DependencyCategory::System.as_str(), "system", "system")?;
        ensure(DependencyCategory::Tool.as_str(), "tool", "tool")?;
        ensure(DependencyCategory::Data.as_str(), "data", "data")?;
        ensure(DependencyCategory::Other.as_str(), "other", "other")
    }

    #[test]
    fn dependency_category_round_trip() -> TestResult {
        for cat in [
            DependencyCategory::Crate,
            DependencyCategory::System,
            DependencyCategory::Tool,
            DependencyCategory::Data,
            DependencyCategory::Other,
        ] {
            let parsed = DependencyCategory::from_str(cat.as_str());
            ensure(parsed, Ok(cat), cat.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn dependency_category_rejects_invalid() {
        let result = DependencyCategory::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn repro_provenance_builder() -> TestResult {
        let mut prov = ReproProvenance::new("2026-04-30T12:00:00Z")
            .with_trace_id("trace_00000000000000000000000001")
            .with_evidence_id("ev_00000000000000000000000001");

        prov.add_source(ProvenanceSource::new(
            "git://github.com/user/repo",
            "blake3:src123",
            "2026-04-30T11:00:00Z",
        ));
        prov.add_event(ProvenanceEvent::new(
            ProvenanceEventType::Build,
            "2026-04-30T11:30:00Z",
        ));
        prov.add_verification(ProvenanceVerification::new(
            "ci-agent",
            "2026-04-30T12:00:00Z",
            true,
        ));

        ensure(prov.schema, REPRO_PROVENANCE_SCHEMA_V1, "schema")?;
        ensure(prov.sources.len(), 1, "source count")?;
        ensure(prov.events.len(), 1, "event count")?;
        ensure(prov.verifications.len(), 1, "verification count")
    }

    #[test]
    fn provenance_event_type_strings_are_stable() -> TestResult {
        ensure(ProvenanceEventType::Capture.as_str(), "capture", "capture")?;
        ensure(ProvenanceEventType::Build.as_str(), "build", "build")?;
        ensure(ProvenanceEventType::Test.as_str(), "test", "test")?;
        ensure(ProvenanceEventType::Pack.as_str(), "pack", "pack")?;
        ensure(ProvenanceEventType::Sign.as_str(), "sign", "sign")?;
        ensure(ProvenanceEventType::Publish.as_str(), "publish", "publish")?;
        ensure(ProvenanceEventType::Custom.as_str(), "custom", "custom")
    }

    #[test]
    fn provenance_event_type_round_trip() -> TestResult {
        for et in [
            ProvenanceEventType::Capture,
            ProvenanceEventType::Build,
            ProvenanceEventType::Test,
            ProvenanceEventType::Pack,
            ProvenanceEventType::Sign,
            ProvenanceEventType::Publish,
            ProvenanceEventType::Custom,
        ] {
            let parsed = ProvenanceEventType::from_str(et.as_str());
            ensure(parsed, Ok(et), et.as_str())?;
        }
        Ok(())
    }

    #[test]
    fn provenance_event_type_rejects_invalid() {
        let result = ProvenanceEventType::from_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn provenance_source_builder() -> TestResult {
        let source =
            ProvenanceSource::new("file://AGENTS.md", "blake3:abc", "2026-04-30T12:00:00Z")
                .with_captured_by("ee-cli");

        ensure(source.uri, "file://AGENTS.md".to_string(), "uri")?;
        ensure(
            source.captured_by,
            Some("ee-cli".to_string()),
            "captured_by",
        )
    }

    #[test]
    fn provenance_event_builder() -> TestResult {
        let mut event = ProvenanceEvent::new(ProvenanceEventType::Build, "2026-04-30T12:00:00Z")
            .with_actor("ci-agent")
            .with_description("Release build");
        event.add_input("blake3:in1");
        event.add_output("blake3:out1");

        ensure(event.event_type, ProvenanceEventType::Build, "type")?;
        ensure(event.actor, Some("ci-agent".to_string()), "actor")?;
        ensure(event.inputs.len(), 1, "inputs")?;
        ensure(event.outputs.len(), 1, "outputs")
    }

    #[test]
    fn provenance_verification_builder() -> TestResult {
        let verification = ProvenanceVerification::new("human", "2026-04-30T12:00:00Z", true)
            .with_method("manual review")
            .with_notes("Looks good");

        ensure(verification.verifier, "human".to_string(), "verifier")?;
        ensure(verification.passed, true, "passed")?;
        ensure(
            verification.method,
            Some("manual review".to_string()),
            "method",
        )?;
        ensure(verification.notes, Some("Looks good".to_string()), "notes")
    }
}
