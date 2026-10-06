#![no_main]

//! PQ-2 target 2 — arbitrary bytes through the dispatcher's wire decode
//! (`fauna_protocol::envelope::decode_frame` + the `Request`/`Reply` shapes).
//! Must error on garbage, never panic/OOM.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fauna_peer_channel::hardening::check_frame_decode(data);
});
