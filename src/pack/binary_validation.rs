//! Bind binary item projections to the hash-checked canonical response.
//!
//! The v1 frame hashes its JSON footer, not its separate item blob. Geometry
//! checks alone therefore cannot establish that zero-copy item reads agree
//! with the response being replayed. Deserialize only the content projection,
//! compare every item in order, and discard the temporary strings afterwards.
//! The owning view caches the verdict for its immutable borrowed frame.

use serde::Deserialize;

use super::{PackBinaryError, PackBinaryView};

#[derive(Deserialize)]
struct CanonicalResponse {
    data: CanonicalData,
}

#[derive(Deserialize)]
struct CanonicalData {
    pack: CanonicalPack,
}

#[derive(Deserialize)]
struct CanonicalPack {
    // The batch renderer merges native memories and evidence into this array.
    // Do not deserialize just the in-memory draft's memory-only collection.
    items: Vec<CanonicalItem>,
}

#[derive(Deserialize)]
struct CanonicalItem {
    content: String,
}

pub(super) fn validate(view: &PackBinaryView<'_>) -> Result<(), PackBinaryError> {
    let json = view.canonical_json()?;
    let response: CanonicalResponse =
        serde_json::from_str(json).map_err(|_| PackBinaryError::InvalidItemContent {
            index: None,
            reason: "canonical JSON must contain data.pack.items with string content fields",
        })?;
    let items = response.data.pack.items;
    if items.len() != view.entries.len() {
        return Err(PackBinaryError::InvalidItemContent {
            index: None,
            reason: "binary item count differs from the canonical response",
        });
    }
    for (index, (item, entry)) in items.iter().zip(&view.entries).enumerate() {
        // Bounds and contiguity were already checked by PackBinaryView::parse.
        // Do not call item_slice here: it invokes this cached validation.
        if item.content.as_bytes() != &view.bytes[entry.offset..entry.offset + entry.len] {
            return Err(PackBinaryError::InvalidItemContent {
                index: Some(index),
                reason: "binary item bytes differ from the canonical response",
            });
        }
    }
    Ok(())
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
        let slices = contents.iter().map(|item| item.as_bytes()).collect::<Vec<_>>();
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
}
