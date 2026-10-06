//! Page-level ATProto login-plane settings state machine.
//!
//! Mirrors `fauna-labeler-catalog-machine`: a shared-Rust `#[uniffi::Object]`
//! state machine (`AtprotoSettingsMachine`) driving an observer-rendered page,
//! exposed to native apps via UniFFI (`libs/fauna-ffi`) and to web via WASM.
//! It owns the client half of ATProto **F1**: app-credential mint /
//! list / re-reveal / revoke, the live-session list, and the per-account
//! external-apps kill-switch.
//!
//! Surface it renders (`docs/goal/behavior/atproto-pds-full.md` § Client
//! surface): `atproto-app-credentials-list` + `atproto-app-credential-item` /
//! `-mint` / `-reveal` / `-revoke`, `atproto-external-apps-enable`, and the
//! session rows of `atproto-connected-apps-list`. The OAuth *grant* rows and
//! the consent card on that same page arrive with F4.
//!
//! ## Why this is a crate and not a module
//!
//! The two-layer split is the established shape for a page machine: a thin,
//! wasm-clean typed-call crate owning the kinds, and a machine crate owning the
//! state — `fauna-client-labelers` ↔ `fauna-labeler-catalog-machine`, and
//! `fauna_client_bridges::MailAccountClient` ↔ `fauna-client-mail-settings`.
//! Here the typed half is `fauna_client_bridges::AtprotoSettingsClient`, which
//! stays in `fauna-client-bridges` because that crate already owns the
//! `fauna.bridges.*` namespace *and* the `atproto_credential` primitives.
//!
//! The machine could not join it there: `fauna-client-bridges` is deliberately
//! a 4-dependency, wasm-clean, **non-optional base dependency** of
//! `fauna-client-mail-settings` (and now of this crate), while a machine needs
//! `fauna-client-config`, `uniffi` scaffolding and `tracing`. Folding those in
//! would fatten the leaf crate that everything else builds on top of.
//!
//! Named for `atproto`, not `bluesky`, because that is what the code it binds
//! is called end-to-end — the `fauna.bridges.atproto.*` kinds, the
//! `fauna.state.atproto` custody kind, `fauna_protocol::atproto_pds`,
//! `fauna-bridge-atproto` — and because the credentials it mints authenticate
//! any ATProto client, not only the Bluesky app. The page it drives is named
//! the same way: ui.yaml page `atproto` ("AT Protocol"), `atproto-*` element
//! IDs, the `atproto_settings` i18n block (`ui/atproto.md`;
//! `behavior/atproto-pds-bridge.md` § Naming).

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_atproto_settings_machine");

pub mod consent_grant;
pub mod contest;
pub mod credentials;
pub mod custody;
pub mod delegation_clock;
pub mod depth;
pub mod error;
pub mod machine;
pub mod nest_api;
pub mod observer;
#[cfg(feature = "account-port")]
pub mod port;
pub mod retirement;
pub mod snapshots;

pub use consent_grant::{
    ConsentFolder, ConsentFolderSeam, ConsentGrantSeams, CustodyConsentFolders,
    consent_folder_from, consent_folder_names, names_twin,
};
pub use contest::{ContestActor, DirectoryContestActor};
#[cfg(any(test, feature = "test-helpers"))]
pub use contest::{FakeContestActor, FakeContestLog};
pub use credentials::AtprotoCredentialStore;
#[cfg(any(test, feature = "test-helpers"))]
pub use credentials::FakeCredentialStore;
#[cfg(any(test, feature = "test-helpers"))]
pub use custody::FakeGenesisVerifier;
pub use custody::{DirectoryGenesisVerifier, GenesisVerifier};
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use delegation_clock::set_clock_offset_secs as set_delegation_clock_offset_secs;
pub use depth::{DepthLevelOption, depth_level_options};
pub use error::AtprotoSettingsError;
pub use machine::{AtprotoSettingsMachine, consent_card_row, scope_words};
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_atproto_settings_machine;
pub use nest_api::{
    AtprotoSettingsApiError, AtprotoSettingsNestApi, CredentialListing, IntegrationStatus,
    LinkSummary, NestConsentRow, NestConsentSet, NestCredentialRow, NestIdentitySummary,
    RevokeOutcome,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::{FakeAtprotoSettingsNestApi, FakeCall};
pub use observer::AtprotoSettingsObserver;
pub use snapshots::*;
