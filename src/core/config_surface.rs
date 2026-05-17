//! First-class config inspection and mutation surfaces.
//!
//! This module keeps CLI parsing separate from workspace config mechanics:
//! callers provide a workspace, an optional config path, and graph keys. The
//! implementation preserves TOML with `toml_edit` and only writes the requested
//! workspace config path.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::{DocumentMut, Item};

use crate::config::{
    ConfigFile, ConfigLayers, ConfigShowEntry, ConfigShowReport, EnvironmentConfigError,
    GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY, GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
    GRAPH_CURATE_ONION_DECAY_MAX_KEY, GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
    GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY, GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
    GRAPH_FEATURE_PACK_DNA_ENABLED_KEY, GRAPH_FEATURE_PPR_ENABLED_KEY,
    GRAPH_FEATURE_PROXIMITY_ENABLED_KEY, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
    GRAPH_FEATURE_SKYLINE_ENABLED_KEY, GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY,
    GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY, GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
    GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY, GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
    GRAPH_HITS_PROFILE_BOOST_KEY, GRAPH_PACK_DNA_MAX_EDGES_KEY, GRAPH_PACK_DNA_MAX_ITEMS_KEY,
    GRAPH_PPR_ALPHA_KEY, PathExpander, built_in_config, config_from_env, merge_config,
};

pub const CONFIG_GET_SCHEMA_V1: &str = "ee.config.get.v1";
pub const CONFIG_SET_SCHEMA_V1: &str = "ee.config.set.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigSurfaceOptions {
    pub workspace_root: PathBuf,
    pub config_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigGetReport {
    pub schema: &'static str,
    pub key: &'static str,
    pub value: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSetReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub key: &'static str,
    pub value: String,
    pub before: Option<String>,
    pub config_path: String,
    pub path_redaction: &'static str,
    pub config_exists: bool,
    pub would_write: bool,
    pub applied: bool,
    pub repair: Option<&'static str>,
    pub planned_toml: String,
}

#[derive(Debug)]
pub enum ConfigSurfaceError {
    UnknownKey {
        key: String,
    },
    InvalidPattern {
        pattern: String,
    },
    InvalidValue {
        key: &'static str,
        value: String,
        expected: &'static str,
    },
    Environment {
        source: EnvironmentConfigError,
    },
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ConfigSurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey { key } => write!(formatter, "unknown config key `{key}`"),
            Self::InvalidPattern { pattern } => {
                write!(formatter, "unsupported config pattern `{pattern}`")
            }
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                formatter,
                "invalid value `{value}` for `{key}`; expected {expected}"
            ),
            Self::Environment { source } => write!(formatter, "could not load config: {source}"),
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "could not read config `{}`: {source}",
                    path.display()
                )
            }
            Self::Parse { path, message } => {
                write!(
                    formatter,
                    "could not parse config `{}`: {message}",
                    path.display()
                )
            }
            Self::Write { path, source } => {
                write!(
                    formatter,
                    "could not write config `{}`: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ConfigSurfaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Environment { source } => Some(source),
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::UnknownKey { .. }
            | Self::InvalidPattern { .. }
            | Self::InvalidValue { .. }
            | Self::Parse { .. } => None,
        }
    }
}

#[must_use]
pub fn graph_config_keys() -> &'static [&'static str] {
    &[
        GRAPH_PPR_ALPHA_KEY,
        GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
        GRAPH_CURATE_ONION_DECAY_MAX_KEY,
        GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
        GRAPH_HITS_PROFILE_BOOST_KEY,
        GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
        GRAPH_PACK_DNA_MAX_ITEMS_KEY,
        GRAPH_PACK_DNA_MAX_EDGES_KEY,
        GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
        GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
        GRAPH_FEATURE_PPR_ENABLED_KEY,
        GRAPH_FEATURE_PACK_DNA_ENABLED_KEY,
        GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
        GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY,
        GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY,
        GRAPH_FEATURE_PROXIMITY_ENABLED_KEY,
        GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
        GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
        GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
        GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
    ]
}

pub fn show_config(
    options: &ConfigSurfaceOptions,
    pattern: Option<&str>,
) -> Result<ConfigShowReport, ConfigSurfaceError> {
    let mut report = merged_config(options)?.to_show_report();
    if let Some(pattern) = pattern {
        report.entries = filter_entries(report.entries, pattern)?;
        report.entry_count = report.entries.len();
    }
    Ok(report)
}

pub fn get_config(
    options: &ConfigSurfaceOptions,
    key: &str,
) -> Result<ConfigGetReport, ConfigSurfaceError> {
    let spec = graph_key_spec(key).ok_or_else(|| ConfigSurfaceError::UnknownKey {
        key: key.to_owned(),
    })?;
    let report = show_config(options, Some(spec.key))?;
    let entry =
        report
            .entries
            .into_iter()
            .next()
            .ok_or_else(|| ConfigSurfaceError::UnknownKey {
                key: key.to_owned(),
            })?;
    Ok(ConfigGetReport {
        schema: CONFIG_GET_SCHEMA_V1,
        key: entry.key,
        value: entry.value,
        source: entry.source,
    })
}

pub fn set_config(
    options: &ConfigSurfaceOptions,
    key: &str,
    value: &str,
    dry_run: bool,
) -> Result<ConfigSetReport, ConfigSurfaceError> {
    let spec = graph_key_spec(key).ok_or_else(|| ConfigSurfaceError::UnknownKey {
        key: key.to_owned(),
    })?;
    let scalar = parse_graph_value(spec, value)?;
    let path = effective_config_path(&options.workspace_root, options.config_path.as_deref());
    let (config_exists, input) = read_optional_config(&path)?;
    let mut document =
        input
            .parse::<DocumentMut>()
            .map_err(|source| ConfigSurfaceError::Parse {
                path: path.clone(),
                message: source.to_string(),
            })?;
    let before = item_for_path(&document, spec.path).map(item_value_for_report);
    let after = scalar.report_value();
    set_toml_value(&mut document, spec.path, scalar);
    let planned_toml = document.to_string();
    ConfigFile::parse(&planned_toml).map_err(|source| ConfigSurfaceError::Parse {
        path: path.clone(),
        message: source.to_string(),
    })?;

    let would_write = before.as_deref() != Some(after.as_str());
    if !dry_run && would_write {
        ensure_no_config_symlink_components(&path, "write").map_err(|source| {
            ConfigSurfaceError::Write {
                path: path.clone(),
                source,
            }
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigSurfaceError::Write {
                path: path.clone(),
                source,
            })?;
        }
        ensure_no_config_symlink_components(&path, "write").map_err(|source| {
            ConfigSurfaceError::Write {
                path: path.clone(),
                source,
            }
        })?;
        ensure_config_write_path_is_regular_or_missing(&path).map_err(|source| {
            ConfigSurfaceError::Write {
                path: path.clone(),
                source,
            }
        })?;
        let mut temp_path = path.clone();
        temp_path.set_extension("tmp");
        ensure_no_config_symlink_components(&temp_path, "write temp").map_err(|source| {
            ConfigSurfaceError::Write {
                path: temp_path.clone(),
                source,
            }
        })?;
        ensure_config_temp_path_is_regular_or_missing(&temp_path).map_err(|source| {
            ConfigSurfaceError::Write {
                path: temp_path.clone(),
                source,
            }
        })?;
        {
            use std::io::Write;
            let mut file =
                fs::File::create(&temp_path).map_err(|source| ConfigSurfaceError::Write {
                    path: temp_path.clone(),
                    source,
                })?;
            file.write_all(planned_toml.as_bytes()).map_err(|source| {
                ConfigSurfaceError::Write {
                    path: temp_path.clone(),
                    source,
                }
            })?;
            file.sync_data()
                .map_err(|source| ConfigSurfaceError::Write {
                    path: temp_path.clone(),
                    source,
                })?;
        }
        fs::rename(&temp_path, &path).map_err(|source| ConfigSurfaceError::Write {
            path: path.clone(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_data();
            }
        }
    }

    Ok(ConfigSetReport {
        schema: CONFIG_SET_SCHEMA_V1,
        command: "config set",
        dry_run,
        key: spec.key,
        value: after,
        before,
        config_path: path.display().to_string(),
        path_redaction: "operator_requested_config_path",
        config_exists,
        would_write,
        applied: !dry_run && would_write,
        repair: if dry_run && would_write {
            Some("Rerun without `--dry-run` to write .ee/config.toml.")
        } else {
            None
        },
        planned_toml,
    })
}

fn merged_config(
    options: &ConfigSurfaceOptions,
) -> Result<crate::config::MergedConfig, ConfigSurfaceError> {
    let expander = PathExpander::from_process_env();
    let defaults =
        built_in_config(&expander).map_err(|source| ConfigSurfaceError::Environment { source })?;
    let environment = config_from_env(&process_env(), &expander)
        .map_err(|source| ConfigSurfaceError::Environment { source })?;
    let project = read_project_config(options, &expander)?.unwrap_or_else(ConfigFile::default);
    let mut layers = ConfigLayers::with_defaults(defaults);
    layers.environment = environment;
    layers.project = project;
    Ok(merge_config(&layers))
}

fn read_project_config(
    options: &ConfigSurfaceOptions,
    expander: &PathExpander,
) -> Result<Option<ConfigFile>, ConfigSurfaceError> {
    let path = effective_config_path(&options.workspace_root, options.config_path.as_deref());
    match read_optional_config_contents(&path)? {
        Some(contents) => ConfigFile::parse_with_expander(&contents, expander)
            .map(Some)
            .map_err(|source| ConfigSurfaceError::Parse {
                path,
                message: source.to_string(),
            }),
        None => Ok(None),
    }
}

fn process_env() -> BTreeMap<String, std::ffi::OsString> {
    std::env::vars_os()
        .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
        .collect()
}

fn filter_entries(
    entries: Vec<ConfigShowEntry>,
    pattern: &str,
) -> Result<Vec<ConfigShowEntry>, ConfigSurfaceError> {
    if pattern == "*" {
        return Ok(entries);
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        if prefix.is_empty() {
            return Err(ConfigSurfaceError::InvalidPattern {
                pattern: pattern.to_owned(),
            });
        }
        let dotted_prefix = format!("{prefix}.");
        return Ok(entries
            .into_iter()
            .filter(|entry| entry.key.starts_with(&dotted_prefix))
            .collect());
    }
    if graph_key_spec(pattern).is_some() {
        return Ok(entries
            .into_iter()
            .filter(|entry| entry.key == pattern)
            .collect());
    }
    Err(ConfigSurfaceError::InvalidPattern {
        pattern: pattern.to_owned(),
    })
}

fn effective_config_path(workspace_root: &Path, config_path: Option<&Path>) -> PathBuf {
    match config_path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => workspace_root.join(path),
        None => workspace_root.join(".ee").join("config.toml"),
    }
}

fn read_optional_config(path: &Path) -> Result<(bool, String), ConfigSurfaceError> {
    match read_optional_config_contents(path)? {
        Some(contents) => Ok((true, contents)),
        None => Ok((false, String::new())),
    }
}

fn read_optional_config_contents(path: &Path) -> Result<Option<String>, ConfigSurfaceError> {
    ensure_no_config_symlink_components(path, "read").map_err(|source| {
        ConfigSurfaceError::Read {
            path: path.to_path_buf(),
            source,
        }
    })?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(ConfigSurfaceError::Read {
                path: path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "config path is not a regular file",
                ),
            });
        }
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(source) => {
            return Err(ConfigSurfaceError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|source| ConfigSurfaceError::Read {
            path: path.to_path_buf(),
            source,
        })
}

fn ensure_config_write_path_is_regular_or_missing(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to write config `{}` because it is not a regular file",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_config_temp_path_is_regular_or_missing(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to write config temp `{}` because it is not a regular file",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_no_config_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), io::Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to {operation} config `{}` through symlinked path component `{}`",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GraphValueKind {
    Bool,
    UnitFloat,
    PositiveFloat,
    NonNegativeFloat,
    UnsignedInteger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GraphKeySpec {
    key: &'static str,
    path: &'static [&'static str],
    kind: GraphValueKind,
}

fn graph_key_spec(key: &str) -> Option<GraphKeySpec> {
    match key {
        GRAPH_PPR_ALPHA_KEY => Some(GraphKeySpec {
            key: GRAPH_PPR_ALPHA_KEY,
            path: &["graph", "ppr", "alpha"],
            kind: GraphValueKind::UnitFloat,
        }),
        GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY => Some(GraphKeySpec {
            key: GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
            path: &["graph", "health", "contradiction_threshold"],
            kind: GraphValueKind::UnitFloat,
        }),
        GRAPH_CURATE_ONION_DECAY_MAX_KEY => Some(GraphKeySpec {
            key: GRAPH_CURATE_ONION_DECAY_MAX_KEY,
            path: &["graph", "curate", "onion_decay_max"],
            kind: GraphValueKind::PositiveFloat,
        }),
        GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY => Some(GraphKeySpec {
            key: GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
            path: &["graph", "curate", "articulation_protection_multiplier"],
            kind: GraphValueKind::UnitFloat,
        }),
        GRAPH_HITS_PROFILE_BOOST_KEY => Some(GraphKeySpec {
            key: GRAPH_HITS_PROFILE_BOOST_KEY,
            path: &["graph", "hits", "profile_boost"],
            kind: GraphValueKind::NonNegativeFloat,
        }),
        GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY => Some(GraphKeySpec {
            key: GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
            path: &["graph", "causal", "min_cost_normalization"],
            kind: GraphValueKind::PositiveFloat,
        }),
        GRAPH_PACK_DNA_MAX_ITEMS_KEY => Some(GraphKeySpec {
            key: GRAPH_PACK_DNA_MAX_ITEMS_KEY,
            path: &["graph", "pack_dna", "max_items"],
            kind: GraphValueKind::UnsignedInteger,
        }),
        GRAPH_PACK_DNA_MAX_EDGES_KEY => Some(GraphKeySpec {
            key: GRAPH_PACK_DNA_MAX_EDGES_KEY,
            path: &["graph", "pack_dna", "max_edges"],
            kind: GraphValueKind::UnsignedInteger,
        }),
        GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY => Some(GraphKeySpec {
            key: GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
            path: &["graph", "gomory_hu", "sample_threshold"],
            kind: GraphValueKind::UnsignedInteger,
        }),
        GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY => Some(GraphKeySpec {
            key: GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
            path: &["graph", "gomory_hu", "sample_size"],
            kind: GraphValueKind::UnsignedInteger,
        }),
        GRAPH_FEATURE_PPR_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_PPR_ENABLED_KEY,
            path: &["graph", "feature", "ppr", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_PACK_DNA_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_PACK_DNA_ENABLED_KEY,
            path: &["graph", "feature", "pack_dna", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
            path: &["graph", "feature", "causal_explain", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY,
            path: &["graph", "feature", "structural_health", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY,
            path: &["graph", "feature", "structural_decay", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_PROXIMITY_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_PROXIMITY_ENABLED_KEY,
            path: &["graph", "feature", "proximity", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
            path: &["graph", "feature", "revision_dominance", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_SKYLINE_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
            path: &["graph", "feature", "skyline", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
            path: &["graph", "feature", "load_bearing", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY => Some(GraphKeySpec {
            key: GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
            path: &["graph", "feature", "hits_profiles", "enabled"],
            kind: GraphValueKind::Bool,
        }),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TomlScalar {
    Bool(bool),
    Float(f64),
    Integer(i64),
}

impl TomlScalar {
    fn report_value(self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
        }
    }
}

fn parse_graph_value(spec: GraphKeySpec, raw: &str) -> Result<TomlScalar, ConfigSurfaceError> {
    match spec.kind {
        GraphValueKind::Bool => match raw {
            "true" => Ok(TomlScalar::Bool(true)),
            "false" => Ok(TomlScalar::Bool(false)),
            _ => Err(invalid_value(spec, raw, "`true` or `false`")),
        },
        GraphValueKind::UnitFloat => {
            let value = parse_finite_float(spec, raw, "a finite number in the range 0.0..=1.0")?;
            if (0.0..=1.0).contains(&value) {
                Ok(TomlScalar::Float(value))
            } else {
                Err(invalid_value(
                    spec,
                    raw,
                    "a finite number in the range 0.0..=1.0",
                ))
            }
        }
        GraphValueKind::PositiveFloat => {
            let value = parse_finite_float(spec, raw, "a finite number greater than 0.0")?;
            if value > 0.0 {
                Ok(TomlScalar::Float(value))
            } else {
                Err(invalid_value(spec, raw, "a finite number greater than 0.0"))
            }
        }
        GraphValueKind::NonNegativeFloat => {
            let value =
                parse_finite_float(spec, raw, "a finite number greater than or equal to 0.0")?;
            if value >= 0.0 {
                Ok(TomlScalar::Float(value))
            } else {
                Err(invalid_value(
                    spec,
                    raw,
                    "a finite number greater than or equal to 0.0",
                ))
            }
        }
        GraphValueKind::UnsignedInteger => {
            let value = raw
                .parse::<u64>()
                .map_err(|_| invalid_value(spec, raw, "a non-negative integer"))?;
            if value <= i64::MAX as u64 {
                Ok(TomlScalar::Integer(value as i64))
            } else {
                Err(invalid_value(
                    spec,
                    raw,
                    "a non-negative integer <= i64::MAX",
                ))
            }
        }
    }
}

fn parse_finite_float(
    spec: GraphKeySpec,
    raw: &str,
    expected: &'static str,
) -> Result<f64, ConfigSurfaceError> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| invalid_value(spec, raw, expected))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid_value(spec, raw, expected))
    }
}

fn invalid_value(spec: GraphKeySpec, raw: &str, expected: &'static str) -> ConfigSurfaceError {
    ConfigSurfaceError::InvalidValue {
        key: spec.key,
        value: raw.to_owned(),
        expected,
    }
}

fn item_for_path<'a>(document: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let mut item = document.as_table().get(path.first()?)?;
    for key in &path[1..] {
        item = item.get(*key)?;
    }
    Some(item)
}

fn item_value_for_report(item: &Item) -> String {
    if let Some(value) = item.as_float() {
        value.to_string()
    } else if let Some(value) = item.as_integer() {
        value.to_string()
    } else if let Some(value) = item.as_bool() {
        value.to_string()
    } else {
        item.type_name().to_string()
    }
}

fn set_toml_value(document: &mut DocumentMut, path: &[&str], value: TomlScalar) {
    if path.is_empty() {
        return;
    }
    let mut current = &mut document[path[0]];
    for &segment in &path[1..] {
        current = &mut current[segment];
    }
    *current = match value {
        TomlScalar::Bool(value) => toml_edit::value(value),
        TomlScalar::Float(value) => toml_edit::value(value),
        TomlScalar::Integer(value) => toml_edit::value(value),
    };
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigSurfaceOptions, ensure_config_write_path_is_regular_or_missing, get_config,
        graph_config_keys, set_config, show_config,
    };
    use std::fs;

    type TestResult = Result<(), String>;

    fn workspace() -> Result<tempfile::TempDir, String> {
        tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))
    }

    fn options(root: &std::path::Path) -> ConfigSurfaceOptions {
        ConfigSurfaceOptions {
            workspace_root: root.to_path_buf(),
            config_path: None,
        }
    }

    #[test]
    fn graph_show_filters_to_graph_namespace() -> TestResult {
        let temp = workspace()?;
        let report = show_config(&options(temp.path()), Some("graph.*"))
            .map_err(|error| error.to_string())?;
        let keys = report
            .entries
            .iter()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        if keys == graph_config_keys() {
            Ok(())
        } else {
            Err(format!("unexpected graph keys: {keys:?}"))
        }
    }

    #[test]
    fn graph_get_reads_default_with_source() -> TestResult {
        let temp = workspace()?;
        let report = get_config(&options(temp.path()), "graph.ppr.alpha")
            .map_err(|error| error.to_string())?;
        if report.value != "0.3" {
            return Err(format!("unexpected graph.ppr.alpha: {}", report.value));
        }
        if report.source != "default" {
            return Err(format!("unexpected source: {}", report.source));
        }
        Ok(())
    }

    #[test]
    fn graph_set_round_trips_all_supported_keys() -> TestResult {
        let temp = workspace()?;
        let samples = [
            ("graph.ppr.alpha", "0.0"),
            ("graph.health.contradiction_threshold", "1.0"),
            ("graph.curate.onion_decay_max", "4.5"),
            ("graph.curate.articulation_protection_multiplier", "0.75"),
            ("graph.hits.profile_boost", "0.0"),
            ("graph.causal.min_cost_normalization", "2.0"),
            ("graph.pack_dna.max_items", "12"),
            ("graph.pack_dna.max_edges", "34"),
            ("graph.gomory_hu.sample_threshold", "600"),
            ("graph.gomory_hu.sample_size", "150"),
            ("graph.feature.ppr.enabled", "true"),
            ("graph.feature.pack_dna.enabled", "true"),
            ("graph.feature.causal_explain.enabled", "false"),
            ("graph.feature.structural_health.enabled", "true"),
            ("graph.feature.structural_decay.enabled", "true"),
            ("graph.feature.proximity.enabled", "false"),
            ("graph.feature.revision_dominance.enabled", "false"),
            ("graph.feature.skyline.enabled", "true"),
            ("graph.feature.load_bearing.enabled", "false"),
            ("graph.feature.hits_profiles.enabled", "true"),
        ];

        for (key, value) in samples {
            let report = set_config(&options(temp.path()), key, value, false)
                .map_err(|error| format!("set {key}: {error}"))?;
            if !report.applied && report.before.as_deref() != Some(report.value.as_str()) {
                return Err(format!("{key} did not apply or report idempotence"));
            }
            let observed = get_config(&options(temp.path()), key)
                .map_err(|error| format!("get {key}: {error}"))?;
            if observed.value != report.value {
                return Err(format!(
                    "{key}: expected {}, got {}",
                    report.value, observed.value
                ));
            }
            if observed.source != "project" {
                return Err(format!("{key}: unexpected source {}", observed.source));
            }
        }
        Ok(())
    }

    #[test]
    fn graph_set_rejects_invalid_ranges() -> TestResult {
        let temp = workspace()?;
        let error = match set_config(&options(temp.path()), "graph.ppr.alpha", "1.5", false) {
            Ok(report) => return Err(format!("invalid alpha unexpectedly succeeded: {report:?}")),
            Err(error) => error.to_string(),
        };
        if error.contains("0.0..=1.0") {
            Ok(())
        } else {
            Err(format!("unexpected error: {error}"))
        }
    }

    #[test]
    fn graph_set_rejects_invalid_feature_flag_bool() -> TestResult {
        let temp = workspace()?;
        let flag_keys = graph_config_keys()
            .iter()
            .copied()
            .filter(|key| key.starts_with("graph.feature.") && key.ends_with(".enabled"))
            .collect::<Vec<_>>();

        if flag_keys.len() != 10 {
            return Err(format!(
                "expected 10 graph feature flags, got {flag_keys:?}"
            ));
        }

        for key in flag_keys {
            let error = match set_config(&options(temp.path()), key, "yes", false) {
                Ok(report) => {
                    return Err(format!(
                        "invalid graph feature flag {key} unexpectedly succeeded: {report:?}"
                    ));
                }
                Err(error) => error.to_string(),
            };
            if !error.contains("`true` or `false`") {
                return Err(format!("{key}: unexpected error: {error}"));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn graph_set_rejects_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = workspace()?;
        let real_metadata = temp.path().join("real-ee");
        fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        symlink(&real_metadata, temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let error = set_config(&options(temp.path()), "graph.ppr.alpha", "0.5", false)
            .expect_err("symlinked .ee parent should reject config set")
            .to_string();
        if !error.contains("symlinked path component") {
            return Err(format!("unexpected symlink error: {error}"));
        }
        if real_metadata.join("config.toml").exists() {
            return Err("config set wrote through symlinked .ee parent".to_string());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn graph_get_rejects_symlinked_config_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = workspace()?;
        let config_dir = temp.path().join(".ee");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        fs::write(
            &outside_config,
            "[graph.ppr]\nalpha = 0.9\n[graph.feature.ppr]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(&outside_config, config_dir.join("config.toml"))
            .map_err(|error| error.to_string())?;

        let error = get_config(&options(temp.path()), "graph.ppr.alpha")
            .expect_err("symlinked config file should reject config get")
            .to_string();
        if error.contains("symlinked path component") {
            Ok(())
        } else {
            Err(format!("unexpected symlink error: {error}"))
        }
    }

    #[test]
    fn graph_get_rejects_non_regular_config_path() -> TestResult {
        let temp = workspace()?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let error = get_config(&options(temp.path()), "graph.ppr.alpha")
            .expect_err("directory config path should reject config get")
            .to_string();
        if error.contains("not a regular file") {
            Ok(())
        } else {
            Err(format!("unexpected non-regular config error: {error}"))
        }
    }

    #[test]
    fn config_write_preflight_rejects_non_regular_final_path() -> TestResult {
        let temp = workspace()?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let error = ensure_config_write_path_is_regular_or_missing(&config_path)
            .expect_err("write preflight should reject a directory config path");
        let message = error.to_string();
        if !message.contains("not a regular file") {
            return Err(format!("unexpected non-regular config error: {message}"));
        }
        if !config_path.is_dir() {
            return Err(
                "write preflight should leave non-regular config path untouched".to_owned(),
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn graph_set_rejects_symlinked_temp_config_before_write() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = workspace()?;
        let config_dir = temp.path().join(".ee");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        fs::write(&outside_config, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_config, config_dir.join("config.tmp"))
            .map_err(|error| error.to_string())?;

        let error = set_config(&options(temp.path()), "graph.ppr.alpha", "0.5", false)
            .expect_err("symlinked config temp path should reject config set")
            .to_string();
        if !error.contains("symlinked path component") {
            return Err(format!("unexpected symlink temp error: {error}"));
        }
        let outside_after =
            fs::read_to_string(&outside_config).map_err(|error| error.to_string())?;
        if outside_after != "outside sentinel" {
            return Err("config set must not write through a symlinked temp path".to_owned());
        }
        if config_dir.join("config.toml").exists() {
            return Err("config final path should not be created after temp rejection".to_owned());
        }
        Ok(())
    }

    #[test]
    fn graph_set_rejects_non_regular_temp_config_before_write() -> TestResult {
        let temp = workspace()?;
        let config_dir = temp.path().join(".ee");
        let temp_path = config_dir.join("config.tmp");
        fs::create_dir_all(&temp_path).map_err(|error| error.to_string())?;

        let error = set_config(&options(temp.path()), "graph.ppr.alpha", "0.5", false)
            .expect_err("directory config temp path should reject config set")
            .to_string();
        if !error.contains("not a regular file") {
            return Err(format!("unexpected non-regular temp error: {error}"));
        }
        if !temp_path.is_dir() {
            return Err("config set should leave non-regular temp path untouched".to_owned());
        }
        if config_dir.join("config.toml").exists() {
            return Err("config final path should not be created after temp rejection".to_owned());
        }
        Ok(())
    }
}
