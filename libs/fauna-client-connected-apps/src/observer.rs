//! Reactivity callback for the connected-apps page — apps re-render off a
//! fresh `ConnectedAppsSnapshot` on every tick. Mirrors
//! `fauna_labeler_catalog_machine::observer`.

// `NullObserver` is ungated: tui and linux repaint from a fresh `snapshot()`
// after every awaited gesture, so the machine's own callback has nothing to do
// (the `fauna-launch-machine` shape).
fauna_core::declare_snapshot_observer!(
    ConnectedAppsObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, feature = "test-observer")),
);
