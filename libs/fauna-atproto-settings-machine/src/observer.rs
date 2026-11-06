//! Reactivity callback for the ATProto login-plane settings surface — clients
//! re-render off a fresh [`crate::snapshots::AtprotoSettingsSnapshot`] on every
//! tick. Mirrors `fauna_labeler_catalog_machine::observer`.

// `NullObserver` is ungated: tui repaints from a fresh `snapshot()` after every
// awaited gesture, so the machine's own callback has nothing to do (the
// `fauna-launch-machine` shape).
fauna_core::declare_snapshot_observer!(
    AtprotoSettingsObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, feature = "test-observer")),
);
