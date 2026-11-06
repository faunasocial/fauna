//! Reactivity callback for the Devices page — clients re-render off a fresh
//! `DevicesSnapshot` on every tick. Invoked synchronously after every state
//! mutation (and forwarded from the embedded wizard's observer, so opening /
//! driving the wizard ticks the page too). Mirrors
//! `fauna_folders_machine::FolderWizardObserver`.

// `NullObserver` is ungated: tui and linux repaint from a fresh `snapshot()`
// after every awaited gesture, so the machine's own callback has nothing to do
// (the `fauna-launch-machine` shape).
fauna_core::declare_snapshot_observer!(
    DevicesObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, feature = "test-observer")),
);
