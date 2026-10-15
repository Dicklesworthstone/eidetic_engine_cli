//! Bind a SARIF repair to the analyzer's reported execution, not just the
//! command that printed its log. No shell parsing, environment expansion or
//! filesystem access is performed. These commitments live only in the
//! in-memory coverage check; arguments, environment values and paths never
//! enter a fingerprint, repair link or incident card.

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;
use std::marker::PhantomData;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::locations::{ArtifactLocation, Locations, Resolved};
use super::{Subject, charge, present};

/// Missing metadata retains the existing outer-invocation contract. Explicit
/// unknown metadata is not proof. Preserve this distinction instead of
/// letting a null overwrite an observed value or alias an empty argument list.
#[derive(Serialize)]
pub(super) enum Reported<T> {
    Missing,
    Unknown,
    Value(T),
}

impl<T> Default for Reported<T> {
    fn default() -> Self {
        Self::Missing
    }
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
        unique_map(deserializer).map(Self)
    }
}

/// Object key order is not identity, but duplicate keys are ambiguous. Share
/// the same original-byte guard between environments and URI-base tables.
pub(super) fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct UniqueMapVisitor<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for UniqueMapVisitor<T> {
        type Value = BTreeMap<String, T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object with unique property names")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, T>()? {
                match values.entry(name) {
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    Entry::Occupied(_) => {
                        return Err(serde::de::Error::custom("duplicate property name"));
                    }
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InvocationScope {
    #[serde(default)]
    command_line: Reported<String>,
    #[serde(default)]
    arguments: Reported<Vec<String>>,
    #[serde(default)]
    environment_variables: Reported<Environment>,
    #[serde(default)]
    executable_location: Reported<ArtifactLocation>,
    #[serde(default)]
    working_directory: Reported<ArtifactLocation>,
    #[serde(default)]
    response_files: Reported<Vec<ArtifactLocation>>,
    // A pathname alone cannot bind bytes consumed through standard input.
    #[serde(default, deserialize_with = "present")]
    stdin: bool,
}

impl InvocationScope {
    pub(super) fn key(
        &self,
        locations: &Locations<'_>,
        remaining: &mut usize,
    ) -> Result<Option<Subject>, ()> {
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
            || self.response_files.is_unknown()
            || self.stdin
            || matches!(&self.command_line, Reported::Value(command) if command.trim().is_empty())
            || matches!(&self.response_files, Reported::Value(files) if !files.is_empty())
        {
            return Ok(None);
        }
        // Typed serialization preserves argv boundaries, absent/empty values,
        // and all supplied command representations. BTreeMap makes environment
        // ordering immaterial without erasing duplicate keys at ingestion.
        // Transient run fields (timestamps, exit status, process ID) are not
        // scope: a later successful invocation must be able to verify a failure.
        let (Ok(executable), Ok(directory)) = (
            execution_location(&self.executable_location, locations, false),
            execution_location(&self.working_directory, locations, true),
        ) else {
            return Ok(None);
        };
        let encoded = serde_json::to_vec(&(
            &self.command_line,
            &self.arguments,
            &self.environment_variables,
            executable,
            directory,
            matches!(&self.response_files, Reported::Value(_)),
        ))
        .map_err(|_| ())?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"ee.sarif.invocation.v2\0");
        hash.update(&(encoded.len() as u64).to_le_bytes());
        hash.update(&encoded);
        Ok(Some(*hash.finalize().as_bytes()))
    }
}

fn execution_location(
    location: &Reported<ArtifactLocation>,
    locations: &Locations<'_>,
    absolute: bool,
) -> Result<Reported<Resolved>, ()> {
    match location {
        Reported::Missing => Ok(Reported::Missing),
        Reported::Unknown => Err(()),
        Reported::Value(location) => {
            let resolved = locations.resolve(location).ok_or(())?;
            if absolute && !resolved.absolute {
                return Err(());
            }
            Ok(Reported::Value(resolved))
        }
    }
}
