//! Claim verification core services (EE-362).
//!
//! Provides the business logic for listing, showing, and verifying
//! executable claims defined in claims.yaml.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::models::{
    ArtifactType, CLAIMS_FILE_SCHEMA_V1, ClaimId, ClaimStatus, DemoId, EvidenceId,
    ManifestVerificationStatus, PolicyId, TraceId, VerificationFrequency, validate_artifact_entry,
    validate_manifest_structure,
};

pub const CLAIM_LIST_SCHEMA_V1: &str = "ee.claim_list.v1";
pub const CLAIM_SHOW_SCHEMA_V1: &str = "ee.claim_show.v1";
pub const CLAIM_VERIFY_SCHEMA_V1: &str = "ee.claim_verify.v1";
pub const DIAG_CLAIMS_SCHEMA_V1: &str = "ee.diag_claims.v1";

#[derive(Clone, Debug)]
pub struct ClaimSummary {
    pub id: String,
    pub title: String,
    pub status: ClaimStatus,
    pub frequency: VerificationFrequency,
    pub tags: Vec<String>,
    pub evidence_count: usize,
    pub demo_count: usize,
}

#[derive(Clone, Debug)]
pub struct ClaimListReport {
    pub schema: &'static str,
    pub claims_file: String,
    pub claims_file_exists: bool,
    pub total_count: usize,
    pub filtered_count: usize,
    pub claims: Vec<ClaimSummary>,
    pub filter_status: Option<String>,
    pub filter_frequency: Option<String>,
    pub filter_tag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimDetail {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: ClaimStatus,
    pub frequency: VerificationFrequency,
    pub policy_id: Option<String>,
    pub evidence_ids: Vec<String>,
    pub demo_ids: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ManifestDetail {
    pub claim_id: String,
    pub artifact_count: usize,
    pub last_verified_at: Option<String>,
    pub last_trace_id: Option<String>,
    pub verification_status: ManifestVerificationStatus,
}

#[derive(Clone, Debug)]
pub struct ClaimShowReport {
    pub schema: &'static str,
    pub claim_id: String,
    pub found: bool,
    pub claim: Option<ClaimDetail>,
    pub manifest: Option<ManifestDetail>,
    pub include_manifest: bool,
}

#[derive(Clone, Debug)]
pub struct ClaimVerifyResult {
    pub claim_id: String,
    pub status: ManifestVerificationStatus,
    pub artifacts_checked: usize,
    pub artifacts_passed: usize,
    pub artifacts_failed: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ClaimVerifyReport {
    pub schema: &'static str,
    pub claim_id: String,
    pub verify_all: bool,
    pub claims_file: String,
    pub artifacts_dir: String,
    pub total_claims: usize,
    pub verified_count: usize,
    pub failed_count: usize,
    pub skipped_count: usize,
    pub results: Vec<ClaimVerifyResult>,
    pub fail_fast: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedClaim {
    id: ClaimId,
    title: String,
    description: String,
    status: ClaimStatus,
    frequency: VerificationFrequency,
    policy_id: Option<PolicyId>,
    evidence_ids: Vec<EvidenceId>,
    demo_ids: Vec<DemoId>,
    tags: Vec<String>,
    evidence_count: usize,
    demo_count: usize,
}

impl ParsedClaim {
    fn summary(&self) -> ClaimSummary {
        ClaimSummary {
            id: self.id.to_string(),
            title: self.title.clone(),
            status: self.status,
            frequency: self.frequency,
            tags: self.tags.clone(),
            evidence_count: self.evidence_count,
            demo_count: self.demo_count,
        }
    }

    fn detail(&self) -> ClaimDetail {
        ClaimDetail {
            id: self.id.to_string(),
            title: self.title.clone(),
            description: self.description.clone(),
            status: self.status,
            frequency: self.frequency,
            policy_id: self.policy_id.map(|id| id.to_string()),
            evidence_ids: self.evidence_ids.iter().map(ToString::to_string).collect(),
            demo_ids: self.demo_ids.iter().map(ToString::to_string).collect(),
            tags: self.tags.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedManifest {
    claim_id: ClaimId,
    artifacts: Vec<ParsedManifestArtifact>,
    last_verified_at: Option<String>,
    last_trace_id: Option<TraceId>,
    verification_status: ManifestVerificationStatus,
}

impl ParsedManifest {
    fn detail(&self) -> ManifestDetail {
        ManifestDetail {
            claim_id: self.claim_id.to_string(),
            artifact_count: self.artifacts.len(),
            last_verified_at: self.last_verified_at.clone(),
            last_trace_id: self.last_trace_id.map(|id| id.to_string()),
            verification_status: self.verification_status,
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedManifestArtifact {
    path: String,
    _artifact_type: ArtifactType,
    blake3_hash: String,
    size_bytes: u64,
}

/// Options for building a file-backed `claim list` report.
#[derive(Clone, Debug, Default)]
pub struct ClaimListOptions {
    pub workspace_path: PathBuf,
    pub claims_file: Option<PathBuf>,
    pub status: Option<String>,
    pub frequency: Option<String>,
    pub tag: Option<String>,
}

/// Options for building a file-backed `claim show` report.
#[derive(Clone, Debug, Default)]
pub struct ClaimShowOptions {
    pub workspace_path: PathBuf,
    pub claims_file: Option<PathBuf>,
    pub artifacts_dir: Option<PathBuf>,
    pub claim_id: String,
    pub include_manifest: bool,
}

/// Options for building a file-backed `claim verify` report.
#[derive(Clone, Debug, Default)]
pub struct ClaimVerifyOptions {
    pub workspace_path: PathBuf,
    pub claims_file: Option<PathBuf>,
    pub artifacts_dir: Option<PathBuf>,
    pub claim_id: String,
    pub fail_fast: bool,
}

#[derive(Debug, Deserialize)]
struct RawClaimsFile {
    schema: Option<String>,
    version: Option<u32>,
    #[serde(default)]
    claims: Vec<RawClaimEntry>,
}

#[derive(Debug, Deserialize)]
struct RawClaimEntry {
    id: Option<String>,
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    frequency: Option<String>,
    #[serde(default, alias = "policyId")]
    policy_id: Option<String>,
    #[serde(default, alias = "evidenceIds")]
    evidence_ids: Vec<String>,
    #[serde(default, alias = "demoIds")]
    demo_ids: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawClaimManifest {
    schema: Option<String>,
    #[serde(default, alias = "claimId")]
    claim_id: Option<String>,
    #[serde(default)]
    artifacts: Option<Vec<RawManifestArtifact>>,
    #[serde(default, alias = "lastVerifiedAt")]
    last_verified_at: Option<String>,
    #[serde(default, alias = "lastTraceId")]
    last_trace_id: Option<String>,
    #[serde(default, alias = "verificationStatus")]
    verification_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawManifestArtifact {
    path: Option<String>,
    #[serde(default, alias = "artifactType")]
    artifact_type: Option<String>,
    #[serde(default, alias = "blake3Hash")]
    blake3_hash: Option<String>,
    #[serde(default, alias = "sizeBytes")]
    size_bytes: Option<u64>,
    #[serde(default, alias = "createdAt")]
    _created_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimParseError {
    pub message: String,
}

impl ClaimParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ClaimParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClaimParseError {}

fn claims_file_path(workspace_path: &Path, claims_file: Option<&Path>) -> PathBuf {
    claims_file.map_or_else(|| workspace_path.join("claims.yaml"), Path::to_path_buf)
}

fn artifacts_root_path(workspace_path: &Path, artifacts_dir: Option<&Path>) -> PathBuf {
    artifacts_dir.map_or_else(|| workspace_path.join("artifacts"), Path::to_path_buf)
}

fn claim_artifacts_dir(artifacts_root: &Path, claim_id: &ClaimId) -> PathBuf {
    let claim_id = claim_id.to_string();
    if artifacts_root.file_name().and_then(|name| name.to_str()) == Some(claim_id.as_str()) {
        artifacts_root.to_path_buf()
    } else {
        artifacts_root.join(claim_id)
    }
}

fn manifest_path_for_claim(artifacts_root: &Path, claim_id: &ClaimId) -> PathBuf {
    claim_artifacts_dir(artifacts_root, claim_id).join("manifest.json")
}

fn read_claims_file(path: &Path) -> Result<Vec<ParsedClaim>, ClaimParseError> {
    let input = fs::read_to_string(path).map_err(|error| {
        ClaimParseError::new(format!("failed to read {}: {error}", path.display()))
    })?;
    parse_claims_file_yaml(&input)
}

fn parse_claims_file_yaml(input: &str) -> Result<Vec<ParsedClaim>, ClaimParseError> {
    let raw: RawClaimsFile = serde_yaml::from_str(input)
        .map_err(|error| ClaimParseError::new(format!("failed to parse claims.yaml: {error}")))?;

    if let Some(schema) = raw.schema.as_deref() {
        if schema != CLAIMS_FILE_SCHEMA_V1 {
            return Err(ClaimParseError::new(format!(
                "unsupported claims schema `{schema}`; expected `{CLAIMS_FILE_SCHEMA_V1}`"
            )));
        }
    }

    if let Some(version) = raw.version {
        if version != 1 {
            return Err(ClaimParseError::new(format!(
                "unsupported claims manifest version `{version}`; expected `1`"
            )));
        }
    }

    let mut seen_ids = HashSet::new();
    let mut claims = Vec::with_capacity(raw.claims.len());
    for (claim_index, raw_claim) in raw.claims.into_iter().enumerate() {
        let claim = convert_raw_claim(claim_index, raw_claim)?;
        if !seen_ids.insert(claim.id) {
            return Err(ClaimParseError::new(format!(
                "duplicate claim id `{}` at claims[{claim_index}]",
                claim.id
            )));
        }
        claims.push(claim);
    }

    claims.sort_by_key(|claim| claim.id);
    Ok(claims)
}

fn convert_raw_claim(
    claim_index: usize,
    raw_claim: RawClaimEntry,
) -> Result<ParsedClaim, ClaimParseError> {
    let raw_id = required_claim_field(raw_claim.id, "claim id", claim_index)?;
    let id = raw_id.parse::<ClaimId>().map_err(|error| {
        ClaimParseError::new(format!(
            "invalid claim id `{raw_id}` at claims[{claim_index}]: {error}"
        ))
    })?;

    let title = required_claim_field(raw_claim.title, "claim title", claim_index)?;
    let description = raw_claim.description.unwrap_or_default();
    let status = parse_claim_status(raw_claim.status.as_deref(), claim_index)?;
    let frequency = parse_verification_frequency(raw_claim.frequency.as_deref(), claim_index)?;
    let policy_id = parse_optional_policy_id(raw_claim.policy_id.as_deref(), claim_index)?;
    let evidence_ids = parse_evidence_ids(&raw_claim.evidence_ids, claim_index)?;
    let demo_ids = parse_demo_ids(&raw_claim.demo_ids, claim_index)?;
    let mut tags = raw_claim.tags;
    tags.sort();
    tags.dedup();

    Ok(ParsedClaim {
        id,
        title,
        description,
        status,
        frequency,
        policy_id,
        evidence_count: evidence_ids.len(),
        demo_count: demo_ids.len(),
        evidence_ids,
        demo_ids,
        tags,
    })
}

fn required_claim_field(
    value: Option<String>,
    field_name: &str,
    claim_index: usize,
) -> Result<String, ClaimParseError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ClaimParseError::new(format!(
            "missing {field_name} at claims[{claim_index}]"
        ))),
    }
}

fn parse_claim_status(
    raw_status: Option<&str>,
    claim_index: usize,
) -> Result<ClaimStatus, ClaimParseError> {
    raw_status
        .unwrap_or(ClaimStatus::Draft.as_str())
        .parse::<ClaimStatus>()
        .map_err(|error| {
            ClaimParseError::new(format!(
                "invalid claim status at claims[{claim_index}]: {error}"
            ))
        })
}

fn parse_verification_frequency(
    raw_frequency: Option<&str>,
    claim_index: usize,
) -> Result<VerificationFrequency, ClaimParseError> {
    raw_frequency
        .unwrap_or(VerificationFrequency::OnChange.as_str())
        .parse::<VerificationFrequency>()
        .map_err(|error| {
            ClaimParseError::new(format!(
                "invalid verification frequency at claims[{claim_index}]: {error}"
            ))
        })
}

fn parse_optional_policy_id(
    raw_policy_id: Option<&str>,
    claim_index: usize,
) -> Result<Option<PolicyId>, ClaimParseError> {
    match raw_policy_id {
        Some(policy_id) => policy_id.parse::<PolicyId>().map(Some).map_err(|error| {
            ClaimParseError::new(format!(
                "invalid policy id `{policy_id}` at claims[{claim_index}]: {error}"
            ))
        }),
        None => Ok(None),
    }
}

fn parse_evidence_ids(
    ids: &[String],
    claim_index: usize,
) -> Result<Vec<EvidenceId>, ClaimParseError> {
    let mut parsed = Vec::with_capacity(ids.len());
    for (evidence_index, evidence_id) in ids.iter().enumerate() {
        parsed.push(evidence_id.parse::<EvidenceId>().map_err(|error| {
            ClaimParseError::new(format!(
                "invalid evidence id `{evidence_id}` at claims[{claim_index}].evidenceIds[{evidence_index}]: {error}"
            ))
        })?);
    }
    Ok(parsed)
}

fn parse_demo_ids(ids: &[String], claim_index: usize) -> Result<Vec<DemoId>, ClaimParseError> {
    let mut parsed = Vec::with_capacity(ids.len());
    for (demo_index, demo_id) in ids.iter().enumerate() {
        parsed.push(demo_id.parse::<DemoId>().map_err(|error| {
            ClaimParseError::new(format!(
                "invalid demo id `{demo_id}` at claims[{claim_index}].demoIds[{demo_index}]: {error}"
            ))
        })?);
    }
    Ok(parsed)
}

fn read_claim_manifest(path: &Path) -> Result<ParsedManifest, ClaimParseError> {
    let input = fs::read_to_string(path).map_err(|error| {
        ClaimParseError::new(format!("failed to read {}: {error}", path.display()))
    })?;
    parse_claim_manifest_json(&input)
}

fn parse_claim_manifest_json(input: &str) -> Result<ParsedManifest, ClaimParseError> {
    let raw: RawClaimManifest = serde_json::from_str(input)
        .map_err(|error| ClaimParseError::new(format!("failed to parse manifest.json: {error}")))?;

    validate_manifest_structure(
        raw.schema.as_deref(),
        raw.claim_id.as_deref(),
        raw.artifacts.is_some(),
    )
    .map_err(|error| ClaimParseError::new(error.to_string()))?;

    let raw_claim_id = raw
        .claim_id
        .ok_or_else(|| ClaimParseError::new("manifest.json must have a claimId field"))?;
    let claim_id = raw_claim_id.parse::<ClaimId>().map_err(|error| {
        ClaimParseError::new(format!(
            "invalid manifest claim id `{raw_claim_id}`: {error}"
        ))
    })?;
    let last_trace_id = raw
        .last_trace_id
        .as_deref()
        .map(str::parse::<TraceId>)
        .transpose()
        .map_err(|error| ClaimParseError::new(format!("invalid lastTraceId: {error}")))?;
    let verification_status = raw
        .verification_status
        .as_deref()
        .unwrap_or(ManifestVerificationStatus::Unverified.as_str())
        .parse::<ManifestVerificationStatus>()
        .map_err(|error| {
            ClaimParseError::new(format!("invalid manifest verificationStatus: {error}"))
        })?;

    let mut seen_paths = HashSet::new();
    let mut artifacts = Vec::new();
    for (artifact_index, raw_artifact) in raw.artifacts.unwrap_or_default().into_iter().enumerate()
    {
        let artifact = convert_raw_manifest_artifact(artifact_index, raw_artifact)?;
        validate_artifact_entry(&artifact.path, &artifact.blake3_hash, &mut seen_paths)
            .map_err(|error| ClaimParseError::new(error.to_string()))?;
        artifacts.push(artifact);
    }

    Ok(ParsedManifest {
        claim_id,
        artifacts,
        last_verified_at: raw.last_verified_at,
        last_trace_id,
        verification_status,
    })
}

fn convert_raw_manifest_artifact(
    artifact_index: usize,
    raw_artifact: RawManifestArtifact,
) -> Result<ParsedManifestArtifact, ClaimParseError> {
    let path = required_manifest_field(raw_artifact.path, "artifact path", artifact_index)?;
    let raw_artifact_type =
        required_manifest_field(raw_artifact.artifact_type, "artifact type", artifact_index)?;
    let artifact_type = raw_artifact_type.parse::<ArtifactType>().map_err(|error| {
        ClaimParseError::new(format!(
            "invalid artifact type `{raw_artifact_type}` at artifacts[{artifact_index}]: {error}"
        ))
    })?;
    let blake3_hash = required_manifest_field(
        raw_artifact.blake3_hash,
        "artifact blake3Hash",
        artifact_index,
    )?;
    let size_bytes = raw_artifact.size_bytes.ok_or_else(|| {
        ClaimParseError::new(format!(
            "missing artifact sizeBytes at artifacts[{artifact_index}]"
        ))
    })?;

    Ok(ParsedManifestArtifact {
        path,
        _artifact_type: artifact_type,
        blake3_hash,
        size_bytes,
    })
}

fn required_manifest_field(
    value: Option<String>,
    field_name: &str,
    artifact_index: usize,
) -> Result<String, ClaimParseError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ClaimParseError::new(format!(
            "missing {field_name} at artifacts[{artifact_index}]"
        ))),
    }
}

fn find_claim<'a>(claims: &'a [ParsedClaim], claim_id: &ClaimId) -> Option<&'a ParsedClaim> {
    claims.iter().find(|claim| claim.id == *claim_id)
}

fn verify_claim_artifacts(
    claim_id: ClaimId,
    artifacts_root: &Path,
) -> (ClaimVerifyResult, Option<ParsedManifest>) {
    let manifest_path = manifest_path_for_claim(artifacts_root, &claim_id);
    let claim_id_string = claim_id.to_string();
    let manifest = match read_claim_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return (
                ClaimVerifyResult {
                    claim_id: claim_id_string,
                    status: ManifestVerificationStatus::Failing,
                    artifacts_checked: 0,
                    artifacts_passed: 0,
                    artifacts_failed: 1,
                    errors: vec![format!(
                        "manifest_unavailable: {}: {}",
                        manifest_path.display(),
                        error
                    )],
                },
                None,
            );
        }
    };

    let mut errors = Vec::new();
    if manifest.claim_id != claim_id {
        errors.push(format!(
            "manifest_claim_id_mismatch: expected {}, got {}",
            claim_id, manifest.claim_id
        ));
    }

    let claim_artifacts_dir = claim_artifacts_dir(artifacts_root, &claim_id);
    let mut artifacts_passed = 0usize;
    let mut artifacts_failed = 0usize;

    for artifact in &manifest.artifacts {
        let artifact_path = claim_artifacts_dir.join(&artifact.path);
        match fs::read(&artifact_path) {
            Ok(bytes) => {
                let actual_hash = blake3::hash(&bytes).to_hex().to_string();
                let actual_size = bytes.len() as u64;
                let hash_matches = actual_hash == artifact.blake3_hash;
                let size_matches = actual_size == artifact.size_bytes;

                if hash_matches && size_matches {
                    artifacts_passed += 1;
                } else {
                    artifacts_failed += 1;
                }

                if !hash_matches {
                    errors.push(format!(
                        "hash_mismatch: {} expected {} got {}",
                        artifact.path, artifact.blake3_hash, actual_hash
                    ));
                }

                if !size_matches {
                    errors.push(format!(
                        "size_mismatch: {} expected {} got {}",
                        artifact.path, artifact.size_bytes, actual_size
                    ));
                }
            }
            Err(error) => {
                artifacts_failed += 1;
                errors.push(format!(
                    "artifact_not_found: {}: {}",
                    artifact_path.display(),
                    error
                ));
            }
        }
    }

    let status = if errors.is_empty() {
        ManifestVerificationStatus::Passing
    } else {
        ManifestVerificationStatus::Failing
    };

    (
        ClaimVerifyResult {
            claim_id: claim_id_string,
            status,
            artifacts_checked: manifest.artifacts.len(),
            artifacts_passed,
            artifacts_failed,
            errors,
        },
        Some(manifest),
    )
}

const fn posture_for_claim_status(status: ClaimStatus) -> ClaimPosture {
    match status {
        ClaimStatus::Draft | ClaimStatus::Active => ClaimPosture::Unverified,
        ClaimStatus::Verified => ClaimPosture::Verified,
        ClaimStatus::Stale => ClaimPosture::Stale,
        ClaimStatus::Regressed => ClaimPosture::Regressed,
        ClaimStatus::Retired => ClaimPosture::Unknown,
    }
}

/// Build a deterministic report for `ee claim list` from a real claims file.
///
/// This does not mutate state or verify artifacts. Callers are expected to
/// translate parse errors into the CLI's degraded error envelope.
pub fn build_claim_list_report(
    options: &ClaimListOptions,
) -> Result<ClaimListReport, ClaimParseError> {
    let claims_file = claims_file_path(&options.workspace_path, options.claims_file.as_deref());
    let claims_file_str = claims_file.display().to_string();
    if !claims_file.exists() {
        return Ok(ClaimListReport {
            schema: CLAIM_LIST_SCHEMA_V1,
            claims_file: claims_file_str,
            claims_file_exists: false,
            total_count: 0,
            filtered_count: 0,
            claims: Vec::new(),
            filter_status: options.status.clone(),
            filter_frequency: options.frequency.clone(),
            filter_tag: options.tag.clone(),
        });
    }

    let claims = read_claims_file(&claims_file)?;
    let filter_status = parse_optional_filter::<ClaimStatus>(options.status.as_deref(), "status")?;
    let filter_frequency =
        parse_optional_filter::<VerificationFrequency>(options.frequency.as_deref(), "frequency")?;
    let filtered = claims
        .iter()
        .filter(|claim| filter_status.is_none_or(|status| claim.status == status))
        .filter(|claim| filter_frequency.is_none_or(|frequency| claim.frequency == frequency))
        .filter(|claim| {
            options
                .tag
                .as_deref()
                .is_none_or(|tag| claim.tags.iter().any(|claim_tag| claim_tag == tag))
        })
        .map(ParsedClaim::summary)
        .collect::<Vec<_>>();

    Ok(ClaimListReport {
        schema: CLAIM_LIST_SCHEMA_V1,
        claims_file: claims_file_str,
        claims_file_exists: true,
        total_count: claims.len(),
        filtered_count: filtered.len(),
        claims: filtered,
        filter_status: options.status.clone(),
        filter_frequency: options.frequency.clone(),
        filter_tag: options.tag.clone(),
    })
}

/// Build a deterministic report for `ee claim show` from real files.
pub fn build_claim_show_report(
    options: &ClaimShowOptions,
) -> Result<ClaimShowReport, ClaimParseError> {
    let claim_id = options.claim_id.parse::<ClaimId>().map_err(|error| {
        ClaimParseError::new(format!("invalid claim id `{}`: {error}", options.claim_id))
    })?;
    let claims_file = claims_file_path(&options.workspace_path, options.claims_file.as_deref());
    if !claims_file.exists() {
        return Ok(ClaimShowReport {
            schema: CLAIM_SHOW_SCHEMA_V1,
            claim_id: options.claim_id.clone(),
            found: false,
            claim: None,
            manifest: None,
            include_manifest: options.include_manifest,
        });
    }
    let claims = read_claims_file(&claims_file)?;
    let Some(claim) = find_claim(&claims, &claim_id) else {
        return Ok(ClaimShowReport {
            schema: CLAIM_SHOW_SCHEMA_V1,
            claim_id: options.claim_id.clone(),
            found: false,
            claim: None,
            manifest: None,
            include_manifest: options.include_manifest,
        });
    };

    let artifacts_root =
        artifacts_root_path(&options.workspace_path, options.artifacts_dir.as_deref());
    let manifest = if options.include_manifest {
        let manifest_path = manifest_path_for_claim(&artifacts_root, &claim_id);
        if manifest_path.exists() {
            Some(read_claim_manifest(&manifest_path)?.detail())
        } else {
            None
        }
    } else {
        None
    };

    Ok(ClaimShowReport {
        schema: CLAIM_SHOW_SCHEMA_V1,
        claim_id: claim_id.to_string(),
        found: true,
        claim: Some(claim.detail()),
        manifest,
        include_manifest: options.include_manifest,
    })
}

/// Build a deterministic report for `ee claim verify` from real manifests and artifacts.
pub fn build_claim_verify_report(
    options: &ClaimVerifyOptions,
) -> Result<ClaimVerifyReport, ClaimParseError> {
    let claims_file = claims_file_path(&options.workspace_path, options.claims_file.as_deref());
    let artifacts_root =
        artifacts_root_path(&options.workspace_path, options.artifacts_dir.as_deref());
    let claims = read_claims_file(&claims_file)?;
    let verify_all = options.claim_id == "all";
    let selected_claim_ids = if verify_all {
        claims.iter().map(|claim| claim.id).collect::<Vec<_>>()
    } else {
        let claim_id = options.claim_id.parse::<ClaimId>().map_err(|error| {
            ClaimParseError::new(format!("invalid claim id `{}`: {error}", options.claim_id))
        })?;
        if find_claim(&claims, &claim_id).is_none() {
            return Err(ClaimParseError::new(format!(
                "claim id `{claim_id}` is not present in {}",
                claims_file.display()
            )));
        }
        vec![claim_id]
    };

    let mut results = Vec::new();
    let mut verified_count = 0usize;
    let mut failed_count = 0usize;
    let mut skipped_count = 0usize;
    let total_selected = selected_claim_ids.len();

    for claim_id in selected_claim_ids {
        let (result, _) = verify_claim_artifacts(claim_id, &artifacts_root);
        match result.status {
            ManifestVerificationStatus::Passing => verified_count += 1,
            ManifestVerificationStatus::Failing => failed_count += 1,
            _ => skipped_count += 1,
        }
        let should_stop = options.fail_fast && result.status == ManifestVerificationStatus::Failing;
        results.push(result);
        if should_stop {
            skipped_count += total_selected.saturating_sub(results.len());
            break;
        }
    }

    Ok(ClaimVerifyReport {
        schema: CLAIM_VERIFY_SCHEMA_V1,
        claim_id: options.claim_id.clone(),
        verify_all,
        claims_file: claims_file.display().to_string(),
        artifacts_dir: artifacts_root.display().to_string(),
        total_claims: claims.len(),
        verified_count,
        failed_count,
        skipped_count,
        results,
        fail_fast: options.fail_fast,
    })
}

fn parse_optional_filter<T>(value: Option<&str>, label: &str) -> Result<Option<T>, ClaimParseError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .map(|raw| {
            raw.parse::<T>().map_err(|error| {
                ClaimParseError::new(format!("invalid claim {label} filter `{raw}`: {error}"))
            })
        })
        .transpose()
}

/// Status posture for a claim in diagnostic context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimPosture {
    /// Claim has been verified recently and passed.
    Verified,
    /// Claim exists but has never been verified.
    Unverified,
    /// Claim was verified but is now stale (verification too old).
    Stale,
    /// Claim was verified but has regressed (previously passed, now fails).
    Regressed,
    /// Claim verification status is unknown or unavailable.
    Unknown,
}

impl ClaimPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Stale => "stale",
            Self::Regressed => "regressed",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn severity(self) -> &'static str {
        match self {
            Self::Verified => "ok",
            Self::Unverified => "warning",
            Self::Stale => "warning",
            Self::Regressed => "error",
            Self::Unknown => "info",
        }
    }
}

/// Individual claim diagnostic entry.
#[derive(Clone, Debug)]
pub struct DiagClaimEntry {
    pub id: String,
    pub title: String,
    pub posture: ClaimPosture,
    pub last_verified_at: Option<String>,
    pub staleness_days: Option<u32>,
    pub evidence_count: usize,
    pub demo_count: usize,
    pub frequency: VerificationFrequency,
}

/// Options for generating claim diagnostics.
#[derive(Clone, Debug, Default)]
pub struct DiagClaimsOptions {
    pub workspace_path: std::path::PathBuf,
    pub claims_file: Option<std::path::PathBuf>,
    pub staleness_threshold_days: u32,
    pub include_verified: bool,
}

/// Summary counts for claim diagnostic report.
#[derive(Clone, Debug, Default)]
pub struct DiagClaimsCounts {
    pub total: usize,
    pub verified: usize,
    pub unverified: usize,
    pub stale: usize,
    pub regressed: usize,
    pub unknown: usize,
}

/// Diagnostic report for claims status and posture.
#[derive(Clone, Debug)]
pub struct DiagClaimsReport {
    pub schema: &'static str,
    pub claims_file: String,
    pub claims_file_exists: bool,
    pub staleness_threshold_days: u32,
    pub counts: DiagClaimsCounts,
    pub entries: Vec<DiagClaimEntry>,
    pub health_status: &'static str,
    pub repair_actions: Vec<String>,
}

impl DiagClaimsReport {
    #[must_use]
    pub fn gather(options: &DiagClaimsOptions) -> Self {
        let claims_file = options
            .claims_file
            .clone()
            .unwrap_or_else(|| options.workspace_path.join("claims.yaml"));
        let claims_file_str = claims_file.display().to_string();
        let claims_file_exists = claims_file.exists();

        let staleness_threshold_days = if options.staleness_threshold_days == 0 {
            30
        } else {
            options.staleness_threshold_days
        };

        let mut entries = Vec::new();
        let mut counts = DiagClaimsCounts::default();
        let mut repair_actions = Vec::new();

        if !claims_file_exists {
            repair_actions.push(format!("Create claims file at {}", claims_file_str));
            return Self {
                schema: DIAG_CLAIMS_SCHEMA_V1,
                claims_file: claims_file_str,
                claims_file_exists,
                staleness_threshold_days,
                counts,
                entries,
                health_status: "degraded",
                repair_actions,
            };
        }

        let parsed_claims = match read_claims_file(&claims_file) {
            Ok(claims) => claims,
            Err(error) => {
                repair_actions.push(format!("Fix claims file at {}: {}", claims_file_str, error));
                return Self {
                    schema: DIAG_CLAIMS_SCHEMA_V1,
                    claims_file: claims_file_str,
                    claims_file_exists,
                    staleness_threshold_days,
                    counts,
                    entries,
                    health_status: "degraded",
                    repair_actions,
                };
            }
        };

        for claim in parsed_claims {
            let posture = posture_for_claim_status(claim.status);
            match posture {
                ClaimPosture::Verified => counts.verified += 1,
                ClaimPosture::Unverified => counts.unverified += 1,
                ClaimPosture::Stale => counts.stale += 1,
                ClaimPosture::Regressed => counts.regressed += 1,
                ClaimPosture::Unknown => counts.unknown += 1,
            }
            counts.total += 1;

            if posture != ClaimPosture::Verified || options.include_verified {
                entries.push(DiagClaimEntry {
                    id: claim.id.to_string(),
                    title: claim.title,
                    posture,
                    last_verified_at: None,
                    staleness_days: None,
                    evidence_count: claim.evidence_count,
                    demo_count: claim.demo_count,
                    frequency: claim.frequency,
                });
            }
        }

        if counts.total == 0 {
            repair_actions.push(format!("Add claims to {}", claims_file_str));
        }
        if counts.unverified > 0 {
            repair_actions.push(format!(
                "ee claim verify --all to verify {} unverified claims",
                counts.unverified
            ));
        }
        if counts.stale > 0 {
            repair_actions.push(format!(
                "ee claim verify --stale to re-verify {} stale claims",
                counts.stale
            ));
        }
        if counts.regressed > 0 {
            repair_actions.push(format!(
                "ee claim show <id> to investigate {} regressed claims",
                counts.regressed
            ));
        }

        let health_status = if counts.total == 0 {
            "degraded"
        } else if counts.regressed > 0 {
            "unhealthy"
        } else if counts.unverified > 0 || counts.stale > 0 {
            "degraded"
        } else {
            "healthy"
        };

        Self {
            schema: DIAG_CLAIMS_SCHEMA_V1,
            claims_file: claims_file_str,
            claims_file_exists,
            staleness_threshold_days,
            counts,
            entries,
            health_status,
            repair_actions,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{TestResult, ensure_equal};

    use super::*;

    #[test]
    fn claim_list_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_LIST_SCHEMA_V1,
            &"ee.claim_list.v1",
            "claim list schema",
        )
    }

    #[test]
    fn claim_show_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_SHOW_SCHEMA_V1,
            &"ee.claim_show.v1",
            "claim show schema",
        )
    }

    #[test]
    fn claim_verify_report_schema_is_stable() -> TestResult {
        ensure_equal(
            &CLAIM_VERIFY_SCHEMA_V1,
            &"ee.claim_verify.v1",
            "claim verify schema",
        )
    }

    #[test]
    fn diag_claims_schema_is_stable() -> TestResult {
        ensure_equal(
            &DIAG_CLAIMS_SCHEMA_V1,
            &"ee.diag_claims.v1",
            "diag claims schema",
        )
    }

    #[test]
    fn claim_posture_as_str_is_stable() -> TestResult {
        ensure_equal(
            &ClaimPosture::Verified.as_str(),
            &"verified",
            "verified posture",
        )?;
        ensure_equal(
            &ClaimPosture::Unverified.as_str(),
            &"unverified",
            "unverified posture",
        )?;
        ensure_equal(&ClaimPosture::Stale.as_str(), &"stale", "stale posture")?;
        ensure_equal(
            &ClaimPosture::Regressed.as_str(),
            &"regressed",
            "regressed posture",
        )?;
        ensure_equal(
            &ClaimPosture::Unknown.as_str(),
            &"unknown",
            "unknown posture",
        )
    }

    #[test]
    fn claim_posture_severity_reflects_urgency() -> TestResult {
        ensure_equal(
            &ClaimPosture::Verified.severity(),
            &"ok",
            "verified severity",
        )?;
        ensure_equal(
            &ClaimPosture::Unverified.severity(),
            &"warning",
            "unverified severity",
        )?;
        ensure_equal(
            &ClaimPosture::Stale.severity(),
            &"warning",
            "stale severity",
        )?;
        ensure_equal(
            &ClaimPosture::Regressed.severity(),
            &"error",
            "regressed severity",
        )?;
        ensure_equal(
            &ClaimPosture::Unknown.severity(),
            &"info",
            "unknown severity",
        )
    }

    #[test]
    fn diag_claims_report_returns_degraded_when_file_missing() -> TestResult {
        let options = DiagClaimsOptions {
            workspace_path: std::path::PathBuf::from("/nonexistent"),
            claims_file: Some(std::path::PathBuf::from("/nonexistent/claims.yaml")),
            staleness_threshold_days: 30,
            include_verified: false,
        };
        let report = DiagClaimsReport::gather(&options);
        ensure_equal(&report.claims_file_exists, &false, "file exists")?;
        ensure_equal(&report.health_status, &"degraded", "health status")?;
        ensure_equal(
            &report.repair_actions.is_empty(),
            &false,
            "has repair actions",
        )
    }

    #[test]
    fn diag_claims_default_staleness_is_thirty_days() -> TestResult {
        let options = DiagClaimsOptions {
            workspace_path: std::path::PathBuf::from("/nonexistent"),
            staleness_threshold_days: 0,
            ..Default::default()
        };
        let report = DiagClaimsReport::gather(&options);
        ensure_equal(&report.staleness_threshold_days, &30, "staleness threshold")
    }
}
