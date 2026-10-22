//! The ceremony admission clock moves with its offset and zeroes back to the
//! real one.
//!
//! **Its own test binary, and the only test in it.** The offset is
//! process-global and every seat the lib's `offline_share` tests bind reads it
//! (`offline_share::now_fn` reads it per call), so this test setting an hour
//! ahead and then a clock before the epoch under the lib binary's parallel
//! siblings could lapse or forward-date whichever expectation they minted in
//! the window. A separate binary is a separate process, so nothing else here
//! can observe the skew.
#![cfg(any(debug_assertions, feature = "e2e-agent"))]

use fauna_sync_engine::ceremony_clock::{now_secs, set_clock_offset_secs};

/// The offset moves the clock in both directions and zeroes back to the
/// real one.
#[test]
fn the_offset_moves_the_admission_clock_and_resets() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            set_clock_offset_secs(0);
        }
    }
    let _reset = Reset;

    let real = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
    assert!(
        now_secs().abs_diff(real) <= 2,
        "with no offset this is the wall clock"
    );

    set_clock_offset_secs(3600);
    assert!(
        now_secs() >= real + 3595,
        "an hour of offset must move the admission clock an hour"
    );

    // A negative offset is legal and must not underflow the u64 — a test
    // that moves the clock backwards past the epoch would otherwise wrap
    // to a colossal `now` and admit everything for ever.
    set_clock_offset_secs(-(real as i64) - 10_000);
    assert_eq!(now_secs(), 0, "the clock floors at zero, never wraps");

    set_clock_offset_secs(0);
    assert!(
        now_secs().abs_diff(real) <= 2,
        "zero puts the real clock back"
    );
}
