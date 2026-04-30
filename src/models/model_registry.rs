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

    use super::{
        EMBEDDING_METADATA_SCHEMA_V1, EmbeddingMetadataRecord, EmbeddingMetadataValidationError,
        EmbeddingPooling, EmbeddingVectorDtype, ModelDistanceMetric, ModelProvider, ModelPurpose,
        ModelRegistryStatus, ParseModelRegistryValueError, embedding_metadata_schema_catalog_json,
        embedding_metadata_schemas,
    };

    const EMBEDDING_METADATA_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/embedding_metadata_schemas.json.golden");

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
}
