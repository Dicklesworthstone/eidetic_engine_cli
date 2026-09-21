//! One field vocabulary for canonical search emission and daemon admission.
//!
//! The emitter can only insert a `SearchResultField`, never an arbitrary JSON
//! key. Adding a variant below extends the validator's accepted fields in the
//! same declaration. Requiredness remains an explicit compatibility decision.

use serde_json::{Map, Value};

macro_rules! search_result_fields {
    (
        required { $( $required:ident => $required_key:literal, )* }
        optional { $( $optional:ident => $optional_key:literal, )* }
    ) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub(crate) enum SearchResultField {
            $( $required, )*
            $( $optional, )*
        }

        impl SearchResultField {
            const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$required => $required_key, )*
                    $( Self::$optional => $optional_key, )*
                }
            }
        }

        pub(crate) const REQUIRED: &[&str] = &[$( $required_key, )*];
        pub(crate) const OPTIONAL: &[&str] = &[$( $optional_key, )*];

        #[cfg(test)]
        const ALL: &[SearchResultField] = &[
            $( SearchResultField::$required, )*
            $( SearchResultField::$optional, )*
        ];
    };
}

search_result_fields! {
    required {
        DocId => "docId",
        Score => "score",
        RelevanceScore => "relevanceScore",
        ScoreKind => "scoreKind",
        ScoreInterval => "scoreInterval",
        CoverageGuarantee => "coverageGuarantee",
        Calibrated => "calibrated",
        Source => "source",
        Why => "why",
        Provenance => "provenance",
    }
    optional {
        // Current emitters include this, but older v3 replies can omit it.
        CalibrationId => "calibrationId",
        MemoryId => "memoryId",
        FastScore => "fastScore",
        QualityScore => "qualityScore",
        LexicalScore => "lexicalScore",
        RerankScore => "rerankScore",
        Metadata => "metadata",
        DriftHint => "driftHint",
        MeshProvenance => "meshProvenance",
        MeshTrustAdjustment => "meshTrustAdjustment",
        Content => "content",
        ContentTruncated => "content_truncated",
        ContentRedacted => "contentRedacted",
        Redactions => "redactions",
        Tombstoned => "tombstoned",
        TombstonedAt => "tombstonedAt",
        ValidFrom => "validFrom",
        ValidTo => "validTo",
        ValidityStatus => "validityStatus",
        ValidityWindowKind => "validityWindowKind",
        Explanation => "explanation",
    }
}

/// Closed-key builder. Deliberately exposes neither the map nor `DerefMut`.
/// Values retain the canonical serializer's existing JSON representation.
pub(crate) struct SearchResultDocument(Map<String, Value>);

impl<const N: usize> From<[(SearchResultField, Value); N]> for SearchResultDocument {
    fn from(fields: [(SearchResultField, Value); N]) -> Self {
        Self(
            fields
                .into_iter()
                .map(|(field, value)| (field.as_str().to_owned(), value))
                .collect(),
        )
    }
}

impl SearchResultDocument {
    pub(crate) fn insert(&mut self, field: SearchResultField, value: Value) {
        let _ = self.0.insert(field.as_str().to_owned(), value);
    }

    #[must_use]
    pub(crate) fn into_json(self) -> Value {
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{ALL, OPTIONAL, REQUIRED, SearchResultDocument, SearchResultField};
    use serde_json::Value;

    #[test]
    fn emitted_and_accepted_fields_are_the_same_declaration() {
        let mut document = SearchResultDocument::from([]);
        for field in ALL {
            document.insert(*field, Value::Null);
        }
        let value = document.into_json();
        let emitted: BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let accepted: BTreeSet<_> = REQUIRED.iter().chain(OPTIONAL).copied().collect();
        assert_eq!(emitted, accepted);
        assert_eq!(accepted.len(), ALL.len(), "duplicate wire field name");
    }

    #[test]
    fn calibration_metadata_preserves_null_and_string_values() {
        for id in [Value::Null, Value::String("calibration-v1".to_owned())] {
            let document = SearchResultDocument::from([
                (SearchResultField::DocId, Value::String("doc-1".to_owned())),
                (SearchResultField::CalibrationId, id.clone()),
            ]);
            let wire = serde_json::to_vec(&document.into_json()).unwrap();
            let decoded: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(decoded["calibrationId"], id);
            assert!(OPTIONAL.contains(&"calibrationId"));
            assert!(!REQUIRED.contains(&"calibrationId"));
        }
    }
}
