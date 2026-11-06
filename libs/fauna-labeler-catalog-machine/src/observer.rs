//! Reactivity callback for the labeler-catalog page — clients re-render off a
//! fresh `LabelerCatalogSnapshot` on every tick. Mirrors
//! `fauna_devices_machine::observer`.

// `NullObserver` is ungated: the linux room editor repaints from each gesture's
// own completion, so the machine's callback has nothing to do (the
// `fauna-devices-machine` shape).
fauna_core::declare_snapshot_observer!(
    LabelerCatalogObserver,
    null_gate: cfg(all()),
    counting_gate: cfg(any(test, feature = "test-observer")),
);
