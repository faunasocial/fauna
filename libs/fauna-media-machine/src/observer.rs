//! Reactivity callback for the Media page — clients re-render off a fresh
//! `MediaPageSnapshot` on every tick. Invoked synchronously after every state
//! mutation (a `refresh()` completing, or a view-state setter). Mirrors
//! `fauna_devices_machine::DevicesObserver`.

fauna_core::declare_snapshot_observer!(
    MediaObserver,
    null_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
    counting_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
);
