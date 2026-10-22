//! The launch clock and the client clock every app-held bearer's refresh
//! reads are ONE clock: moving it here moves the one `fauna-client`'s
//! `TokenCache` and `fauna-anon-client`'s mint anchor read. Two statics
//! would let case M go green on a seat whose refresh never saw the offset.
//!
//! **Its own test binary, and the only test in it.** The offset is
//! process-global and every launch-machine test reads it, so this test set
//! −18 000 s and then 7 200 s under the lib binary's parallel siblings and
//! skewed whichever was reading the clock: on 2026-10-19 the public-CI
//! replay's `Test (rest of the workspace)` went red twice running on
//! `machine::tests::a_mint_reply_is_anchored_to_the_machines_own_clock`, its
//! deadline read exactly 18 000 s off. A separate binary is a separate
//! process, so nothing else here can observe the skew.
#![cfg(any(debug_assertions, feature = "e2e-agent"))]

use fauna_launch_machine::launch_clock::{
    clock_offset_secs, now_secs_or_zero, set_clock_offset_secs,
};

#[test]
fn the_launch_clock_is_the_client_clock() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            set_clock_offset_secs(0);
        }
    }
    let _reset = Reset;

    set_clock_offset_secs(-18_000);
    assert_eq!(fauna_protocol::client_clock::clock_offset_secs(), -18_000);
    assert!(
        (now_secs_or_zero() - fauna_protocol::client_clock::now_secs_or_zero()).abs() <= 10,
        "the launch clock must read the client clock's skewed instant"
    );
    fauna_protocol::client_clock::set_clock_offset_secs(7_200);
    assert_eq!(clock_offset_secs(), 7_200);

    set_clock_offset_secs(0);
    assert!(
        (fauna_core::data::Timestamp::now_secs_or_zero() - now_secs_or_zero()).abs() <= 10,
        "reset must return the launch clock to the real one"
    );
}
