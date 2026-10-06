//! Re-exports the LaunchMachine so its UniFFI exports surface in the
//! generated Swift / Kotlin / C# bindings. The machine itself lives in
//! libs/fauna-launch-machine; this file is a thin glue layer that mirrors
//! the onboarding.rs pattern.

pub use fauna_launch_machine::{
    AwaitingDnsRecord, LaunchError, LaunchMachine, LaunchObserver, LaunchPersistence, LaunchPhase,
    LaunchSnapshot, LaunchWizardEntry, PendingInviteRecord, RefreshReason, TokenStatus,
};

/// UniFFI face of [`fauna_launch_machine::launch_clock::clock_offset_secs`] —
/// the launch clock's e2e offset, for the `clock` state key a native shell's
/// automation agent publishes (`fauna_e2e_agent::CLOCK_KEY` owns the shape:
/// `{"offset_secs", "now_secs"}`, this getter and
/// [`launch_clock_now_secs_for_test`]). The wrong-clock launch witness reads it
/// as the in-app control that the `FAUNA_E2E_CLOCK_OFFSET_SECS` seed reached
/// the process that signed in.
///
/// Feature-keyed, never profile-keyed (convention 15): compiled only into the
/// test-flavored native FFI build, whose `test-helpers` forwards
/// `fauna-launch-machine/e2e-agent` — the gate on the offset itself, which a
/// `--release` build has no `debug_assertions` to open. `*_for_test` so every
/// flavor-diff witness classifies it as a seam.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn launch_clock_offset_secs_for_test() -> i64 {
    fauna_launch_machine::launch_clock::clock_offset_secs()
}

/// UniFFI face of [`fauna_launch_machine::launch_clock::now_secs_or_zero`] —
/// epoch seconds on the launch clock (real clock plus the offset above). Same
/// gating and purpose as [`launch_clock_offset_secs_for_test`].
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn launch_clock_now_secs_for_test() -> i64 {
    fauna_launch_machine::launch_clock::now_secs_or_zero()
}
