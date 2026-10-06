//! Native-client FFI for the **encrypted-mode author-side mint + upload
//! orchestration** — the high-level `create_tier` / `approve_subscriber` /
//! `remove_subscriber` calls that wrap the broadcast-`KeyBlob` crypto dance so
//! all 7 apps dispatch **one** call instead of re-implementing the mint per
//! client (priority #2; `docs/goal/behavior/monetization.md` § Pillar 1 —
//! *Where the logic lives*: "the approve/remove mint+upload orchestration MUST
//! live in shared Rust … exposed via UniFFI + WASM … never re-implement the
//! crypto dance per client"). The Rust-native Linux app calls
//! `fauna_client_subscriptions::orchestration::SubscriptionsAuthor` directly;
//! this seam gives Apple / Windows / Android the identical high-level surface
//! over UniFFI, and `fauna-wasm` gives the web SPA its twin.
//!
//! The thin per-kind reads + plaintext-mode writes live on the companion
//! [`crate::FfiSubscriptionsClient`] (`subscriptions_client.rs`); this module is
//! the **encrypted-mode-only** mint orchestration (the nest holds no period
//! key, so the author's client mints — `monetization.md` § Pillar 1). In
//! plaintext mode the UI drives the thin client with `encrypted_upload = None`
//! and never touches these fns.
//!
//! Per priority #2 there is **no** new logic here: each fn rebuilds the actor's
//! [`SubscriptionsAuthor`] (the thin client + the owner's period-key store for
//! period-key custody + the crash-staged-removal sentinel, both over the same
//! WS-RPC transport) from the connection's `owner_secret` and dispatches one
//! orchestration call. Mirrors the config-owning-orchestration FFI shape of
//! `src/backup_destinations.rs` (free async fns over the account store), not a
//! stateful Object — an instance per call is cheap and keeps the native glue to
//! a single call.
//!
//! Gated behind the default-on `subscriptions-author` feature, exactly like
//! `backup_destinations` (the true analog — config-owning orchestration for the
//! native app lift): unlike the companion *thin* `subscriptions_client.rs`
//! (which reaches `self.nest` from inside `FfiNestClient` and so stays ungated),
//! these free fns need the `pub(crate) FfiNestClient::nest_arc()` accessor +
//! account-runtime store, both of which are themselves gated out of the Go mail-bridge
//! `--no-default-features` build (the bridge is a server with no author
//! subscription-management UI). Default-on so the Apple/Android/Windows app
//! FFI exports it; the gate keeps the bridge build compiling (`nest_arc` would
//! otherwise be missing) and lean. The returns cross only built-in types, so the
//! gate is about the gated `nest_arc` dependency + dead code, not a Go-binding
//! incompatibility.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_subscriptions::orchestration::SubscriptionsAuthor;
use fauna_client_subscriptions::subscriptions::PendingRequest;
use fauna_client_subscriptions::{ReconcilePass, SubscriptionsClient, author_poll_secs};
use fauna_core::data::Timestamp;

use crate::nest_client::FfiNestClient;
use crate::{
    FfiApproveReply, FfiError, FfiPendingRequest, FfiSubscribeReply, bytes_to_actor_id,
    general_err, keypair_from_bytes,
};

/// Rebuild the actor's encrypted-mode author orchestration from the connection's
/// 32-byte `owner_secret`: the thin `fauna.subscriptions.*` client and the
/// owner's identity over this connection's WS-RPC transport, and the live
/// account runtime's period-key custody (the resume sentinel included). Cheap
/// to build per call.
fn author(
    nest: &Arc<FfiNestClient>,
    owner_secret: &[u8],
) -> Result<SubscriptionsAuthor<Arc<NestClient>>, FfiError> {
    let keypair = keypair_from_bytes(owner_secret)?;
    Ok(SubscriptionsAuthor::over(
        nest.nest_arc(),
        keypair,
        crate::account_runtime::period_key_store(),
    ))
}

/// Reconstruct a wire [`PendingRequest`] from the [`FfiPendingRequest`] the UI
/// received from `requests_list` — `approve_subscriber` keys the mint off its
/// `tier_name` + `subscriber_id`. Total field map (the reverse of
/// `From<PendingRequest>`), so it can't silently drift.
fn pending_from_ffi(p: FfiPendingRequest) -> Result<PendingRequest, FfiError> {
    Ok(PendingRequest {
        request_id: p.request_id,
        subscriber_id: bytes_to_actor_id(&p.subscriber_id)?,
        tier_name: p.tier_name,
        kind: p.kind,
        created_at: Timestamp(p.created_at),
        mlkem_encaps_key: p.mlkem_encaps_key.map(fauna_protocol::ByteBuf::from),
        payment_entitled: p.payment_entitled,
        extra: Default::default(),
    })
}

/// Create a subscription tier (encrypted mode): record a fresh period key in the
/// owner's period-key custody (`fauna.state.subscriptions`), persist it, then create the tier server-side.
/// Custody-first + persist-first crash-safety lives in the shared orchestration
/// ([`SubscriptionsAuthor::create_tier`]). Returns whether the server created a
/// new row (`false` = idempotent repeat).
///
/// `asking_price_sats` is the **machine-comparable** purchase threshold in the
/// author's own unit (`monetization.md` § The asking price), converted once by
/// the shared pair so no app writes the arithmetic. `None` leaves the tier
/// unbuyable by an *inferring* mechanism — the permanently-correct default,
/// not a gap: a zap on such a tier stays a tip.
// Exported FFI signature: the 9 parameters are the tier-create binding contract
// (each is a uniffi argument across the Swift/Kotlin/C# bindings); bundling them
// into a Record would churn every native app for a pure-lint reason.
#[allow(clippy::too_many_arguments)]
#[fauna_uniffi_async::export]
pub async fn subscriptions_create_tier(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    name: String,
    rank: u32,
    description: Option<String>,
    price_hint: Option<String>,
    payment_url: Option<String>,
    auto_approve: bool,
    asking_price_sats: Option<u64>,
) -> Result<bool, FfiError> {
    let asking_price = match asking_price_sats {
        Some(sats) => Some(
            fauna_protocol::subscriptions::TierAskingPrice::from_sats(sats).ok_or_else(|| {
                FfiError::from("asking price is too large to express in msats".to_string())
            })?,
        ),
        None => None,
    };
    author(&nest, &owner_secret)?
        .create_tier(
            &name,
            rank,
            description,
            price_hint,
            payment_url,
            auto_approve,
            // Ordinary tier-management form — never a per-post pay-to-unlock
            // tier: that designation is set only by the "sell this post"
            // orchestration (monetization.md gap 2).
            None,
            asking_price,
            // The tier-management form mints OFFERED tiers; the reserved hidden
            // tier is provisioned by the archive-import machine, not by hand.
            false,
        )
        .await
        .map_err(general_err)
}

/// Approve a pending subscribe request: mint a `KeyBlob` over the post-approval
/// roster under the tier's current period key and upload it via
/// `requests.approve`, retrying `roster_mismatch` / `stale_rotation` internally
/// ([`SubscriptionsAuthor::approve_subscriber`]). `request` is the same
/// [`FfiPendingRequest`] the UI got from `requests_list`.
#[fauna_uniffi_async::export]
pub async fn subscriptions_approve_subscriber(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    request: FfiPendingRequest,
) -> Result<FfiApproveReply, FfiError> {
    let author = author(&nest, &owner_secret)?;
    let req = pending_from_ffi(request)?;
    author
        .approve_subscriber(&req)
        .await
        .map(FfiApproveReply::from)
        .map_err(general_err)
}

/// Remove a subscriber from a tier: rotate to a fresh period key, mint over the
/// post-removal roster, upload via `subscribers.remove`, then commit the
/// rotation — crash-staged before the upload
/// ([`SubscriptionsAuthor::remove_subscriber`]). A no-op (subscriber already
/// absent, nothing staged) returns `Ok` without rotating.
#[fauna_uniffi_async::export]
pub async fn subscriptions_remove_subscriber(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    tier_name: String,
    subscriber_id: Vec<u8>,
) -> Result<(), FfiError> {
    let author = author(&nest, &owner_secret)?;
    let subscriber = bytes_to_actor_id(&subscriber_id)?;
    author
        .remove_subscriber(&tier_name, subscriber)
        .await
        .map_err(general_err)
}

/// `fauna.subscriptions.subscribe`, **publishing** the caller's identity-seed
/// ML-KEM ek (surface B, S4b) unconditionally (no capability token), so an
/// author can later wrap hybrid `KeyBlob`s to this subscriber. The native twin
/// of [`fauna_client_subscriptions::SubscriptionsClient::subscribe_publishing_ek`]
/// (the web SPA's twin is `fauna-wasm`'s `subscriptionsSubscribePublishingEk`).
/// `subscriber_secret` is the caller's own 32-byte identity seed; `author_id` is
/// the 32-byte `ActorId` being subscribed to.
#[fauna_uniffi_async::export]
pub async fn subscriptions_subscribe_publishing_ek(
    nest: Arc<FfiNestClient>,
    subscriber_secret: Vec<u8>,
    author_id: Vec<u8>,
    tier: String,
) -> Result<FfiSubscribeReply, FfiError> {
    let subscriber = keypair_from_bytes(&subscriber_secret)?;
    let author = bytes_to_actor_id(&author_id)?;
    let subs = SubscriptionsClient::new(nest.nest_arc());
    subs.subscribe_publishing_ek(author, tier, &subscriber)
        .await
        .map_err(general_err)?
        .try_into()
}

/// Re-drive every staged subscriber-removal whose upload was interrupted by a
/// crash ([`SubscriptionsAuthor::resume_pending_removals`]). Call on client
/// startup (and after a config sync that may have merged a peer device's
/// staging). Returns the count resumed.
#[fauna_uniffi_async::export]
pub async fn subscriptions_resume_pending_removals(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<u32, FfiError> {
    let n = author(&nest, &owner_secret)?
        .resume_pending_removals()
        .await
        .map_err(general_err)?;
    Ok(n as u32)
}

/// Auto-approve every pending **subscribe** request whose tier is `auto_approve`,
/// minting the covering `KeyBlob` for each
/// ([`SubscriptionsAuthor::drain_auto_approvals`]). This is what makes an
/// encrypted-mode **follow** frictionless: the nest cannot mint, so a follow
/// *enqueues* (`Queued`), and the author's client drains it here. Call on client
/// connect (and on a subscribe-request push, with a poll backstop — the
/// `start_receive_loop` shape). Only `auto_approve` tiers + `subscribe` rows are
/// touched; an `unsubscribe` row is left for `remove_subscriber`. Returns the
/// count approved this pass (a poisoned request is skipped so it can't starve the
/// rest; the failure surfaces only when nothing drained).
#[fauna_uniffi_async::export]
pub async fn subscriptions_drain_auto_approvals(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<u32, FfiError> {
    let n = author(&nest, &owner_secret)?
        .drain_auto_approvals()
        .await
        .map_err(general_err)?;
    Ok(n as u32)
}

/// FFI mirror of [`fauna_client_subscriptions::ReconcilePass`] — what one
/// author-pump tick did. Both halves are best-effort and independent, so each
/// carries its own rendered error rather than the call failing.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq)]
pub struct FfiReconcilePass {
    /// Crash-staged subscriber removals driven to a confirmed nest upload.
    pub resumed: u32,
    /// Queued subscribe requests auto-approved (a `KeyBlob` minted for each).
    pub approved: u32,
    /// Why the resume half failed, if it did. Log it; never fatal.
    pub resume_error: Option<String>,
    /// Why the drain half failed, if it did. Log it; never fatal.
    pub drain_error: Option<String>,
}

impl From<ReconcilePass> for FfiReconcilePass {
    fn from(p: ReconcilePass) -> Self {
        FfiReconcilePass {
            resumed: p.resumed,
            approved: p.approved,
            resume_error: p.resume_error,
            drain_error: p.drain_error,
        }
    }
}

/// **One author-pump tick** — the whole body a native app's reconcile loop
/// should run, in the one correct order: heal crash-staged subscriber removals,
/// then auto-approve queued follows
/// ([`SubscriptionsAuthor::reconcile_once`]). Call it on connect and then every
/// [`subscriptions_author_poll_secs`] seconds; the shell supplies only the
/// scheduler.
///
/// Prefer this over calling [`subscriptions_resume_pending_removals`] and
/// [`subscriptions_drain_auto_approvals`] yourself: the order is load-bearing
/// (a staged removal must be driven out before the drain mints over the roster,
/// or the fresh `KeyBlob` re-covers the subscriber being removed) and **both**
/// belong in every tick (a removal staged mid-session — including one merged
/// from a peer device via config sync — otherwise heals only at the next
/// connect). This call makes both mistakes unrepresentable.
///
/// Once per connection it also runs the stale-keyed-blob pass
/// ([`SubscriptionsAuthor::republish_stale_keyed_blobs`]), keyed on the latch
/// `nest` carries, and logs that pass's own outcome — the record below carries
/// only the two per-tick halves.
///
/// Never fails: a bad tick is reported in the returned record so the pump can
/// log it and keep going.
#[fauna_uniffi_async::export]
pub async fn subscriptions_reconcile_once(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<FfiReconcilePass, FfiError> {
    Ok(author(&nest, &owner_secret)?
        .with_connect_pass(nest.subscriptions_connect_pass())
        .reconcile_once()
        .await
        .into())
}

/// The author pump's backstop cadence in seconds — shared policy, so every app
/// waits the same 30 s (`monetization.md` § Pillar 1) and honours the same
/// `FAUNA_SUBS_POLL_SECS` e2e override instead of hard-coding its own.
#[uniffi::export]
pub fn subscriptions_author_poll_secs() -> u64 {
    author_poll_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_pass_mirror_preserves_every_field() {
        // Total field map, like `pending_from_ffi` above, so the record can't
        // silently drift from the shared type it mirrors.
        let pass = ReconcilePass {
            resumed: 2,
            approved: 3,
            resume_error: Some("resume boom".into()),
            drain_error: Some("drain boom".into()),
        };
        let ffi: FfiReconcilePass = pass.into();
        assert_eq!(ffi.resumed, 2);
        assert_eq!(ffi.approved, 3);
        assert_eq!(ffi.resume_error.as_deref(), Some("resume boom"));
        assert_eq!(ffi.drain_error.as_deref(), Some("drain boom"));
        assert_eq!(FfiReconcilePass::from(ReconcilePass::default()).resumed, 0);
    }

    /// The cadence the natives read must be the shared one, not a per-binding
    /// literal — the drift this seam exists to remove.
    #[test]
    fn the_exported_cadence_is_the_shared_policy() {
        assert_eq!(subscriptions_author_poll_secs(), author_poll_secs());
    }

    #[test]
    fn pending_from_ffi_preserves_fields() {
        let ffi = FfiPendingRequest {
            request_id: 42,
            subscriber_id: vec![7u8; 32],
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: 1234,
            mlkem_encaps_key: Some(vec![9u8; 1184]),
            payment_entitled: true,
        };
        let req = pending_from_ffi(ffi).expect("valid 32-byte actor id");
        assert_eq!(req.request_id, 42);
        assert_eq!(req.subscriber_id.0, [7u8; 32]);
        assert_eq!(req.tier_name, "gold");
        assert_eq!(req.kind, "subscribe");
        assert_eq!(req.created_at.0, 1234);
        assert_eq!(
            req.mlkem_encaps_key.as_deref(),
            Some(&vec![9u8; 1184]),
            "ek round-trips back to the wire request"
        );
    }

    #[test]
    fn pending_from_ffi_rejects_short_actor_id() {
        let ffi = FfiPendingRequest {
            request_id: 1,
            subscriber_id: vec![0u8; 31],
            tier_name: "t".into(),
            kind: "subscribe".into(),
            created_at: 0,
            mlkem_encaps_key: None,
            payment_entitled: false,
        };
        assert!(pending_from_ffi(ffi).is_err());
    }
}
