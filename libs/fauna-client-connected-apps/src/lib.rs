//! The connected-apps page machine (`docs/goal/ui/connected-apps.md`).
//!
//! One page shows the user everything that acts for them from outside the
//! seven apps. This crate composes it once for all seven:
//!
//! - **the roster** — every third-party principal (`fauna.principals.list`),
//!   plus the rows of the legacy rosters the page lifts: ATProto app-password
//!   sessions, NIP-46 bunker apps and mail app passwords (and the OAuth
//!   grants, whose horizons a principal row falls back to when the grant log
//!   is unreadable);
//! - **the Requests tray** — pending consents, composed by the ATProto
//!   settings machine's own card composition (one card, never a second);
//! - **Connect an app** — the typed-code start (`fauna.oauth.consent.lookup_code`);
//! - **per-client blocks** — "Never show requests from this app".
//!
//! Every derivation lives here (priority #2): scope words, the row's class
//! (the grouping key), *lasts-until*, and **which verb revokes a row** — an app
//! never chooses it.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_connected_apps");

pub mod client;
pub mod machine;
pub mod nest_api;
pub mod observer;
pub mod snapshots;

pub use client::ConnectedAppsClient;
pub use machine::ConnectedAppsMachine;
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_connected_apps_machine;
pub use nest_api::{ConnectedAppsApiError, ConnectedAppsNestApi};
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::{FakeCall, FakeConnectedAppsNestApi};
pub use observer::ConnectedAppsObserver;
pub use snapshots::*;
