//! Reactivity callback. The `LaunchMachine` notifies its observer on every
//! state mutation; the observer reads a fresh `LaunchSnapshot` via the
//! machine's getter. `NullObserver` is available unconditionally (not just
//! under `test-observer`): run-once flows like the Linux launch path await
//! `LaunchMachine::start()` and read the final snapshot rather than reacting
//! to intermediate transitions.

fauna_core::declare_snapshot_observer!(
    LaunchObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
);
