//! Validate every decoded metadata value before retaining its original bytes.
//!
//! Excerpt provenance does not authenticate the privacy of arbitrary metadata.
//! A streaming visitor sees duplicate keys as well as JSON escapes: building a
//! Value first would discard earlier duplicate members and could bless raw JSON
//! containing a value that disappeared from the parsed map.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use super::{RedactionLevel, redact_content};

const MAX_BYTES: usize = 64 * 1024;
const MAX_VALUES: usize = 4096;
const MAX_DEPTH: usize = 32;

// These are the structural names emitted by db::prepare_evidence_security,
// not secret-bearing text. Exempt names only: every value (including values
// under these names and earlier duplicate members) still visits Check.
fn canonical_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "schema"
            | "producerKind"
            | "screeningVersion"
            | "securityPolicyEpoch"
            | "secretRedactionStatus"
            | "redactionClasses"
            | "instructionRisk"
            | "searchEligibility"
            | "packEligibility"
            | "canonicalProvenanceRevision"
            | "canonicalExcerptHash"
            | "upstreamRefHash"
            | "sourceMetadataHash"
    )
}

pub(super) fn safe_to_retain(raw: &str, level: RedactionLevel) -> bool {
    if raw.len() > MAX_BYTES || matches!(level, RedactionLevel::Paranoid | RedactionLevel::Full) {
        return false;
    }
    let mut remaining = MAX_VALUES;
    let mut decoder = serde_json::Deserializer::from_str(raw);
    Check {
        level,
        remaining: &mut remaining,
        depth: 0,
    }
    .deserialize(&mut decoder)
    .is_ok()
        && decoder.end().is_ok()
}

struct Check<'a> {
    level: RedactionLevel,
    remaining: &'a mut usize,
    depth: usize,
}

fn refused<E: de::Error>() -> E {
    E::custom("evidence metadata cannot be retained in this redacted backup")
}

fn check_text<E: de::Error>(text: &str, level: RedactionLevel) -> Result<(), E> {
    if redact_content(text, level) == text {
        Ok(())
    } else {
        Err(refused())
    }
}

impl<'de> DeserializeSeed<'de> for Check<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        if self.depth > MAX_DEPTH || *self.remaining == 0 {
            return Err(refused());
        }
        *self.remaining -= 1;
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Check<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bounded redaction-safe evidence metadata")
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<(), E> {
        check_text(text, self.level)
    }

    fn visit_borrowed_str<E: de::Error>(self, text: &'de str) -> Result<(), E> {
        self.visit_str(text)
    }

    fn visit_string<E: de::Error>(self, text: String) -> Result<(), E> {
        self.visit_str(&text)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence
            .next_element_seed(Check {
                level: self.level,
                remaining: &mut *self.remaining,
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if !canonical_metadata_key(&key) {
                check_text(&key, self.level)?;
                let assignment =
                    serde_json::to_string(&key).map_err(|_| refused::<A::Error>())? + ":";
                check_text(&assignment, self.level)?;
            }
            map.next_value_seed(Check {
                level: self.level,
                remaining: &mut *self.remaining,
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key_names_do_not_exempt_secret_values_or_unknown_keys() {
        assert!(safe_to_retain(
            r#"{"secretRedactionStatus":"clean","redactionClasses":[]}"#,
            RedactionLevel::Standard,
        ));
        for raw in [
            r#"{"secretRedactionStatus":"api_key=CANONICAL_VALUE_CANARY"}"#,
            r#"{"secretRedactionStatus":"\u0061pi_key=CANONICAL_VALUE_CANARY"}"#,
            r#"{"secretRedactionStatus":"api_key=CANONICAL_VALUE_CANARY","secretRedactionStatus":"clean"}"#,
            r#"{"secretRedactionStatus":{"nested":"api_key=CANONICAL_VALUE_CANARY"}}"#,
            r#"{"secretRedactionStatusExtra":"clean"}"#,
            r#"{"SecretRedactionStatus":"clean"}"#,
            r#"{"api_key":"otherwise benign"}"#,
        ] {
            assert!(!safe_to_retain(raw, RedactionLevel::Standard), "{raw}");
        }
    }
}
