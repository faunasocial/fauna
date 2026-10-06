//! Transport-free + transport client for the `__drafts` reserved folder.
//!
//! The shared client half of draft-persistence v2 (`docs/goal/behavior/file-sync.md`
//! § Drafts Sync). It pairs with `libs/fauna-conversations`'s seal-agnostic
//! `DraftStore` (which produces/consumes canonical snapshot bytes) to keep the 6
//! client legs pure-glue. A leg constructs a [`DraftsSync`] (the stateful wrapper
//! that owns the load gate + last-saved baseline so the safety properties are
//! written once, not 6×) over its WS-RPC transport, then
//!
//! * on launch: `let e = manager.identity_epoch(); if let Some(b) = drafts.load().await? { manager.restore_drafts_at(e, b).await }`
//!   — the conversations manager's restore is async because it owes a restored
//!   recipient picker its rail probe (`docs/goal/ui/conversations.md`
//!   § Persistence), and takes the epoch read BEFORE the load so a reply the
//!   outgoing account's fetch delivers after an identity change fills nothing
//!   (`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy);
//!   the feed manager's is still a plain call.
//! * after a compose change (debounced): `drafts.save_if_changed(&manager.drafts_snapshot_bytes()).await?`
//!
//! [`DraftsSync`] wraps the lower-level [`DraftsClient`] (the raw, stateless
//! `fauna.drafts.{get,put}` call surface) — a leg that needs the unguarded calls
//! can use `DraftsClient` directly, but the load gate then becomes its own
//! responsibility.
//!
//! Two parts: a [`seal`] module implementing the exact at-rest pipeline the
//! nest stores (zstd → ChaCha20-Poly1305 under the owner's `BackupKey`), and a [`store`] module with the typed
//! `fauna.drafts.{get,put}` call surface generic over the
//! `fauna_protocol::RpcRequester` seam. Drafts are owner-only (no signing).
//! There is **no HTTP** here.

mod seal;
pub mod store;
pub mod succession_aftermath;
pub mod sync;

pub use seal::{
    DraftSealError, backup_key_from_seed, rekey_drafts_blob, seal_drafts, unseal_drafts,
};
pub use store::{DraftsClient, DraftsClientError, DraftsRekeyOutcome};
pub use succession_aftermath::{
    DraftsResealOutcome, DraftsResealProgress, rekey_drafts_after_succession,
};
pub use sync::{AUTOSAVE_DEBOUNCE, DraftsSync, autosave_debounce};

// Re-export the at-rest key type so consumers depend on this crate's surface
// rather than reaching into `fauna_core::crypto` directly.
pub use fauna_core::crypto::BackupKey;
