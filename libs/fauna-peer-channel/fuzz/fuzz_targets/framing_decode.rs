#![no_main]

//! PQ-2 target 1 — arbitrary bytes as a peer byte stream into
//! [`fauna_peer_channel::PeerStreamAdapter`]'s length-delimited decode.
//! The shared check body asserts the max-frame bound and must never panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fauna_peer_channel::hardening::check_framing_decode(data);
});
