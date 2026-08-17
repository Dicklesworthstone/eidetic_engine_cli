#![no_main]

//! T2.7 (`bd-tc-epic-qzk7o.3.9`) — untrusted frame-v2 decode must not panic.

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 8 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = ee::mesh::transport_session::decode_frame(data);
});
