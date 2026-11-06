//! The forced UI-thread stall that proves the windows app's stall stack
//! (`e2e-conventions.md` convention 11, the windows app's leg).
//!
//! The windows app's TestAgent calls [`fauna_e2e_stall_fixture_parks_ui_thread`]
//! on its UI thread (command `e2e_stall_ui_thread`), so the thread sits inside
//! THIS library for a bounded time. A bridge op on the app then stalls, and the
//! FlaUI bridge's app-side verdict captures the thread's native stack from
//! outside the app; the e2e test asserts the report names this function with its
//! `file:line`, which no amount of `ntdll` frames can fake.
//!
//! A plain `extern "C"` export rather than a UniFFI one, so the generated
//! bindings are the same in every flavor; compiled only into a debug build or
//! the e2e flavor (`test-helpers`, which `just windows-ffi-test` turns on),
//! never into a shipped `fauna_ffi.dll` (convention 15).

use std::time::Duration;

/// The longest park the fixture takes, whatever it is asked for: a stall the
/// app cannot end would outlive the test that asked for it.
pub const STALL_FIXTURE_MAX_MILLIS: u32 = 300_000;

/// Park the calling thread for `millis` milliseconds (at most
/// [`STALL_FIXTURE_MAX_MILLIS`]) in a frame whose name nothing else in this
/// workspace has. Never inlined, so the frame exists for the walk to find.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn fauna_e2e_stall_fixture_parks_ui_thread(millis: u32) {
    std::thread::sleep(Duration::from_millis(u64::from(
        millis.min(STALL_FIXTURE_MAX_MILLIS),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_zero_park_returns_at_once() {
        let started = Instant::now();
        fauna_e2e_stall_fixture_parks_ui_thread(0);
        // A generous bound on a zero-length sleep: this proves only that the
        // call returns, never a duration (convention 14).
        assert!(started.elapsed() < Duration::from_secs(30));
    }
}
