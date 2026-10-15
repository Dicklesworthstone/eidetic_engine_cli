//! Bind a SARIF repair to the analyzer's reported execution, not just the
//! command that printed its log. No shell parsing, environment expansion or
//! filesystem access is performed. These commitments live only in the
//! in-memory coverage check; arguments, environment values and paths never
//! enter a fingerprint, repair link or incident card.

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::{Subject, charge, present};

/// Missing metadata retains the existing outer-invocation contract. Explicit
/// unknown metadata is not proof. Preserve this distinction instead of
/// letting a null overwrite an observed value or alias an empty argument list.
#[derive(Default, Serialize)]
enum Reported<T> {
    #[default]
    Missing,
    Unknown,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Reported<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer)
            .map(|value| value.map_or(Self::Unknown, Self::Value))
    }
}

impl<T> Reported<T> {
    fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

#[derive(Serialize)]
struct Environment(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EnvironmentVisitor;
        impl<'de> Visitor<'de> for EnvironmentVisitor {
            type Value = Environment;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of unique environment names to string values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((name, value)) = map.next_entry::<String, String>()? {
                    match values.entry(name) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(_) => {
                            return Err(serde::de::Error::custom("duplicate environment name"));
                        }
                    }
                }
                Ok(Environment(values))
            }
        }
        deserializer.deserialize_map(EnvironmentVisitor)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiteralLocation {
    uri: Option<String>,
    // Referenced executable/working-directory locations need a resolver.
    // Presence, including null, cannot silently mean an inline literal URI.
    #[serde(default, deserialize_with = "present")]
    uri_base_id: bool,
    #[serde(default, deserialize_with = "present")]
    index: bool,
}

impl LiteralLocation {
    fn admitted(&self, absolute: bool) -> bool {
        !self.uri_base_id
            && !self.index
            && self.uri.as_deref().is_some_and(|uri| {
                !uri.is_empty()
                    && !uri
                        .chars()
                        .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
                    && (!absolute || absolute_uri(uri))
            })
    }
}

/// Recognize a literal URI scheme, not a local path. Do not normalize, decode,
/// join paths or resolve symlinks: exact bytes deliberately under-approximate
/// equivalence. In particular, different analysis roots must never collapse.
fn absolute_uri(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InvocationScope {
    #[serde(default)]
    command_line: Reported<String>,
    #[serde(default)]
    arguments: Reported<Vec<String>>,
    #[serde(default)]
    environment_variables: Reported<Environment>,
    #[serde(default)]
    executable_location: Reported<LiteralLocation>,
    #[serde(default)]
    working_directory: Reported<LiteralLocation>,
    #[serde(default)]
    response_files: Reported<Vec<LiteralLocation>>,
    // A pathname alone cannot bind bytes consumed through standard input.
    #[serde(default, deserialize_with = "present")]
    stdin: bool,
}

impl InvocationScope {
    pub(super) fn key(&self, remaining: &mut usize) -> Result<Option<Subject>, ()> {
        if let Reported::Value(arguments) = &self.arguments {
            charge(remaining, arguments.len())?;
        }
        if let Reported::Value(environment) = &self.environment_variables {
            charge(remaining, environment.0.len())?;
        }
        if let Reported::Value(files) = &self.response_files {
            charge(remaining, files.len())?;
        }
        if self.command_line.is_unknown()
            || self.arguments.is_unknown()
            || self.environment_variables.is_unknown()
            || self.executable_location.is_unknown()
            || self.working_directory.is_unknown()
            || self.response_files.is_unknown()
            || self.stdin
            || matches!(&self.command_line, Reported::Value(command) if command.trim().is_empty())
            || matches!(&self.response_files, Reported::Value(files) if !files.is_empty())
            || matches!(
                &self.executable_location,
                Reported::Value(location) if !location.admitted(false)
            )
            || matches!(
                &self.working_directory,
                Reported::Value(location) if !location.admitted(true)
            )
        {
            return Ok(None);
        }
        // Typed serialization preserves argv boundaries, absent/empty values,
        // and all supplied command representations. BTreeMap makes environment
        // ordering immaterial without erasing duplicate keys at ingestion.
        // Transient run fields (timestamps, exit status, process ID) are not
        // scope: a later successful invocation must be able to verify a failure.
        let encoded = serde_json::to_vec(self).map_err(|_| ())?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"ee.sarif.invocation.v1\0");
        hash.update(&(encoded.len() as u64).to_le_bytes());
        hash.update(&encoded);
        Ok(Some(*hash.finalize().as_bytes()))
    }
}
