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

// bd-hpuik: error-path tests for parse() reject branches that weren't covered.
// Each test surfaces a distinct PackBinaryError code so the parser's diagnostic
// vocabulary stays stable as the frame format evolves.

#[test]
fn empty_item_list_round_trips() {
    // Zero-item packs are legitimate (e.g. a context query with no candidates):
    // serialize must produce a parseable frame whose item_count is 0, and the
    // reader must not require any item-table entries.
    let json = r#"{"schema":"ee.response.v2","success":true,"data":{"pack":{"items":[]}}}"#;
    let frame = serialize_pack_binary(json, &[], 0);
    let view = PackBinaryView::parse(&frame).expect("empty-items frame should parse");

    assert_eq!(view.item_count(), 0);
    assert_eq!(view.canonical_json().expect("json should be utf8"), json);
    assert!(view.item_slice(0).is_err());
}

#[test]
fn frame_below_header_plus_trailer_minimum_is_truncated() {
    let needed_min = PACK_BINARY_HEADER_LEN + PACK_BINARY_TRAILER_LEN;
    let too_short = vec![0_u8; needed_min - 1];
    let error =
        PackBinaryView::parse(&too_short).expect_err("undersized buffer should be truncated");

    assert!(
        matches!(error, PackBinaryError::Truncated { .. }),
        "expected Truncated, got {error:?}"
    );
    assert_eq!(error.code(), "pack_bin_truncated");
}

#[test]
fn item_count_that_overflows_table_arithmetic_is_rejected() {
    // Poke u64::MAX into the item_count slot. usize::try_from succeeds on a
    // 64-bit host, but item_count * 16 overflows in checked_mul, so the parser
    // must surface ItemCountTooLarge rather than panicking or wrapping.
    let mut frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    frame[8..16].copy_from_slice(&u64::MAX.to_le_bytes());

    let error = PackBinaryView::parse(&frame).expect_err("u64::MAX item_count should reject");
    assert!(
        matches!(error, PackBinaryError::ItemCountTooLarge { .. }),
        "expected ItemCountTooLarge, got {error:?}"
    );
    assert_eq!(error.code(), "pack_bin_item_count_too_large");
}

#[test]
fn declared_total_bytes_must_match_frame_length() {
    let mut frame = serialize_pack_binary(fixture_json(), &fixture_items(), 0);
    let bogus_total = (frame.len() as u64) + 1;
    frame[16..24].copy_from_slice(&bogus_total.to_le_bytes());

    let error =
        PackBinaryView::parse(&frame).expect_err("mismatched total_bytes should be rejected");
    assert!(
        matches!(error, PackBinaryError::TotalBytesMismatch { .. }),
        "expected TotalBytesMismatch, got {error:?}"
    );
    assert_eq!(error.code(), "pack_bin_total_bytes_mismatch");
}

#[test]
fn non_utf8_canonical_json_surfaces_dedicated_error_code() {
    // Build a frame whose canonical_json footer is intentionally not UTF-8.
    // The frame must be otherwise well-formed (correct BLAKE3 over the invalid
    // bytes, correct item offsets, correct trailer); otherwise the parser
    // would short-circuit on a different error before canonical_json() is
    // called.
    let invalid_json: [u8; 4] = [0xff, 0xfe, 0xfd, 0xfc];
    let content_hash = blake3::hash(&invalid_json);
    let item_contents: [&[u8]; 1] = [b"alpha"];
    let table_len = item_contents.len() * PACK_BINARY_ITEM_TABLE_ENTRY_LEN;
    let blob_start = PACK_BINARY_HEADER_LEN + table_len;
    let canonical_json_offset = blob_start + item_contents[0].len();
    let total_bytes = canonical_json_offset + invalid_json.len() + PACK_BINARY_TRAILER_LEN;

    let mut frame: Vec<u8> = Vec::with_capacity(total_bytes);
    frame.extend_from_slice(&PACK_BINARY_MAGIC);
    frame.extend_from_slice(&PACK_BINARY_VERSION_V1.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes()); // flags
    frame.extend_from_slice(&(item_contents.len() as u64).to_le_bytes());
    frame.extend_from_slice(&(total_bytes as u64).to_le_bytes());
    frame.extend_from_slice(content_hash.as_bytes());
    frame.extend_from_slice(&(blob_start as u64).to_le_bytes());
    frame.extend_from_slice(&(item_contents[0].len() as u32).to_le_bytes());
    frame.extend_from_slice(&0_u32.to_le_bytes()); // reserved
    frame.extend_from_slice(item_contents[0]);
    frame.extend_from_slice(&invalid_json);
    frame.extend_from_slice(&(canonical_json_offset as u64).to_le_bytes());
    frame.extend_from_slice(&(invalid_json.len() as u64).to_le_bytes());
    frame.extend_from_slice(&0_u32.to_le_bytes()); // reserved
    assert_eq!(frame.len(), total_bytes);

    let view = PackBinaryView::parse(&frame).expect("frame structure should parse");
    let error = view
        .canonical_json()
        .expect_err("non-utf8 canonical_json should reject at decode time");
    assert!(
        matches!(error, PackBinaryError::NonUtf8Json),
        "expected NonUtf8Json, got {error:?}"
    );
    assert_eq!(error.code(), "pack_bin_json_non_utf8");
}
