//! Bind binary item projections to the hash-checked canonical response.
//!
//! The v1 frame hashes its JSON footer, not its separate item blob. Geometry
//! checks alone therefore cannot establish that zero-copy item reads agree
//! with the response being replayed. Stream the content projection and compare
//! each decoded string immediately, without retaining a second copy of the
//! pack. Unescaped strings borrow the input; escaped strings use the JSON
//! decoder's reusable scratch buffer. Unknown fields are parsed but discarded.
//! The owning view exposes no item before the whole document passes and caches
//! only the verdict for its immutable borrowed frame.

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::{PackBinaryError, PackBinaryItemEntry, PackBinaryView};

const INVALID_PROJECTION: &str =
    "canonical JSON must contain data.pack.items with string content fields";
const COUNT_MISMATCH: &str = "binary item count differs from the canonical response";
const CONTENT_MISMATCH: &str = "binary item bytes differ from the canonical response";

// Deserialize field names without allocating a String for every map key.
// Escaped JSON keys are still compared after decoding, including duplicates.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(field_identifier, rename_all = "lowercase")]
enum Field {
    Data,
    Pack,
    Items,
    Content,
    #[serde(other)]
    Other,
}

struct Check<'frame> {
    bytes: &'frame [u8],
    entries: &'frame [PackBinaryItemEntry],
    failure: Option<PackBinaryError>,
}

impl Check<'_> {
    fn reject<E: de::Error>(&mut self, index: Option<usize>, reason: &'static str) -> E {
        self.failure = Some(PackBinaryError::InvalidItemContent { index, reason });
        E::custom("invalid binary pack item projection")
    }
}

// Each object has exactly one required projection field. All other fields
// retain the previous derived-struct behavior: validate JSON, then ignore.
struct Object<'check, 'frame> {
    check: &'check mut Check<'frame>,
    field: Field,
    index: usize,
}

impl<'de> DeserializeSeed<'de> for Object<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        if self.field == Field::Content && self.index >= self.check.entries.len() {
            // The frame table is already geometry-checked. An extra canonical
            // item cannot be legitimate, regardless of the size of its payload.
            return Err(self.check.reject(None, COUNT_MISMATCH));
        }
        decoder.deserialize_struct(
            "CanonicalProjection",
            &["data", "pack", "items", "content"],
            self,
        )
    }
}

impl<'de> Visitor<'de> for Object<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a canonical pack projection object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = false;
        while let Some(field) = map.next_key::<Field>()? {
            if field == self.field {
                if seen {
                    return Err(de::Error::custom("duplicate canonical projection field"));
                }
                seen = true;
                map.next_value_seed(FieldValue(Object {
                    check: &mut *self.check,
                    field: self.field,
                    index: self.index,
                }))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if seen {
            Ok(())
        } else {
            Err(de::Error::custom("missing canonical projection field"))
        }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        // Preserve the one-field positional representation accepted by the
        // former derived structs. The normal renderer still emits objects.
        if sequence
            .next_element_seed(FieldValue(Object {
                check: &mut *self.check,
                field: self.field,
                index: self.index,
            }))?
            .is_none()
            || sequence.next_element::<IgnoredAny>()?.is_some()
        {
            return Err(de::Error::custom(
                "invalid canonical projection field count",
            ));
        }
        Ok(())
    }
}

struct FieldValue<'check, 'frame>(Object<'check, 'frame>);

impl<'de> DeserializeSeed<'de> for FieldValue<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        match self.0.field {
            Field::Data => Object {
                field: Field::Pack,
                ..self.0
            }
            .deserialize(decoder),
            Field::Pack => Object {
                field: Field::Items,
                ..self.0
            }
            .deserialize(decoder),
            Field::Items => Items {
                check: self.0.check,
            }
            .deserialize(decoder),
            Field::Content => Content {
                check: self.0.check,
                index: self.0.index,
            }
            .deserialize(decoder),
            Field::Other => Err(de::Error::custom("unknown canonical projection field")),
        }
    }
}

struct Items<'check, 'frame> {
    check: &'check mut Check<'frame>,
}

impl<'de> DeserializeSeed<'de> for Items<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        decoder.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Items<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the ordered canonical pack items")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        let mut index = 0;
        while sequence
            .next_element_seed(Object {
                check: &mut *self.check,
                field: Field::Content,
                index,
            })?
            .is_some()
        {
            index += 1;
        }
        if index != self.check.entries.len() {
            return Err(self.check.reject(None, COUNT_MISMATCH));
        }
        Ok(())
    }
}

struct Content<'check, 'frame> {
    check: &'check mut Check<'frame>,
    index: usize,
}

impl<'de> DeserializeSeed<'de> for Content<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        decoder.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Content<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("byte-exact canonical item content")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<(), E> {
        // Bounds and contiguity were already checked by PackBinaryView::parse;
        // Object checked the item index before deserializing this payload.
        // Do not call item_slice here: it invokes this cached validation.
        let entry = &self.check.entries[self.index];
        if text.as_bytes() != &self.check.bytes[entry.offset..entry.offset + entry.len] {
            return Err(self.check.reject(Some(self.index), CONTENT_MISMATCH));
        }
        Ok(())
    }
}

pub(super) fn validate(view: &PackBinaryView<'_>) -> Result<(), PackBinaryError> {
    let json = view.canonical_json()?;
    let mut check = Check {
        bytes: view.bytes,
        entries: &view.entries,
        failure: None,
    };
    let mut decoder = serde_json::Deserializer::from_str(json);
    let result = Object {
        check: &mut check,
        field: Field::Data,
        index: 0,
    }
    .deserialize(&mut decoder)
    .and_then(|()| decoder.end());
    result.map_err(|_| {
        check
            .failure
            .unwrap_or(PackBinaryError::InvalidItemContent {
                index: None,
                reason: INVALID_PROJECTION,
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::binary::{
        PACK_BINARY_HEADER_LEN, PACK_BINARY_ITEM_TABLE_ENTRY_LEN, serialize_pack_binary,
    };

    fn json(contents: &[&str]) -> String {
        serde_json::json!({
            "schema": "ee.response.v2",
            "data": {
                "pack": {
                    "items": contents.iter().enumerate().map(|(index, content)| {
                        serde_json::json!({
                            "entityKind": if index == 0 { "memory" } else { "evidence_span" },
                            "content": content,
                        })
                    }).collect::<Vec<_>>()
                }
            }
        })
        .to_string()
    }

    fn frame(contents: &[&str]) -> Vec<u8> {
        let slices = contents
            .iter()
            .map(|item| item.as_bytes())
            .collect::<Vec<_>>();
        serialize_pack_binary(&json(contents), &slices, 0)
    }

    #[test]
    fn verifies_mixed_items_including_escaped_unicode_and_empty_content() {
        let contents = ["release checks", "café\n\"quoted\" \\ evidence", ""];
        let bytes = frame(&contents);
        let view = PackBinaryView::parse_verified(&bytes).expect("verified mixed frame");
        for (index, content) in contents.iter().enumerate() {
            let slice = view.item_slice(index).expect("verified zero-copy item");
            assert_eq!(slice, content.as_bytes());
            assert_eq!(slice.as_ptr(), bytes[view.entries[index].offset..].as_ptr());
        }
    }

    #[test]
    fn rejects_item_tampering_without_changing_the_json_hash() {
        let mut bytes = frame(&["alpha", "bravo"]);
        let blob = PACK_BINARY_HEADER_LEN + 2 * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
        bytes[blob] ^= 1;
        let view = PackBinaryView::parse(&bytes).expect("unchanged canonical hash");
        assert!(matches!(
            view.item_slice(0),
            Err(PackBinaryError::InvalidItemContent { index: Some(0), .. })
        ));
        assert!(PackBinaryView::parse_verified(&bytes).is_err());
    }

    #[test]
    fn refuses_every_item_if_a_later_item_is_corrupt() {
        let mut bytes = frame(&["alpha", "bravo"]);
        let second = PACK_BINARY_HEADER_LEN + 2 * PACK_BINARY_ITEM_TABLE_ENTRY_LEN + 5;
        bytes[second] ^= 1;
        let view = PackBinaryView::parse(&bytes).expect("valid frame geometry");
        assert!(matches!(
            view.item_slice(0),
            Err(PackBinaryError::InvalidItemContent { index: Some(1), .. })
        ));
        assert!(view.item_slice(1).is_err());
    }

    #[test]
    fn rejects_missing_extra_and_reordered_item_projections() {
        let canonical = json(&["alpha", "bravo"]);
        for items in [
            vec![&b"alpha"[..]],
            vec![&b"alpha"[..], &b"bravo"[..], &b"extra"[..]],
            vec![&b"bravo"[..], &b"alpha"[..]],
        ] {
            let bytes = serialize_pack_binary(&canonical, &items, 0);
            let view = PackBinaryView::parse(&bytes).expect("valid frame geometry");
            assert!(view.validate_item_contents().is_err());
        }
    }

    #[test]
    fn rejects_invalid_or_ambiguous_canonical_content_shapes() {
        for canonical in [
            "not JSON",
            "{}",
            r#"{"data":{"pack":{"items":null}}}"#,
            r#"{"data":{"pack":{"items":[{}]}}}"#,
            r#"{"data":{"pack":{"items":[{"content":7}]}}}"#,
            r#"{"data":{"pack":{"items":[],"items":[]}}}"#,
            r#"{"data":{"pack":{"items":[{"content":"alpha","content":"bravo"}]}}}"#,
        ] {
            let bytes = serialize_pack_binary(canonical, &[], 0);
            let view = PackBinaryView::parse(&bytes).expect("hash and geometry only");
            assert!(matches!(
                view.validate_item_contents(),
                Err(PackBinaryError::InvalidItemContent { .. })
            ));
        }
    }

    #[test]
    fn valid_empty_pack_verifies_without_item_reads() {
        let bytes = frame(&[]);
        let view = PackBinaryView::parse_verified(&bytes).expect("empty context pack");
        assert_eq!(view.item_count(), 0);
        assert!(view.validate_item_contents().is_ok());
        assert!(matches!(
            view.item_slice(0),
            Err(PackBinaryError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn caches_success_and_failure_without_retaining_decoded_item_copies() {
        let mut bytes = frame(&["alpha"]);
        let view = PackBinaryView::parse(&bytes).expect("frame");
        assert!(view.item_validation.get().is_none());
        assert!(view.item_slice(0).is_ok());
        assert_eq!(view.item_validation.get(), Some(&Ok(())));
        assert!(view.item_slice(0).is_ok());
        drop(view);

        bytes[PACK_BINARY_HEADER_LEN + PACK_BINARY_ITEM_TABLE_ENTRY_LEN] ^= 1;
        let view = PackBinaryView::parse(&bytes).expect("unchanged canonical hash");
        assert!(view.item_validation.get().is_none());
        let first_error = view.item_slice(0).expect_err("corrupt item");
        assert_eq!(view.item_validation.get(), Some(&Err(first_error.clone())));
        assert_eq!(view.item_slice(0).expect_err("cached verdict"), first_error);
    }

    #[test]
    fn reports_integrity_failures_without_disclosing_item_text() {
        let bytes = serialize_pack_binary(&json(&["private-original"]), &[b"private-tampered"], 0);
        let view = PackBinaryView::parse(&bytes).expect("valid frame geometry");
        let error = view.item_slice(0).expect_err("different content");
        assert_eq!(error.code(), "pack_bin_content_hash_mismatch");
        let message = error.to_string();
        assert!(!message.contains("private-original"));
        assert!(!message.contains("private-tampered"));
    }

    #[test]
    fn large_mixed_pack_keeps_borrowed_item_slices_and_one_cached_verdict() {
        let content = ["ordinary source text", "café\n\"quoted\" \\ evidence", ""];
        let contents = (0..2048)
            .map(|index| content[index % content.len()])
            .collect::<Vec<_>>();
        let bytes = frame(&contents);
        let view = PackBinaryView::parse_verified(&bytes).expect("large mixed pack");
        assert_eq!(view.item_count(), contents.len());
        for (index, expected) in contents.iter().enumerate() {
            let actual = view.item_slice(index).expect("verified item");
            assert_eq!(actual, expected.as_bytes());
            assert_eq!(
                actual.as_ptr(),
                bytes[view.entries[index].offset..].as_ptr()
            );
        }
        assert_eq!(view.item_validation.get(), Some(&Ok(())));
    }

    #[test]
    fn extra_item_is_refused_before_its_payload_is_deserialized() {
        let bytes = frame(&[]);
        let view = PackBinaryView::parse(&bytes).expect("empty frame");
        let mut check = Check {
            bytes: view.bytes,
            entries: &view.entries,
            failure: None,
        };
        let entries = std::iter::once_with(|| -> (&'static str, &'static str) {
            panic!("an excess item payload must never be visited")
        });
        let decoder = serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(entries);
        assert!(
            Object {
                check: &mut check,
                field: Field::Content,
                index: 0,
            }
            .deserialize(decoder)
            .is_err()
        );
        assert_eq!(
            check.failure,
            Some(PackBinaryError::InvalidItemContent {
                index: None,
                reason: COUNT_MISMATCH,
            })
        );
    }

    #[test]
    fn escaped_keys_unknown_metadata_and_positional_structs_remain_compatible() {
        for canonical in [
            r#"{"d\u0061ta":{"pack":{"it\u0065ms":[{"cont\u0065nt":"alpha"}]}}}"#,
            r#"{"metadata":{"nested":[null,true,7]},"data":{"ignored":"value","pack":{"items":[{"metadata":[1,2],"content":"alpha"}],"tail":false}}}"#,
            r#"[[[[["alpha"]]]]]"#,
        ] {
            let bytes = serialize_pack_binary(canonical, &[b"alpha"], 0);
            let view = PackBinaryView::parse_verified(&bytes).expect("compatible projection");
            assert_eq!(view.item_slice(0).expect("content"), b"alpha");
        }
    }

    #[test]
    fn valid_item_prefix_never_hides_late_syntax_duplicates_or_trailing_documents() {
        for canonical in [
            r#"{"data":{"pack":{"items":[{"content":"alpha"}]}},"tail":[}"#,
            r#"{"data":{"pack":{"items":[{"content":"alpha"}]}},"data":{}}"#,
            r#"{"data":{"pack":{"items":[{"content":"alpha"}]},"pack":{}}}"#,
            r#"{"data":{"pack":{"items":[{"content":"alpha","cont\u0065nt":"alpha"}]}}}"#,
            r#"{"data":{"pack":{"items":[{"content":"alpha"}]}}} {}"#,
            r#"[[[[["alpha","extra field"]]]]]"#,
        ] {
            let bytes = serialize_pack_binary(canonical, &[b"alpha"], 0);
            let view = PackBinaryView::parse(&bytes).expect("hash-checked frame");
            assert!(view.item_slice(0).is_err());
            assert!(PackBinaryView::parse_verified(&bytes).is_err());
        }
    }
}
