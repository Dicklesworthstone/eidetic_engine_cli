//! Resolve the bounded, inline SARIF artifact namespace once for both result
//! subjects and analyzer execution roots. Index numbers and base-ID spellings
//! are not file identities. Never read local files, expand environment names,
//! fetch external properties, or treat an archive member as a top-level file.
//!
//! URI bases follow the constrained concatenation in SARIF 2.1.0 section
//! 3.14.14. Other URI equivalences (case, escapes, dot segments, symlinks) are
//! deliberately not inferred. An unresolved reference preserves diagnostics
//! but cannot supply repair coverage.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::context::Reported;
use super::present;

const MAX_BASE_DEPTH: usize = 16;
const MAX_RESOLVED_URI_BYTES: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArtifactLocation {
    #[serde(default)]
    uri: Reported<String>,
    #[serde(default)]
    uri_base_id: Reported<String>,
    #[serde(default)]
    index: Reported<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Artifact {
    location: Option<ArtifactLocation>,
    #[serde(default)]
    parent_index: Reported<i64>,
    #[serde(default, deserialize_with = "present")]
    offset: bool,
}

#[derive(Eq, PartialEq, Serialize)]
pub(super) struct Resolved {
    pub(super) uri: String,
    pub(super) absolute: bool,
}

pub(super) struct Locations<'a> {
    pub(super) artifacts: &'a [Artifact],
    pub(super) bases: &'a BTreeMap<String, ArtifactLocation>,
}

impl Locations<'_> {
    pub(super) fn resolve(&self, location: &ArtifactLocation) -> Option<Resolved> {
        let Some(index) = array_index(&location.index).ok()? else {
            let resolved = self.inline(location)?;
            // Without an index, a relative name cannot distinguish an archive
            // member from an ordinary file when the inventory includes nested
            // artifacts. An explicit absolute URI still identifies a root.
            if !resolved.absolute
                && self.artifacts.iter().any(|artifact| {
                    artifact.offset || !matches!(array_index(&artifact.parent_index), Ok(None))
                })
            {
                return None;
            }
            return Some(resolved);
        };
        let artifact = self.artifacts.get(index)?;
        if array_index(&artifact.parent_index).ok()?.is_some() || artifact.offset {
            return None;
        }
        let target = artifact.location.as_ref()?;
        // An artifact's own location may repeat its position, not redirect to
        // another entry. Do not recurse through arbitrary index chains.
        if array_index(&target.index)
            .ok()?
            .is_some_and(|own| own != index)
        {
            return None;
        }
        let resolved = self.inline(target)?;
        match (&location.uri, &location.uri_base_id) {
            (Reported::Missing, Reported::Missing) => Some(resolved),
            (Reported::Value(_), _) => {
                // Both representations, when supplied, must denote the same
                // artifact. Never prefer a convenient URI over a bad index.
                (self.inline(location)? == resolved).then_some(resolved)
            }
            _ => None,
        }
    }

    fn inline(&self, location: &ArtifactLocation) -> Option<Resolved> {
        let Reported::Value(uri) = &location.uri else {
            return None;
        };
        if !literal_uri(uri) || uri.len() > MAX_RESOLVED_URI_BYTES {
            return None;
        }
        let absolute = absolute_uri(uri);
        if absolute {
            return matches!(location.uri_base_id, Reported::Missing).then(|| Resolved {
                uri: uri.clone(),
                absolute: true,
            });
        }
        // Network/absolute-path references need URI component replacement,
        // not base concatenation. They are outside this proof subset.
        if uri.starts_with(['/', '?', '#']) {
            return None;
        }
        let mut base_id = match &location.uri_base_id {
            Reported::Missing => {
                return Some(Resolved {
                    uri: uri.clone(),
                    absolute: false,
                });
            }
            Reported::Value(id) => id.as_str(),
            Reported::Unknown => return None,
        };
        let mut parts = vec![uri.as_str()];
        let mut length = uri.len();
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_BASE_DEPTH {
            if !seen.insert(base_id) {
                return None;
            }
            let base = self.bases.get(base_id)?;
            // External/index-only base declarations cannot substitute for an
            // observed root URI. Omitted/redacted roots remain unproven.
            if array_index(&base.index).ok()?.is_some() {
                return None;
            }
            let Reported::Value(prefix) = &base.uri else {
                return None;
            };
            if !base_directory(prefix) {
                return None;
            }
            length = length.checked_add(prefix.len())?;
            if length > MAX_RESOLVED_URI_BYTES {
                return None;
            }
            parts.push(prefix.as_str());
            if absolute_uri(prefix) {
                if !matches!(base.uri_base_id, Reported::Missing) {
                    return None;
                }
                let mut uri = String::with_capacity(length);
                for part in parts.into_iter().rev() {
                    uri.push_str(part);
                }
                return Some(Resolved {
                    uri,
                    absolute: true,
                });
            }
            let Reported::Value(parent) = &base.uri_base_id else {
                return None;
            };
            base_id = parent;
        }
        None
    }
}

fn array_index(value: &Reported<i64>) -> Result<Option<usize>, ()> {
    match value {
        Reported::Missing | Reported::Value(-1) => Ok(None),
        Reported::Value(index) => usize::try_from(*index).map(Some).map_err(|_| ()),
        Reported::Unknown => Err(()),
    }
}

fn literal_uri(uri: &str) -> bool {
    if uri.is_empty()
        || uri
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
        || uri
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sarif:"))
    {
        return false;
    }
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%'
            && !(bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                && bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit()))
        {
            return false;
        }
    }
    true
}

fn absolute_uri(uri: &str) -> bool {
    uri.split_once(':').is_some_and(|(scheme, rest)| {
        !rest.is_empty()
            && scheme
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
    })
}

fn base_directory(uri: &str) -> bool {
    if !literal_uri(uri) || uri.contains(['?', '#']) {
        return false;
    }
    let path = if absolute_uri(uri) {
        let Some((_, rest)) = uri.split_once(':') else {
            return false;
        };
        if let Some(authority) = rest.strip_prefix("//") {
            let Some(start) = authority.find('/') else {
                return false;
            };
            &authority[start..]
        } else if rest.starts_with('/') {
            rest
        } else {
            return false;
        }
    } else if uri.starts_with('/') {
        return false;
    } else {
        uri
    };
    path.ends_with('/') && !path.ends_with("//") && !path.split('/').any(dot_segment)
}

fn dot_segment(segment: &str) -> bool {
    // Dot is an unreserved URI character: its percent-encoded spelling must
    // not smuggle parent traversal into a supposedly concatenation-safe base.
    let mut bytes = segment.bytes();
    let mut dots = 0;
    while let Some(byte) = bytes.next() {
        match byte {
            b'.' => dots += 1,
            b'%' if bytes.next() == Some(b'2')
                && bytes.next().is_some_and(|byte| matches!(byte, b'e' | b'E')) =>
            {
                dots += 1;
            }
            _ => return false,
        }
    }
    matches!(dots, 1 | 2)
}
