//! UniFFI bindings for Fauna iOS and Android apps.
//!
//! Wraps fauna-core functions for native Swift/Kotlin consumption.
//! Mirrors fauna-wasm's surface but uses #[uniffi::export]
//! instead of #[wasm_bindgen].

use fauna_core::data::ContentHash;
use fauna_core::identity::{ActorId, ActorKeypair};

uniffi::setup_scaffolding!();

mod identity;
pub use identity::*;

// qr: the shared QR module-matrix encoder the identity-export section draws
// (settings.md § Identity export). Feature-gated so the Go mail-bridge build
// (`--no-default-features --features labeler`) doesn't carry a QR encoder it never
// renders — which also keeps the tracked Go bindings out of this surface's churn.
#[cfg(feature = "qr-render")]
mod qr;
#[cfg(feature = "qr-render")]
pub use qr::*;

mod auth;
pub use auth::*;

mod crypto;
pub use crypto::*;

mod chunking;
pub use chunking::*;

// scan: placeholder module for future media-scanning FFI bindings — body is a stub
// today (`//! Stub`), declared but not re-exported so cargo doesn't warn.
mod scan;
// spam: the shared spam-prefs *presentation* contract (spam_threshold_band). `#[uniffi::export]` registers the scaffolding from the
// compiled module, so no `pub use` is needed for the bindings.
mod spam;

// handle: the shared canonical handle-format validator
// (`fauna_protocol::handle::validate_handle`). Like `spam`, its
// `#[uniffi::export]` fn returns a built-in `Option<String>` — no custom type
// crosses the boundary, so no `value-format`-style feature gate.
mod handle;

// nostr_relay: the shared Nostr relay-URL validation and list-add rule
// (`fauna_protocol::nostr_relay`). Like `version`, its `#[uniffi::export]` fns
// are built-in-typed — only `relay_url_error`, which returns a `LocalizedText`,
// carries the `value-format` feature gate.
mod nostr_relay;

// version: the shared release-version comparison
// (`fauna_core::version::is_newer`) and the update notice's asked check +
// once-per-sign-in look (`fauna_client::update_look`). Only built-ins and this
// module's own records cross the boundary — no custom `fauna_core` type — so no
// `value-format`-style feature gate (`fauna_core`/`fauna_client` are already
// non-optional deps). Lets the client update-checkers drop hand-rolled tuple
// parsers (windows `UpdateService.IsNewer`) and per-app feed round trips.
mod version;

// caltime: the shared parity-bearing calendar primitives — week/day overlap
// packing (`find_overlaps`), the full day-column layout (all-day/timed
// classification + timed minute geometry + packing), and event-datetime input
// normalization. Returns fauna-ffi-LOCAL `uniffi::Record` mirrors /
// built-ins — self-contained, no bare `fauna_core` type crosses the boundary,
// so no feature gate (like `markdown`). The natives keep their platform date
// library for Gregorian date math (events.md § Where logic lives).
mod caltime;

// offline: the shared offline-affordance gate
// (`fauna_protocol::offline_class::affordance`) — W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing
// decision, so Swift/Kotlin/C# read the one rule instead of re-deriving
// `class == OnlineOnly` per app. Its record carries a bare `fauna_core`
// `LocalizedText` (the per-affordance reason), so it is `value-format`-gated for
// the same Go-incompatibility reason as `nostr_key_source_label`; the Go
// mail-bridge has no UI and therefore no gate to apply.
mod offline;

// Shared recipient-input classification (`fauna_core::resolve::classify_recipient`).
// Like `spam`, its `#[uniffi::export]` fn returns built-in `Vec<String>` — no
// `fauna_core` custom type crosses the boundary, so no `value-format`-style feature gate.
mod resolve;

// Shared NIP-23 Markdown parser (`fauna_core::markdown::parse_markdown`). Returns
// fauna-ffi-LOCAL `uniffi::Record` mirrors (FfiMdBlock/Line/Span), so the Go binding is
// self-contained — no bare-`fauna_core` import, hence no feature gate (unlike value_format).
mod markdown;

// Shared post-source classification (`fauna_feed::classify_sources`). Its
// `#[uniffi::export]` fn returns a fauna-ffi-LOCAL `uniffi::Record` (FfiSourceBadge)
// — BUT its `glyph` field is a `fauna_core::source_glyph::SourceGlyph` (the shared
// source-icon concept the conversations rail also resolves to, so a client keys ONE
// `SourceGlyph → asset` map off both surfaces — render-model.md § Deltas → D5), for
// which uniffi-bindgen-go emits an uncompilable bare `fauna_core` cross-namespace
// import. So it's gated `feed-badge` (default-on; off in the Go mail-bridge
// `--no-default-features` build) for the same reason as `render`/`value-format`/
// `search`. The bridge has no feed-badge surface. See Cargo.toml `feed-badge`.
#[cfg(feature = "feed-badge")]
mod feed;

// The create-feed rule-builder *presentation* FFI (src/feed_rules.rs — the
// 11-type picker catalog + input-kind classification, the added-rule summary
// chip, the required/excluded toggle label). Every export returns a `fauna_core`
// LocalizedText, so it's gated `feed-rules` (default-on; off in the Go
// mail-bridge `--no-default-features` build) for exactly the same
// uniffi-bindgen-go bare-`fauna_core`-import reason as `feed-badge` above. The
// bridge has no create-feed surface. Note the rule builder's *encoding* half
// (`encode_filter_rule` / `normalize_filter_rules`) lives UNGATED in
// src/feed_client.rs and does cross to Go — it returns only built-in types.
// See Cargo.toml `feed-rules`.
#[cfg(feature = "feed-rules")]
mod feed_rules;
#[cfg(feature = "feed-rules")]
pub use feed_rules::*;

// The shared stateful Feed-page manager façade (`FfiFeedManager` over
// `fauna_feed::FeedManager<Arc<NestClient>>`). Unlike `feed`, it returns the
// cross-crate `fauna_feed` snapshot types DIRECTLY (no mirrors — priority #2),
// so it's gated `feed-manager` (default-on; off in the Go mail-bridge
// `--no-default-features` build) for the same uniffi-bindgen-go cross-namespace
// reason as `logs`/`web-content`/`conversations-session`. See src/feed_manager.rs.
#[cfg(feature = "feed-manager")]
mod feed_manager;
#[cfg(feature = "feed-manager")]
pub use feed_manager::*;
// The Search-page façade returns cross-crate `fauna_client_search` snapshot types
// + the SearchSnapshotObserver foreign trait, which uniffi-bindgen-go can't emit,
// so it's gated `search-manager` (default-on; off in the Go mail-bridge
// `--no-default-features` build) — same Go-incompatibility reason as
// `feed-manager`/`logs`/`web-content`. See src/search_manager.rs.
#[cfg(feature = "search-manager")]
mod search_manager;
#[cfg(feature = "search-manager")]
pub use search_manager::*;

// Shared spam/phishing text heuristic (`fauna_core::text_heuristic::classify_text`). Like
// `markdown`/`feed`, its `#[uniffi::export]` fn returns a fauna-ffi-LOCAL `uniffi::Record`
// (FfiClassifyLabel, `String`/`f64` fields), so the Go binding is self-contained — no
// feature gate. Lets native apps drop hand-rolled copies (android `TextHeuristic.kt`).
mod text_heuristic;

mod email;
pub use email::*;

mod post;
pub use post::*;

mod peer;
pub use peer::*;

pub mod provisioning;
pub use provisioning::*;

mod onboarding;
pub use onboarding::*;

// Gated behind the default-on `folders` feature so the Go mail-bridge's
// `--no-default-features` FFI build drops the wizard machine (and its native
// `rpc-glue` client crates), same as `mail-admin` / `pairing`.
#[cfg(feature = "folders")]
mod folders;
#[cfg(feature = "folders")]
pub use folders::*;
// The shared on-demand presence plan + device-local toggle store — same
// `folders` gate (it rides the folders machine crate).
#[cfg(feature = "folders")]
mod on_demand_presence;
#[cfg(feature = "folders")]
pub use on_demand_presence::*;
// Page-level Devices machine — same `folders` gate (shares the wizard machine's
// native `rpc-glue` client crates the Go bridge build can't emit).
#[cfg(feature = "folders")]
mod devices;
#[cfg(feature = "folders")]
pub use devices::*;
// Page-level Backups machine (the snapshot half) — same `folders` gate: its
// `rpc-glue` seam pulls the same native app crates the Go bridge build
// can't emit.
#[cfg(feature = "folders")]
mod backups;
#[cfg(feature = "folders")]
pub use backups::*;
// The photo-library ingress binding (apple PhotoKit / android MediaStore) — the
// adopt-or-create resolver over the same wizard machine, so the same `folders`
// gate.
#[cfg(feature = "folders")]
mod photo_library;
#[cfg(feature = "folders")]
pub use photo_library::*;
// Page-level Media machine (the cross-set all-media browser — content plane).
// Same `folders` gate (shares the native `rpc-glue` client crate the Go bridge
// build can't emit).
#[cfg(feature = "folders")]
mod media;
#[cfg(feature = "folders")]
pub use media::*;
// Owner-side shared-folder content-key orchestration (share_set / remove_member
// / resume_pending_removals over FoldersAuthor). Gated behind the default-on
// `folders-author` feature (which flips on `fauna-client-folders/mls` + implies
// `conversations-session`) so the Go mail-bridge `--no-default-features` build
// drops it — the fns take the cross-crate `ConversationsSession` Object, same
// Go-incompatibility reason as `conversations-session` / `subscriptions-author`.
#[cfg(feature = "folders-author")]
mod folders_author;
#[cfg(feature = "folders-author")]
pub use folders_author::*;

// Recipient-side shared-folder pending-share surface (list / accept / decline).
// Gated behind the default-on `conversations-session` feature — the accept fn
// takes the cross-crate `ConversationsSession` Object — so the Go mail-bridge
// `--no-default-features` build drops it (no share UI), same Go-incompatibility
// reason as `conversations-session` / `folders-author`. Distinct from
// `folders-author` (which pulls `fauna-client-folders/mls`): the recipient
// surface needs only the inbox client + the conversations session, not the
// owner-side content-key crate.
#[cfg(feature = "conversations-session")]
mod folders_recipient;
#[cfg(feature = "conversations-session")]
pub use folders_recipient::*;

// Cross-device MLS state-sync launcher — the native (apple/windows/android) leg of
// the replica plane. The FFI `conversations_session` factory injects it via
// `ConversationsSession::set_mls_sync_launcher`, and `start_receive_loop` runs it
// once before the first poll (restore-before-first-poll + gate/cursor inject +
// debounced autosave). Gated on `conversations-session` (it holds the cross-crate
// `MlsStateSync` + `ConversationsSession` handles) so the Go mail-bridge
// `--no-default-features` build drops it. Internal — no UniFFI export: the plane
// activates entirely from the shared factory, no per-app glue (priority #2).
// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync.
// The MEMBER side of a succession — the in-group witness + its peer-anchor
// harvest sweep, registered from the conversations session factory so all four
// FFI legs inherit them with no per-app glue. The receive-side twin of
// `succession_aftermath`, which is the side that RUNS one.
// `succession-aftermath.md` § Propagation → *MLS groups*.
#[cfg(feature = "conversations-session")]
mod succession_witness;

#[cfg(feature = "conversations-session")]
mod mls_sync_launch;

// Content-index launcher — the native (apple/windows/android) leg of the
// per-user sealed index. Same shape as `mls_sync_launch` above: the FFI
// `conversations_session` factory builds `NestMailIndexLauncher` and injects it
// via `ConversationsSession::set_index_builder_launcher` (build side) while
// stashing it for `attach_local_search_index` (query side) — internal, no UniFFI
// export, no per-app glue. Also owns `CLIENT_BUILDS_INDEX`, the ratified
// desktop-builds / phone-queries split, which `task_delegation` reads so the
// picker and the builder cannot disagree. `docs/goal/behavior/content-index.md`
// § Where the index is built + § Where queries run.
//
// NOT gated as a whole: the constant is needed by `task-delegation` too, and the
// two features are independently switchable. The launcher half inside is
// `conversations-session`-gated.
mod index_launch;

mod launch;
pub use launch::*;

mod mail;
pub use mail::*;

// atproto: the PDS-bridge projection translator — deterministic TID minting +
// stored Post/Profile bytes → `app.bsky.*` record JSON, over the pure
// `fauna-bridge-atproto` translation core (`default-features = false`: no
// reqwest/tokio/atrium — see that crate's `client` feature). Only built-in
// types cross the boundary, and the consumer is the Go atproto.pds bridge
// itself, so the module is UNgated: the `--no-default-features --features
// labeler` Go build must see these exports (unlike the client-only modules
// gated above). `atproto-pds-bridge.md` § Where logic lives.
mod atproto;
pub use atproto::*;

#[cfg(feature = "labeler")]
mod labeler;
#[cfg(feature = "labeler")]
pub use labeler::*;

#[cfg(feature = "labeler-catalog")]
mod labeler_catalog;
#[cfg(feature = "labeler-catalog")]
pub use labeler_catalog::*;

#[cfg(feature = "connected-apps")]
mod connected_apps;
#[cfg(feature = "connected-apps")]
pub use connected_apps::*;

#[cfg(feature = "atproto-settings")]
mod atproto_settings;
#[cfg(feature = "atproto-settings")]
pub use atproto_settings::*;

#[cfg(feature = "atproto-settings")]
mod critical_alerts;
#[cfg(feature = "atproto-settings")]
pub use critical_alerts::*;

mod media_upload;
pub use media_upload::*;

pub mod segment_backup;
pub use segment_backup::*;

// Backup-destination management FFI (config read + mutate-and-save for the
// 5-client lift). Gated default-on so the Go mail-bridge `--no-default-features`
// build drops it — the bridge has no destination-management UI.
#[cfg(feature = "backup-destinations")]
mod backup_destinations;
#[cfg(feature = "backup-destinations")]
pub use backup_destinations::*;
// The shell's side of a store's cloud-backup exclusion — shared by the
// custodian host below and the account runtime (both stores state one), so it
// rides no feature gate: the account runtime is not gated on backup features.
mod cloud_backup;
pub use cloud_backup::*;
// The mobile custodian-hosting face. Same gate and dead-code rationale as
// `backup_destinations`, whose config surface it is the hosting half of.
#[cfg(feature = "backup-destinations")]
mod custodian_host;
#[cfg(feature = "backup-destinations")]
pub use custodian_host::*;
// The re-seed gesture's shared face — the rows it paints on and the result it
// hands back; its two ceremony entry points sit beside the store each reads
// (`custodian_host` in-process, `sync_agent_provisioning` via the agent).
#[cfg(feature = "backup-destinations")]
mod reseed;
#[cfg(feature = "backup-destinations")]
pub use reseed::*;

// Deployment-seed custody-map FFI (the recovery reads over the account plane,
// the custody leg's post-auth entry, the plane rotation drive). Gated default-on
// so the Go mail-bridge `--no-default-features` build drops it — the bridge has
// no onboarding/recovery surface.
#[cfg(feature = "deployment-seed")]
mod deployment_seed;
#[cfg(feature = "deployment-seed")]
pub use deployment_seed::*;

// The Task-delegation Settings surface FFI (src/task_delegation.rs — the Ffi*
// mirrors of the shared delegation view-model types + FfiTaskDelegationView over
// `fauna_client_delegation::TaskDelegationView` + FfiNestClient::task_delegation_view_for_device).
// Gated default-on so the Apple/Android/Windows app FFI exports it for the
// `task-delegation` page, but gated so the Go mail-bridge `--no-default-features`
// build drops it — the bridge is a server with no Settings UI, its row's
// `name: LocalizedText` return would make uniffi-bindgen-go emit an uncompilable
// bare `fauna_core` import (same Go-incompatibility reason as `value-format` /
// `render`), and it pulls the `fauna-client-delegation` dep. See Cargo.toml
// `task-delegation`.
#[cfg(feature = "task-delegation")]
mod task_delegation;
#[cfg(feature = "task-delegation")]
pub use task_delegation::*;

// Client host-address reporting FFI (classify the dial-address, never publish a
// private/LAN one, report the public IP via `fauna.dns.set_host_address`). Gated
// default-on so the Go mail-bridge `--no-default-features` build drops it — the
// bridge has no onboarding surface.
#[cfg(feature = "host-address")]
mod host_address;
#[cfg(feature = "host-address")]
pub use host_address::*;

// Tier-1 muted-keywords FFI (the `muted-words` Settings sub-page CRUD over the
// account-store seam + the matches_muted_keywords collapse-decision wrapper).
// Gated default-on so the Go mail-bridge `--no-default-features` build drops it
// — no muted-words UI on the bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "muted-keywords")]
mod muted_keywords;
#[cfg(feature = "muted-keywords")]
pub use muted_keywords::*;

// Sync-preferences FFI (the Folders page's "Sync defaults" section CRUD over
// the account-store seam — today the default conflict policy for new sets).
// Gated default-on so the Go mail-bridge `--no-default-features` build drops it
// — no sync-settings UI on the bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "sync-prefs")]
mod sync_prefs;
#[cfg(feature = "sync-prefs")]
pub use sync_prefs::*;

// Unattested-member review FFI (the Settings Recovery Kit's ephemeral pass +
// the permanent `member_review` page — the roster read, row-text formatting,
// and Keep/Remove verdict persistence; `ConversationsManager`'s eviction
// methods are exported directly on the manager, not here). Gated default-on
// so the Go mail-bridge `--no-default-features` build drops it — no
// settings UI on the bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "member-review")]
mod member_review;
#[cfg(feature = "member-review")]
pub use member_review::*;

// The private contact overlay's FFI face (`contacts.md` § The private overlay
// — the projection's reads, the private section's staged editor and its
// Save), the one face android, windows, macOS and iOS consume. Gated
// default-on so the Go mail-bridge `--no-default-features` build drops it.
#[cfg(feature = "contact-overlays")]
mod contact_overlays;
#[cfg(feature = "contact-overlays")]
pub use contact_overlays::*;

// The stolen-identity succession ceremony FFI (`identity-succession.md`
// § Implementation status today — the native leg macOS, iOS and windows owe).
// Composes `fauna_client_recovery::ceremony`'s shared orchestration into ONE
// export, because the ceremony's correctness is almost entirely its order.
// Gated default-on so the Go mail-bridge `--no-default-features` build drops it
// — no settings UI and no identity to succeed on the bridge; keeps the Go
// bindings byte-identical. Unlike its neighbours this gate carries a real dep.
#[cfg(feature = "recovery-ceremony")]
mod recovery;
#[cfg(feature = "recovery-ceremony")]
pub use recovery::*;

// The post-succession aftermath FFI (`succession-aftermath.md` § Re-key scope's
// `BackupKey` corpus row — "started at first successor sign-in, surfaced with
// progress, resumed until complete"). The ceremony's sibling half: `recovery`
// above hands the account over, this drives what the successor's first session
// then owes its inherited corpus. Composes
// `fauna_client_recovery::aftermath::run_succession_aftermath` into ONE export
// for the same reason `recovery` is one — the pass's correctness is its order.
// Same gating shape and rationale as its sibling: default-on via `store-safe`,
// dropped by the Go mail-bridge `--no-default-features` build (no identity to
// succeed on the bridge), keeping the checked-in Go bindings byte-identical.
#[cfg(feature = "recovery-aftermath")]
mod succession_aftermath;
#[cfg(feature = "recovery-aftermath")]
pub use succession_aftermath::*;

// Email-filter succession review FFI (the twin plane of member_review —
// the Privacy filter list's unattested-mark render + Keep verdict, and the
// post-succession raise). Gated default-on so the Go mail-bridge
// `--no-default-features` build drops it — no settings UI on the bridge;
// keeps the Go bindings byte-identical.
#[cfg(feature = "filter-marks")]
mod filter_marks;
#[cfg(feature = "filter-marks")]
pub use filter_marks::*;

// The T16 custody facet's boundary (`ui/devices.md` § Custody facet) — the FFI
// projection of the shared fold's three row families plus the owner-side revoke
// and the ceremony drive pass. Piece 2 only: the two store-writing gestures are
// deliberately absent while no UniFFI app can reach the W3 account store (the
// module docs carry the full reasoning). Gated default-on so the Go mail-bridge
// `--no-default-features` build drops it — no settings UI on the bridge; keeps
// the Go bindings byte-identical, and keeps the bridge off
// `fauna-sync-engine/account-runtime`, which this feature's dep turns on.
#[cfg(feature = "custody")]
mod custody;
#[cfg(feature = "custody")]
pub use custody::*;

// Trained-topic lifecycle FFI (the Personalization home's Trained-topics facet
// CRUD over the shared fauna_client_personalization::topics::TrainedTopics
// service — topic-factors.md § Authoring surface & picker). Gated default-on
// so the Go mail-bridge `--no-default-features` build drops it — no
// Personalization UI on the bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "personalization")]
mod personalization;
#[cfg(feature = "personalization")]
pub use personalization::*;

mod content_index;
pub use content_index::*;

// The MDA bridge's session-scoped mail/calendar index handle (rollout S5's FFI
// leg). Unconditional for the same reason `content_index` is: the Go
// mail-bridge build is its first consumer.
mod content_index_session;
pub use content_index_session::*;

mod conversations;
pub use conversations::*;

mod subscription;
pub use subscription::*;

// WS-RPC client façade: the per-actor connection (FfiNestClient) plus the
// Layer-3 typed-call clients for `fauna.account.*` (FfiAccountClient),
// `fauna.bridges.*` (FfiBridgesClient) and `fauna.email.*` (FfiEmailClient),
// with FFI mirrors of the CBOR / account / bridge / email wire types. Wraps
// libs/fauna-client{,-account,-bridges,-email}. The shared seam
// Apple/Windows/Android consume; Linux calls the same crates natively
// (tracked internally).
mod cbor;
pub use cbor::*;

mod nest_client;

// The loud surfaces' e2e seams (reconnect pace, connection reports, painted
// errors) — the test flavors only (convention 15).
#[cfg(feature = "test-helpers")]
mod e2e_seams;
pub use nest_client::*;

// TLS-trust startup wiring (the one `install_nest_identity_pin_store` free fn
// the native shells call so TOFU nest-identity pins survive restarts; linux
// does the same natively). security.md § Transport trust.
mod trust;
pub use trust::*;

mod account;
pub use account::*;

// The multi-account registry seam (Stage 1): FfiSecretStore callback
// interface + FfiAccountRegistry + the RegistryLaunchPersistence handle for
// LaunchMachine::new. long-term-store.md § Multi-account evolution. Gated
// default-on; the Go mail-bridge `--no-default-features` build drops it (the
// `Arc<dyn LaunchPersistence>` handle would drag the fauna_launch_machine
// namespace into uniffi-bindgen-go output — see Cargo.toml `accounts-registry`).
#[cfg(feature = "accounts-registry")]
mod accounts_registry;
#[cfg(feature = "accounts-registry")]
pub use accounts_registry::*;

// Where each account's class-1 stores live, and how they are erased
// (account-scoping.md § The scoping taxonomy /
// § Serialized switching). Rides the same default-on gate as the registry it
// complements: every app has account-scoped stores; only the Go mail-bridge
// build, which holds no user's local stores, drops it.
#[cfg(feature = "accounts-registry")]
mod account_state;
#[cfg(feature = "accounts-registry")]
pub use account_state::*;

// The question every erasing gesture asks before it erases: is another instance
// still serving an account the erase would reach (account-scoping.md
// § Concurrent instances). Rides the erase pair's gate — it asks about exactly
// what that pair sweeps.
#[cfg(feature = "accounts-registry")]
mod erase_guard;
#[cfg(feature = "accounts-registry")]
pub use erase_guard::*;

// The residue surface's record and re-sweep (account-scoping.md § Erasure
// follows scope → the residue surface) for the FFI seats — the same shared
// `SignOutResidue` / `retry_sign_out_residue` tui and linux call directly.
// Rides the erase pair's gate: it re-sweeps what that pair left behind.
#[cfg(feature = "accounts-registry")]
mod sign_out_residue;
#[cfg(feature = "accounts-registry")]
pub use sign_out_residue::*;

// The W3 account-store runtime this app HOSTS (account-data-plane.md § The
// account store → *The client-side lifecycle*) — the windows/macOS/iOS seat of
// the shared `fauna-client-account-runtime` assembly tui and linux already
// consume. No `pub use`: the surface it serves is `FfiNestClient`'s own
// start/stop/state exports in nest_client.rs, and the module's own items are
// crate-internal wiring.
#[cfg(feature = "account-runtime")]
mod account_runtime;

// The let-go of a dead generation (`account-data-taxonomy.md` § The generation
// machinery → *Fleet-scope reclamation*, clause (3)(j); `ui/settings.md`
// § Recovery kit, the fifth act) — the UniFFI door to the runtime handle's
// dead read and act, the shared copy projection and the confirm word. Rides
// `account-runtime`: the handle is its whole surface.
#[cfg(feature = "account-runtime")]
mod generation_let_go;
#[cfg(feature = "account-runtime")]
pub use generation_let_go::*;

// The store-change notice's foreign face — the one shared watch, relayed to
// the app's registered listener from the runtime's store edge
// (account-runtime.md § Multi-instance concurrency → *A runtime's own pump is
// a source of the notice too*, part 4).
#[cfg(feature = "account-runtime")]
mod store_change;
#[cfg(feature = "account-runtime")]
pub use store_change::*;

// The offline co-present share ceremony FFI (p2p.md § Offline share
// initiation) — android/windows/macos/ios reach
// `fauna_client_capabilities::group_ceremony_node` through this one module,
// the same crate tui and linux already call directly. Needs
// `account-runtime` (the initiate/consent acts refuse with no
// `AccountStoreHandle`) and `fauna-client-capabilities`'s `p2p-share` +
// `uniffi` features (composed in this crate's own Cargo.toml feature).
#[cfg(feature = "offline-share")]
mod offline_share;
// The cross-user share plane's host — the one the four native apps share
// (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer).
#[cfg(feature = "p2p-share")]
mod share_plane;
#[cfg(feature = "offline-share")]
pub use offline_share::*;
#[cfg(feature = "p2p-share")]
pub use share_plane::*;

mod admin;
mod family;
pub use admin::*;
pub use family::*;

// The controversial-class feature plane's transparency read. Gated on
// `value-format` for its LocalizedText returns (the bare-`fauna_core`-import
// footgun below), NOT on `payments`/`zaps`: the plane gates three members, so
// its read answers for whichever ones a build ships — same reason the nest's
// handler is ungated (`dynamic-features.md` § Implementation status item (c)).
#[cfg(feature = "value-format")]
mod features;
#[cfg(feature = "value-format")]
pub use features::*;

// The `fauna://` in-app routes (`fauna_core::app_route`) an app parses off its
// launch arguments — today the Windows Explorer Share leaf's hand-off. Gated on
// `value-format` like `features`: the Go mail-bridge has no launch surface.
#[cfg(feature = "value-format")]
mod app_route;
#[cfg(feature = "value-format")]
pub use app_route::*;

// The shared Status snapshot's FFI face (`ui/status.md` § State & data shape) —
// the node read and the `status-*` text projection. Gated on `value-format`
// like `features`: the Go mail-bridge has no Status surface.
#[cfg(feature = "value-format")]
mod status;
#[cfg(feature = "value-format")]
pub use status::*;

// The region content plane's app side (`region-blocking.md` § The content
// plane) — the device's plane, its composed render and its settings view.
// Gated on `value-format` like `family`'s content-policy faces: its render
// crosses `ContentLabelEntry`, and the Go mail-bridge has no render surface.
#[cfg(feature = "value-format")]
mod region;
#[cfg(feature = "value-format")]
pub use region::*;

// Gated default-on; the Go mail-bridge `--no-default-features` build drops it
// (its fauna_core LocalizedText/RelativeTimeDisplay returns make uniffi-bindgen-go
// emit an uncompilable bare `fauna_core` import). See Cargo.toml `value-format`.
#[cfg(feature = "value-format")]
mod value_format;
#[cfg(feature = "value-format")]
pub use value_format::*;

// Shared search-result card formatters (`fauna_client_search::render::*` — the
// prefix-aware badge map + FTS snippet cleanup). Gated default-on for the SAME
// reason as `value-format`: `search_content_type_badge` returns a `fauna_core`
// LocalizedText, so the Go mail-bridge `--no-default-features` build drops it
// (else uniffi-bindgen-go emits an uncompilable bare `fauna_core` import). The
// Go bridge has no search surface, so dropping it is harmless. See Cargo.toml
// `search`.
#[cfg(feature = "search")]
mod search;
#[cfg(feature = "search")]
pub use search::*;

// Shared content-label badge presentation
// (`fauna_core::content_category::content_label_style`). Gated default-on for the
// SAME reason as `search` / `render`: it returns a `fauna_core` `ContentLabelStyle`
// embedding a `LocalizedText`, so the Go mail-bridge `--no-default-features` build
// drops it (else uniffi-bindgen-go emits an uncompilable bare `fauna_core` import).
// The Go bridge has no moderation surface. Lets native apps drop hard-coded
// category→label/icon/colour maps (android `ContentLabelBadge.kt`). See
// Cargo.toml `moderation-badge` + moderation.md § Where logic lives.
#[cfg(feature = "moderation-badge")]
mod content_category;
#[cfg(feature = "moderation-badge")]
pub use content_category::*;

// Shared MIME-type detection (src/mime.rs — content_type_for_filename over the
// fauna_core::share catalog). Default-on for the Apple/Android/Windows app FFI;
// gated out of the Go mail-bridge `--no-default-features` build to keep this free
// fn over built-in types off the checked-in Go bindings and avoid an off-win Go
// regen (same rationale as `nest-trust`: Go COULD emit it, but the bridge has no
// client-filename MIME surface — memory reference_ffi_gate_conversations_session_excludes_go).
// Pure cfg gate — fauna-core is non-optional.
#[cfg(feature = "mime")]
mod mime;
#[cfg(feature = "mime")]
pub use mime::*;

// Shared render-model plaintext flattener (`fauna_core::render::RenderDocument::
// to_plaintext`). Gated default-on for the SAME reason as `value-format`/`search`:
// the export takes a `fauna_core` RenderDocument, so the Go mail-bridge
// `--no-default-features` build drops it (else uniffi-bindgen-go emits an
// uncompilable bare `fauna_core` import). Clients read the painted body text
// through it instead of the raw `body` source (render-model.md P1 read-uniformity
// residual). See Cargo.toml `render`.
#[cfg(feature = "render")]
mod render;
#[cfg(feature = "render")]
pub use render::*;

// Shared log-ring exposure (`fauna_log` install + snapshot/clear reads) for the
// Settings → Logs page. Gated default-on, same Go-bridge reason as the others:
// its `LogEntry` / `LogLevel` returns are `fauna_log` types and uniffi-bindgen-go
// emits an uncompilable bare `fauna_log` cross-namespace import for them; the Go
// bridge is a server with no Logs surface. See Cargo.toml `logs` +
// `docs/goal/architecture/apps/observability.md`.
#[cfg(feature = "logs")]
mod logs;
#[cfg(feature = "logs")]
pub use logs::*;

mod bridges;
pub use bridges::*;

mod email_client;
pub use email_client::*;

mod subscriptions_client;
pub use subscriptions_client::*;

// The money plane — compiled away in an App-Store escape-hatch flavor
// (`dynamic-features.md` § Compile-time excision). Gating the module is what
// drops the `fauna.payments.*` / `fauna.tips.*` kind strings AND the
// `FfiPaymentsClient` UniFFI exports from the artifact, so the generated
// Swift/Kotlin/C# face is a pure function of the feature set — the same rule
// (c) convention 15's seams follow.
#[cfg(feature = "payments")]
mod payments_client;
#[cfg(feature = "payments")]
pub use payments_client::*;

#[cfg(feature = "subscriptions-author")]
mod subscriptions_author;
#[cfg(feature = "subscriptions-author")]
pub use subscriptions_author::*;

mod contacts_client;
pub use contacts_client::*;

mod nostr_client;
pub use nostr_client::*;

// The succession-aftermath npub-confirm banner FFI (nostr.md § Key succession
// and rotation, leg 3) — android/windows/macos/ios all
// reach fauna_client_config::npub_confirmation_owed_for and the account
// plane's `fauna.state.nostr-confirmation` stamp through this one module. Gated default-on so the Go mail-bridge
// `--no-default-features` build drops it — no Nostr settings UI on the
// bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "nostr-npub-confirm")]
mod nostr_npub_confirm;
#[cfg(feature = "nostr-npub-confirm")]
pub use nostr_npub_confirm::*;

// The per-account spam-threshold override FFI (mail-policy-config.md § Tier 3
// — the `mail-spam` page's threshold input). Gated
// default-on so the Go mail-bridge `--no-default-features` build drops it —
// no settings UI on the bridge; keeps the Go bindings byte-identical.
#[cfg(feature = "spam-threshold-override")]
mod spam_threshold_override;
#[cfg(feature = "spam-threshold-override")]
pub use spam_threshold_override::*;

mod notifications_client;
pub use notifications_client::*;

mod inbox_client;
pub use inbox_client::*;

// Bluesky-native thread-view RPC seam (`bluesky.feed.thread`), wrapped as
// FfiBlueskyClient — the crossposted-post thread the post-detail surface shows,
// off the deleted GET /api/v1/bluesky/{feed/thread,thread} HTTP twins. A pure
// RPC client over a flat display projection of the proto thread types.
mod bluesky_client;
pub use bluesky_client::*;

// The client-facing `fauna.moderation.*` seam (FfiModerationClient) — queue
// read + training correction + report-/signal-sharing opt-ins. (`scan_report`
// removed 2026-07-19, the kind itself off the wire 2026-09-24; moderation.md.)
// Ungated, built-in types only (clean in the Go-bridge build).
mod moderation_client;
// User-initiated reporting's faces — `moderation-badge` for the takedown
// console's reason (bare `LocalizedText` in the views).
#[cfg(feature = "moderation-badge")]
mod abuse_report;
pub use moderation_client::*;

// The encrypted-CalDAV Events seam (FfiCaldavClient) — the Events page reads/writes
// the encrypted `bridge_caldav_*` store via `fauna.bridges.*` + client-side
// seal/unseal (events.md Decision B), wrapping `fauna_client_caldav::CalDavClient`.
// Ungated, built-in-typed records only (clean in the Go-bridge build, same as the
// moderation / snapshots seams).
mod caldav_client;
pub use caldav_client::*;

// The encrypted-CardDAV Address Book seam (FfiCarddavClient) — the Contacts page's
// "Address Book" segment reads the encrypted `bridge_carddav_*` store via
// `fauna.bridges.*` + client-side unseal, wrapping `fauna_client_carddav::CardDavClient`.
// Read-only (slice 4b); shares the msek gate with the caldav seam. Built-in-typed
// records only, so it stays clean in the Go-bridge build like the caldav seam.
mod carddav_client;
pub use carddav_client::*;

mod feed_client;
pub use feed_client::*;

mod posts_client;
pub use posts_client::*;

// The profile edit-form seam, split the way post/posts_client already is: the
// PURE half (the record mirrors, decode_profile_display, and the
// build_edited_profile{,_with_images} read-modify-write sign step) compiles
// unconditionally, because composing and decoding a `Profile` needs no nest
// connection and the Go atproto bridge's projection tests build real profile
// bytes with it. Only the RPC face — FfiProfileClient over
// `fauna.profile.{get,set}` — is gated behind the default-on `profile-client`
// feature, so the Go mail-bridge `--no-default-features` build drops the part it
// genuinely has no use for (a server with no profile-edit UI; same dead-code
// rationale as `markdown-authoring`). Only built-in types cross either
// boundary. See src/profile.rs + src/profile_client.rs + docs/goal/ui/profile.md.
mod profile;
pub use profile::*;
#[cfg(feature = "profile-client")]
mod profile_client;
#[cfg(feature = "profile-client")]
pub use profile_client::*;

mod search_client;
pub use search_client::*;

// The push-subscription-management seam (`fauna.push.{vapid_key,subscribe,
// unsubscribe}`), wrapped over UniFFI as FfiPushClient — the native-client
// twin of `fauna_client_push::PushClient`, off the deleted
// `/api/v1/push/{vapid-key,subscribe}` HTTP twins.
mod push_client;
pub use push_client::*;

// The MLS key-package pool seam (`fauna.conversations.keypackage.{upload,count}`),
// wrapped over UniFFI as FfiConversationsClient — the Encryption-settings page's
// publish + count, off the deleted `POST|GET /api/v1/keypackage/{actor}` HTTP
// twins. The conversation-page send/receive plane stays on the
// `ConversationsManager` binding (`conversations` above); this is the pool only.
mod conversations_client;
pub use conversations_client::*;

// The `__drafts` reserved-folder persistence seam (`fauna.drafts.{get,put}`),
// wrapped over UniFFI as FfiDraftsClient — the draft-persistence v2 client legs'
// restore-on-launch + debounced-save-after-compose-change trigger, off the shared
// fauna_client_drafts::DraftsClient (seal + WS, never re-done per client;
// reserved-folders.md § Drafts Sync). Gated behind the default-on `drafts` feature so the
// Go mail-bridge `--no-default-features` build drops it (no compose UI on the
// bridge → no off-win Go regen).
#[cfg(feature = "drafts")]
mod drafts;
#[cfg(feature = "drafts")]
pub use drafts::*;

// The events-rail typed face (FfiEventDraftsSync) — the android/apple/windows
// twin of fauna-wasm's WasmEventDrafts, carrying the record itself across the
// boundary rather than raw bytes (the events page has no manager to hold the
// encoding, unlike the conversations/feed rails above). Same feature gate as
// `drafts` — it wraps the same DraftsSync.
#[cfg(feature = "drafts")]
mod event_drafts;
#[cfg(feature = "drafts")]
pub use event_drafts::*;

// The source-side IMAP import client (FfiMailImportClient) — mailbox migration's
// client half (mailbox-migration.md § Client-driven streaming model), wrapping the
// shared `fauna_mail::imap_client` protocol core over its native tokio+rustls
// transport. UniFFI cannot export the generic `ImapSession<T>`, so this is the
// concrete native-bound facade; it also derives the `dedup_key` + `sender_domain`
// wire fields so no client shell reimplements them. Gated behind the default-on
// `mail-import` feature so the Go mail-bridge `--no-default-features` build drops
// it (the bridge imports from no foreign mailbox → no Go regen).
#[cfg(feature = "mail-import")]
mod mail_import;
#[cfg(feature = "mail-import")]
pub use mail_import::*;

// Raw `fauna.folders.*` typed-call client (FfiFoldersClient) — the
// Devices/Backups folder control plane. Unconditional (wasm-clean,
// Go-bridge-emittable), unlike the `folders`-feature wizard/devices machines.
mod folders_client;
pub use folders_client::*;

mod snapshots_client;
pub use snapshots_client::*;

mod stats_client;
pub use stats_client::*;

mod sync_client;
pub use sync_client::*;

// The construct-run-drop worker-thread helper (src/worker_thread.rs) shared by
// `sync_engine_host` and `file_provider_host` (the latter implies this feature
// — Cargo.toml's `file-provider-host = ["sync-engine-host"]`). Gated the same
// way as its one base consumer below.
#[cfg(feature = "sync-engine-host")]
mod worker_thread;

// The in-process file-sync engine host (src/sync_engine_host.rs) — the apple
// apps' byte-sync deployment (`file-sync.md` § Apple apps — convergence
// design): N shared `SyncEngine`s multiplexed over the shared `EngineHost`, plus
// the iOS one-shot pass, the photo-library sealed ingest, and the per-file
// display-state read. Gated on `sync-engine-host` (default-on for the native
// apps, dropped by the Go mail-bridge's `--no-default-features` build, which
// has no file-sync surface and cannot emit the `fauna-client*` namespaces).
#[cfg(feature = "sync-engine-host")]
mod sync_engine_host;
#[cfg(feature = "sync-engine-host")]
pub use sync_engine_host::*;

// The control-inverted on-demand host (src/file_provider_host.rs) — the
// hydration host apple's `NSFileProviderReplicatedExtension` and android's SAF
// `DocumentsProvider` call, vending the shared `fauna_sync_engine::provider_face`
// callback→engine primitives (`on-demand-files.md` § Apple File Provider binding,
// § Android SAF DocumentsProvider binding). Reuses the byte-sync host's
// `HostContext` (requires `sync-engine-host`), but is its own default-OFF
// feature so windows, which has no use for it, doesn't compile it. The apple and
// android recipes enable it explicitly (Cargo.toml's `file-provider-host`
// comment has the full rationale + the recipe list).
#[cfg(feature = "file-provider-host")]
mod file_provider_host;
#[cfg(feature = "file-provider-host")]
pub use file_provider_host::*;

// The desktop sync-agent provisioning loop (src/sync_agent_provisioning.rs) — the
// FaunaKit (macOS) **and windows C#** driver that runs the shared
// `fauna_ipc::convergence` loop over this user's agent endpoint to keep the
// external `fauna-sync-agent` provisioned so sync + backup survive the app
// closing (`sync-agent.md` § Control plane split + § Credential model,
// milestone A4). `#[cfg(any(unix, windows))]` because the seam resolves through
// `fauna_ipc::endpoint::AgentEndpoint` — the per-user unix socket on macOS/linux,
// the per-SID named pipe on windows (the C# codec this replaced retired
// 2026-07-24, `sync-agent.md` § Consumers); on apple it is present on every slice
// for flat-binding consistency but only macOS drives it. Default-on
// `sync-agent-provisioning` (pulls `fauna-ipc`); the Go mail-bridge
// `--no-default-features` build drops it.
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
mod sync_agent_provisioning;
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
pub use sync_agent_provisioning::*;

// Single-file restore for the native apps (src/snapshot_download.rs) — the
// FFI twin of the wasm `downloadSnapshotFileBytes`, over the shared
// `fauna_core::file_download` walk (`backups.md` § Where logic lives →
// *Single-file byte download*). Shares `sync-engine-host`'s gate because it binds
// the same `engine-lifecycle` restore-engine builder — but it is a free fn, not a
// host method, so windows/android (which build no engine host) reach it too.
#[cfg(feature = "sync-engine-host")]
mod snapshot_download;
#[cfg(feature = "sync-engine-host")]
pub use snapshot_download::*;

// Admin mail machines (DNS / local-domains / bridge-approval / forwarders / policy):
// re-exports + free-fn constructors over the FfiNestClient connection. Wraps
// libs/fauna-client-{dns,mail-settings}; linux builds the same machines
// natively (tracked internally). Gated behind the default-on
// `mail-admin` feature so the Go mail-bridge's `--no-default-features` FFI build
// drops these client-only machines (their fauna-client-{dns,mail-settings}
// namespaces make uniffi-bindgen-go emit uncompilable bare imports).
#[cfg(feature = "mail-admin")]
mod mail_admin;
#[cfg(feature = "mail-admin")]
pub use mail_admin::*;

// User-settings Linked-nests machine: re-exports + free-fn constructor over the
// FfiNestClient bearer connection (fauna.pair.* — User-gated). Wraps
// libs/fauna-client-pair; linux builds the same machine natively. Gated behind
// the default-on `pairing` feature so the Go mail-bridge's
// `--no-default-features` FFI build drops it (same cross-namespace-import reason
// as `mail-admin`; tracked internally).
#[cfg(feature = "pairing")]
mod pairing;
#[cfg(feature = "pairing")]
pub use pairing::*;

// Web-content authoring client (fauna.web.* — User-class subdomain toggle +
// Admin-class apex picker): the FfiWebClient + re-exported view types over the
// FfiNestClient bearer connection. Wraps libs/fauna-client-web; linux builds the
// same WebClient natively + web drives it over the wasm twin. Gated behind the
// default-on `web-content` feature so the Go mail-bridge's `--no-default-features`
// FFI build drops it (same cross-namespace-import reason as `mail-admin` /
// `pairing`). See docs/goal/behavior/web-content-hosting.md § Admin apex
// hosting / § Published-post management.
#[cfg(feature = "web-content")]
mod web_content;
#[cfg(feature = "web-content")]
pub use web_content::*;

// Minimal C-ABI surface used by tests/e2e-unified/fauna_ffi.py — test surface,
// compiled out of every shipped artifact (e2e convention 15): the module
// exists only under the `e2e-harness` feature, which `just e2e-ffi` and the
// windows harness flavor (`test-helpers` forwards it) turn on. All production
// clients use UniFFI bindings instead.
#[cfg(feature = "e2e-harness")]
pub mod cabi;

// ── Error type ──

/// Unified error type for all FFI exports.
/// UniFFI 0.31 supports `Result<T, String>` natively, but a proper error enum
/// gives better codegen on both Swift and Kotlin and is forward-compatible.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("{msg}")]
    General { msg: String },
    /// The nest is running an **outdated version** and has booted into a degraded
    /// "needs-update" mode (version-compatibility.md § 2.2 / Dim 4) — it answers
    /// `fauna.nest.outdated` to the connect/auth handshake. Distinct from
    /// [`Self::General`] so native apps route this to a **non-retry** "update
    /// your nest" surface instead of the retryable-error bucket (the macOS launch
    /// gate's `catch { .retrying }` would otherwise spin a degraded nest forever).
    /// `msg` is the already-localized actionable banner. The classification is
    /// shared (`fauna_protocol::RpcError::action()` → `NeedsUpdate`); this variant
    /// just carries it across the UniFFI boundary.
    #[error("{msg}")]
    NestOutdated { msg: String },
    /// The nest's **pinned deployment identity** changed — or a pinned nest
    /// could no longer prove any identity (`seen_hex` empty, the
    /// withdrawn/downgrade case). Raised by the connect-time channel-binding
    /// graduation on the silent-challenge / handshake paths (security.md
    /// § Transport trust — the SSH `known_hosts` model). Distinct
    /// from [`Self::General`] so native apps BLOCK on the
    /// `launch_identity_changed` warning surface (explicit re-trust or
    /// use-a-different-nest) instead of spinning the retry loop on a MITM
    /// signal. Fingerprints are hex `nest_actor_id`s for the warning's detail
    /// line.
    #[error("nest identity changed for {host}")]
    NestIdentityChanged {
        host: String,
        pinned_hex: String,
        seen_hex: Option<String>,
    },
    /// **This identity was succeeded** — a `fauna.auth.superseded` refusal. The
    /// account is alive and well; it simply belongs to `new_actor_id_hex` now,
    /// and the secret this device is signing with retired with the old identity
    /// (`identity-succession.md` § The succession statement).
    ///
    /// Distinct from [`Self::General`] for the same reason
    /// [`Self::NestIdentityChanged`] is: the app owes a *specific* affordance —
    /// import the successor identity — and a retry loop over the generic bucket
    /// can never succeed, because nothing about waiting brings the old key back.
    /// Carrying the successor's id in the variant rather than only inside a
    /// message string is what lets that affordance be one tap instead of asking
    /// the user to re-type 64 hex characters off an error banner.
    ///
    /// ⚠ Added 2026-08-21 with apple's ceremony leg. `auth.rs` had put this
    /// refusal in the generic bucket **deliberately**, with a comment naming
    /// exactly this landing as the trigger to split it out: none of the three
    /// FFI apps rendered the import affordance, so a typed variant would have
    /// been a variant nothing branched on.
    #[error("this identity was succeeded — import the new identity {new_actor_id_hex}")]
    IdentitySuperseded { new_actor_id_hex: String },
    /// **A guardian must approve this action** — the nest's typed refusal
    /// `fauna.{ns}.guardian_approval_required` (`family-safety.md` § Guardian
    /// policy pillar 1: a supervised ward's send to a non-contact, a blocked
    /// feed source, …). The shared classification is
    /// `fauna_protocol::RpcError::is_guardian_approval_required`; this variant
    /// only carries its verdict across the UniFFI boundary.
    ///
    /// Distinct from [`Self::General`] because the app owes a *specific*
    /// affordance — replace the dead error banner with "ask your guardian"
    /// (`contact-request-guardian-button` / `bridge-source-request-button`) —
    /// and the display text cannot tell it apart: `msg` is the localized
    /// sentence, which carries no wire code, so an app matching on it would be
    /// re-deriving shared logic by string. Offering the ask on any *other*
    /// failure tells an unsupervised user their account is supervised, so the
    /// ask hangs off this variant alone.
    ///
    /// `msg` is the already-localized refusal, shown on the page's
    /// `error-message` exactly as a `General` one would be (the refusal is
    /// still a real failure, just not a dead end).
    #[error("{msg}")]
    GuardianApprovalRequired { msg: String },
    /// **The account is locked out** — a `fauna.auth.account_locked` refusal,
    /// standing until `locked_until_secs` (Unix seconds; `devices.md` § The
    /// locked state).
    ///
    /// Distinct from [`Self::General`] for the reason
    /// [`Self::IdentitySuperseded`] is: a retry loop over the generic bucket
    /// re-signs a ceremony the nest must refuse until that time, and the app
    /// owes a specific surface — the standing notice naming the unlock time.
    /// Carrying the time as a field is what lets that surface format it
    /// (through the shared `format_unix_local`) instead of parsing a message.
    /// The `#[error]` string is verbatim what the generic arm formatted, so an
    /// app still rendering the error's description shows what it showed before.
    #[error("account locked until {locked_until_secs} (Unix seconds)")]
    AccountLocked { locked_until_secs: u64 },
}

impl From<String> for FfiError {
    fn from(msg: String) -> Self {
        FfiError::General { msg }
    }
}

/// A foreign implementation of a `with_foreign` trait (Go, Swift, Kotlin, C#)
/// returned an error UniFFI could not lift into [`FfiError`] — a Go callback
/// returning a plain `error`, say, which the generated Go wrapper passes back as
/// an unexpected result with no payload. With no conversion for that case the
/// call panicked (`UnexpectedUniFFICallbackError(reason: "")`), and the mail
/// bridge's IMAP server turned the panic into a dropped session. Mapped here, it
/// is an ordinary `Err` the caller already handles: a failing content-index rail
/// becomes the retryable index error it was always meant to be. Every foreign
/// trait in this crate returns `FfiError`, so this one impl covers them all.
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        FfiError::General {
            msg: format!("a foreign callback failed: {}", e.reason),
        }
    }
}

// ── Shared helpers ──

/// Maps any displayable error onto [`FfiError::General`] via `to_string()` —
/// the pattern every call site in this crate independently reinvented before
/// this helper existed.
pub(crate) fn general_err(e: impl std::fmt::Display) -> FfiError {
    FfiError::General { msg: e.to_string() }
}

/// The succession-ledger seam (`fauna.state.succession-ledger` — the grant-event
/// log and its marks) every grant-minting machine this crate builds records
/// through: this process's account-store handle, resolved per call and waited
/// for while its assembly is in flight
/// (`fauna_client_config::ResolvingLedgerStore`). A build without the
/// `account-runtime` feature has no store, so its machines refuse every ledger
/// write rather than record nowhere. The exported builders keep their
/// signatures: no foreign caller passes identity or a store.
#[cfg_attr(
    not(any(
        feature = "pairing",
        feature = "mail-admin",
        feature = "labeler-catalog",
        feature = "recovery-aftermath"
    )),
    allow(dead_code)
)]
pub(crate) fn ledger_seam() -> std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore> {
    #[cfg(feature = "account-runtime")]
    {
        std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(
            crate::account_runtime::handle,
        ))
    }
    #[cfg(not(feature = "account-runtime"))]
    {
        std::sync::Arc::new(fauna_client_config::NoLedgerStore)
    }
}

/// The backup-destination seam (`fauna.state.backup` — the per-source-box
/// destination list and its unattested marks) every backup surface this crate
/// exports reads and writes through: the same account-store handle
/// [`ledger_seam`] resolves, per call, waited for while its assembly is in
/// flight (`fauna_client_config::ResolvingLedgerStore`). A build without the
/// `account-runtime` feature has no store, so it reads the empty state and
/// refuses every write — never a fall back to any other store.
#[cfg_attr(
    not(any(
        feature = "pairing",
        feature = "mail-admin",
        feature = "backup-destinations",
        feature = "recovery-aftermath"
    )),
    allow(dead_code)
)]
pub(crate) fn backup_seam() -> std::sync::Arc<dyn fauna_client_config::BackupStateStore> {
    #[cfg(feature = "account-runtime")]
    {
        std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(
            crate::account_runtime::handle,
        ))
    }
    #[cfg(not(feature = "account-runtime"))]
    {
        std::sync::Arc::new(fauna_client_config::NoLedgerStore)
    }
}

pub(crate) fn keypair_from_bytes(secret: &[u8]) -> Result<ActorKeypair, FfiError> {
    let arr: [u8; 32] = secret.try_into().map_err(|_| FfiError::General {
        msg: "secret must be 32 bytes".into(),
    })?;
    Ok(ActorKeypair::from_secret(arr))
}

/// The followed-folders seam (`fauna.state.follows`, one row per followed
/// folder) every follow face and the followed-folders source read and write
/// through: this process's account-store handle, resolved per call and waited
/// for while its assembly is in flight — the ledger seam's shape
/// ([`ledger_seam`]). A build without the `account-runtime` feature has no
/// store, so a follow is refused rather than recorded nowhere.
#[cfg(any(feature = "folders", feature = "folders-author"))]
pub(crate) fn follows_seam() -> std::sync::Arc<dyn fauna_client_config::FollowsStore> {
    #[cfg(feature = "account-runtime")]
    {
        std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(
            crate::account_runtime::handle,
        ))
    }
    #[cfg(not(feature = "account-runtime"))]
    {
        std::sync::Arc::new(fauna_client_config::NoLedgerStore)
    }
}

/// Build the followed-public-folders source shared by the Devices page
/// (`wire_devices_followed_folders`) and the Media page
/// (`wire_media_followed_folders`) — each hands the resulting source to its
/// own page-specific setter, so the availability-verdict cache is one
/// mechanism per page rather than two racing ones. The follows come from
/// [`follows_seam`]; `owner_secret` no longer derives anything, but a
/// malformed one is still refused, as at every follow export.
#[cfg(any(feature = "folders", feature = "folders-author"))]
pub(crate) fn build_followed_folders_source(
    nest: &FfiNestClient,
    owner_secret: Vec<u8>,
) -> Result<std::sync::Arc<fauna_devices_machine::StoreFollowedFoldersSource>, FfiError> {
    keypair_from_bytes(&owner_secret)?;
    Ok(std::sync::Arc::new(
        fauna_devices_machine::StoreFollowedFoldersSource::new(nest.nest_arc(), follows_seam()),
    ))
}

pub(crate) fn bytes_to_actor_id(bytes: &[u8]) -> Result<ActorId, FfiError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| FfiError::General {
        msg: "actor ID must be 32 bytes".into(),
    })?;
    Ok(ActorId(arr))
}

pub(crate) fn bytes_to_content_hash(bytes: &[u8]) -> Result<ContentHash, FfiError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| FfiError::General {
        msg: "hash must be 32 bytes".into(),
    })?;
    Ok(ContentHash::from_digest_raw(arr))
}

/// Decode a 36-byte CID from raw bytes (the on-wire / FFI shape of a
/// `PostId` after the Task 2.8 collapse). Returns the full 36-byte array
/// suitable for passing into `build_post(..., reply_to: Option<[u8; 36]>)`.
pub(crate) fn bytes_to_post_id(bytes: &[u8]) -> Result<[u8; 36], FfiError> {
    let arr: [u8; 36] = bytes.try_into().map_err(|_| FfiError::General {
        msg: "post ID must be 36 bytes (Cid: v1+dag-cbor+blake3-256+32)".into(),
    })?;
    Ok(arr)
}

/// Shared `NestClientError` → [`FfiError`] conversion the `…_client` modules
/// feed their call results through. Was 21 byte-identical local
/// `fn stringify` copies (priority #2/#4 dedup).
///
/// Almost everything becomes `FfiError::General { msg }`, as it always did.
/// The **two** exceptions are typed verdicts an app must branch on. The
/// nest-identity verdict gets its own variant so the native apps block on the
/// `launch_identity_changed` surface instead of showing "posting failed" over
/// a session that can no longer authenticate its nest (`security.md` §
/// Post-auth surfacing). The guardian-approval refusal
/// (`RpcError::is_guardian_approval_required`) gets
/// [`FfiError::GuardianApprovalRequired`] so the ward-side ask is offered on
/// that refusal and no other. Routing them here rather than at each call site
/// is what gives every one of this crate's authenticated calls the behaviour
/// for free — the alternative was ~200 sites each deciding, i.e. ~200 chances
/// to forget.
///
/// (Kept named `stringify`, and still usable as `.map_err(stringify)`, so the
/// call sites are untouched; a `.map_err(Into::into)` chained after it stays
/// valid too, via the blanket `From<T> for T`.)
pub(crate) fn stringify(e: fauna_client::NestClientError) -> FfiError {
    match e {
        fauna_client::NestClientError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        } => FfiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        },
        fauna_client::NestClientError::Rpc(ref err) if err.is_guardian_approval_required() => {
            FfiError::GuardianApprovalRequired { msg: e.to_string() }
        }
        other => FfiError::General {
            msg: other.to_string(),
        },
    }
}

#[cfg(test)]
mod stringify_tests {
    use super::*;

    /// A foreign callback's undeclared error must reach Rust as an ordinary
    /// `FfiError`, carrying its reason — not as the panic that dropped the mail
    /// bridge's IMAP sessions.
    #[test]
    fn an_unexpected_foreign_callback_error_is_a_general_error_not_a_panic() {
        let e: FfiError = uniffi::UnexpectedUniFFICallbackError::new("index rail refused").into();
        let FfiError::General { msg } = e else {
            panic!("expected the general bucket, got {e:?}");
        };
        assert!(
            msg.contains("index rail refused"),
            "the foreign reason must survive the conversion: {msg}"
        );
    }

    /// The whole point of routing through one helper: an authenticated call
    /// that fails because the nest's identity changed must reach the FFI apps
    /// as the verdict, not as `General { msg }` — the generic bucket is what
    /// makes a MITM signal render as "posting failed" and invites a retry.
    #[test]
    fn the_identity_verdict_keeps_its_own_variant() {
        let e = stringify(fauna_client::NestClientError::NestIdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
        });
        let FfiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        } = e
        else {
            panic!("expected the identity verdict, got {e:?}");
        };
        assert_eq!(host, "nest.example");
        assert_eq!(pinned_hex, "aa".repeat(32));
        assert_eq!(seen_hex.as_deref(), Some("bb".repeat(32).as_str()));
    }

    /// The ward-side ask hinges on this: the display text is the localized
    /// sentence, which no longer carries the wire code, so a Swift/Kotlin app
    /// can only tell "a guardian must approve this" from any other refusal if
    /// the shared predicate's verdict crosses the boundary as its own variant.
    /// One route per gated namespace, so a namespace added nest-side needs no
    /// change here — the predicate matches the suffix, this only carries it.
    #[test]
    fn a_guardian_approval_refusal_keeps_its_own_variant() {
        for ns in ["inbox", "knocks", "bridges", "conversations", "account"] {
            let err = fauna_protocol::RpcError::new(
                format!("fauna.{ns}.guardian_approval_required"),
                "error.x",
            );
            let e = stringify(fauna_client::NestClientError::Rpc(err));
            assert!(
                matches!(e, FfiError::GuardianApprovalRequired { .. }),
                "{ns}: expected the guardian verdict, got {e:?}"
            );
        }
    }

    /// The other half of rule (a): the ask is offered ONLY on the typed
    /// refusal. A near-miss code, or any other rejection, must stay in the
    /// generic bucket — painting the ask there tells an unsupervised user their
    /// account is supervised.
    #[test]
    fn a_near_miss_refusal_stays_general() {
        for code in [
            "fauna.knocks.guardian_approval_required_notice",
            "fauna.knocks.denied",
            "guardian_approval_required",
            "fauna..guardian_approval_required",
        ] {
            let err = fauna_protocol::RpcError::new(code, "error.x");
            let e = stringify(fauna_client::NestClientError::Rpc(err));
            assert!(matches!(e, FfiError::General { .. }), "{code}: got {e:?}");
        }
    }

    /// …and everything else keeps the behaviour it always had, message and all.
    /// Widening the special case would be worse than not having it: an ordinary
    /// timeout routed to the blocking surface strands a healthy session.
    #[test]
    fn every_other_failure_stays_general() {
        let e = stringify(fauna_client::NestClientError::RpcTimeout);
        assert!(matches!(e, FfiError::General { .. }), "got {e:?}");
        let e = stringify(fauna_client::NestClientError::WebSocket("boom".into()));
        let FfiError::General { msg } = e else {
            panic!("expected General, got {e:?}");
        };
        assert!(msg.contains("boom"), "the cause must survive: {msg}");
    }
}
