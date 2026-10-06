//! App-launch state machine — owns the four-case launch decision tree and
//! the bearer-token lifecycle (TTL refresh + 401-reactive).
//!
//! Reading order:
//! - `docs/goal/behavior/onboarding.md` § App-launch routing (lines 230–252)
//! - `docs/goal/behavior/login.md` (auth wire format)
//! - implementation brief tracked internally (revised)
//!
//! Mirrors the structure of `fauna-onboarding-machine` so the existing
//! UniFFI / WASM toolchains in clients pick this up without per-platform
//! glue.

// `AuthConnector` is bounded by `MaybeSendSync` (`Send + Sync` natively, empty
// on wasm32), so `Arc<LaunchMachine>` (which holds one) is correctly
// `!Send`/`!Sync` on wasm32 but trips `arc_with_non_send_sync` there.
// wasm32-scoped so native, where the bound resolves to `Send + Sync`, keeps
// the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_launch_machine");

pub(crate) mod auth;
pub(crate) mod connector;
pub mod dial;
pub mod error;
pub mod launch_clock;
pub mod machine;
pub mod observer;
pub mod persistence;
pub(crate) mod probe;
pub mod render_text;
pub mod snapshots;
pub(crate) mod state;

pub use machine::LaunchMachine;

pub use dial::resolved_dial_url;
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub use dial::{nest_dial_override, set_nest_dial_override};

pub use auth::{SilentChallengeOutcome, SilentSignInVerdict, classify_silent_challenge};
pub use connector::{AuthConnector, WsAuthConnector};
pub use error::LaunchError;
pub use observer::LaunchObserver;
pub use persistence::{
    AccountIndexRefusal, AwaitingDnsRecord, LaunchPersistence, PendingFactoryResetRecord,
    PendingInviteRecord, PendingProvisionStore, complete_pending_provision_reach,
    mint_and_persist_pending_factory_reset, mint_and_persist_pending_provision,
    persist_pending_provision,
};
pub use probe::ClaimProbe;
pub use snapshots::*;

pub use observer::NullObserver;

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use connector::MockAuthConnector;
#[cfg(any(test, debug_assertions, feature = "test-observer"))]
pub use observer::CountingObserver;
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use persistence::InMemoryPersistence;
