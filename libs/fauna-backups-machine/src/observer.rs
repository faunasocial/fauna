//! Reactivity callback for the Backups page — clients re-render off a fresh
//! `BackupsSnapshot` on every tick. Invoked synchronously after every state
//! mutation. Mirrors `fauna_devices_machine::DevicesObserver`.

// `NullObserver` is ungated: tui repaints from a fresh `snapshot()` after every
// awaited gesture, so the machine's own callback has nothing to do (the
// `fauna-launch-machine` shape).
fauna_core::declare_snapshot_observer!(
    BackupsObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, feature = "test-observer")),
);
