//! The `NullObserver`/`CountingObserver` pair every `fauna-*-machine` crate
//! needs alongside its own reactivity trait: production wants a no-op some
//! machines can hand out directly, tests want one that counts calls. Eight
//! crates had hand-copied an identical pair (same fields, same atomics,
//! differing only in the trait name and how widely each helper is compiled
//! in) — [`declare_snapshot_observer`] is the one definition they now all
//! expand, so the shape can't drift between them again.

/// Declare a state machine's reactivity observer trait — `$trait_name:
/// Send + Sync` with one `on_changed(&self)` method,
/// `#[uniffi::export(with_foreign)]` so native FFI consumers can implement it.
///
/// Also declares the two standard helpers. `NullObserver` is a no-op.
/// `CountingObserver` counts calls, for tests asserting "the machine
/// notified N times".
///
/// The 1-arg form gates both helpers to `cfg(any(test, feature =
/// "test-observer"))`, the shape most crates want. The 3-arg form lets a
/// crate widen either gate — e.g. `cfg(any(test, debug_assertions, feature =
/// "test-observer"))`, or `cfg(all())` (unconditionally true) for a crate
/// that wants `NullObserver` available in production too, such as a
/// run-once flow that awaits the final snapshot instead of reacting to
/// callbacks.
///
/// ```ignore
/// fauna_core::declare_snapshot_observer!(DevicesObserver);
///
/// fauna_core::declare_snapshot_observer!(
///     LaunchObserver,
///     null_gate: cfg(all()),
///     counting_gate: cfg(any(test, debug_assertions, feature = "test-observer")),
/// );
/// ```
#[macro_export]
macro_rules! declare_snapshot_observer {
    ($trait_name:ident) => {
        $crate::declare_snapshot_observer!(
            $trait_name,
            null_gate: cfg(any(test, feature = "test-observer")),
            counting_gate: cfg(any(test, feature = "test-observer")),
        );
    };
    (
        $trait_name:ident,
        null_gate: $null_gate:meta,
        counting_gate: $counting_gate:meta $(,)?
    ) => {
        #[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
        pub trait $trait_name: Send + Sync {
            /// Called whenever the machine's observable state changes. The
            /// observer reads a fresh snapshot via the machine's own getter.
            fn on_changed(&self);
        }

        /// No-op observer.
        #[$null_gate]
        pub struct NullObserver;

        #[$null_gate]
        impl $trait_name for NullObserver {
            fn on_changed(&self) {}
        }

        /// Test observer that counts notifications.
        #[$counting_gate]
        pub struct CountingObserver {
            pub count: ::std::sync::atomic::AtomicU64,
        }

        #[$counting_gate]
        impl CountingObserver {
            pub fn new() -> ::std::sync::Arc<Self> {
                ::std::sync::Arc::new(Self { count: 0.into() })
            }
            pub fn count(&self) -> u64 {
                self.count.load(::std::sync::atomic::Ordering::SeqCst)
            }
        }

        #[$counting_gate]
        impl $trait_name for CountingObserver {
            fn on_changed(&self) {
                self.count.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
            }
        }
    };
}
