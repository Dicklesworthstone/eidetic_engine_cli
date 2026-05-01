//! Model registry domain types (EE-293).
//!
//! The registry records local model capabilities that search, ranking, and
//! curation code may depend on. Wire values are intentionally small, stable
//! strings so database rows and future JSON output remain deterministic.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Schema identifier for model registry records.
pub const MODEL_REGISTRY_SCHEMA_V1: &str = "ee.model_registry.v1";

/// Schema identifier for embedding metadata stored in model registry rows.
pub const EMBEDDING_METADATA_SCHEMA_V1: &str = "ee.embedding.metadata.v1";

/// Schema identifier for semantic model admissibility reports.
pub const SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1: &str = "ee.semantic_model_admissibility.v1";

/// Provider or embedding implementation family for a registered model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    /// Deterministic hash embedder used by tests and degraded local mode.
    Hash,
    /// Frankensearch model2vec embedding backend.
    #[serde(rename = "model2vec")]
    Model2Vec,
    /// Frankensearch fastembed backend when explicitly enabled.
    #[serde(rename = "fastembed")]
    FastEmbed,
    /// Externally supplied model metadata.
    External,
    /// Project-specific model adapter.
    Custom,
}

impl ModelProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hash => "hash",
            Self::Model2Vec => "model2vec",
            Self::FastEmbed => "fastembed",
            Self::External => "external",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Hash,
            Self::Model2Vec,
            Self::FastEmbed,
            Self::External,
            Self::Custom,
        ]
    }
}

impl fmt::Display for ModelProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelProvider {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "hash" => Ok(Self::Hash),
            "model2vec" => Ok(Self::Model2Vec),
            "fastembed" => Ok(Self::FastEmbed),
            "external" => Ok(Self::External),
            "custom" => Ok(Self::Custom),
            _ => Err(ParseModelRegistryValueError::new(
                "provider",
                input,
                "hash, model2vec, fastembed, external, custom",
            )),
        }
    }
}

/// What a registered model is used for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    /// Embedding model used to build or query vector indexes.
    Embedding,
    /// Reranking model applied after retrieval.
    Reranker,
    /// Classifier used for labels, policy, or curation.
    Classifier,
    /// Other explicitly recorded model capability.
    Other,
}

impl ModelPurpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Reranker => "reranker",
            Self::Classifier => "classifier",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Embedding,
            Self::Reranker,
            Self::Classifier,
            Self::Other,
        ]
    }
}

impl fmt::Display for ModelPurpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelPurpose {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "embedding" => Ok(Self::Embedding),
            "reranker" => Ok(Self::Reranker),
            "classifier" => Ok(Self::Classifier),
            "other" => Ok(Self::Other),
            _ => Err(ParseModelRegistryValueError::new(
                "purpose",
                input,
                "embedding, reranker, classifier, other",
            )),
        }
    }
}

/// Distance metric used by an embedding model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDistanceMetric {
    /// Cosine similarity.
    Cosine,
    /// Dot product similarity.
    Dot,
    /// Euclidean/L2 distance.
    L2,
}

impl ModelDistanceMetric {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::Dot => "dot",
            Self::L2 => "l2",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Cosine, Self::Dot, Self::L2]
    }
}

impl fmt::Display for ModelDistanceMetric {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelDistanceMetric {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "cosine" => Ok(Self::Cosine),
            "dot" => Ok(Self::Dot),
            "l2" => Ok(Self::L2),
            _ => Err(ParseModelRegistryValueError::new(
                "distance_metric",
                input,
                "cosine, dot, l2",
            )),
        }
    }
}

/// Availability state for a registered model.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRegistryStatus {
    /// Model is usable for its declared purpose.
    #[default]
    Available,
    /// Model is configured but not currently usable.
    Unavailable,
    /// Model has been intentionally disabled.
    Disabled,
}

impl ModelRegistryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Available, Self::Unavailable, Self::Disabled]
    }
}

impl fmt::Display for ModelRegistryStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelRegistryStatus {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "available" => Ok(Self::Available),
            "unavailable" => Ok(Self::Unavailable),
            "disabled" => Ok(Self::Disabled),
            _ => Err(ParseModelRegistryValueError::new(
                "status",
                input,
                "available, unavailable, disabled",
            )),
        }
    }
}

/// Error when parsing a model registry enum wire value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseModelRegistryValueError {
    field: &'static str,
    input: String,
    expected: &'static str,
}

impl ParseModelRegistryValueError {
    #[must_use]
    pub fn new(field: &'static str, input: &str, expected: &'static str) -> Self {
        Self {
            field,
            input: input.to_owned(),
            expected,
        }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    #[must_use]
    pub const fn expected(&self) -> &'static str {
        self.expected
    }
}

impl fmt::Display for ParseModelRegistryValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown model registry {} `{}`; expected one of {}",
            self.field, self.input, self.expected
        )
    }
}

impl std::error::Error for ParseModelRegistryValueError {}

/// Scalar storage format for embedding vectors.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingVectorDtype {
    /// 32-bit floating point vectors.
    Float32,
    /// 16-bit floating point vectors.
    Float16,
    /// Signed 8-bit quantized vectors.
    Int8,
    /// Unsigned 8-bit quantized vectors.
    Uint8,
}

impl EmbeddingVectorDtype {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Float32 => "float32",
            Self::Float16 => "float16",
            Self::Int8 => "int8",
            Self::Uint8 => "uint8",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Float32, Self::Float16, Self::Int8, Self::Uint8]
    }
}

impl fmt::Display for EmbeddingVectorDtype {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EmbeddingVectorDtype {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "float32" => Ok(Self::Float32),
            "float16" => Ok(Self::Float16),
            "int8" => Ok(Self::Int8),
            "uint8" => Ok(Self::Uint8),
            _ => Err(ParseModelRegistryValueError::new(
                "vector_dtype",
                input,
                "float32, float16, int8, uint8",
            )),
        }
    }
}

/// How token embeddings are pooled into one vector.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPooling {
    /// Mean pooling across token vectors.
    Mean,
    /// Use a classifier token or equivalent model-specific pooled output.
    Cls,
    /// Model-specific pooling that is not further interpreted by ee.
    ModelDefault,
}

impl EmbeddingPooling {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Cls => "cls",
            Self::ModelDefault => "model_default",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Mean, Self::Cls, Self::ModelDefault]
    }
}

impl fmt::Display for EmbeddingPooling {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EmbeddingPooling {
    type Err = ParseModelRegistryValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "mean" => Ok(Self::Mean),
            "cls" => Ok(Self::Cls),
            "model_default" => Ok(Self::ModelDefault),
            _ => Err(ParseModelRegistryValueError::new(
                "pooling",
                input,
                "mean, cls, model_default",
            )),
        }
    }
}

/// Versioned, redaction-safe metadata needed to decide embedding compatibility.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingMetadataRecord {
    pub schema: String,
    pub dimension: u32,
    pub distance_metric: ModelDistanceMetric,
    pub vector_dtype: EmbeddingVectorDtype,
    pub normalized: bool,
    pub pooling: EmbeddingPooling,
    pub max_input_tokens: Option<u32>,
    pub tokenizer: Option<String>,
    pub model_revision: Option<String>,
    pub deterministic: bool,
}

impl EmbeddingMetadataRecord {
    #[must_use]
    pub fn new(dimension: u32, distance_metric: ModelDistanceMetric) -> Self {
        Self {
            schema: EMBEDDING_METADATA_SCHEMA_V1.to_string(),
            dimension,
            distance_metric,
            vector_dtype: EmbeddingVectorDtype::Float32,
            normalized: true,
            pooling: EmbeddingPooling::Mean,
            max_input_tokens: None,
            tokenizer: None,
            model_revision: None,
            deterministic: false,
        }
    }

    /// Validate stable record invariants before durable storage or parsing.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingMetadataValidationError`] when the record has an
    /// unsupported schema, non-positive numeric field, or blank optional text.
    pub fn validate(&self) -> Result<(), EmbeddingMetadataValidationError> {
        if self.schema != EMBEDDING_METADATA_SCHEMA_V1 {
            return Err(EmbeddingMetadataValidationError::SchemaMismatch {
                expected: EMBEDDING_METADATA_SCHEMA_V1,
                actual: self.schema.clone(),
            });
        }

        if self.dimension == 0 {
            return Err(EmbeddingMetadataValidationError::InvalidField {
                field: "dimension",
                message: "must be greater than zero".to_string(),
            });
        }

        if let Some(max_input_tokens) = self.max_input_tokens
            && max_input_tokens == 0
        {
            return Err(EmbeddingMetadataValidationError::InvalidField {
                field: "max_input_tokens",
                message: "must be greater than zero when present".to_string(),
            });
        }

        validate_optional_text("tokenizer", self.tokenizer.as_deref())?;
        validate_optional_text("model_revision", self.model_revision.as_deref())
    }

    /// Parse and validate a serialized metadata record.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingMetadataValidationError`] if JSON decoding fails or
    /// the decoded record violates the stable schema.
    pub fn from_json(input: &str) -> Result<Self, EmbeddingMetadataValidationError> {
        let record: Self = serde_json::from_str(input).map_err(|source| {
            EmbeddingMetadataValidationError::InvalidJson {
                message: source.to_string(),
            }
        })?;
        record.validate()?;
        Ok(record)
    }

    /// Serialize the record with stable field ordering.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingMetadataValidationError`] when validation fails or
    /// JSON serialization fails.
    pub fn to_canonical_json(&self) -> Result<String, EmbeddingMetadataValidationError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|source| {
            EmbeddingMetadataValidationError::InvalidJson {
                message: source.to_string(),
            }
        })
    }
}

fn validate_optional_text(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), EmbeddingMetadataValidationError> {
    if value.is_some_and(|text| text.trim().is_empty()) {
        Err(EmbeddingMetadataValidationError::InvalidField {
            field,
            message: "must not be blank when present".to_string(),
        })
    } else {
        Ok(())
    }
}

/// Validation error for embedding metadata records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EmbeddingMetadataValidationError {
    SchemaMismatch {
        expected: &'static str,
        actual: String,
    },
    InvalidField {
        field: &'static str,
        message: String,
    },
    InvalidJson {
        message: String,
    },
}

impl fmt::Display for EmbeddingMetadataValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaMismatch { expected, actual } => write!(
                formatter,
                "embedding metadata schema mismatch: expected {expected}, got {actual}"
            ),
            Self::InvalidField { field, message } => {
                write!(formatter, "invalid embedding metadata {field}: {message}")
            }
            Self::InvalidJson { message } => {
                write!(formatter, "invalid embedding metadata JSON: {message}")
            }
        }
    }
}

impl std::error::Error for EmbeddingMetadataValidationError {}

/// Runtime mode selected after semantic model admissibility checks.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticModelAdmissionMode {
    /// Semantic retrieval may use this model.
    Semantic,
    /// Semantic retrieval is skipped, but lexical retrieval may continue.
    LexicalFallback,
    /// The model choice is unsafe or invalid for this command.
    Rejected,
}

impl SemanticModelAdmissionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::LexicalFallback => "lexical_fallback",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for SemanticModelAdmissionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Budget policy used to decide whether a semantic model is admissible.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelAdmissibilityBudget {
    pub schema: String,
    pub max_dimension: u32,
    pub max_query_tokens: u32,
    pub max_model_bytes: Option<u64>,
    pub max_p95_latency_ms: Option<u32>,
    pub allow_remote_models: bool,
    pub require_deterministic: bool,
    pub require_known_token_limit: bool,
}

impl SemanticModelAdmissibilityBudget {
    #[must_use]
    pub fn local_default() -> Self {
        Self {
            schema: SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1.to_string(),
            max_dimension: 4096,
            max_query_tokens: 4096,
            max_model_bytes: Some(500_000_000),
            max_p95_latency_ms: Some(250),
            allow_remote_models: false,
            require_deterministic: false,
            require_known_token_limit: true,
        }
    }

    /// Validate budget invariants before evaluating a model.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticModelAdmissibilityError`] when a positive budget
    /// field is zero or the schema identifier is unsupported.
    pub fn validate(&self) -> Result<(), SemanticModelAdmissibilityError> {
        if self.schema != SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1 {
            return Err(SemanticModelAdmissibilityError::InvalidBudget {
                field: "schema",
                message: format!("expected {}", SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1),
            });
        }
        validate_positive_budget("max_dimension", u64::from(self.max_dimension))?;
        validate_positive_budget("max_query_tokens", u64::from(self.max_query_tokens))?;
        if let Some(max_model_bytes) = self.max_model_bytes {
            validate_positive_budget("max_model_bytes", max_model_bytes)?;
        }
        if let Some(max_p95_latency_ms) = self.max_p95_latency_ms {
            validate_positive_budget("max_p95_latency_ms", u64::from(max_p95_latency_ms))?;
        }
        Ok(())
    }
}

fn validate_positive_budget(
    field: &'static str,
    value: u64,
) -> Result<(), SemanticModelAdmissibilityError> {
    if value == 0 {
        Err(SemanticModelAdmissibilityError::InvalidBudget {
            field,
            message: "must be greater than zero".to_string(),
        })
    } else {
        Ok(())
    }
}

/// Candidate model and observed local metadata for an admissibility decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelCandidate {
    pub model_id: String,
    pub provider: ModelProvider,
    pub purpose: ModelPurpose,
    pub status: ModelRegistryStatus,
    pub metadata: EmbeddingMetadataRecord,
    pub model_bytes: Option<u64>,
    pub p95_latency_ms: Option<u32>,
    pub remote: bool,
}

impl SemanticModelCandidate {
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        provider: ModelProvider,
        metadata: EmbeddingMetadataRecord,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            provider,
            purpose: ModelPurpose::Embedding,
            status: ModelRegistryStatus::Available,
            metadata,
            model_bytes: None,
            p95_latency_ms: None,
            remote: false,
        }
    }

    #[must_use]
    pub const fn with_purpose(mut self, purpose: ModelPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    #[must_use]
    pub const fn with_status(mut self, status: ModelRegistryStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub const fn with_model_bytes(mut self, bytes: u64) -> Self {
        self.model_bytes = Some(bytes);
        self
    }

    #[must_use]
    pub const fn with_p95_latency_ms(mut self, latency_ms: u32) -> Self {
        self.p95_latency_ms = Some(latency_ms);
        self
    }

    #[must_use]
    pub const fn with_remote(mut self, remote: bool) -> Self {
        self.remote = remote;
        self
    }

    fn validate(&self) -> Result<(), SemanticModelAdmissibilityError> {
        if self.model_id.trim().is_empty() {
            return Err(SemanticModelAdmissibilityError::InvalidCandidate {
                field: "model_id",
                message: "must not be blank".to_string(),
            });
        }
        Ok(())
    }
}

/// One check within a semantic model admissibility report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelAdmissibilityCheck {
    pub name: String,
    pub actual: Option<u64>,
    pub limit: Option<u64>,
    pub passed: bool,
    pub code: Option<String>,
    pub message: String,
}

impl SemanticModelAdmissibilityCheck {
    #[must_use]
    pub fn new(
        name: &'static str,
        actual: Option<u64>,
        limit: Option<u64>,
        passed: bool,
        code: Option<&'static str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            actual,
            limit,
            passed,
            code: code.map(str::to_string),
            message: message.into(),
        }
    }
}

/// Stable report for a semantic model admissibility decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticModelAdmissibilityReport {
    pub schema: String,
    pub model_id: String,
    pub provider: ModelProvider,
    pub mode: SemanticModelAdmissionMode,
    pub admissible: bool,
    pub degradation_codes: Vec<String>,
    pub rejection_codes: Vec<String>,
    pub repair_actions: Vec<String>,
    pub checks: Vec<SemanticModelAdmissibilityCheck>,
}

impl SemanticModelAdmissibilityReport {
    /// Evaluate a candidate against the supplied budget.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticModelAdmissibilityError`] when the budget or candidate
    /// shape is invalid. Invalid embedding metadata is represented as a
    /// rejected report so callers can emit a stable machine-readable reason.
    pub fn evaluate(
        candidate: &SemanticModelCandidate,
        budget: &SemanticModelAdmissibilityBudget,
    ) -> Result<Self, SemanticModelAdmissibilityError> {
        budget.validate()?;
        candidate.validate()?;

        let mut checks = Vec::new();
        let mut degradation_codes = Vec::new();
        let mut rejection_codes = Vec::new();
        let mut repair_actions = Vec::new();

        if let Err(error) = candidate.metadata.validate() {
            record_rejection(
                &mut rejection_codes,
                &mut repair_actions,
                "semantic_metadata_invalid",
            );
            checks.push(SemanticModelAdmissibilityCheck::new(
                "metadata",
                None,
                None,
                false,
                Some("semantic_metadata_invalid"),
                error.to_string(),
            ));
        } else {
            checks.push(SemanticModelAdmissibilityCheck::new(
                "metadata",
                None,
                None,
                true,
                None,
                "embedding metadata validates",
            ));
        }

        if candidate.purpose == ModelPurpose::Embedding {
            checks.push(SemanticModelAdmissibilityCheck::new(
                "purpose",
                None,
                None,
                true,
                None,
                "model purpose is embedding",
            ));
        } else {
            record_rejection(
                &mut rejection_codes,
                &mut repair_actions,
                "semantic_model_wrong_purpose",
            );
            checks.push(SemanticModelAdmissibilityCheck::new(
                "purpose",
                None,
                None,
                false,
                Some("semantic_model_wrong_purpose"),
                format!("model purpose is {}", candidate.purpose),
            ));
        }

        match candidate.status {
            ModelRegistryStatus::Available => checks.push(SemanticModelAdmissibilityCheck::new(
                "status",
                None,
                None,
                true,
                None,
                "model is available",
            )),
            ModelRegistryStatus::Disabled => {
                record_degradation(
                    &mut degradation_codes,
                    &mut repair_actions,
                    "semantic_disabled",
                );
                checks.push(SemanticModelAdmissibilityCheck::new(
                    "status",
                    None,
                    None,
                    false,
                    Some("semantic_disabled"),
                    "model is intentionally disabled",
                ));
            }
            ModelRegistryStatus::Unavailable => {
                record_degradation(
                    &mut degradation_codes,
                    &mut repair_actions,
                    "semantic_model_unavailable",
                );
                checks.push(SemanticModelAdmissibilityCheck::new(
                    "status",
                    None,
                    None,
                    false,
                    Some("semantic_model_unavailable"),
                    "model is configured but unavailable",
                ));
            }
        }

        push_numeric_check(
            &mut checks,
            &mut degradation_codes,
            &mut repair_actions,
            NumericCheck {
                name: "dimension",
                actual: u64::from(candidate.metadata.dimension),
                limit: u64::from(budget.max_dimension),
                code: "semantic_dimension_exceeds_budget",
                ok_message: "embedding dimension is within the configured budget",
                fail_message: "embedding dimension exceeds the configured budget",
            },
        );

        push_optional_limit_check(
            &mut checks,
            &mut degradation_codes,
            &mut repair_actions,
            OptionalLimitCheck {
                name: "query_tokens",
                actual: u64::from(budget.max_query_tokens),
                limit: candidate.metadata.max_input_tokens.map(u64::from),
                require_known_limit: budget.require_known_token_limit,
                unknown_code: "semantic_token_limit_unknown",
                exceeds_code: "semantic_input_tokens_exceed_model_limit",
                ok_message: "query token budget fits the model token limit",
                unknown_message: "model token limit is unknown",
                fail_message: "query token budget exceeds the model token limit",
            },
        );

        if let Some(max_model_bytes) = budget.max_model_bytes {
            push_optional_limit_check(
                &mut checks,
                &mut degradation_codes,
                &mut repair_actions,
                OptionalLimitCheck {
                    name: "model_bytes",
                    actual: candidate.model_bytes.unwrap_or(0),
                    limit: candidate.model_bytes.map(|_| max_model_bytes),
                    require_known_limit: true,
                    unknown_code: "semantic_model_size_unknown",
                    exceeds_code: "semantic_model_bytes_exceeds_budget",
                    ok_message: "model size is within the configured budget",
                    unknown_message: "model size is unknown",
                    fail_message: "model size exceeds the configured budget",
                },
            );
        }

        if let Some(max_p95_latency_ms) = budget.max_p95_latency_ms {
            push_optional_limit_check(
                &mut checks,
                &mut degradation_codes,
                &mut repair_actions,
                OptionalLimitCheck {
                    name: "p95_latency_ms",
                    actual: candidate.p95_latency_ms.map_or(0, u64::from),
                    limit: candidate
                        .p95_latency_ms
                        .map(|_| u64::from(max_p95_latency_ms)),
                    require_known_limit: true,
                    unknown_code: "semantic_latency_unknown",
                    exceeds_code: "semantic_latency_exceeds_budget",
                    ok_message: "p95 latency is within the configured budget",
                    unknown_message: "p95 latency is unknown",
                    fail_message: "p95 latency exceeds the configured budget",
                },
            );
        }

        if candidate.remote && !budget.allow_remote_models {
            record_rejection(
                &mut rejection_codes,
                &mut repair_actions,
                "remote_semantic_model_denied",
            );
            checks.push(SemanticModelAdmissibilityCheck::new(
                "remote",
                None,
                None,
                false,
                Some("remote_semantic_model_denied"),
                "remote semantic models require explicit opt-in",
            ));
        } else {
            checks.push(SemanticModelAdmissibilityCheck::new(
                "remote",
                None,
                None,
                true,
                None,
                "remote model policy is satisfied",
            ));
        }

        if budget.require_deterministic && !candidate.metadata.deterministic {
            record_rejection(
                &mut rejection_codes,
                &mut repair_actions,
                "semantic_nondeterministic_model",
            );
            checks.push(SemanticModelAdmissibilityCheck::new(
                "deterministic",
                None,
                None,
                false,
                Some("semantic_nondeterministic_model"),
                "deterministic embeddings are required by the semantic budget",
            ));
        } else {
            checks.push(SemanticModelAdmissibilityCheck::new(
                "deterministic",
                None,
                None,
                true,
                None,
                "determinism policy is satisfied",
            ));
        }

        let mode = if rejection_codes.is_empty() {
            if degradation_codes.is_empty() {
                SemanticModelAdmissionMode::Semantic
            } else {
                SemanticModelAdmissionMode::LexicalFallback
            }
        } else {
            SemanticModelAdmissionMode::Rejected
        };

        Ok(Self {
            schema: SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1.to_string(),
            model_id: candidate.model_id.clone(),
            provider: candidate.provider,
            mode,
            admissible: mode == SemanticModelAdmissionMode::Semantic,
            degradation_codes,
            rejection_codes,
            repair_actions,
            checks,
        })
    }
}

struct NumericCheck {
    name: &'static str,
    actual: u64,
    limit: u64,
    code: &'static str,
    ok_message: &'static str,
    fail_message: &'static str,
}

fn push_numeric_check(
    checks: &mut Vec<SemanticModelAdmissibilityCheck>,
    degradation_codes: &mut Vec<String>,
    repair_actions: &mut Vec<String>,
    check: NumericCheck,
) {
    let passed = check.actual <= check.limit;
    if passed {
        checks.push(SemanticModelAdmissibilityCheck::new(
            check.name,
            Some(check.actual),
            Some(check.limit),
            true,
            None,
            check.ok_message,
        ));
    } else {
        record_degradation(degradation_codes, repair_actions, check.code);
        checks.push(SemanticModelAdmissibilityCheck::new(
            check.name,
            Some(check.actual),
            Some(check.limit),
            false,
            Some(check.code),
            check.fail_message,
        ));
    }
}

struct OptionalLimitCheck {
    name: &'static str,
    actual: u64,
    limit: Option<u64>,
    require_known_limit: bool,
    unknown_code: &'static str,
    exceeds_code: &'static str,
    ok_message: &'static str,
    unknown_message: &'static str,
    fail_message: &'static str,
}

fn push_optional_limit_check(
    checks: &mut Vec<SemanticModelAdmissibilityCheck>,
    degradation_codes: &mut Vec<String>,
    repair_actions: &mut Vec<String>,
    check: OptionalLimitCheck,
) {
    match check.limit {
        Some(limit) if check.actual <= limit => checks.push(SemanticModelAdmissibilityCheck::new(
            check.name,
            Some(check.actual),
            Some(limit),
            true,
            None,
            check.ok_message,
        )),
        Some(limit) => {
            record_degradation(degradation_codes, repair_actions, check.exceeds_code);
            checks.push(SemanticModelAdmissibilityCheck::new(
                check.name,
                Some(check.actual),
                Some(limit),
                false,
                Some(check.exceeds_code),
                check.fail_message,
            ));
        }
        None if check.require_known_limit => {
            record_degradation(degradation_codes, repair_actions, check.unknown_code);
            checks.push(SemanticModelAdmissibilityCheck::new(
                check.name,
                None,
                None,
                false,
                Some(check.unknown_code),
                check.unknown_message,
            ));
        }
        None => checks.push(SemanticModelAdmissibilityCheck::new(
            check.name,
            None,
            None,
            true,
            None,
            check.ok_message,
        )),
    }
}

fn record_degradation(
    degradation_codes: &mut Vec<String>,
    repair_actions: &mut Vec<String>,
    code: &'static str,
) {
    push_unique(degradation_codes, code);
    push_unique(repair_actions, repair_action_for_code(code));
}

fn record_rejection(
    rejection_codes: &mut Vec<String>,
    repair_actions: &mut Vec<String>,
    code: &'static str,
) {
    push_unique(rejection_codes, code);
    push_unique(repair_actions, repair_action_for_code(code));
}

fn push_unique(values: &mut Vec<String>, value: &'static str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn repair_action_for_code(code: &'static str) -> &'static str {
    match code {
        "semantic_disabled" => {
            "enable a local embedding model in Frankensearch config, then run ee index reembed --workspace <workspace>"
        }
        "semantic_model_unavailable" => {
            "install or refresh the configured local embedding model, then run ee index reembed --workspace <workspace>"
        }
        "semantic_dimension_exceeds_budget"
        | "semantic_input_tokens_exceed_model_limit"
        | "semantic_model_bytes_exceeds_budget"
        | "semantic_latency_exceeds_budget" => {
            "select a smaller local embedding model or raise the explicit semantic budget"
        }
        "semantic_token_limit_unknown"
        | "semantic_model_size_unknown"
        | "semantic_latency_unknown" => {
            "record complete embedding metadata before enabling semantic retrieval"
        }
        "remote_semantic_model_denied" => {
            "configure a local embedding model or explicitly opt in to remote semantic models"
        }
        "semantic_nondeterministic_model" => {
            "use the deterministic hash embedder or disable deterministic semantic mode"
        }
        "semantic_metadata_invalid" => {
            "repair the model registry row so embedding metadata validates against ee.embedding.metadata.v1"
        }
        "semantic_model_wrong_purpose" => {
            "register an embedding-purpose model for semantic retrieval"
        }
        _ => "inspect ee status --json for semantic retrieval repair guidance",
    }
}

/// Error returned before an admissibility report can be produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticModelAdmissibilityError {
    InvalidBudget {
        field: &'static str,
        message: String,
    },
    InvalidCandidate {
        field: &'static str,
        message: String,
    },
}

impl fmt::Display for SemanticModelAdmissibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBudget { field, message } => {
                write!(
                    formatter,
                    "invalid semantic model budget {field}: {message}"
                )
            }
            Self::InvalidCandidate { field, message } => {
                write!(
                    formatter,
                    "invalid semantic model candidate {field}: {message}"
                )
            }
        }
    }
}

impl std::error::Error for SemanticModelAdmissibilityError {}

/// Field descriptor used by the embedding metadata schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingMetadataFieldSchema {
    pub name: &'static str,
    pub field_type: &'static str,
    pub required: bool,
    pub description: &'static str,
}

/// Stable JSON-schema-like catalog for embedding metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingMetadataObjectSchema {
    pub json_schema: &'static str,
    pub id: &'static str,
    pub ee_schema: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub object_type: &'static str,
    pub required: &'static [&'static str],
    pub fields: &'static [EmbeddingMetadataFieldSchema],
    pub additional_properties: bool,
}

const EMBEDDING_METADATA_REQUIRED: &[&str] = &[
    "schema",
    "dimension",
    "distanceMetric",
    "vectorDtype",
    "normalized",
    "pooling",
    "deterministic",
];

const EMBEDDING_METADATA_FIELDS: &[EmbeddingMetadataFieldSchema] = &[
    EmbeddingMetadataFieldSchema {
        name: "schema",
        field_type: "string",
        required: true,
        description: "Schema identifier.",
    },
    EmbeddingMetadataFieldSchema {
        name: "dimension",
        field_type: "integer",
        required: true,
        description: "Embedding vector dimension.",
    },
    EmbeddingMetadataFieldSchema {
        name: "distanceMetric",
        field_type: "string",
        required: true,
        description: "Distance metric used by the index.",
    },
    EmbeddingMetadataFieldSchema {
        name: "vectorDtype",
        field_type: "string",
        required: true,
        description: "Scalar storage type for embedding vectors.",
    },
    EmbeddingMetadataFieldSchema {
        name: "normalized",
        field_type: "boolean",
        required: true,
        description: "Whether vectors are normalized before indexing.",
    },
    EmbeddingMetadataFieldSchema {
        name: "pooling",
        field_type: "string",
        required: true,
        description: "Pooling strategy used to produce one vector.",
    },
    EmbeddingMetadataFieldSchema {
        name: "maxInputTokens",
        field_type: "integer|null",
        required: false,
        description: "Maximum model input tokens when known.",
    },
    EmbeddingMetadataFieldSchema {
        name: "tokenizer",
        field_type: "string|null",
        required: false,
        description: "Tokenizer identity when compatibility depends on it.",
    },
    EmbeddingMetadataFieldSchema {
        name: "modelRevision",
        field_type: "string|null",
        required: false,
        description: "Model revision or adapter revision when known.",
    },
    EmbeddingMetadataFieldSchema {
        name: "deterministic",
        field_type: "boolean",
        required: true,
        description: "Whether repeated inputs produce byte-identical embeddings.",
    },
];

/// Return the stable embedding metadata schema catalog.
#[must_use]
pub const fn embedding_metadata_schemas() -> [EmbeddingMetadataObjectSchema; 1] {
    [EmbeddingMetadataObjectSchema {
        json_schema: "https://json-schema.org/draft/2020-12/schema",
        id: "urn:ee:schema:embedding-metadata:v1",
        ee_schema: EMBEDDING_METADATA_SCHEMA_V1,
        kind: "embedding_metadata",
        title: "EmbeddingMetadataRecord",
        description: "Compatibility metadata for one registered embedding model.",
        object_type: "object",
        required: EMBEDDING_METADATA_REQUIRED,
        fields: EMBEDDING_METADATA_FIELDS,
        additional_properties: false,
    }]
}

/// Render the embedding metadata schema catalog as deterministic JSON.
#[must_use]
pub fn embedding_metadata_schema_catalog_json() -> String {
    let schemas = embedding_metadata_schemas();
    let mut output = String::from("{\n");
    output.push_str("  \"schema\": \"ee.embedding.metadata.schemas.v1\",\n");
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str("      \"$schema\": ");
        push_json_string(&mut output, schema.json_schema);
        output.push_str(",\n");
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.id);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.ee_schema);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": ");
        push_json_string(&mut output, schema.object_type);
        output.push_str(",\n");
        output.push_str("      \"required\": [\n");
        for (required_index, field) in schema.required.iter().enumerate() {
            output.push_str("        ");
            push_json_string(&mut output, field);
            if required_index + 1 == schema.required.len() {
                output.push('\n');
            } else {
                output.push_str(",\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.field_type);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    output.push_str(&encoded);
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::io;
    use std::str::FromStr;

    use serde::Serialize;

    use super::{
        EMBEDDING_METADATA_SCHEMA_V1, EmbeddingMetadataRecord, EmbeddingMetadataValidationError,
        EmbeddingPooling, EmbeddingVectorDtype, ModelDistanceMetric, ModelProvider, ModelPurpose,
        ModelRegistryStatus, ParseModelRegistryValueError, SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1,
        SemanticModelAdmissibilityBudget, SemanticModelAdmissibilityReport,
        SemanticModelAdmissionMode, SemanticModelCandidate, embedding_metadata_schema_catalog_json,
        embedding_metadata_schemas,
    };

    const EMBEDDING_METADATA_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/embedding_metadata_schemas.json.golden");
    const SEMANTIC_MODEL_ADMISSIBILITY_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/semantic_model_admissibility.json.golden");

    type TestResult = Result<(), Box<dyn Error>>;

    fn test_failure(message: impl Into<String>) -> Box<dyn Error> {
        Box::new(io::Error::other(message.into()))
    }

    #[test]
    fn model_provider_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = ModelProvider::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            rendered,
            vec!["hash", "model2vec", "fastembed", "external", "custom"]
        );

        for provider in ModelProvider::all() {
            let parsed = ModelProvider::from_str(provider.as_str())?;
            assert_eq!(parsed, provider);
        }

        Ok(())
    }

    #[test]
    fn model_purpose_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = ModelPurpose::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            rendered,
            vec!["embedding", "reranker", "classifier", "other"]
        );

        for purpose in ModelPurpose::all() {
            let parsed = ModelPurpose::from_str(purpose.as_str())?;
            assert_eq!(parsed, purpose);
        }

        Ok(())
    }

    #[test]
    fn model_distance_metric_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = ModelDistanceMetric::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rendered, vec!["cosine", "dot", "l2"]);

        for metric in ModelDistanceMetric::all() {
            let parsed = ModelDistanceMetric::from_str(metric.as_str())?;
            assert_eq!(parsed, metric);
        }

        Ok(())
    }

    #[test]
    fn model_registry_status_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = ModelRegistryStatus::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rendered, vec!["available", "unavailable", "disabled"]);

        for status in ModelRegistryStatus::all() {
            let parsed = ModelRegistryStatus::from_str(status.as_str())?;
            assert_eq!(parsed, status);
        }

        Ok(())
    }

    #[test]
    fn model_registry_parse_error_preserves_context() -> TestResult {
        let error = match ModelProvider::from_str("slowembed") {
            Ok(provider) => {
                return Err(test_failure(format!(
                    "unexpected valid provider: {provider:?}"
                )));
            }
            Err(error) => error,
        };
        assert_eq!(
            error,
            ParseModelRegistryValueError::new(
                "provider",
                "slowembed",
                "hash, model2vec, fastembed, external, custom"
            )
        );
        assert_eq!(error.field(), "provider");
        assert_eq!(error.input(), "slowembed");

        Ok(())
    }

    #[test]
    fn embedding_vector_dtype_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = EmbeddingVectorDtype::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rendered, vec!["float32", "float16", "int8", "uint8"]);

        for dtype in EmbeddingVectorDtype::all() {
            let parsed = EmbeddingVectorDtype::from_str(dtype.as_str())?;
            assert_eq!(parsed, dtype);
        }

        Ok(())
    }

    #[test]
    fn embedding_pooling_round_trips_in_stable_order() -> TestResult {
        let rendered: Vec<String> = EmbeddingPooling::all()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rendered, vec!["mean", "cls", "model_default"]);

        for pooling in EmbeddingPooling::all() {
            let parsed = EmbeddingPooling::from_str(pooling.as_str())?;
            assert_eq!(parsed, pooling);
        }

        Ok(())
    }

    #[test]
    fn embedding_metadata_serializes_deterministically() -> TestResult {
        let mut record = EmbeddingMetadataRecord::new(384, ModelDistanceMetric::Cosine);
        record.max_input_tokens = Some(512);
        record.tokenizer = Some("bpe:frankensearch-model2vec".to_string());
        record.model_revision = Some("2026-04-30".to_string());
        record.deterministic = true;

        let json = record.to_canonical_json()?;
        assert_eq!(
            json,
            r#"{"schema":"ee.embedding.metadata.v1","dimension":384,"distanceMetric":"cosine","vectorDtype":"float32","normalized":true,"pooling":"mean","maxInputTokens":512,"tokenizer":"bpe:frankensearch-model2vec","modelRevision":"2026-04-30","deterministic":true}"#
        );

        let parsed = EmbeddingMetadataRecord::from_json(&json)?;
        assert_eq!(parsed, record);

        Ok(())
    }

    #[test]
    fn embedding_metadata_rejects_invalid_records() -> TestResult {
        let mut bad_dimension = EmbeddingMetadataRecord::new(0, ModelDistanceMetric::Cosine);
        assert_eq!(
            bad_dimension.validate(),
            Err(EmbeddingMetadataValidationError::InvalidField {
                field: "dimension",
                message: "must be greater than zero".to_string(),
            })
        );

        bad_dimension.dimension = 384;
        bad_dimension.max_input_tokens = Some(0);
        assert_eq!(
            bad_dimension.validate(),
            Err(EmbeddingMetadataValidationError::InvalidField {
                field: "max_input_tokens",
                message: "must be greater than zero when present".to_string(),
            })
        );

        let mut blank_tokenizer = EmbeddingMetadataRecord::new(384, ModelDistanceMetric::Cosine);
        blank_tokenizer.tokenizer = Some("  ".to_string());
        assert_eq!(
            blank_tokenizer.validate(),
            Err(EmbeddingMetadataValidationError::InvalidField {
                field: "tokenizer",
                message: "must not be blank when present".to_string(),
            })
        );

        let mut wrong_schema = EmbeddingMetadataRecord::new(384, ModelDistanceMetric::Cosine);
        wrong_schema.schema = "ee.embedding.metadata.v2".to_string();
        assert_eq!(
            wrong_schema.validate(),
            Err(EmbeddingMetadataValidationError::SchemaMismatch {
                expected: EMBEDDING_METADATA_SCHEMA_V1,
                actual: "ee.embedding.metadata.v2".to_string(),
            })
        );

        Ok(())
    }

    #[test]
    fn embedding_metadata_schema_catalog_order_is_stable() -> TestResult {
        let schemas = embedding_metadata_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].ee_schema, EMBEDDING_METADATA_SCHEMA_V1);
        assert_eq!(schemas[0].fields[0].name, "schema");
        assert_eq!(schemas[0].fields[9].name, "deterministic");

        Ok(())
    }

    #[test]
    fn embedding_metadata_schema_catalog_matches_golden_fixture() -> TestResult {
        assert_eq!(
            embedding_metadata_schema_catalog_json(),
            EMBEDDING_METADATA_SCHEMA_GOLDEN
        );

        Ok(())
    }

    #[test]
    fn embedding_metadata_schema_catalog_is_valid_json() -> TestResult {
        serde_json::from_str::<serde_json::Value>(&embedding_metadata_schema_catalog_json())?;
        Ok(())
    }

    fn budgeted_metadata(
        dimension: u32,
        max_input_tokens: u32,
        deterministic: bool,
    ) -> EmbeddingMetadataRecord {
        let mut metadata = EmbeddingMetadataRecord::new(dimension, ModelDistanceMetric::Cosine);
        metadata.max_input_tokens = Some(max_input_tokens);
        metadata.tokenizer = Some("bpe:frankensearch-model2vec".to_string());
        metadata.model_revision = Some("2026-04-30".to_string());
        metadata.deterministic = deterministic;
        metadata
    }

    fn admissible_hash_candidate() -> SemanticModelCandidate {
        SemanticModelCandidate::new(
            "hash-384-local",
            ModelProvider::Hash,
            budgeted_metadata(384, 4096, true),
        )
        .with_model_bytes(42_000_000)
        .with_p95_latency_ms(24)
    }

    #[test]
    fn semantic_model_admissibility_accepts_budgeted_local_model() -> TestResult {
        let budget = SemanticModelAdmissibilityBudget::local_default();
        let report =
            SemanticModelAdmissibilityReport::evaluate(&admissible_hash_candidate(), &budget)?;

        assert_eq!(report.schema, SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1);
        assert_eq!(report.mode, SemanticModelAdmissionMode::Semantic);
        assert!(report.admissible);
        assert!(report.degradation_codes.is_empty());
        assert!(report.rejection_codes.is_empty());
        assert!(report.checks.iter().all(|check| check.passed));

        Ok(())
    }

    #[test]
    fn semantic_model_admissibility_falls_back_for_disabled_or_oversized_model() -> TestResult {
        let budget = SemanticModelAdmissibilityBudget::local_default();
        let disabled = admissible_hash_candidate().with_status(ModelRegistryStatus::Disabled);
        let disabled_report = SemanticModelAdmissibilityReport::evaluate(&disabled, &budget)?;

        assert_eq!(
            disabled_report.mode,
            SemanticModelAdmissionMode::LexicalFallback
        );
        assert!(!disabled_report.admissible);
        assert_eq!(disabled_report.degradation_codes, vec!["semantic_disabled"]);
        assert!(disabled_report.rejection_codes.is_empty());

        let oversized = SemanticModelCandidate::new(
            "model2vec-8192-local",
            ModelProvider::Model2Vec,
            budgeted_metadata(8192, 2048, true),
        )
        .with_model_bytes(640_000_000)
        .with_p95_latency_ms(280);
        let oversized_report = SemanticModelAdmissibilityReport::evaluate(&oversized, &budget)?;

        assert_eq!(
            oversized_report.mode,
            SemanticModelAdmissionMode::LexicalFallback
        );
        assert_eq!(
            oversized_report.degradation_codes,
            vec![
                "semantic_dimension_exceeds_budget",
                "semantic_input_tokens_exceed_model_limit",
                "semantic_model_bytes_exceeds_budget",
                "semantic_latency_exceeds_budget",
            ]
        );
        assert!(oversized_report.rejection_codes.is_empty());

        Ok(())
    }

    #[test]
    fn semantic_model_admissibility_rejects_remote_or_nondeterministic_model() -> TestResult {
        let mut budget = SemanticModelAdmissibilityBudget::local_default();
        budget.require_deterministic = true;

        let remote = SemanticModelCandidate::new(
            "external-semantic-api",
            ModelProvider::External,
            budgeted_metadata(1024, 4096, false),
        )
        .with_model_bytes(1)
        .with_p95_latency_ms(40)
        .with_remote(true);

        let report = SemanticModelAdmissibilityReport::evaluate(&remote, &budget)?;

        assert_eq!(report.mode, SemanticModelAdmissionMode::Rejected);
        assert!(!report.admissible);
        assert_eq!(
            report.rejection_codes,
            vec![
                "remote_semantic_model_denied",
                "semantic_nondeterministic_model",
            ]
        );

        Ok(())
    }

    #[test]
    fn semantic_model_admissibility_rejects_invalid_budget_shape() -> TestResult {
        let mut budget = SemanticModelAdmissibilityBudget::local_default();
        budget.max_dimension = 0;

        let error =
            SemanticModelAdmissibilityReport::evaluate(&admissible_hash_candidate(), &budget)
                .expect_err("zero dimension budget must be rejected");

        assert_eq!(
            error.to_string(),
            "invalid semantic model budget max_dimension: must be greater than zero"
        );

        Ok(())
    }

    #[test]
    fn semantic_model_admissibility_rejects_wrong_purpose() -> TestResult {
        let budget = SemanticModelAdmissibilityBudget::local_default();
        let candidate = admissible_hash_candidate().with_purpose(ModelPurpose::Classifier);

        let report = SemanticModelAdmissibilityReport::evaluate(&candidate, &budget)?;

        assert_eq!(report.mode, SemanticModelAdmissionMode::Rejected);
        assert_eq!(report.rejection_codes, vec!["semantic_model_wrong_purpose"]);

        Ok(())
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SemanticModelAdmissibilityRegressionFixture {
        schema: &'static str,
        cases: Vec<SemanticModelAdmissibilityRegressionCase>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SemanticModelAdmissibilityRegressionCase {
        model_id: String,
        mode: SemanticModelAdmissionMode,
        admissible: bool,
        degradation_codes: Vec<String>,
        rejection_codes: Vec<String>,
        failed_checks: Vec<String>,
    }

    fn regression_case(
        report: SemanticModelAdmissibilityReport,
    ) -> SemanticModelAdmissibilityRegressionCase {
        let failed_checks = report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name.clone())
            .collect();
        SemanticModelAdmissibilityRegressionCase {
            model_id: report.model_id,
            mode: report.mode,
            admissible: report.admissible,
            degradation_codes: report.degradation_codes,
            rejection_codes: report.rejection_codes,
            failed_checks,
        }
    }

    fn semantic_model_admissibility_regression_json() -> Result<String, Box<dyn Error>> {
        let budget = SemanticModelAdmissibilityBudget::local_default();
        let accepted =
            SemanticModelAdmissibilityReport::evaluate(&admissible_hash_candidate(), &budget)?;
        let disabled = SemanticModelAdmissibilityReport::evaluate(
            &admissible_hash_candidate().with_status(ModelRegistryStatus::Disabled),
            &budget,
        )?;
        let oversized = SemanticModelAdmissibilityReport::evaluate(
            &SemanticModelCandidate::new(
                "model2vec-8192-local",
                ModelProvider::Model2Vec,
                budgeted_metadata(8192, 2048, true),
            )
            .with_model_bytes(640_000_000)
            .with_p95_latency_ms(280),
            &budget,
        )?;
        let mut deterministic_budget = SemanticModelAdmissibilityBudget::local_default();
        deterministic_budget.require_deterministic = true;
        let rejected = SemanticModelAdmissibilityReport::evaluate(
            &SemanticModelCandidate::new(
                "external-semantic-api",
                ModelProvider::External,
                budgeted_metadata(1024, 4096, false),
            )
            .with_model_bytes(1)
            .with_p95_latency_ms(40)
            .with_remote(true),
            &deterministic_budget,
        )?;

        let fixture = SemanticModelAdmissibilityRegressionFixture {
            schema: SEMANTIC_MODEL_ADMISSIBILITY_SCHEMA_V1,
            cases: vec![
                regression_case(accepted),
                regression_case(disabled),
                regression_case(oversized),
                regression_case(rejected),
            ],
        };

        let mut json = serde_json::to_string_pretty(&fixture)?;
        json.push('\n');
        Ok(json)
    }

    #[test]
    fn semantic_model_admissibility_regression_matches_golden_fixture() -> TestResult {
        assert_eq!(
            semantic_model_admissibility_regression_json()?,
            SEMANTIC_MODEL_ADMISSIBILITY_GOLDEN
        );

        Ok(())
    }
}
