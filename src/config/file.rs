//! TOML config file parsing (EE-021).
//!
//! Config files are intentionally parsed into optional, typed fields.
//! Precedence merging lives in the next layer; this module only answers
//! "what did this file say?" with deterministic validation errors.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use toml_edit::{DocumentMut, Item, Value};

use super::path::{PathExpander, PathExpansionError};

/// Parsed `.ee/config.toml` or user config file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigFile {
    pub storage: StorageConfig,
    pub runtime: RuntimeConfig,
    pub cass: CassConfig,
    pub search: SearchConfig,
    pub pack: PackConfig,
    pub curation: CurationConfig,
    pub feedback: FeedbackConfig,
    pub privacy: PrivacyConfig,
    pub trust: TrustConfig,
}

impl ConfigFile {
    /// Parse a TOML config string without expanding storage paths.
    ///
    /// Path values are returned lexically. Call
    /// [`ConfigFile::parse_with_expander`] when user/home/env expansion is
    /// required.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigParseError`] when TOML syntax is invalid or a known
    /// key has the wrong type/value.
    pub fn parse(input: &str) -> Result<Self, ConfigParseError> {
        Self::parse_inner(input, None)
    }

    /// Parse a TOML config string and expand path-like storage values.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigParseError`] when TOML syntax is invalid, a known
    /// key has the wrong type/value, or path expansion fails.
    pub fn parse_with_expander(
        input: &str,
        expander: &PathExpander,
    ) -> Result<Self, ConfigParseError> {
        Self::parse_inner(input, Some(expander))
    }

    fn parse_inner(input: &str, expander: Option<&PathExpander>) -> Result<Self, ConfigParseError> {
        let document = input
            .parse::<DocumentMut>()
            .map_err(|source| ConfigParseError::Toml {
                message: source.to_string(),
            })?;

        Ok(Self {
            storage: StorageConfig::parse(&document, expander)?,
            runtime: RuntimeConfig::parse(&document)?,
            cass: CassConfig::parse(&document)?,
            search: SearchConfig::parse(&document)?,
            pack: PackConfig::parse(&document)?,
            curation: CurationConfig::parse(&document)?,
            feedback: FeedbackConfig::parse(&document)?,
            privacy: PrivacyConfig::parse(&document)?,
            trust: TrustConfig::parse(&document)?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageConfig {
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub jsonl_export: Option<bool>,
}

impl StorageConfig {
    fn parse(
        document: &DocumentMut,
        expander: Option<&PathExpander>,
    ) -> Result<Self, ConfigParseError> {
        Ok(Self {
            database_path: optional_path(document, "storage", "database_path", expander)?,
            index_dir: optional_path(document, "storage", "index_dir", expander)?,
            jsonl_export: optional_bool(document, "storage", "jsonl_export")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub daemon: Option<bool>,
    pub job_budget_ms: Option<u64>,
    pub import_batch_size: Option<u64>,
}

impl RuntimeConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            daemon: optional_bool(document, "runtime", "daemon")?,
            job_budget_ms: optional_u64(document, "runtime", "job_budget_ms")?,
            import_batch_size: optional_u64(document, "runtime", "import_batch_size")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CassConfig {
    pub enabled: Option<bool>,
    pub binary: Option<String>,
    pub since: Option<String>,
}

impl CassConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            enabled: optional_bool(document, "cass", "enabled")?,
            binary: optional_string(document, "cass", "binary")?,
            since: optional_string(document, "cass", "since")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchConfig {
    pub default_speed: Option<SearchSpeed>,
    pub lexical_weight: Option<f64>,
    pub semantic_weight: Option<f64>,
    pub graph_weight: Option<f64>,
}

impl SearchConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_speed: optional_search_speed(document, "search", "default_speed")?,
            lexical_weight: optional_unit_float(document, "search", "lexical_weight")?,
            semantic_weight: optional_unit_float(document, "search", "semantic_weight")?,
            graph_weight: optional_unit_float(document, "search", "graph_weight")?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSpeed {
    Fast,
    Balanced,
    Thorough,
}

impl SearchSpeed {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Thorough => "thorough",
        }
    }
}

impl FromStr for SearchSpeed {
    type Err = ConfigParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "fast" => Ok(Self::Fast),
            "balanced" => Ok(Self::Balanced),
            "thorough" => Ok(Self::Thorough),
            other => Err(ConfigParseError::InvalidValue {
                key: "search.default_speed".to_string(),
                value: other.to_string(),
                message: "expected one of `fast`, `balanced`, or `thorough`".to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackConfig {
    pub default_profile: Option<String>,
    pub default_format: Option<String>,
    pub default_max_tokens: Option<u64>,
    pub mmr_lambda: Option<f64>,
    pub candidate_pool: Option<u64>,
}

impl PackConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_profile: optional_string(document, "pack", "default_profile")?,
            default_format: optional_string(document, "pack", "default_format")?,
            default_max_tokens: optional_u64(document, "pack", "default_max_tokens")?,
            mmr_lambda: optional_unit_float(document, "pack", "mmr_lambda")?,
            candidate_pool: optional_u64(document, "pack", "candidate_pool")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CurationConfig {
    pub duplicate_similarity: Option<f64>,
    pub harmful_weight: Option<f64>,
    pub decay_half_life_days: Option<u64>,
    pub specificity_min: Option<f64>,
}

impl CurationConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            duplicate_similarity: optional_unit_float(
                document,
                "curation",
                "duplicate_similarity",
            )?,
            harmful_weight: optional_nonnegative_float(document, "curation", "harmful_weight")?,
            decay_half_life_days: optional_u64(document, "curation", "decay_half_life_days")?,
            specificity_min: optional_unit_float(document, "curation", "specificity_min")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedbackConfig {
    pub harmful_per_source_per_hour: Option<u64>,
    pub harmful_burst_window_seconds: Option<u64>,
}

impl FeedbackConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            harmful_per_source_per_hour: optional_u64(
                document,
                "feedback",
                "harmful_per_source_per_hour",
            )?,
            harmful_burst_window_seconds: optional_u64(
                document,
                "feedback",
                "harmful_burst_window_seconds",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrivacyConfig {
    pub redact_secrets: Option<bool>,
    pub redaction_classes: Option<Vec<String>>,
}

impl PrivacyConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            redact_secrets: optional_bool(document, "privacy", "redact_secrets")?,
            redaction_classes: optional_string_array(document, "privacy", "redaction_classes")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustConfig {
    pub default_class: Option<String>,
    pub prompt_injection_guard: Option<bool>,
}

impl TrustConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_class: optional_string(document, "trust", "default_class")?,
            prompt_injection_guard: optional_bool(document, "trust", "prompt_injection_guard")?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigParseError {
    Toml {
        message: String,
    },
    InvalidType {
        key: String,
        expected: &'static str,
    },
    InvalidValue {
        key: String,
        value: String,
        message: String,
    },
    PathExpansion {
        key: String,
        source: PathExpansionError,
    },
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml { message } => write!(formatter, "invalid TOML config: {message}"),
            Self::InvalidType { key, expected } => {
                write!(formatter, "config key `{key}` must be {expected}")
            }
            Self::InvalidValue {
                key,
                value,
                message,
            } => write!(
                formatter,
                "config key `{key}` has invalid value `{value}`: {message}"
            ),
            Self::PathExpansion { key, source } => {
                write!(formatter, "failed to expand config path `{key}`: {source}")
            }
        }
    }
}

impl std::error::Error for ConfigParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PathExpansion { source, .. } => Some(source),
            Self::Toml { .. } | Self::InvalidType { .. } | Self::InvalidValue { .. } => None,
        }
    }
}

fn item<'a>(document: &'a DocumentMut, section: &str, key: &str) -> Option<&'a Item> {
    document.get(section).and_then(|table| table.get(key))
}

fn key_name(section: &str, key: &str) -> String {
    format!("{section}.{key}")
}

fn optional_string(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<String>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a string",
            }),
        None => Ok(None),
    }
}

fn optional_bool(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<bool>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a boolean",
            }),
        None => Ok(None),
    }
}

fn optional_u64(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<u64>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => match value.as_integer() {
            Some(integer) if integer >= 0 => Ok(Some(integer as u64)),
            Some(integer) => Err(ConfigParseError::InvalidValue {
                key: key_name(section, key),
                value: integer.to_string(),
                message: "expected a non-negative integer".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "an integer",
            }),
        },
        None => Ok(None),
    }
}

fn optional_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => match value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64))
        {
            Some(number) if number.is_finite() => Ok(Some(number)),
            Some(number) => Err(ConfigParseError::InvalidValue {
                key: key_name(section, key),
                value: number.to_string(),
                message: "expected a finite number".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a number",
            }),
        },
        None => Ok(None),
    }
}

fn optional_unit_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float(document, section, key)? {
        Some(number) if (0.0..=1.0).contains(&number) => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_name(section, key),
            value: number.to_string(),
            message: "expected a number in 0.0..=1.0".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_nonnegative_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float(document, section, key)? {
        Some(number) if number >= 0.0 => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_name(section, key),
            value: number.to_string(),
            message: "expected a non-negative number".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_path(
    document: &DocumentMut,
    section: &str,
    key: &str,
    expander: Option<&PathExpander>,
) -> Result<Option<PathBuf>, ConfigParseError> {
    let Some(raw) = optional_string(document, section, key)? else {
        return Ok(None);
    };
    match expander {
        Some(expander) => {
            expander
                .expand(&raw)
                .map(Some)
                .map_err(|source| ConfigParseError::PathExpansion {
                    key: key_name(section, key),
                    source,
                })
        }
        None => Ok(Some(PathBuf::from(raw))),
    }
}

fn optional_search_speed(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<SearchSpeed>, ConfigParseError> {
    match optional_string(document, section, key)? {
        Some(value) => value.parse().map(Some),
        None => Ok(None),
    }
}

fn optional_string_array(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<Vec<String>>, ConfigParseError> {
    let Some(value) = item(document, section, key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigParseError::InvalidType {
            key: key_name(section, key),
            expected: "an array of strings",
        });
    };

    let mut out = Vec::new();
    for entry in array.iter() {
        match entry {
            Value::String(text) => out.push(text.value().to_string()),
            _ => {
                return Err(ConfigParseError::InvalidType {
                    key: key_name(section, key),
                    expected: "an array of strings",
                });
            }
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{ConfigFile, ConfigParseError, PathExpander, SearchSpeed, optional_string_array};

    type TestResult = Result<(), String>;

    fn expect_config_error(input: &str) -> Result<ConfigParseError, String> {
        match ConfigFile::parse(input) {
            Ok(config) => Err(format!("expected parse error, got {config:?}")),
            Err(error) => Ok(error),
        }
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn parses_readme_style_config() -> TestResult {
        let input = r#"
[storage]
database_path = "~/.local/share/ee/ee.db"
index_dir = "$EE_INDEX_ROOT"
jsonl_export = false

[runtime]
daemon = false
job_budget_ms = 5000
import_batch_size = 200

[cass]
enabled = true
binary = "cass"
since = "90d"

[search]
default_speed = "balanced"
lexical_weight = 0.45
semantic_weight = 0.45
graph_weight = 0.10

[pack]
default_profile = "default"
default_format = "markdown"
default_max_tokens = 4000
mmr_lambda = 0.7
candidate_pool = 100

[curation]
duplicate_similarity = 0.92
harmful_weight = 2.5
decay_half_life_days = 60
specificity_min = 0.45

[privacy]
redact_secrets = true
redaction_classes = ["api_key", "jwt", "password"]

[trust]
default_class = "agent_assertion"
prompt_injection_guard = true
"#;
        let mut env = BTreeMap::new();
        env.insert(
            "EE_INDEX_ROOT".to_string(),
            OsString::from("/tmp/ee-indexes"),
        );
        let expander = PathExpander::with_env(Some(PathBuf::from("/home/tester")), env);

        let config = ConfigFile::parse_with_expander(input, &expander)
            .map_err(|error| format!("config should parse: {error}"))?;

        ensure_equal(
            &config.storage.database_path,
            &Some(PathBuf::from("/home/tester/.local/share/ee/ee.db")),
            "database path",
        )?;
        ensure_equal(
            &config.storage.index_dir,
            &Some(PathBuf::from("/tmp/ee-indexes")),
            "index dir",
        )?;
        ensure_equal(&config.storage.jsonl_export, &Some(false), "jsonl export")?;
        ensure_equal(&config.runtime.job_budget_ms, &Some(5000), "job budget")?;
        ensure_equal(&config.cass.binary.as_deref(), &Some("cass"), "cass binary")?;
        ensure_equal(
            &config.search.default_speed,
            &Some(SearchSpeed::Balanced),
            "search speed",
        )?;
        ensure_equal(&config.search.lexical_weight, &Some(0.45), "lexical weight")?;
        ensure_equal(
            &config.pack.default_format.as_deref(),
            &Some("markdown"),
            "pack default format",
        )?;
        ensure_equal(&config.pack.default_max_tokens, &Some(4000), "max tokens")?;
        ensure_equal(
            &config.curation.harmful_weight,
            &Some(2.5),
            "harmful weight",
        )?;
        ensure_equal(
            &config.curation.specificity_min,
            &Some(0.45),
            "specificity min",
        )?;
        ensure_equal(
            &config.privacy.redaction_classes,
            &Some(vec![
                "api_key".to_string(),
                "jwt".to_string(),
                "password".to_string(),
            ]),
            "redaction classes",
        )?;
        ensure_equal(
            &config.trust.prompt_injection_guard,
            &Some(true),
            "prompt injection guard",
        )
    }

    #[test]
    fn missing_sections_default_to_none() -> TestResult {
        let config =
            ConfigFile::parse("").map_err(|error| format!("empty config should parse: {error}"))?;

        ensure_equal(&config.storage.database_path, &None, "database path")?;
        ensure_equal(&config.runtime.daemon, &None, "runtime daemon")?;
        ensure_equal(&config.search.default_speed, &None, "search default speed")?;
        ensure_equal(
            &config.privacy.redaction_classes,
            &None,
            "redaction classes",
        )
    }

    #[test]
    fn rejects_wrong_type_for_known_key() -> TestResult {
        let error = expect_config_error("[runtime]\njob_budget_ms = \"slow\"\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidType { ref key, expected }
                    if key == "runtime.job_budget_ms" && expected == "an integer"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_unknown_search_speed() -> TestResult {
        let error = expect_config_error("[search]\ndefault_speed = \"reckless\"\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "search.default_speed"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_out_of_range_unit_weights() -> TestResult {
        let error = expect_config_error("[pack]\nmmr_lambda = 1.5\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. } if key == "pack.mmr_lambda"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_non_string_redaction_classes() -> TestResult {
        let parsed =
            "[privacy]\nredaction_classes = [\"api_key\", 7]\n".parse::<toml_edit::DocumentMut>();
        let document = parsed.map_err(|error| format!("test TOML should parse: {error}"))?;

        let error = match optional_string_array(&document, "privacy", "redaction_classes") {
            Ok(value) => return Err(format!("expected array type error, got {value:?}")),
            Err(error) => error,
        };

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidType { ref key, expected }
                    if key == "privacy.redaction_classes" && expected == "an array of strings"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn wraps_path_expansion_errors_with_config_key() -> TestResult {
        let expander = PathExpander::with_env(Some(PathBuf::from("/home/tester")), BTreeMap::new());
        let error = match ConfigFile::parse_with_expander(
            "[storage]\nindex_dir = \"$EE_MISSING\"\n",
            &expander,
        ) {
            Ok(config) => return Err(format!("expected path expansion error, got {config:?}")),
            Err(error) => error,
        };

        ensure(
            matches!(
                error,
                ConfigParseError::PathExpansion { ref key, .. } if key == "storage.index_dir"
            ),
            format!("unexpected error: {error:?}"),
        )
    }
}
