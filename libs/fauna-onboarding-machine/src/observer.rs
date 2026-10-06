//! Reactivity callback — clients notify their view layer that they should
//! re-render. Invoked synchronously after every state mutation. Clients
//! debounce / throttle on their side if the framework demands it.

fauna_core::declare_snapshot_observer!(
    OnboardingObserver,
    null_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
    counting_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
);
