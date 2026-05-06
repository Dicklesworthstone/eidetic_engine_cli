#![no_main]

use std::str::FromStr;

use ee::models::{Id, IdKind, MemoryId, MemoryLinkId, PackId, WorkspaceId};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 4096;
const ENCODED_PAYLOAD_LEN: usize = 26;

fn assert_display_round_trip<K: IdKind>(id: Id<K>) {
    let rendered = id.to_string();
    assert!(rendered.starts_with(K::PREFIX));
    assert_eq!(rendered.as_bytes().get(K::PREFIX.len()), Some(&b'_'));
    assert_eq!(rendered.len(), K::PREFIX.len() + 1 + ENCODED_PAYLOAD_LEN);
    assert_eq!(
        Id::<K>::from_str(&rendered).expect("displayed ID should parse"),
        id
    );
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    let memory_from_str = MemoryId::from_str(input);
    let memory_from_parse = input.parse::<MemoryId>();
    assert_eq!(memory_from_str, memory_from_parse);
    if let Ok(id) = memory_from_str {
        assert_display_round_trip(id);
    }

    if let Ok(id) = MemoryLinkId::from_str(input) {
        assert_display_round_trip(id);
        assert!(MemoryId::from_str(input).is_err());
    }
    if let Ok(id) = WorkspaceId::from_str(input) {
        assert_display_round_trip(id);
        assert!(MemoryId::from_str(input).is_err());
    }
    if let Ok(id) = PackId::from_str(input) {
        assert_display_round_trip(id);
        assert!(MemoryId::from_str(input).is_err());
    }
});
