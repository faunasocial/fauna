//! The conversations seams that rest on the **account plane** — one
//! implementation for every app that hosts the account runtime (priority #2),
//! in a home every host can compile.
//!
//! `fauna-conversations` declares four seams and must never learn about the
//! account runtime: the community class's group-reception keys
//! (`backend::GroupReceptionKeys`), the fauna-native rail's read positions
//! (`backend::ReadPositions`), the private contact-overlay folds
//! (`backend::ContactOverlayFolds`) and the succession witness's peer anchors
//! (`backend::PeerAnchorStore`). `fauna-account-plane` serves each through
//! the driver's `AccountStoreHandle` and must never learn about conversations.
//! This crate is the join — [`group_reception`], [`read_positions`],
//! [`contact_overlays`], [`peer_anchors`], registered together by
//! [`conversation_seams::wire`]
//! at the account-store-ready edge — and it compiles for wasm32 because web
//! hosts the same runtime over its own store backend and registers the same
//! seams (`docs/goal/architecture/account-client-lifecycle.md` § The
//! client-side lifecycle → *The trigger fired*, ruling (4)). Beside them
//! sit the non-conversations seams with the same need — a home every
//! runtime-hosting app compiles, web included: [`blessed_nests`], the Nests
//! page's blessing door (`fauna_client_pair::BlessedNestsStore`),
//! [`fleet_removal`], the Devices page's fleet door
//! (`fauna_devices_machine::FleetRemoval`, here since 2026-09-30 — web's core
//! chunk serves it through the account port), [`period_keys`], the
//! subscription-tier period-key custody
//! (`fauna_client_subscriptions::PeriodKeyStore`),
//! [`folder_keys`], the shared-folder content-key custody
//! (`fauna_client_folders::FolderKeyStore`), [`atproto_identity`],
//! the ATProto identity custody door
//! (`fauna_client_atproto::identity_store::AtprotoIdentityStore`, whose
//! consumers — the ATProto settings machine and the alert sweep — run in the
//! core chunk and in the settings chunk alike), and [`atproto_credentials`],
//! the ATProto settings page's credential seam
//! (`fauna_atproto_settings_machine::AtprotoCredentialStore`, served to web's
//! ATProto chunk the same way). Until 2026-09-29
//! these modules lived in `fauna-client-account-runtime`, the native assembly
//! crate, which still re-exports them at their old paths.
//!
//! What a host brings is **how to spawn**: the read-position and overlay seams
//! each run a writer task and a watcher task, and the crate names no runtime
//! of its own — a tokio handle natively (a UniFFI host may complete the
//! (session, store) pair from a synchronous foreign call and hands in the
//! runtime its store runs on), the browser's `spawn_local` on web
//! ([`spawner::TaskSpawner`]). Both watchers wait on [`store_change`], the
//! one store-change watch every app's open store-backed surface consumes too
//! (`account-runtime.md` § Multi-instance concurrency → *A runtime's own
//! pump is a source of the notice too*). The watchers' one wait is
//! `fauna_sleep::sleep`; `tests/no_native_time.rs` pins textually that no
//! thread, timer or monotonic clock is ever named here.

pub mod atproto_credentials;
pub mod atproto_identity;
pub mod blessed_nests;
pub mod contact_overlays;
pub mod conversation_seams;
pub mod fleet_removal;
pub mod folder_keys;
pub mod group_reception;
pub mod peer_anchors;
pub mod period_keys;
pub mod read_positions;
pub mod refused_changes;
pub mod spawner;
pub mod store_change;

pub use spawner::{SeamTask, TaskSpawner};
