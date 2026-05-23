use ee::pack::binary::{
    PACK_BINARY_HEADER_LEN, PACK_BINARY_ITEM_TABLE_ENTRY_LEN, PACK_BINARY_MAGIC,
    PACK_BINARY_SCHEMA_V1, PACK_BINARY_TRAILER_LEN, PACK_BINARY_VERSION_V1, PackBinaryError,
    PackBinaryView, serialize_pack_binary,
};

fn fixture_json() -> &'static str {
    r#"{"schema":"ee.response.v2","success":true,"data":{"pack":{"items":[{"content":"alpha"},{"content":"bravo"},{"content":"charlie"},{"content":"delta"}]}}}"#
}

fn fixture_items() -> Vec<&'static [u8]> {
    vec![b"alpha", b"bravo", b"charlie", b"delta"]
}

#[test]
fn binary_round_trip_preserves_canonical_json() {
    let json = fixture_json();
    let items = fixture_items();
    let frame = serialize_pack_binary(json, &items, 0);
    let view = PackBinaryView::parse(&frame).expect("frame should parse");

    assert_eq!(view.schema(), PACK_BINARY_SCHEMA_V1);
    assert_eq!(view.canonical_json().expect("json should be utf8"), json);
    assert_eq!(view.to_json_bytes(), json.as_bytes());
}

#[test]
fn content_hash_matches_blake3_over_canonical_json() {
    let json = fixture_json();
    let items = fixture_items();
    let frame = serialize_pack_binary(json, &items, 0);
    let view = PackBinaryView::parse(&frame).expect("frame should parse");
    let expected = format!("blake3:{}", blake3::hash(json.as_bytes()).to_hex());

    assert_eq!(view.content_hash_hex(), expected);
}

#[test]
fn item_offsets_enable_zero_copy_slices() {
    let frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let view = PackBinaryView::parse(&frame).expect("frame should parse");

    assert_eq!(view.item_count(), 4);
    assert_eq!(view.item_slice(3).expect("item 3 should exist"), b"delta");
}

#[test]
fn frame_uses_little_endian_header_and_offsets() {
    let frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let item_table = PACK_BINARY_HEADER_LEN;
    let first_item_offset = u64::from_le_bytes(
        frame[item_table..item_table + 8]
            .try_into()
            .expect("first offset bytes"),
    );
    let first_item_len = u32::from_le_bytes(
        frame[item_table + 8..item_table + 12]
            .try_into()
            .expect("first len bytes"),
    );

    assert_eq!(&frame[0..4], PACK_BINARY_MAGIC);
    assert_eq!(
        u16::from_le_bytes(frame[4..6].try_into().expect("version bytes")),
        PACK_BINARY_VERSION_V1
    );
    assert_eq!(
        first_item_offset,
        (PACK_BINARY_HEADER_LEN + fixture_items().len() * PACK_BINARY_ITEM_TABLE_ENTRY_LEN) as u64
    );
    assert_eq!(first_item_len, 5);
}

#[test]
fn version_two_rejects_with_explicit_code() {
    let mut frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    frame[4..6].copy_from_slice(&2_u16.to_le_bytes());

    let error = PackBinaryView::parse(&frame).expect_err("v2 should be too new for v1 reader");
    assert!(matches!(error, PackBinaryError::VersionTooNew { .. }));
    assert_eq!(error.code(), "pack_bin_version_too_new");
}

#[test]
fn binary_serialization_is_deterministic() {
    let json = fixture_json();
    let items = fixture_items();
    let first = serialize_pack_binary(json, &items, 0);
    let second = serialize_pack_binary(json, &items, 0);
    let third = serialize_pack_binary(json, &items, 0);

    assert_eq!(first, second);
    assert_eq!(second, third);
}

#[test]
fn magic_mismatch_and_hash_mismatch_have_catalog_codes() {
    let mut bad_magic = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    bad_magic[0..4].copy_from_slice(b"NOPE");
    let magic_error = PackBinaryView::parse(&bad_magic).expect_err("bad magic should reject");
    assert_eq!(magic_error.code(), "pack_bin_magic_mismatch");

    let mut bad_hash = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let trailer = bad_hash.len() - PACK_BINARY_TRAILER_LEN;
    bad_hash[trailer - 1] ^= 0x01;
    let hash_error = PackBinaryView::parse(&bad_hash).expect_err("bad hash should reject");
    assert_eq!(hash_error.code(), "pack_bin_content_hash_mismatch");
}

#[test]
fn item_length_u32_max_surfaces_as_invalid_offset_not_content_hash_mismatch() {
    // bd-2frot: pins the downstream invariant that `serialize_pack_binary`'s
    // saturating-cast (item.len() -> u32, clamped at u32::MAX) now relies on.
    // If a writer ever emits an item-table entry with len = u32::MAX (either
    // via the saturating path on a real >4 GiB item, or via direct frame
    // construction here), the reader must fail with the precise InvalidOffset
    // diagnostic, NOT with the older ContentHashMismatch confusion. Without
    // this contract, the saturating cast would still produce a misleading
    // hash-mismatch error and the bd-2frot improvement would be invisible.
    let mut frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let first_len_offset = PACK_BINARY_HEADER_LEN + 8; // header (56) + offset (8 bytes)
    frame[first_len_offset..first_len_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());

    let error = PackBinaryView::parse(&frame)
        .expect_err("oversized item length should surface as InvalidOffset");
    assert!(
        matches!(error, PackBinaryError::InvalidOffset { .. }),
        "expected InvalidOffset, got {error:?}"
    );
    assert_eq!(error.code(), "pack_bin_invalid_offset");
}

#[test]
fn event_log_line_matches_pack_binary_contract() {
    let frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let view = PackBinaryView::parse(&frame).expect("frame should parse");
    let line = view.event("deserialize", 17).to_json_line();

    assert!(line.contains(r#""schema":"ee.test_event.v1""#));
    assert!(line.contains(r#""kind":"pack_binary""#));
    assert!(line.contains(r#""operation":"deserialize""#));
    assert!(line.contains(r#""endianness_swap_required":false"#));
}
