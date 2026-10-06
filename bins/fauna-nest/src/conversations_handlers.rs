//! WS-RPC handlers for the user-facing `fauna.conversations.*` surface
//! — the MLS-channel ciphertext plane end-user clients invoke from the
//! conversations page (DM + group chat send/fetch/list + per-actor
//! key-package publish/fetch/count + same-nest Welcome delivery + the room
//! family).
//! T1b shipped the channel cluster
//! (`fauna.conversations.channel.{send,fetch,list_for_actor}`); T2
//! added the keypackage cluster
//! (`fauna.conversations.keypackage.{upload,fetch,count}`); T3 adds
//! same-nest welcome (`fauna.conversations.welcome.deliver`); the
//! `fauna.conversations.room.*` family carries the operations of
//! `conversation-rooms.md` § The room. (The nine `fauna.conversations.group.*`
//! kinds these handlers once also served were retired under the alpha
//! carve-out — § The group plane's fate, step 3.)
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::
//! is_permitted` (User-only arms for these kinds); the same gate the
//! `fauna.bridges.*` and `fauna.email.*` user-facing kinds use,
//! partitioned by `CallerClass`.
//!
//! The business logic (storage-mode-driven ingest verification,
//! behavioral-anomaly scoring on `dm_sent`, `segments::conv` append,
//! push fan-out via `notify_push(... ChannelMessage(...) ...)`) is the
//! sole home for the conversations surface: the HTTP twins were **DELETED**
//! in the WS-RPC-everywhere rip (T8) along with the whole `channel_routes.rs`
//! file and the `paths::migratable::conversations` module.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::{
    RpcError, Value,
    conversations::{
        ChannelActorsRemoteRequest, ChannelActorsReply, ChannelActorsRequest, ChannelEnvelope,
        ChannelFetchEntry, ChannelFetchReply, ChannelFetchRequest, ChannelListForActorReply,
        ChannelListForActorRequest, ChannelSendRemoteRequest, ChannelSendReply, ChannelSendRequest,
        ConversationBlobWriteTokenGetReply, ConversationBlobWriteTokenGetRequest,
        KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET, KeypackageCountReply, KeypackageCountRequest,
        KeypackageFetchReply, KeypackageFetchRequest, KeypackageUploadReply,
        KeypackageUploadRequest, RoomAcceptInviteRemoteRequest, RoomAcceptInviteReply,
        RoomAcceptInviteRequest, RoomBackfillGenerationsReply, RoomBackfillGenerationsRequest,
        RoomCreateReply, RoomCreateRequest, RoomGenerationWire, RoomGenerationsRemoteRequest,
        RoomGenerationsReply, RoomGenerationsRequest, RoomInviteRemoteRequest, RoomInviteReply,
        RoomInviteRequest, RoomLeaveRemoteRequest, RoomLeaveReply, RoomLeaveRequest,
        RoomListInvitesReply, RoomListInvitesRequest, RoomListRosterRemoteRequest,
        RoomListRosterReply, RoomListRosterRequest, RoomPendingInviteWire,
        RoomPublishGenerationReply, RoomPublishGenerationRequest, RoomRemoveReply,
        RoomRemoveRequest, RoomRevokeInviteReply, RoomRevokeInviteRequest, RoomRosterEntryWire,
        RoomRosterMemberWire, RoomRosterReportRemoteRequest, RoomRosterReportReply,
        RoomRosterReportRequest, RoomSearchRequest, RoomSetLabelersReply, RoomSetLabelersRequest,
        RoomSetPolicyReply, RoomSetPolicyRequest, RoomSetReceptionKeyReply,
        RoomSetReceptionKeyRequest, RoomTransferOwnershipReply, RoomTransferOwnershipRequest,
        WelcomeDeliverReply, WelcomeDeliverRequest, WelcomeKind,
    },
    decode_strict as decode,
    posts::LegalTakedownMarker,
};

use fauna_core::data::{ArrivalOrigin, InboxMode, ReachVerdict, dm_initiation_mode_verdict};

use crate::routes::AppState;
use crate::routes::parse_32_bytes;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("conversations", reason)
}

/// `fauna.conversations.rate_limited` — the non-claimant folder-channel
/// commit rate cap's refusal (`federation.md` § residual (a)).
/// Retryable (`RpcError::action()` routes it to `Transient`): an honest
/// client at the cap should back off and retry, never conclude it is
/// forbidden.
fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited_ns("conversations")
}

use crate::rpc_errors::internal;

/// Gate 1 of the conv cross-location-backup capability (Plan 9). Raised when a
/// channel-history read targets a nest that holds the channel's `__conv/<hex>`
/// reserved set in pure-backup mode — it stores opaque chunks only and has no
/// local plaintext-framed segments to satisfy the query. Conv-namespaced
/// sibling of mail's `fauna.bridges.pure_backup_destination` (IMAP-side gate).
fn pure_backup_destination() -> RpcError {
    crate::rpc_errors::pure_backup_destination_ns(
        "conversations",
        "serving channel history requires local plaintext-framed segments; this destination holds opaque chunks only",
    )
}

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("conversations", reason)
}

/// The device-owned-epoch commit precondition (`expect_no_commit_since`) failed:
/// a `ChannelEnvelope::Commit` landed on the channel after the caller's seq, so
/// the caller is committing from a stale epoch. The client rebases — clear the
/// pending commit, process the intervening records, retry with the new seq
/// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync). The
/// `details` carry the current commit high-water mark the caller must catch up
/// past.
fn stale(latest_commit_seq: i64) -> RpcError {
    let mut e = RpcError::new(
        "fauna.conversations.channel.stale",
        "error.conversations.channel.stale",
    );
    e.details = Some(Box::new(Value::String(format!(
        "a commit landed since your seq; latest_commit_seq={latest_commit_seq}"
    ))));
    e
}

/// The recipient's reach floor turned this arrival away. Deliberately says
/// nothing about *why* — a supervised recipient's status is not the sender's
/// business (`family-safety.md` § Don't do these — don't surface the ward's
/// policy to third parties).
fn forbidden(reason: &str) -> RpcError {
    crate::rpc_errors::forbidden_ns("conversations", reason)
}

/// The **outbound** half of `contact_approval` for the Welcome plane: a
/// supervised sender may only initiate a conversation with an approved contact,
/// mirroring the `fauna.inbox.send` gate (`inbox_handlers`). The guardian
/// pre-approves a peer via `fauna.family.contact.add`.
///
/// Unlike the inbound refusal, this one names the reason: the caller IS the
/// ward, who already knows they are supervised (§ Don't make supervision silent).
async fn outbound_reach_gate(
    state: &Arc<AppState>,
    sender: &[u8; 32],
    recipient: &[u8; 32],
) -> Result<(), RpcError> {
    let Some(policy) = state
        .db
        .get_guardian_policy(sender)
        .await
        .map_err(|e| internal(format!("guardian policy: {e}")))?
    else {
        return Ok(());
    };
    if !policy.contact_approval {
        return Ok(());
    }
    let status = state
        .db
        .get_contact_status(sender, recipient)
        .await
        .map_err(|e| internal(format!("contact status: {e}")))?;
    if matches!(status.as_deref(), Some("accepted") | Some("confirmed")) {
        return Ok(());
    }
    Err(crate::rpc_errors::guardian_approval_required_ns(
        "conversations",
        "this account can only start conversations with approved contacts — ask your guardian",
    ))
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// The per-record ceiling on `attachment_refs` entries. A chat message carries
/// a handful of attachments; the list is client-asserted, so it is bounded like
/// every other client-sized input — a hostile sender could otherwise pin an
/// unbounded hash list per send. The number is
/// `fauna_core::attachment_limits::MAX_ATTACHMENTS_PER_RECORD`, which every
/// app's receive loop also walks at most.
pub(crate) const MAX_ATTACHMENT_REFS_PER_RECORD: usize =
    fauna_core::attachment_limits::MAX_ATTACHMENTS_PER_RECORD;

/// The labeler facet loop's per-item work ceiling is this same number, and
/// this constant is its reason: a record cannot pin more blobs than this, so
/// an item whose **sealed** body names more attachments than this is naming
/// blobs its own record never pinned. The nest cannot check that list at send
/// time — it is sealed — so the bound is applied where the list is opened
/// (`fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES`,
/// `content-moderation-and-ranking.md` § Tier-3 → *The attachment facet*,
/// rule (4)). Pinned here so a bump of either ceiling cannot silently drift
/// from the other.
const _: () = assert!(
    MAX_ATTACHMENT_REFS_PER_RECORD == fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES,
    "the facet loop's per-item work ceiling is justified by this record cap; keep them equal",
);

/// Parse the wire `attachment_refs` (64-hex sealed-blob content addresses —
/// `ChannelSendRequest::attachment_refs`, the conversation kind's
/// blob-reachability floor) into digests. A malformed entry or an over-long
/// list is `invalid_params`, never silently dropped: a ref the nest cannot key
/// by pins nothing, and dropping it would leave that attachment unpinned while
/// the client believes it named it.
pub(crate) fn parse_attachment_refs(refs: &[String]) -> Result<Vec<[u8; 32]>, RpcError> {
    if refs.len() > MAX_ATTACHMENT_REFS_PER_RECORD {
        return Err(invalid_params(&format!(
            "attachment_refs lists {} hashes; at most {MAX_ATTACHMENT_REFS_PER_RECORD} per record",
            refs.len()
        )));
    }
    refs.iter()
        .map(|h| {
            let bytes =
                hex::decode(h).map_err(|_| invalid_params("attachment_refs entry is not hex"))?;
            <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                invalid_params("attachment_refs entry is not a 32-byte content address")
            })
        })
        .collect()
}

fn parse_channel_id(hex_str: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_params("invalid channel_id hex"))
}

fn parse_actor_id(hex_str: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_params("invalid actor_id hex"))
}

/// A room id is 32 bytes; for an end-to-end room it IS the MLS channel id
/// (`conversation-rooms.md` § The room), which is why the report door's
/// routing-roster gate can key on it directly.
fn parse_room_id(hex_str: &str) -> Result<[u8; 32], RpcError> {
    parse_32_bytes(hex_str).ok_or_else(|| invalid_params("invalid room_id hex"))
}

/// Gate 1 (Plan 9): refuse channel-history reads when this nest holds the
/// channel's `__conv/<hex>` reserved set in pure-backup mode. Mirrors mail's
/// `require_local_mail_serving` (the IMAP-side gate) — opaque chunks cannot
/// satisfy a channel-history query.
async fn require_local_conv_serving(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
) -> Result<(), RpcError> {
    crate::bridge_routing_handlers::refuse_if_pure_backup(
        state,
        "conv",
        channel_id,
        pure_backup_destination,
    )
    .await
}

/// F1 eviction durability.
/// Whether the authenticated `caller` may drive an auto-registration into
/// `channel_id`'s `actor_channels` roster on one of the three raw
/// auto-register paths (`channel.send`, `channel.fetch`, same-nest
/// `welcome.deliver`).
///
/// A **claimed** folder channel (a row in `folder_channel_claims` — the
/// first-binder owner) has an owner-managed roster: members are added only by
/// the owner's `share` and removed only by the owner's `evict`
/// (rotate-on-removal). So only the claimant may drive an auto-register there.
/// This closes the raw self-insert paths through which an **evicted** member —
/// who still knows the stable `group_id` (MLS removal advances the epoch, not
/// the id) — could otherwise re-insert themselves (or a second identity they
/// control) and re-surface the set's discovery metadata (`media.list` /
/// `members.list_actors` / `folders.list`). The claim guard was already the
/// co-requisite that made `members.evict` durable against the
/// `share`-rebind; this extends it to the auto-register paths. No legit
/// folder flow uses `channel.send`/`channel.fetch` (set content flows via
/// `content_key.get` + the sync daemon), and a legit `welcome.deliver` share is
/// sent by the owner (the share claims the channel before delivering the
/// Welcome, so `caller == claimant`).
///
/// **Unclaimed** channels — every DM / group / scheduling conversation — return
/// `true`: their delivery *depends* on auto-register (a DM recipient's roster
/// row is created by `welcome.deliver`; a sender's by their first
/// `channel.send`), and no folder is bound to them
/// (`folder_channel_claimed_by` is `None`), so the gate never touches them.
/// The **cross-nest** rail has two auto-register sites of its own, each gated
/// where it lives, not here: (a) the **relaying** side — [`welcome_deliver_core`]'s
/// own `req.nest_url` branch calls [`crate::db::CacheDb::register_foreign_channel_member`]
/// (`db/channels.rs`) to grant the recipient's home nest a foreign-member row,
/// gated there by [`claim_permits`] on the same claim read this function
/// produces; and (b) the **receiving** side — `federation_handlers`'s
/// `welcome.deliver` ingest, which registers the local recipient on
/// `actor_channels` and, for a claimed folder channel, does not pre-register at
/// all (cross-nest-always-knocks: the recipient joins only on explicit accept).
///
/// Fail-**closed** on a claim-read error: [`FolderChannelClaim::Unknown`]
/// refuses the auto-register the same as a real claimant would, at the one
/// cost that a transient claim-read error now defers first-post delivery on
/// EVERY channel (not just claimed ones) to a client retry — the roster check
/// downstream (`ingest_channel_envelope`) is already fail-closed and already
/// retries the same way, so this is not a new failure shape, only a wider one.
pub(crate) async fn folder_channel_claim(
    db: &crate::db::CacheDb,
    channel_id: &[u8; 32],
) -> FolderChannelClaim {
    match db.folder_channel_claimed_by(channel_id).await {
        Ok(Some(c)) => FolderChannelClaim::Claimed(c),
        Ok(None) => FolderChannelClaim::Unclaimed,
        Err(e) => {
            tracing::error!(
                "folder_channel_claim read ({}): {e}",
                hex::encode(channel_id)
            );
            FolderChannelClaim::Unknown
        }
    }
}

/// The three states a folder-channel claim read can land in — `Unclaimed` and
/// a failed/malformed read are deliberately NOT the same state (an
/// `Option<[u8; 32]>` collapsed them, which is exactly the bug). Only
/// `Unclaimed` is permissive; `Unknown` gates like a real claimant would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderChannelClaim {
    Claimed([u8; 32]),
    Unclaimed,
    Unknown,
}

/// Whether `caller` may drive an auto-register, given `channel_id`'s
/// already-read claim. Pure — split out of the old combined read+gate so
/// [`channel_send_core`] can share ONE claim read between this gate and the
/// non-claimant commit rate cap below, instead of two DB
/// round-trips.
fn claim_permits(claim: FolderChannelClaim, caller: &[u8; 32]) -> bool {
    match claim {
        FolderChannelClaim::Claimed(c) => c == *caller,
        FolderChannelClaim::Unclaimed => true,
        FolderChannelClaim::Unknown => false,
    }
}

/// How much power `caller` holds over an EXISTING cross-nest foreign-member
/// grant — a strictly narrower question than [`claim_permits`]' "may write
/// one".
///
/// `federation.md` § Cross-nest shared folders + channel append accepts
/// re-binding as a residual *"within the inviter's existing power (they chose
/// to add the member)"*. `claim_permits` alone does not hold that bound: on a
/// conversation channel there is no claimant, so its `Unclaimed` arm charges
/// the power to **knowledge of the 32-byte channel id** instead — which an
/// MLS-removed ex-member keeps, since removal advances the epoch and not the
/// id. The grant is an authorization (`require_foreign_member` serves the
/// ciphertext stream, the roster in the clear, appends, leaves and content
/// keys off it), so a stale id must not move it.
///
/// Standing, per rail: the **claimant** on a claimed folder channel (already
/// the owner — [`RebindPower::Claimant`]), or a **rostered actor** on an
/// unclaimed conversation channel ([`RebindPower::Standing`]) — which is what
/// "the inviter" means there. The roster can only attest *was ever rostered*
/// (the nest never sees an MLS removal and nothing deletes conversation-rail
/// roster rows), so `Standing` deliberately stops short of a confirmed
/// binding: the writer's conflict arm refuses it once the bound home nest has
/// exercised the grant — the first-use pin. Callers still gate on [`claim_permits`] first; this only narrows
/// the conflict arm.
async fn may_rebind_foreign_member(
    state: &AppState,
    claim: FolderChannelClaim,
    channel_id: &[u8; 32],
    caller: &[u8; 32],
) -> crate::db::channels::RebindPower {
    use crate::db::channels::RebindPower;
    match claim {
        // The claimant IS the owner — its rebind power is the accepted
        // premise, and it alone reaches past the first-use pin.
        FolderChannelClaim::Claimed(c) if c == *caller => RebindPower::Claimant,
        FolderChannelClaim::Claimed(_) => RebindPower::InsertOnly,
        FolderChannelClaim::Unclaimed => {
            match state.db.is_actor_in_channel(caller, channel_id).await {
                Ok(true) => RebindPower::Standing,
                // A roster read that cannot be answered refuses the REBIND
                // only; the insert arm is unaffected, so a DB blip never
                // breaks DM initiation.
                Ok(false) | Err(_) => RebindPower::InsertOnly,
            }
        }
        FolderChannelClaim::Unknown => RebindPower::InsertOnly,
    }
}

/// Whether `actor` IS the channel's claimant — `true` only for
/// [`FolderChannelClaim::Claimed`] matching `actor` exactly; `Unclaimed` and
/// `Unknown` both read as "not provably the claimant" (the commit rate cap
/// below uses this: an unclaimed channel is out of its scope entirely, so it
/// is excluded separately, but a claim read that came back `Unknown` gets
/// capped rather than exempted — the conservative direction under
/// uncertainty).
fn is_the_claimant(claim: FolderChannelClaim, actor: &[u8; 32]) -> bool {
    matches!(claim, FolderChannelClaim::Claimed(c) if c == *actor)
}

/// Best-effort auto-register of `actor` onto `channel_id`'s roster, gated by
/// [`claim_permits`] against an ALREADY-READ `claim`.
/// A register error is logged (`ctx`) and swallowed — the roster is
/// best-effort on these auto-register paths; a gate rejection (a non-claimant
/// on a claimed folder channel, or an unresolvable claim state) is silent, as
/// it is the expected close.
async fn register_actor_channel_gated_with_claim(
    db: &crate::db::CacheDb,
    channel_id: &[u8; 32],
    claim: FolderChannelClaim,
    caller: &[u8; 32],
    actor: &[u8; 32],
    ctx: &str,
) {
    if !claim_permits(claim, caller) {
        return;
    }
    if let Err(e) = db.register_actor_channel(actor, channel_id).await {
        tracing::warn!("{ctx}: {e}");
    }
}

/// Convenience wrapper for the two call sites that don't otherwise need the
/// claim value — reads it fresh and gates on it.
async fn register_actor_channel_gated(
    db: &crate::db::CacheDb,
    channel_id: &[u8; 32],
    caller: &[u8; 32],
    actor: &[u8; 32],
    ctx: &str,
) {
    let claim = folder_channel_claim(db, channel_id).await;
    register_actor_channel_gated_with_claim(db, channel_id, claim, caller, actor, ctx).await;
}

// ── fauna.conversations.channel.send ───────────────────────────

fn channel_send_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.channel.send").await?;
            let req: ChannelSendRequest = decode(&payload).map_err(malformed)?;
            let channel_id = parse_channel_id(&req.channel_id)?;
            // The sender's plaintext attachment references — refused, never
            // dropped, when malformed (see `parse_attachment_refs`).
            let attachment_refs = parse_attachment_refs(&req.attachment_refs)?;
            let body = Bytes::from(req.envelope);
            // User-facing chat send → run the Layer-2 behavioral anti-spam path.
            // `expect_no_commit_since` (device-owned-epoch commit gate) rides
            // through to the seq-locked append.
            encode_reply(
                &channel_send_core(
                    &state,
                    &actor_id,
                    &channel_id,
                    body,
                    true,
                    req.expect_no_commit_since,
                    &attachment_refs,
                )
                .await?,
            )
        })
    })
}

// ── fauna.conversations.channel.send_remote ────────────────────

/// The foreign-member send relay (`direct-messages.md` § step 3b): the caller's
/// channel lives on another nest (they hold a recorded Welcome `nest_url`), so
/// this nest originates `fauna.federation.channel.append` there. The home nest
/// applies the whole same-nest send pipeline to the append (structural
/// foreign-member gate, S2 commit gate, strict ingest, anti-spam, seq-locked
/// append) — this handler adds no policy of its own beyond the relay.
///
/// Error mapping (S5, `federation.md` § old-peer error shapes): a peer-side
/// `fauna.protocol.unauthenticated` means the home nest does not know the append
/// kind (allowlist-first) — surfaced as the typed `peer_nest_outdated`, never a
/// spurious auth failure. Every other peer error (the commit-gate
/// `permission_denied`, the device-epoch `channel.stale`, the structural
/// `fauna.federation.forbidden`) rides through untouched so the client's
/// existing handling (rebase-and-retry on stale, loud refusal otherwise) works
/// unchanged on the remote path.
fn channel_send_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.channel.send_remote")
                .await?;
            let req: ChannelSendRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id + ref shapes locally; the home nest is
            // authoritative for membership + append policy (and re-parses the
            // refs it records).
            parse_channel_id(&req.channel_id)?;
            parse_attachment_refs(&req.attachment_refs)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the channel's home nest (same-nest sends use channel.send)",
                ));
            }

            match crate::federation_pool::originate_channel_append(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.channel_id,
                req.envelope,
                req.expect_no_commit_since,
                req.attachment_refs,
            )
            .await
            {
                Ok(Ok(seq)) => encode_reply(&ChannelSendReply {
                    seq,
                    extra: std::collections::BTreeMap::new(),
                }),
                // S5: an old home nest's allowlist refuses the append kind
                // before any handler runs → the typed peer_nest_outdated;
                // every other typed refusal rides through untouched.
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest send relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation channel append (send_remote): {pool_err}");
                    Err(internal("federation append failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.channel.actors_remote ──────────────────

/// The foreign-member roster-read relay (`federation.md` § Cross-nest): the
/// caller's channel is foreign-homed (they hold a recorded Welcome `nest_url`),
/// so this nest originates `fauna.federation.channel.actors` there and answers
/// the home nest's authoritative roster union. The home nest applies the
/// structural foreign-member gate; this handler adds no policy of its own
/// beyond the relay — in particular it does **not** auto-register the caller
/// anywhere (the read is strictly read-only on every hop: a roster read that
/// wrote a row would make every phantom look healthy).
///
/// Error mapping (S5, `federation.md` § old-peer error shapes): a peer-side
/// `fauna.protocol.unauthenticated` means the home nest does not know the kind
/// (allowlist-first) — surfaced as the typed `peer_nest_outdated`, never a
/// spurious auth failure. The client's seam degrades every error to "roster
/// unreadable", which lands the heal in its refuse arm — the correct
/// degradation on every skew pairing.
fn channel_actors_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.channel.actors_remote",
            )
            .await?;
            let req: ChannelActorsRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id shape locally; the home nest is authoritative for
            // membership.
            parse_channel_id(&req.channel_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the channel's home nest (same-nest reads use channel.actors)",
                ));
            }

            match crate::federation_pool::originate_channel_actors(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.channel_id,
            )
            .await
            {
                Ok(Ok(actors)) => encode_reply(&ChannelActorsReply {
                    actors,
                    extra: std::collections::BTreeMap::new(),
                }),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest roster read relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation channel actors (actors_remote): {pool_err}");
                    Err(internal("federation roster read failed"))
                }
            }
        })
    })
}

/// What the channel roster says a send is addressed to, once the sender is
/// filtered out — the input to DM behavioral anti-spam
/// (`docs/goal/behavior/direct-messages.md` § Anti-Spam, which owns the
/// per-case rulings).
///
/// Only [`DmPeer::Single`] is a DM in the sense the scorer means. The other two
/// are distinct facts with distinct correct handling, and collapsing them is
/// what let group-forked threads sit silently outside a control the goal doc
/// said covered them.
enum DmPeer {
    /// Exactly one other member: a 1:1 DM. Scored.
    Single([u8; 32]),
    /// No other member on the roster yet — the recipient has not processed the
    /// Welcome. Volume recorded without a target; not scored, because "no path
    /// found" and "unknown" are indistinguishable with no recipient.
    Unseated,
    /// Two or more other members: a group-forked thread. Not DM-fanout-scored.
    Group,
}

impl DmPeer {
    /// The recipient to attribute a `dm_sent` event to, and to score against.
    /// `None` for both non-DM shapes — which is what keeps a group send and an
    /// unseated first message out of every fanout window (they filter
    /// `target_actor IS NOT NULL`).
    fn single(&self) -> Option<[u8; 32]> {
        match self {
            DmPeer::Single(r) => Some(*r),
            DmPeer::Unseated | DmPeer::Group => None,
        }
    }

    /// Stable label for the `nest_dm_antispam_peer_total` metric.
    fn metric_label(&self) -> &'static str {
        match self {
            DmPeer::Single(_) => "dm",
            DmPeer::Unseated => "unseated",
            DmPeer::Group => "group",
        }
    }
}

/// The `fauna.conversations.channel.send` body, minus the caller-class gate +
/// payload decode — shared between the User-facing [`channel_send_handler`]
/// (which passes `run_antispam = true`) and the MDA server-side scheduling
/// gateway ([`deliver_scheduling_as_organizer`], which posts a one-off
/// scheduling iMIP **as the organizer** with `run_antispam = false` — a one-off
/// scheduling channel isn't chat, so the behavioral scoring is skipped). The
/// channel membership + ingest sender + segment-store record all bind to
/// `acting_actor`.
///
/// `expect_no_commit_since` is the device-owned-epoch commit precondition
/// (`Some(seq)` only when a client posts an MLS commit; the scheduling gateway
/// and today's clients pass `None`): when set, the seq-locked append rejects with
/// `fauna.conversations.channel.stale` if a `ChannelEnvelope::Commit` has landed
/// since that seq (`docs/goal/behavior/devices.md` § Cross-device MLS
/// group-state sync).
///
/// `attachment_refs` is the sender's plaintext list of the sealed attachment
/// blobs the (sealed) envelope names — the conversation kind's
/// blob-reachability floor (`encryption-at-rest.md` § Per-content-kind
/// conformance → Conversation messages row, 2026-09-08). Recorded beside the
/// record's mirror row in the same transaction so the blob GC can pin them
/// for as long as the record is live; empty for every non-attachment send.
pub(crate) async fn channel_send_core(
    state: &Arc<AppState>,
    acting_actor: &[u8; 32],
    channel_id: &[u8; 32],
    body: Bytes,
    run_antispam: bool,
    expect_no_commit_since: Option<i64>,
    attachment_refs: &[[u8; 32]],
) -> Result<ChannelSendReply, RpcError> {
    // Rebind to owned values so the body below reads identically to the
    // original handler (which captured owned `actor_id` / `channel_id`).
    let actor_id = *acting_actor;
    let channel_id = *channel_id;

    // A test-only per-channel refusal, consulted FIRST so a refused send
    // consumes no seq, no storage and no rate-cap budget — the staged fault is
    // a flake the client may safely retry, which is exactly what the succession
    // sweep's retry affordance is for (`channel_refusal_test_hook`'s module doc
    // carries the whole reasoning, including why the selector is an envelope
    // CLASS and not a count of sends). Compiled out of every production build.
    #[cfg(feature = "test-hooks")]
    {
        let armed = state
            .channel_send_refusal
            .lock()
            .expect("channel_send_refusal mutex poisoned")
            .get(&channel_id)
            .copied();
        if let Some(class) = armed
            && class.refuses(body.as_ref())
        {
            return Err(internal(format!(
                "test-hook: channel {} refuses {:?} envelopes",
                hex::encode(channel_id),
                class
            )));
        }
    }

    // Refuse an envelope too large to ever be served back in one WS frame. A serve page is byte-budgeted to `SERVE_PAGE_BUDGET_BYTES`, and
    // every record costs its bytes + a fixed `RECORD_WIRE_OVERHEAD`, so a record
    // over `SERVE_PAGE_BUDGET_BYTES - RECORD_WIRE_OVERHEAD` freezes
    // `take_page_within_budget` forever (empty page, head unmoved) — the channel
    // drain then stalls past that seq for EVERY member, silent to the client (an
    // empty page reads as "drained"). The inbound 2 MiB frame alone was a looser
    // cap than the serve budget, so it let such a record through. Refuse it at
    // the door so anything accepted is servable in one frame.
    const MAX_CHANNEL_ENVELOPE_BYTES: usize =
        crate::segments::SERVE_PAGE_BUDGET_BYTES - crate::segments::RECORD_WIRE_OVERHEAD;
    if body.len() > MAX_CHANNEL_ENVELOPE_BYTES {
        return Err(invalid_params(&format!(
            "channel envelope {} bytes exceeds the {}-byte serve-page limit \
             (would freeze the channel drain — transport.md § Max frame)",
            body.len(),
            MAX_CHANNEL_ENVELOPE_BYTES
        )));
    }

    // Folder channel Commit admission (`federation.md` § Cross-nest shared
    // folders + channel append — re-ratified 2026-08-24; supersedes the
    // 2026-07-18/S2 claimant-only refusal). A claimed folder channel's MLS
    // group is owner-managed, but MLS commits are PrivateMessage ciphertext:
    // this nest can see THAT an envelope is a Commit, never WHAT it commits —
    // so "owner-only roster changes" is cryptographically unenforceable here,
    // while it is exactly enforceable at every member (who decrypts the
    // staged commit). And a member's client legitimately MUST commit: the
    // device-owned-epoch invariant (`devices.md` § Cross-device MLS
    // group-state sync) has every member device post a bare self-`Update`
    // takeover before its first application send — the share-plane
    // advertisement and custody doors all ride that path, and the old
    // claimant-only refusal made three ratified rules mutually unsatisfiable
    // (a member could never advertise). The split now:
    //
    //   - NEST: a Commit is admitted from any actor on the channel roster —
    //     the same `ingest_channel_envelope` roster check every envelope
    //     passes below (`ChannelRosterMiss` refuses outsiders and evicted
    //     members; a refused envelope consumes no seq). Every roster-ADD path
    //     stays claimant-gated (`register_actor_channel_gated`, the share-rebind claim,
    //     the federation welcome), so eviction durability is unchanged, and
    //     removed-member exclusion never rested on this gate at all
    //     (rotate-on-removal crypto — `key-material-hierarchy.md` § Rotate-
    //     on-removal).
    //   - MEMBERS: `MlsEngine::process_commit`'s folder commit policy refuses
    //     a proposal-carrying commit (Add/Remove/rotation material) whose
    //     MLS-authenticated committer is not the recorded folder owner —
    //     deterministic at every honest member, skipped like an intrinsically
    //     invalid record; a bare self-`Update` merges from any member.
    //
    // Residual (named in the ratification), throttled below: a
    // hostile member can churn epochs with takeover commits (same set-DoS
    // class as Application-envelope spam, which was always roster-open) —
    // the per-(actor, channel) commit rate cap bounds how often a
    // non-claimant's Commit is admitted, claimant exempt — the cap bounds how often such a commit can be
    // exercised, not whether.
    //
    // Conversation-reuse note (unchanged): `claim_folder_channel` refuses any
    // claim on a roster-populated channel, so a
    // claimed channel is by construction a pure folder channel; a real
    // conversation-binding flow's full obligation list is owned by
    // `mls-group-key-material.md` § M2 / Rotate-on-removal, not restated here.

    // ── the community class's floor gate ───────────────────────────
    //
    // "The home nest refuses a send, invite, remove, delete or policy change
    // whose signer's role does not permit it, **before storing anything**"
    // (`conversation-rooms.md` § The floor roster → *Community rooms*). This
    // is that clause on the send path, and it is the difference the nest
    // actually enforces between the two classes.
    //
    // It fires only for a **floor-authoritative** room — one the create
    // ceremony founded. An end-to-end room's membership is its MLS group's,
    // which this nest cannot read, so its send stays gated on the routing
    // roster alone exactly as before: a stranger's envelope there is spam
    // that decrypts for nobody, not forgery.
    //
    // Why it matters more for a community room: the routing roster
    // self-registers on first send, so without this any actor that learned
    // the 32-byte channel id could append to a community room's log — and
    // unlike the end-to-end case those bytes are ones the home nest holds a
    // wrap for and will fan out and index into its derived views. The floor
    // roster is the non-self-assertable record that stops it.
    //
    // MEMBERSHIP, not rank: the roles table gives "send, react, delete own
    // message" to all three roles, so a live member of any rank may send. A
    // role-keyed gate here would be a stricter rule than the one ratified.
    {
        let room = state
            .db
            .get_room(&channel_id)
            .await
            .map_err(|e| internal(format!("room read: {e}")))?;
        let floor_room = room.filter(|r| r.is_floor_authoritative());
        if floor_room.is_some()
            && !state
                .db
                .is_room_member(&channel_id, &actor_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?
        {
            return Err(permission_denied(
                "not a member of this room's floor — a community room's sends are verified against it",
            ));
        }

        // RANK, for the one send that is a floor act: an owner's or admin's
        // delete of another member's message (`conversation-rooms.md` § Roles
        // and authorization → *Delete any message — the mechanism* →
        // *Community rooms*). The record rides unsealed precisely so this can
        // be judged **from the record alone** — the nest never opens a sealed
        // send to learn whether it is a delete, which its read may not be used
        // for (`community-rooms.md` § What the home nest does with its read).
        // Every other envelope stays membership-gated, above.
        if let Ok(ChannelEnvelope::RoomFloorDelete(record)) =
            ChannelEnvelope::from_bytes(body.as_ref())
        {
            let Some(room) = floor_room else {
                return Err(permission_denied(
                    "a floor delete record is a community room's act — this room's authority is not its floor",
                ));
            };
            judge_floor_delete(state, &room, &channel_id, &actor_id, &record).await?;
        }
    }

    {
        // ONE claim read, shared by the auto-register gate below and the
        // commit rate cap that follows it — was two DB round-trips.
        let claim = folder_channel_claim(&state.db, &channel_id).await;

        // Auto-register: first post to a channel registers the poster.
        // Must run before ingest_channel_envelope so the roster-membership
        // check there sees the sender as registered on the very first
        // post. Mirrors `channel_routes::post_channel_message`. Gated so a
        // claimed folder channel's owner-managed roster can't be
        // self-inserted (no legit folder flow posts
        // here; only unclaimed DM/group channels reach the register).
        register_actor_channel_gated_with_claim(
            &state.db,
            &channel_id,
            claim,
            &actor_id,
            &actor_id,
            "register_actor_channel",
        )
        .await;

        // Non-claimant commit rate cap (`federation.md` § residual (a), row
        // 428): only a Commit, only when the channel is NOT provably
        // unclaimed, only when the actor is NOT provably the claimant. An
        // `Unclaimed` read is excluded (the cap is folder-channel-only, and
        // every DM/group channel must stay uncapped); `Unknown` is NOT
        // excluded — a claim read that failed gets capped rather than
        // exempted, the conservative direction under uncertainty.
        // Runs before `ingest_channel_envelope` so a capped request consumes
        // no seq / storage. Lenient decode, mirroring
        // `segments::conv::append_channel_record`'s own commit
        // classification: a body that doesn't decode as a `ChannelEnvelope`
        // is simply "not a commit", never an error here.
        if claim != FolderChannelClaim::Unclaimed
            && !is_the_claimant(claim, &actor_id)
            && matches!(
                ChannelEnvelope::from_bytes(body.as_ref()),
                Ok(ChannelEnvelope::Commit(_))
            )
            && !state
                .channel_commit_rate_limit
                .check(&actor_id, &channel_id, "commit")
        {
            return Err(rate_limited());
        }

        // Pre-store envelope verification (storage-mode-driven; see
        // channel_routes.rs for the verdict-vs-metric mapping).
        // BARE-decode + AEAD-shape + roster check; rejection surfaces as
        // `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Channel*))`.
        {
            let storage = state.storage();
            let outcome = match storage
                .ingest_channel_envelope(&crate::storage::ChannelEnvelopeIngestItem {
                    sender: actor_id,
                    channel_id,
                    body: body.as_ref(),
                })
                .await
            {
                Ok(o) => o,
                Err(e) => {
                    let reason_label = match &e.kind {
                        crate::storage::StorageUnavailableKind::IngestRejected(r) => {
                            r.as_snake_case()
                        }
                        _ => "error",
                    };
                    tracing::warn!(
                        target: "nest_metrics",
                        metric = "nest_channel_ingest_total",
                        verdict = "rejected",
                        reason = reason_label,
                        "channel ingest rejected: {e}"
                    );
                    let api = e.into_api_error();
                    let mut rpc = RpcError::new(
                        "fauna.conversations.ingest_failed",
                        "error.conversations.ingest_failed",
                    );
                    rpc.details = Some(Box::new(Value::String(api.message)));
                    return Err(rpc);
                }
            };
            match outcome.verdict {
                crate::storage::ChannelEnvelopeIngestVerdict::Accepted => {
                    tracing::debug!(
                        target: "nest_metrics",
                        metric = "nest_channel_ingest_total",
                        verdict = "accepted",
                        "channel ingest"
                    );
                }
            }
        }

        // Layer 2 anti-spam: behavioral event + anomaly score (per
        // direct-messages.md § Anti-Spam). Errors here must not block
        // delivery — mirrors channel_routes.rs. Skipped for the MDA
        // scheduling gateway (`run_antispam = false`): a one-off
        // scheduling channel carries a single server-fanned iMIP, not chat.
        if run_antispam {
            let now_us = fauna_core::data::Timestamp::now().as_i64();
            // Who the send is addressed to, as the roster sees it. The three
            // outcomes are kept distinct on purpose: this match used to collapse
            // "nobody else here yet" and "this is a group" into one `_` arm,
            // which silently exempted every group-forked thread from a control
            // the docs said applied, and made the exemption unreadable at the
            // call site.
            let peer = match state.db.list_channel_actors(&channel_id).await {
                Ok(members) => {
                    let mut others = members.into_iter().filter(|m| *m != actor_id);
                    match (others.next(), others.next()) {
                        // A 1:1 DM — the only shape the DM scorer describes.
                        (Some(r), None) => DmPeer::Single(r),
                        // The recipient has not processed the Welcome yet, so
                        // only the sender is registered. Volume is still worth
                        // recording; social context does not exist yet.
                        (None, _) => DmPeer::Unseated,
                        // A group-forked thread (`direct-messages.md`
                        // § Group-Forked Threads) riding this same kind. NOT
                        // DM-fanout-scored — but be precise about what this
                        // tests: "≥2 others on the roster right now", which a
                        // sender establishes at will via the shipped
                        // add-participant control. So the exemption is
                        // self-service, and § Anti-Spam step 1 says so outright
                        // rather than justifying it as "an established group".
                        // The right long-term instrument measures outreach at
                        // the add/Welcome, not at send time; unbuilt while the
                        // label has no consumer.
                        (Some(_), Some(_)) => DmPeer::Group,
                    }
                }
                Err(e) => {
                    tracing::warn!("list_channel_actors: {e}");
                    DmPeer::Unseated
                }
            };
            // Which shape a send took is observable, so "this channel is never
            // scored" is answerable from the outside instead of being inferred
            // from a match arm. Both non-DM shapes produce a target-less
            // `dm_sent` row, which is indistinguishable in the table — the
            // reason leg D went unnoticed.
            tracing::debug!(
                target: "nest_metrics",
                metric = "nest_dm_antispam_peer_total",
                shape = peer.metric_label(),
                "dm anti-spam peer resolution"
            );
            let recipient = peer.single();

            if let Err(e) = state
                .db
                .record_sender_event(&actor_id, "dm_sent", recipient.as_ref(), now_us)
                .await
            {
                tracing::warn!("record_sender_event: {e}");
            }

            // The reply leg. If this send's recipient has themselves DMed the
            // sender inside the window, this is an answer to that outreach —
            // the only feed for `dm_response_rate`, whose absence degraded the
            // 7-day fanout rule to fanout-alone for the layer's whole life.
            // `record_dm_reply` enforces "they messaged me first" and the
            // one-row-per-pair-per-window rule itself.
            if let Some(recipient) = recipient.as_ref()
                && let Err(e) = state.db.record_dm_reply(recipient, &actor_id, now_us).await
            {
                tracing::warn!("record_dm_reply: {e}");
            }

            if let Some(recipient) = recipient
                && let Ok(profile) = state
                    .db
                    .get_behavioral_profile(&actor_id, &recipient, now_us)
                    .await
            {
                let anomaly_score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
                if anomaly_score >= 0.3 {
                    let scanner_id = [0u8; 32];
                    let channel_hex = hex::encode(channel_id);
                    let _ = state
                        .db
                        .upsert_content_label(
                            "channel",
                            &channel_hex,
                            "spam/behavioral",
                            anomaly_score,
                            0,
                            &scanner_id,
                            1,
                            0,
                            None,
                            None,
                            now_us,
                            &scanner_id,
                            &[],
                        )
                        .await;
                }
            }
        }

        // Segment-store ingest write (Plan 7 T5): the __conv segment store
        // replaces the legacy `payload_store` + `append_channel_message`
        // pair — records are stored inline in the segment, so no separate
        // blob store. `received_at_ms` is the server-assigned receive time
        // (epoch ms), per `segments::conv::append`.
        //
        // The device-owned-epoch commit gate lives inside the seq-locked append
        // (`append_gated`), never here — a check outside that lock would race the
        // very commit it serializes. The register / ingest-verify / anti-spam
        // steps above ran already; on a `stale` rejection the client rebases and
        // retries, so a raced commit's `dm_sent` event is counted twice — an
        // accepted, bounded imprecision (commits are infrequent).
        let received_at_ms = fauna_core::data::Timestamp::now_millis() as i64;
        let map_append_err = |e: anyhow::Error| {
            tracing::error!("conv append error: {e}");
            internal("storage error")
        };
        let outcome = match expect_no_commit_since {
            Some(since) => {
                match crate::segments::conv::append_gated(
                    &state.conv_segments,
                    &state.db,
                    &channel_id,
                    body.as_ref(),
                    received_at_ms,
                    since,
                    attachment_refs,
                    Some(&actor_id),
                )
                .await
                .map_err(map_append_err)?
                {
                    crate::segments::conv::ConvAppendResult::Appended(o) => o,
                    crate::segments::conv::ConvAppendResult::StaleCommit { latest_commit_seq } => {
                        return Err(stale(latest_commit_seq));
                    }
                }
            }
            None => crate::segments::conv::append_with_refs(
                &state.conv_segments,
                &state.db,
                &channel_id,
                body.as_ref(),
                received_at_ms,
                attachment_refs,
                Some(&actor_id),
            )
            .await
            .map_err(map_append_err)?,
        };

        // The community class's read: the ONE nest path that opens a
        // conversation envelope, and only for a room whose members wrapped
        // this nest's room-read key into the tip generation
        // (`conversation-rooms.md` § The three classes → *Community*; the
        // declared exception to `ui/conversations.md` § Encryption at rest's
        // "no live nest path calls into the MLS decode" — which stays true
        // literally, since a community envelope is not MLS).
        //
        // Best-effort by construction: a member's send is a legitimate act
        // whether or not the nest can build a view from it, so every arm here
        // ends in "no view", never in a refusal. What the view is for is
        // § *What the home nest does with its read* — search first.
        index_room_message(state, &channel_id, outcome.seq, body.as_ref()).await;

        if let Ok(actors) = state.db.list_channel_actors(&channel_id).await {
            let event = fauna_protocol::PushEvent::ChannelMessage(
                fauna_protocol::push_events::ChannelMessagePayload {
                    channel_id: hex::encode(channel_id),
                    data: body.to_vec(),
                    extra: std::collections::BTreeMap::new(),
                },
            );
            for subscriber in &actors {
                state.ws.notify_push(subscriber, event.clone());
            }
        }

        Ok(ChannelSendReply {
            seq: outcome.seq,
            extra: std::collections::BTreeMap::new(),
        })
    }
}

/// The nest-attested authors of one served page, 64-hex by `seq`
/// (`conv_record_authors`) — shared by the local `channel.fetch` and the
/// `fauna.federation.channel.fetch` serve so both state the same thing.
/// Best-effort by design: a read fault serves the page with no authors, which
/// every reader treats as *no answer* (a scheduling mutation is then refused,
/// never applied) — the fail-closed direction, and never a failed drain.
pub(crate) async fn page_authors(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    after_seq: i64,
    rows: &[(i64, Vec<u8>, Option<String>)],
) -> std::collections::HashMap<i64, String> {
    let Some(up_to) = rows.iter().map(|(seq, _, _)| *seq).max() else {
        return Default::default();
    };
    match state
        .db
        .conv_record_authors_in_range(channel_id, after_seq, up_to)
        .await
    {
        Ok(found) => found
            .into_iter()
            .map(|(seq, author)| (seq, hex::encode(author)))
            .collect(),
        Err(e) => {
            tracing::error!("conv record authors read: {e}");
            Default::default()
        }
    }
}

/// Server-side scheduling delivery used by the MDA `calendar-auto-schedule`
/// gateway's **mailbox-less** rail (`fauna.bridges.deliver_sealed_scheduling`,
/// `bridge_routing_handlers`). Runs `welcome.deliver`(tagged `Scheduling`) +
/// `channel.send` **as `organizer`** over the SAME cores the User-facing
/// handlers use — so the client rail and the server gateway seal+deliver
/// identically (priority #2) — skipping the chat anti-spam path. The caller
/// (`deliver_sealed_scheduling_handler`) has already class-gated (BridgeMda) +
/// caller-scoped to `organizer`; this fn does no permission check of its own.
///
/// `nest_url` is `Some(base_url)` when the recipient lives on a foreign nest
/// (the Welcome relays there); `None` ⇒ same-nest. The channel log always lives
/// on THIS (the organizer's) nest — a cross-nest recipient drains the iMIP
/// application message via the membership-gated `fauna.federation.channel.fetch`
/// relay (`direct-messages.md` § Technical Flow — Cross-Nest). Returns
/// `(inbox_id, seq)`: the Welcome inbox row id (0 cross-nest) + the iMIP message
/// sequence.
pub(crate) async fn deliver_scheduling_as_organizer(
    state: &Arc<AppState>,
    organizer: &[u8; 32],
    recipient_actor_id: &str,
    channel_id_hex: &str,
    nest_url: Option<String>,
    welcome_bytes: Vec<u8>,
    app_envelope: Vec<u8>,
) -> Result<(i64, i64), RpcError> {
    // 1) Deliver the sealed Welcome (tagged Scheduling) to the recipient — the
    //    same-nest push or the cross-nest federation relay, REUSING the welcome
    //    core (never a replicated relay).
    let welcome_req = WelcomeDeliverRequest {
        recipient_actor_id: recipient_actor_id.to_string(),
        channel_id: channel_id_hex.to_string(),
        welcome_bytes,
        kind: WelcomeKind::Scheduling,
        nest_url,
        extra: std::collections::BTreeMap::new(),
    };
    // The organizer is the "sharer" for a scheduling welcome — but a scheduling
    // channel carries no folder claim, so `welcome_deliver_core` stamps no
    // `shared_by`. `SchedulingGateway` is the rail that earns this delivery its
    // exemption from the attendee's inbox mode: the invite is the CalDAV
    // gateway's designed reach, and the exemption belongs to THIS call site —
    // not to any label a client could put on a Welcome of its
    // own.
    let welcome = welcome_deliver_core(
        state,
        organizer,
        &welcome_req,
        WelcomeRail::SchedulingGateway,
    )
    .await?;

    // 2) Post the iMIP as the channel's first application message AS THE
    //    ORGANIZER (membership/segments bind to the organizer, not the MDA),
    //    skipping the chat anti-spam path.
    let channel_id = parse_channel_id(channel_id_hex)?;
    let send = channel_send_core(
        state,
        organizer,
        &channel_id,
        Bytes::from(app_envelope),
        false,
        // A one-off server-fanned scheduling iMIP is never a multi-device MLS
        // commit, so it carries no device-owned-epoch precondition — and
        // names no attachment blobs.
        None,
        &[],
    )
    .await?;

    Ok((welcome.inbox_id, send.seq))
}

// ── fauna.conversations.blob.write_token.get ───────────────────────────

/// `fauna.conversations.blob.write_token.get` — a **foreign member**'s own nest
/// relays a short-lived byte-plane write token from the channel's home nest
/// (`fauna.federation.conversation.write_token.mint`), so the member's sealed
/// attachment bytes can be POSTed DIRECT to the home nest — where the record
/// and its `attachment_refs` already rest, which is what pins them
/// (`conversation-rooms.md` § The home nest → *Attachment bytes*). Always a
/// relay: a same-nest member uploads under its own session bearer. The home
/// nest applies the structural foreign-member gate before minting; S5 maps an
/// old home nest's `unauthenticated` to the shared typed `peer_nest_outdated`.
/// The blob twin of `channel.send_remote`, and the rail twin of
/// `fauna.folders.write_token.get`.
fn blob_write_token_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET).await?;
            let req: ConversationBlobWriteTokenGetRequest = decode(&payload).map_err(malformed)?;
            parse_channel_id(&req.channel_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the channel's home nest (a same-nest member uploads under its own session)",
                ));
            }
            match crate::federation_pool::originate_conversation_write_token_mint(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.channel_id,
            )
            .await
            {
                Ok(Ok((token, expires_at))) => encode_reply(&ConversationBlobWriteTokenGetReply {
                    token,
                    expires_at,
                    extra: std::collections::BTreeMap::new(),
                }),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest attachment write-token mint",
                )),
                Err(pool_err) => {
                    tracing::error!("federation conversation write_token mint relay: {pool_err}");
                    Err(internal("federation mint failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.channel.fetch ──────────────────────────

fn channel_fetch_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.channel.fetch").await?;
            let req: ChannelFetchRequest = decode(&payload).map_err(malformed)?;
            let channel_id = parse_channel_id(&req.channel_id)?;
            // `limit <= 0` = the full page (the shipped clients' whole-tail
            // poll sends 0) — see `segments::effective_fetch_limit`.
            let limit = crate::segments::effective_fetch_limit(req.limit);

            // Spec Y2 cross-nest relay: when the caller names the channel's foreign
            // home nest (the group creator's nest, learned from the Welcome's
            // `nest_url`), this home nest relays the pull there over the federation
            // channel. The peer authorizes the caller's channel membership bound to
            // this nest's `nest_id` (`direct-messages.md` § Technical Flow —
            // Cross-Nest, step 3). The channel log lives on the peer, so the local
            // serving gate / roster / segments below are the same-nest path only.
            if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                // The id→handle ruling (`federation.md` § Cross-nest shared
                // folders + channel append, the id→handle bullet): this nest
                // is the caller's home and the authority for handles at its
                // domain, so it volunteers the caller's `handle@domain` on
                // the drain it originates — joined from its own `users` row,
                // never taken from the client — and the channel's home nest
                // records it for its roster read after binding the domain to
                // this nest's key. An unset handle announces nothing.
                let announce = state
                    .db
                    .get_handle(&actor_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|h| !h.is_empty())
                    .and_then(|h| state.handle_domain_if_set().map(|d| (h, d)));
                let relayed = crate::federation_pool::originate_channel_fetch(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &hex::encode(actor_id),
                    &req.channel_id,
                    req.after,
                    limit,
                    announce,
                )
                .await
                .map_err(|e| {
                    tracing::error!("federation channel fetch (channel): {e}");
                    internal("federation fetch failed")
                })?;
                // A community room's verdicts ride the relayed page: the room's
                // home nest filled them for the member this drain names, by the
                // same floor gate its own members' reads pass
                // (`page_verdicts`), and this nest forwards them to that member
                // and stores none (`community-rooms.md` § The three classes →
                // *What the home nest does with its read*; `federation.md`, the
                // `channel.fetch` row). Empty where that gate does not pass
                // (another class, a member not live on the floor), for a
                // message no labeler labelled, and beside a withheld envelope.
                let messages: Vec<ChannelFetchEntry> = relayed
                    .into_iter()
                    .map(|m| ChannelFetchEntry {
                        seq: m.seq,
                        envelope: m.envelope,
                        legal_takedown: m.legal_takedown,
                        labels: m.labels,
                        scores: m.scores,
                        // The HOME nest's attestation, forwarded unchanged —
                        // this nest authenticated nobody on that send.
                        author: m.author,
                        extra: Default::default(),
                    })
                    .collect();
                return encode_reply(&ChannelFetchReply {
                    messages,
                    extra: std::collections::BTreeMap::new(),
                });
            }

            // Gate 1 (Plan 9): a pure-backup destination for this channel cannot
            // serve history — refuse before touching the roster or segments.
            require_local_conv_serving(&state, &channel_id).await?;

            // Auto-register on read (parity with channel_routes). Gated so a
            // claimed folder channel's owner-managed roster can't be
            // self-inserted via a read.
            register_actor_channel_gated(
                &state.db,
                &channel_id,
                &actor_id,
                &actor_id,
                "register_actor_channel on read",
            )
            .await;

            let rows = crate::segments::conv::read_after_seq(
                &state.conv_segments,
                &state.db,
                &channel_id,
                req.after,
                limit,
            )
            .await
            .map_err(|e| {
                tracing::error!("conv read error: {e}");
                internal("storage error")
            })?;

            // Page byte-budget (`transport.md` § Max frame): the assembled
            // reply must fit one 2 MiB WS frame. Close the page early before
            // the record that would overflow — never skip (the client walks
            // by a contiguous cursor; a record served out of order would be
            // silently lost to it). A head record alone over the budget
            // freezes the page loudly; the shorter page is otherwise
            // transparent (the poll fetches again from its cursor).
            let (rows, rest) =
                crate::segments::take_page_within_budget(rows, |(_, envelope, _)| envelope.len());
            if rows.is_empty()
                && let Some((seq, envelope, _)) = rest.first()
            {
                tracing::error!(
                    channel = %hex::encode(channel_id),
                    seq,
                    record_bytes = envelope.len(),
                    "conv fetch: a single stored record exceeds the WS frame \
                     budget — this channel's drain cannot advance past it \
                     (transport.md § Max frame; remedy is a targeted heal, \
                     never a skip)"
                );
            }

            let verdicts = page_verdicts(&state, &channel_id, &actor_id, &rows).await;
            let mut authors = page_authors(&state, &channel_id, req.after, &rows).await;
            let messages: Vec<ChannelFetchEntry> = rows
                .into_iter()
                .zip(verdicts)
                .map(|((seq, envelope, legal_ref), derived)| ChannelFetchEntry {
                    seq,
                    envelope,
                    legal_takedown: LegalTakedownMarker::from_ref(legal_ref),
                    labels: derived.labels,
                    scores: derived.scores,
                    author: authors.remove(&seq),
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&ChannelFetchReply {
                messages,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

/// What each row of a page of `channel_id`'s log carries beside its envelope
/// for `reader`, aligned with `rows`: the bus rows a **community room's** named
/// labelers derived ([`room_bus_for_member`]'s gate), and nothing beside a
/// withheld envelope — a verdict is derived from the content, so it goes
/// where the content goes.
///
/// The one home for both rules, shared by the two doors a page is served
/// through — the same-nest `channel.fetch`, whose reader is the caller, and
/// the room home's `fauna.federation.channel.fetch`, whose reader is the
/// member the relay names (bound to the calling nest by
/// `federation_handlers::require_foreign_member`), never the peer — so a
/// member homed elsewhere reads exactly what a member homed here reads
/// (`community-rooms.md` § The three classes → *What the home nest does with
/// its read*, purpose 2).
pub(crate) async fn page_verdicts(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    reader: &[u8; 32],
    rows: &[(i64, Vec<u8>, Option<String>)],
) -> Vec<crate::db::room_labels::RoomBus> {
    let seqs: Vec<i64> = rows.iter().map(|(seq, _, _)| *seq).collect();
    let mut bus = room_bus_for_member(state, channel_id, reader, &seqs).await;
    rows.iter()
        .map(|(seq, _, legal_ref)| {
            if legal_ref.is_some() {
                crate::db::room_labels::RoomBus::default()
            } else {
                bus.remove(seq).unwrap_or_default()
            }
        })
        .collect()
}

/// What a page of a **community room's** log carries beside its envelopes for
/// `actor_id`: the bus rows the room's named labelers derived, keyed by seq
/// (`community-rooms.md` § The three classes → *What the home nest does with
/// its read*, purpose 2) — and nothing, for anyone but a live floor member.
///
/// The envelope read is deliberately not floor-gated, because the bytes are
/// sealed. A verdict is not sealed: it was derived from the plaintext, so it is
/// served exactly where the room's floor says a member stands, rank aside. For
/// every channel that is not a ceremony-born room this is one missed room
/// lookup and an empty answer.
async fn room_bus_for_member(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    actor_id: &[u8; 32],
    seqs: &[i64],
) -> std::collections::BTreeMap<i64, crate::db::room_labels::RoomBus> {
    let empty = std::collections::BTreeMap::new;
    if seqs.is_empty() || !is_live_floor_member(state, channel_id, actor_id).await {
        return empty();
    }
    state
        .db
        .room_message_bus(channel_id, seqs)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("room labels: bus read failed on fetch: {e}");
            empty()
        })
}

/// **The verdict gate**: whether `actor_id` may read what `room_id`'s labelers
/// derived — the room is a community room (its floor is the membership
/// authority) and the actor is a live member of it, of any rank. One home
/// for the two checks, so the message read (`channel.fetch`) and the post
/// read (`fauna.posts.room_labels`) can never drift apart on who a verdict
/// reaches. A read failure answers `false`: silence, never a leak.
pub(crate) async fn is_live_floor_member(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    actor_id: &[u8; 32],
) -> bool {
    match state.db.get_room(room_id).await {
        Ok(Some(room)) if room.is_floor_authoritative() => {}
        Ok(_) => return false,
        Err(e) => {
            tracing::warn!("room labels: room read failed on a verdict read: {e}");
            return false;
        }
    }
    match state.db.is_room_member(room_id, actor_id).await {
        Ok(member) => member,
        Err(e) => {
            tracing::warn!("room labels: floor read failed on a verdict read: {e}");
            false
        }
    }
}

// ── fauna.conversations.channel.list_for_actor ─────────────────

fn channel_list_for_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.channel.list_for_actor",
            )
            .await?;
            let _req: ChannelListForActorRequest = decode(&payload).map_err(malformed)?;

            let channels = state.db.list_actor_channels(&actor_id).await.map_err(|e| {
                tracing::error!("list_actor_channels error: {e}");
                internal("storage error")
            })?;
            let hex_ids: Vec<String> = channels.iter().map(hex::encode).collect();

            encode_reply(&ChannelListForActorReply {
                channels: hex_ids,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.channel.actors ─────────────────────────

/// The inverse of [`channel_list_for_actor_handler`]: which actors sit on one
/// channel's routing roster — the **union** of `actor_channels` (this nest's
/// own users, written at Welcome delivery) and `channel_foreign_members`
/// (cross-nest members, written when this nest relayed their Welcome to their
/// home nest). On the channel's home nest that union is the whole roster.
///
/// **Why it exists.** The roster row is written at Welcome delivery, so its
/// *absence* for an actor who is nonetheless an MLS leaf is the signal that a
/// crash landed between the Add commit's merge and `welcome.deliver` — the
/// **phantom leaf**. It is the discriminator `add_participant`'s heal needs to
/// tell a healthy member (leave alone) from a phantom (evict + re-admit), the
/// chat twin of the folder owner's `actor_members_list`
/// (`mls-group-key-material.md` § M2 *Admitting a member*). Without the
/// foreign half of the union, a **healthy cross-nest member** read as absent
/// and the heal evicted + re-invited a working member on a duplicate add.
///
/// **Authz: member-scoped.** The caller must themselves be on the roster. This
/// leaks nothing a member doesn't already hold — every MLS member reads the
/// full membership off the group's ratchet tree — but it does keep a
/// non-member from probing who talks to whom. Unlike
/// [`channel_fetch_handler`] there is deliberately **no auto-register on
/// read**: a roster read must never be able to write the very row it reports
/// (it would make every phantom look healthy to the next caller).
fn channel_actors_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.channel.actors").await?;
            let req: ChannelActorsRequest = decode(&payload).map_err(malformed)?;
            let channel_id = parse_channel_id(&req.channel_id)?;

            let caller_is_member = state
                .db
                .is_actor_in_channel(&actor_id, &channel_id)
                .await
                .map_err(|e| {
                    tracing::error!("is_actor_in_channel error: {e}");
                    internal("storage error")
                })?;
            if !caller_is_member {
                return Err(forbidden("not a member of this channel"));
            }

            // Cross-nest members' rows live in `channel_foreign_members` (written
            // at Welcome-relay time), not `actor_channels` — the union read is what
            // keeps a healthy foreign member from reading as a phantom to the heal.
            let actors = state
                .db
                .list_channel_actors_union(&channel_id)
                .await
                .map_err(|e| {
                    tracing::error!("list_channel_actors_union error: {e}");
                    internal("storage error")
                })?;

            encode_reply(&ChannelActorsReply {
                actors: actors.iter().map(hex::encode).collect(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.keypackage.upload ──────────────────────

fn keypackage_upload_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.keypackage.upload").await?;
            let req: KeypackageUploadRequest = decode(&payload).map_err(malformed)?;

            let now = fauna_core::data::Timestamp::now_secs() as u64;
            // 30-day expiry matches `channel_routes::publish_key_packages`.
            let expires_at = now + 30 * 24 * 3600;

            let mut stored = 0u64;
            for pkg in &req.packages {
                // MLS-2 defense-in-depth: reject a
                // KeyPackage whose inner credential isn't this authenticated
                // uploader's identity (== its leaf signature key). Stops a
                // patched client publishing a KeyPackage that claims another
                // actor, so the nest never serves a forged-identity KP to a peer
                // building a group. Client-side admission is the necessary
                // defense; this is the belt at the upload choke point.
                fauna_mls::engine::verify_uploaded_key_package(pkg.as_ref(), &actor_id).map_err(
                    |e| {
                        // A CredentialBindingViolation is an active forge attempt:
                        // an authenticated client uploading a KeyPackage whose
                        // inner credential claims a *different* actor (MLS-2). Log
                        // it as a security signal; a parse/structure error is not
                        // an attack. The wire response is unchanged either way.
                        if matches!(e, fauna_mls::error::MlsError::CredentialBindingViolation(_)) {
                            tracing::warn!(
                                uploader = %hex::encode(actor_id),
                                "rejected forged-identity KeyPackage upload (MLS-2 credential binding): {e}"
                            );
                        }
                        invalid_params(&format!("invalid key package: {e}"))
                    },
                )?;

                let id = fauna_core::identity::random_hex(16);
                // Spec Y2: last-resort KPs are reusable (never consumed by
                // `take_key_package`); one-time KPs are the normal consumable pool.
                let put = if req.last_resort {
                    state
                        .db
                        .put_last_resort_key_package(&id, &actor_id, pkg.as_ref(), now, expires_at)
                        .await
                } else {
                    state
                        .db
                        .put_key_package(&id, &actor_id, pkg.as_ref(), now, expires_at)
                        .await
                };
                put.map_err(|e| {
                    tracing::error!("put_key_package error: {e}");
                    internal("storage error")
                })?;
                stored += 1;
            }

            encode_reply(&KeypackageUploadReply {
                stored,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.keypackage.fetch ───────────────────────

fn keypackage_fetch_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.keypackage.fetch").await?;
            let req: KeypackageFetchRequest = decode(&payload).map_err(malformed)?;
            let target = parse_actor_id(&req.actor_id)?;

            // Spec Y2 cross-nest relay: when the client names a foreign nest, the
            // home nest relays the fetch there over the long-lived federation
            // channel (the sole Fauna↔Fauna carrier since slice 5). Otherwise
            // same-nest. KP-fetch is destructive (`take_key_package`), so a
            // mid-call channel disconnect surfaces an error to the client (which
            // retries the user action) rather than re-fetching a second KP (§4.D).
            let key_package = match req.nest_url.as_deref().filter(|u| !u.is_empty()) {
                Some(peer_url) => crate::federation_pool::originate_keypackage_fetch(
                    &state.federation_pool,
                    &state,
                    peer_url,
                    &req.actor_id,
                )
                .await
                .map_err(|e| {
                    tracing::error!("federation keypackage fetch (channel): {e}");
                    internal("federation fetch failed")
                })?,
                None => state.db.take_key_package(&target).await.map_err(|e| {
                    tracing::error!("take_key_package error: {e}");
                    internal("storage error")
                })?,
            };

            encode_reply(&KeypackageFetchReply {
                key_package,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.keypackage.count ───────────────────────

fn keypackage_count_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.keypackage.count").await?;
            let req: KeypackageCountRequest = decode(&payload).map_err(malformed)?;
            let target = parse_actor_id(&req.actor_id)?;

            let count = state.db.count_key_packages(&target).await.map_err(|e| {
                tracing::error!("count_key_packages error: {e}");
                internal("storage error")
            })?;

            encode_reply(&KeypackageCountReply {
                count,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.welcome.deliver ────────────────────────

fn welcome_deliver_handler() -> RpcHandler {
    Box::new(|state, caller_id, payload| {
        Box::pin(async move {
            // Caller-class gate: the welcomer is an end-user chat sender, so
            // the bridges are denied. The admin is permitted only because it
            // inherits every User-class permission (`Admin ⊇ User`,
            // `bridge_method_allowlist::is_permitted`) — i.e. the admin acting
            // as their own Fauna user, not in any bridge capacity. The
            // recipient lives elsewhere — they're addressed via
            // `req.recipient_actor_id`.
            require_permission(&state, &caller_id, "fauna.conversations.welcome.deliver").await?;
            let req: WelcomeDeliverRequest = decode(&payload).map_err(malformed)?;
            encode_reply(&welcome_deliver_core(&state, &caller_id, &req, WelcomeRail::User).await?)
        })
    })
}

/// Which **call rail** a Welcome arrived on — a fact about the nest-side call
/// site, never anything a caller can put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WelcomeRail {
    /// `fauna.conversations.welcome.deliver` — a User-class caller delivering
    /// its own Welcome. Every field of that request, `kind` included, is the
    /// sender's own word about itself.
    User,
    /// The MDA CalDAV gateway's mailbox-less scheduling delivery
    /// ([`deliver_scheduling_as_organizer`]) — class-gated to `BridgeMda` and
    /// caller-scoped to the organizer before it ever reaches the core.
    SchedulingGateway,
}

/// Whether **nest state** exempts a same-nest first reach from the recipient's
/// inbox mode (`direct-messages.md` § Reach policy → § Scope).
///
/// Every other same-nest initiation consults the mode. There are two
/// exemptions, and each is a fact this nest holds — never a word the sender
/// picked:
///
/// - **The scheduling gateway's rail.** A calendar invite is the CalDAV
///   gateway's designed reach (`caldav-server.md` § Server-side
///   auto-schedule): it never surfaces as a chat thread, a knock-first
///   requirement would break invite interop, and its spam surface is the
///   gateway's to own. The **rail** is what is exempt; the `Scheduling`
///   *label* is not. It used to be — and a same-nest stranger who tagged its
///   own Welcome `Scheduling` skipped the mode, took the roster seat that
///   delivery registers, and then sent a `Dm` on the same channel, which the
///   seat made read as in-band traffic: `closed` defeated in two
///   calls.
/// - **The channel's folder claim naming the caller.** A stranger's share is
///   staged by the recipient-side pending-share gate (`folders.md`
///   § Sharing), its own ratified routing, so the mode must not pre-empt it.
///   But what makes a delivery a share is the `folder_channel_claims` row,
///   which `folders.share` writes to the owner BEFORE the client delivers the
///   Welcome (`share_set` / `share_set_add`) — first-binder-wins, and refused
///   outright on any roster-populated channel, so it is not a row a stranger
///   can forge onto somebody else's conversation.
///
/// `Unclaimed` and an unresolvable claim read both fall through to "not
/// exempt" — the conservative direction, and the same reading
/// [`is_the_claimant`] hands every other consumer.
///
/// Deliberately NOT keyed on `req.kind`: a sender's self-declaration must not
/// authorize itself — the rule the cross-nest `closed` gate has been
/// kind-blind under all along. A NEW `WelcomeKind` therefore arrives mode-gated
/// and has to earn its exemption in nest state, which is the safe direction to
/// fail.
fn welcome_mode_exemption(
    rail: WelcomeRail,
    folder_claim: FolderChannelClaim,
    caller_id: &[u8; 32],
) -> bool {
    match rail {
        WelcomeRail::SchedulingGateway => true,
        WelcomeRail::User => is_the_claimant(folder_claim, caller_id),
    }
}

/// The `fauna.conversations.welcome.deliver` body, minus the caller-class gate +
/// payload decode — shared between the User-facing [`welcome_deliver_handler`]
/// and the MDA server-side scheduling gateway
/// ([`deliver_scheduling_as_organizer`], which delivers a one-off
/// [`WelcomeKind::Scheduling`] Welcome). The recipient is addressed by
/// `req.recipient_actor_id`; the caller identity only gated permission above, so
/// the core needs no acting-actor argument. Handles both the same-nest push and
/// the cross-nest federation relay (`req.nest_url`).
///
/// `rail` names which of those two call sites this is. It is the gateway's only
/// authorization-bearing input, and it is deliberately an ARGUMENT rather than
/// anything read off `req`: see [`welcome_mode_exemption`].
pub(crate) async fn welcome_deliver_core(
    state: &Arc<AppState>,
    caller_id: &[u8; 32],
    req: &WelcomeDeliverRequest,
    rail: WelcomeRail,
) -> Result<WelcomeDeliverReply, RpcError> {
    let recipient = parse_actor_id(&req.recipient_actor_id)?;
    let channel_id = parse_channel_id(&req.channel_id)?;

    // ── The reach floor (`family-safety.md` § Guardian policy pillar 1) ──
    //
    // The MLS Welcome plane is a second, fully independent inbox-write path: it
    // pushes an inbox row, a push notification, and a channel-roster row. It used
    // to consult no contact edge and no guardian policy at all, on the premise —
    // stated a few lines below, now deleted — that "the recipient already shares
    // the group/DM with the sender, so there is nothing to gate". For a
    // `WelcomeKind::Dm` that is false: **a DM Welcome is the primitive that
    // *starts* a DM**, which is precisely the moment a reach policy exists to
    // mediate.
    //
    // Initiation vs. in-band traffic is decided from THIS nest's own roster, never
    // from the sender's `req.kind`: a recipient already on the channel is being
    // re-Welcomed (an idempotent retry, or a re-add), which is in-band and must
    // keep flowing; a recipient not on it is being reached for the first time.
    // Branching on `kind` instead would be the same defect as trusting a post's
    // `schema` (F2) — a sender's self-declaration authorizing itself.
    let recipient_established = state
        .db
        .is_actor_in_channel(&recipient, &channel_id)
        .await
        .map_err(|e| {
            tracing::error!("welcome is_actor_in_channel error: {e}");
            internal("storage error")
        })?;

    // ONE claim read for the whole handler, shared by all four consumers below
    // — the inbox-mode exemption, the share-admit spend, the cross-nest relay's
    // foreign-member grant, and the same-nest roster register (the
    // shape: one round trip, not one per gate). The claim is this handler's
    // single answer to "is this channel a folder set's?", which is why every
    // one of those decisions reads it instead of `req.kind`.
    // Read here rather than beside the first consumer because the two
    // roster-add sites live on OPPOSITE sides of the `recipient_established`
    // branch: the share-admit read used to sit inside `if !recipient_established`
    // below, which the relay branch — reached by established recipients too —
    // never enters. Hoisting also makes every path cheaper than before: the
    // non-established same-nest path read the claim twice (share-admit, then the
    // tail register), and now reads it once.
    let folder_claim = folder_channel_claim(&state.db, &channel_id).await;

    // What this nest knows about the delivery, read once and used by every
    // decision below that used to read `req.kind` instead.
    //
    // `mode_exempt` is the authorization half (which first reaches skip the
    // recipient's inbox mode); `is_folder_plane` is the routing half (which
    // channel this Welcome lands on as a folder-set share rather than a
    // conversation). They are separate questions and a claimed channel answers
    // both: only its claimant may reach past the mode, but EVERY Welcome
    // delivered onto it is a folder delivery, whatever its label says. That
    // second half is what stops a stranger's ratified folder share — which
    // does reach a `closed` recipient, and does seat them — from being
    // relabelled `Dm` on the next call and becoming a chat thread.
    //
    // On an UNCLAIMED channel the label still picks the channel type, because
    // there it only routes: it bought no exemption, so the reach floor and the
    // mode have already authorized the delivery by the time it is read.
    let mode_exempt = welcome_mode_exemption(rail, folder_claim, caller_id);
    let is_folder_plane = matches!(folder_claim, FolderChannelClaim::Claimed(_))
        || matches!(req.kind, WelcomeKind::Folder { .. });

    // The claimed folder's own row, resolved once for the whole handler: the
    // claim above names only the claimant, and the residency refusal below, the
    // display `set_name` and its seal all read the row. Best-effort — a resolve
    // failure must never fail an otherwise-good delivery.
    let claimed_fs = if is_folder_plane {
        crate::federation_handlers::claimed_folder_for_channel(state, &channel_id)
            .await
            .ok()
    } else {
        None
    };

    // The interim cross-nest refusal, the member-moves-second direction
    // (`file-sync.md` § Relay serving → *Until that leg is built, the pair is
    // refused*; the folder update handler's residency arm holds the other): a
    // metadata-only folder keeps no bytes on this nest and relay serving has no
    // cross-nest leg yet, so a member on another nest could read nothing.
    // Refused here — ahead of the reach gates' unrefundable share-admit spend
    // and of the relay itself, so the peer nest is never handed a Welcome the
    // recipient cannot use. A recipient already on the channel's foreign roster
    // is an existing pair and is left as it is (a failed roster read refuses).
    if req.nest_url.as_deref().is_some_and(|u| !u.is_empty())
        && claimed_fs
            .as_ref()
            .is_some_and(|fs| crate::folder_handlers::residency_of(fs) == "metadata_only")
        && !state
            .db
            .foreign_channel_member_exists(&channel_id, &recipient)
            .await
            .unwrap_or(false)
    {
        return Err(crate::rpc_errors::invalid_request_ns(
            "conversations",
            "this folder keeps its content off the nest, and a member on another nest \
             cannot reach it yet; set the folder's residency back to full first",
        ));
    }

    if !recipient_established {
        // Outbound: a supervised sender may only initiate to an approved
        // contact. The slice-3a outbound gate covered `fauna.inbox.send` alone,
        // so a ward could fetch any actor's key package (`keypackage.fetch` is
        // User-class) and open a DM with a stranger straight through here.
        outbound_reach_gate(state, caller_id, &recipient).await?;

        // Inbound: the recipient's own floor. A refusal is deliberately the
        // generic `forbidden` — telling an arbitrary sender "this account is
        // supervised" would disclose the ward's status to the whole network.
        // The floor + inbox-mode gate, shared with the room-invite door
        // (`conversation_initiation_reach_gate`). Same-nest deliveries only
        // (`nest_url: None`) consult the mode — a remote recipient's mode is
        // their own nest's fact, enforced at its federation ingest — and only
        // when nest state does not exempt this delivery.
        conversation_initiation_reach_gate(
            state,
            &recipient,
            caller_id,
            req.nest_url.is_none() && !mode_exempt,
            ArrivalOrigin::Local,
        )
        .await?;

        // ── p2p-share.member.admit — rule 8's fan-out chokepoint ─────────
        //
        // (`p2p.md` § Wormability walk — the share leg, rule 8;
        // `dynamic-features.md` § Charter members: "new member admissions to
        // shared sets".) A first-reach Welcome onto a channel the CALLER
        // claims as a folder share is the nest-arbitrated act of admitting a
        // new member to a shared set — and this function is the ONE
        // production door through which such a member lands (the same-nest
        // roster register and the cross-nest federation forward both fork
        // below), so the counterparty bound composed here holds against
        // non-conforming clients. The discriminator is the claim row — nest
        // state — never `req.kind`, because a sender's self-declaration must
        // not route around its own gate (the F2 lesson above): a client is
        // free to tag a Welcome onto a claimed channel with any kind it likes,
        // and the kind it picks may not be the plane it lands
        // on. Established recipients
        // never reach this arm (in-band re-Welcomes: no gate, no spend); a
        // cross-nest recipient already on the foreign-member roster is
        // established the same way, checked here, so an idempotent
        // re-delivery cannot re-spend the unrefundable counterparty quota.
        // The gate runs after the reach floor (a reach refusal must not cost
        // quota) and before every write. Fails closed on a claim-read error —
        // `is_the_claimant` is false for both `Unclaimed` and `Unknown`, so an
        // unresolvable claim state never admits a share (this site was
        // already fail-closed here before row 433; `folder_channel_claim`'s
        // own gate is what changed to match it).
        let share_admit_by_claimant = is_the_claimant(folder_claim, caller_id);
        if share_admit_by_claimant {
            let foreign_established = state
                .db
                .foreign_channel_member_exists(&channel_id, &recipient)
                .await
                .unwrap_or_else(|e| {
                    // Fail toward gating (the spend over-counts — the only
                    // direction an unrefundable bound tolerates).
                    tracing::error!("welcome share-admit foreign check: {e}");
                    false
                });
            if !foreign_established {
                crate::feature_gate::gate(
                    state,
                    caller_id,
                    &fauna_core::feature_gate::GateOp {
                        feature: fauna_core::feature_gate::GatedFeature::P2pShare,
                        surface: fauna_core::feature_gate::SURFACE_P2P_SHARE_MEMBER_ADMIT,
                        new_counterparties: 1,
                        magnitude: 0,
                    },
                )
                .await?;
            }
        }
    }

    // The sharing actor, nest-stamped from the authenticated caller — populated
    // ONLY for folder-plane welcomes, whose recipient-side contact gate reads it
    // to decide auto-join vs. knock (folders.md § Sharing). Same-nest is
    // authoritative (the caller IS the sharer, unspoofable). Conversation-plane
    // welcomes carry no sharer stamp. (Cross-nest sharing intentionally
    // ALWAYS knocks — ratified 2026-07-02, folders.md § Sharing: a relayed
    // folder welcome arrives with `shared_by = None` and the recipient gate
    // treats it as a stranger knock, manually accepted. The receiving nest
    // can't safely bind an asserted cross-nest sharer to the verified origin
    // nest — the `contacts` model records no home-nest — so auto-gating a
    // cross-nest share would be spoofable; always-knock is the safe, intended
    // behavior, not a gap.)
    let shared_by = is_folder_plane.then(|| hex::encode(caller_id));

    // The set's name, nest-resolved from the claimed set's own row (never
    // sender-asserted): this nest is the set's home — `share_core` claims the
    // channel to the owner before delivering the Welcome — so the claimed row
    // is authoritative. Load-bearing for a CROSS-NEST recipient, whose own nest
    // holds no row for the set (the pending share and the accept-time
    // foreign-set account-plane record would otherwise be name-less; Phase 2
    // client read-side). Best-effort + display-only: a resolve failure must
    // never fail an otherwise-good delivery.
    // The claimed row (`claimed_fs`, resolved once above) carries the display
    // `set_name` and its seal.
    // A sealed set's row rests no name (the empty sentinel), so its Welcome
    // carries the sealed pair alone — never `Some("")`, and never a plaintext
    // the row itself does not rest (`path-sealing.md` § the set-name plane).
    let set_name = claimed_fs
        .as_ref()
        .map(|fs| fs.name.clone())
        .filter(|name| !name.is_empty());
    // The set-name seal + salt pair, resolved from the same claimed row —
    // path-sealing S5c-2. `zip` so a set whose name was never stamped ships
    // neither half (a seal without its salt is unrenderable post-scrub).
    let (set_name_sealed, set_name_hash) = claimed_fs
        .as_ref()
        .and_then(|fs| fs.name_sealed.clone().zip(fs.name_hash.clone()))
        .map(|(sealed, hash)| {
            (
                Some(serde_bytes::ByteBuf::from(sealed)),
                Some(serde_bytes::ByteBuf::from(hash)),
            )
        })
        .unwrap_or((None, None));
    // This nest IS the set's home for a folder relay (`share_core` claimed the
    // channel here), so its own deployment identity is the byte-plane trust root
    // the recipient's agent graduates a pin against (`security.md` § Transport
    // trust). Never peer-asserted; gated to folder shares like `set_name`.
    let home_nest_actor_id = if shared_by.is_some() {
        Some(hex::encode(state.nest_identity.public_key_bytes()))
    } else {
        None
    };

    // The recipient's access grant on the set, resolved from THIS nest's own
    // `folder_member_access` row — never sender-asserted, exactly like
    // `set_name` above, and authoritative for the same reason: `share_core`
    // claims the channel to the owner and writes the share-time grant before
    // the client delivers the Welcome, so this nest holds the row. Load-bearing
    // for a CROSS-NEST recipient, whose own nest holds no role row for the set
    // and therefore cannot discover its own grant any other way
    // (`federation.md` § Cross-nest → Recipient-side access discovery).
    //
    // Advisory-for-UI on the receiving side: it decides whether that client
    // OFFERS a folder binding. Enforcement stays here, on the write kinds'
    // `require_foreign_writer` / `writable_folder` gates reading this same
    // row. Best-effort like `set_name` — a missing row (no grant yet ⇒ the
    // reader default) must never fail an otherwise-good delivery.
    let access = if shared_by.is_some() {
        state
            .db
            .get_folder_member_role(&channel_id, &recipient)
            .await
            .ok()
            .flatten()
            .map(|role| role.access)
    } else {
        None
    };

    // ── The delivery's plane + channel metadata, resolved once ────────
    //
    // Shared by the cross-nest relay and the same-nest push below, which used
    // to carry two copies of one `req.kind` match. The conversation arms stay
    // exhaustive — no wildcard — so a NEW kind must state its channel type
    // here at compile time.
    //
    // A folder-claimed channel takes its type from the CLAIM, not the label:
    // the claim row is what makes a channel a folder set's, so a Welcome
    // delivered onto it is a share however the sender tagged it. Its group id
    // comes from that same claimed row when the label carried none — the set's
    // own `mls_group_id`, which is what the channel id was derived from in the
    // first place.
    let (channel_type, group_id): (String, Option<String>) = if is_folder_plane {
        let group_id = match &req.kind {
            WelcomeKind::Folder { group_id } => Some(group_id.clone()),
            _ => claimed_fs
                .as_ref()
                .and_then(|fs| fs.mls_group_id.as_deref())
                .map(hex::encode),
        };
        ("folder".to_string(), group_id)
    } else {
        match &req.kind {
            WelcomeKind::Dm => ("dm".to_string(), None),
            WelcomeKind::Group { group_id } => ("group".to_string(), Some(group_id.clone())),
            // A CalDAV scheduling iMIP delivery (caldav-server.md
            // § Server-side auto-schedule). 1:1 like a Dm (no group_id); the
            // recipient routes the channel to calendar-apply, not chat UI.
            WelcomeKind::Scheduling => ("scheduling".to_string(), None),
            // Unreachable — a `Folder` label is folder-plane by construction
            // above — but spelled out instead of wildcarded so the match stays
            // exhaustive over the kind.
            WelcomeKind::Folder { group_id } => ("folder".to_string(), Some(group_id.clone())),
        }
    };

    {
        // Spec Y2 cross-nest relay: when the recipient is on a foreign nest,
        // the home nest relays the Welcome there over the federation channel
        // (the sole Fauna↔Fauna carrier since slice 5). No local inbox row
        // exists on the originating side, so the reply carries `inbox_id = 0`
        // (the peer's own inbox id is its local detail). Welcome delivery is
        // idempotent (inbox dedup), so a dropped channel is re-dialed + resent.
        if let Some(peer_url) = req.nest_url.as_deref().filter(|u| !u.is_empty()) {
            // Claim-refreshed identity domain, not the `node.domain` boot seed:
            // a provisioned box boots domainless and would hand the peer no
            // origin nest URL at all until a restart.
            let origin_nest_url = state.handle_domain_if_set().map(|d| format!("https://{d}"));
            // The cross-nest owner label (`federation.md` § Cross-nest shared
            // folders + channel append → *The cross-nest owner label*,
            // *Carrier*): this nest is the set's home and the caller is its
            // owner (`share_core` owner-gates the share), so it volunteers the
            // owner's handle + its own domain, joined from its own rows —
            // never client-asserted. Folder shares only, like `access`.
            let (owner_handle, owner_domain) = if shared_by.is_some() {
                crate::federation_handlers::owner_label_stamp(state, caller_id).await
            } else {
                (None, None)
            };
            crate::federation_pool::originate_welcome_deliver(
                &state.federation_pool,
                state,
                peer_url,
                crate::federation_handlers::FedWelcomeDeliverRequest {
                    recipient_actor_id: req.recipient_actor_id.clone(),
                    channel_id: Some(req.channel_id.clone()),
                    welcome_bytes: req.welcome_bytes.clone(),
                    channel_type: Some(channel_type.clone()),
                    group_id: group_id.clone(),
                    origin_nest_url: origin_nest_url.clone(),
                    set_name: set_name.clone(),
                    set_name_sealed: set_name_sealed.clone(),
                    set_name_hash: set_name_hash.clone(),
                    access: access.clone(),
                    home_nest_actor_id: home_nest_actor_id.clone(),
                    // The folder's residency, off the claimed row beside
                    // `access` — what seeds the recipient's foreign record
                    // (`federation.md` § … *Relay serving across nests*).
                    residency: claimed_fs
                        .as_ref()
                        .and_then(crate::federation_handlers::residency_stamp),
                    owner_handle,
                    owner_domain,
                },
            )
            .await
            .map_err(|e| {
                tracing::error!("federation welcome delivery (channel): {e}");
                internal("federation welcome delivery failed")
            })?;

            // Record the foreign recipient as a member of this channel (whose
            // home is THIS nest) bound to its home nest, so a later
            // `fauna.federation.channel.fetch` from that nest can be authorized
            // to pull the channel's application messages (`direct-messages.md`
            // § Technical Flow — Cross-Nest, step 3). Best-effort: the home
            // `nest_id` resolves from the relay `peer_url` (already cached by the
            // welcome dial just above). A failure here only means the recipient
            // can't yet pull cross-nest — the Welcome still delivered.
            //
            // Claimant-gated, the relay-side twin of the same-nest register at
            // the tail of this function: `channel_foreign_members`
            // is the OTHER half of the one channel roster (`federation.md`
            // § Cross-nest shared folders + channel append), and the row it
            // writes IS an authorization grant — `require_foreign_member` serves
            // the peer `channel.fetch` (the ciphertext stream) and
            // `channel.actors` (the roster in the clear) off it, and the push
            // fan-out dials every home nest listed in it. Ungated, any member of
            // a claimed folder channel could relay a Welcome to a puppet account
            // on a nest of their choosing and hand that nest the folder's
            // ciphertext stream, its full roster, and a push dial — owner-only
            // share bypassed in the metadata plane, with MLS confidentiality the
            // only thing left holding. `Unclaimed` stays permissive (a first DM
            // Welcome's caller is on no roster yet — requiring caller standing
            // would break DM initiation, and no folder is bound to such a
            // channel); `Unknown` refuses like a real claimant would.
            // The gate runs BEFORE the `resolve_peer_nest_id` dial: a refused
            // grant should cost no peer round trip. Still best-effort AFTER the
            // gate, exactly as before.
            //
            // `claim_permits` settles who may write a grant; it does NOT settle
            // who may move one that already exists, and on this rail those are
            // different sets — see [`may_rebind_foreign_member`].
            // The writer applies the narrower verdict to its conflict arm only,
            // so the permissive `Unclaimed` arm still reaches every INSERT.
            if claim_permits(folder_claim, caller_id) {
                let rebind_power =
                    may_rebind_foreign_member(state, folder_claim, &channel_id, caller_id).await;
                match state.federation_pool.resolve_peer_nest_id(peer_url).await {
                    Ok(home_nest_id) => {
                        if let Err(e) = state
                            .db
                            .register_foreign_channel_member(
                                &channel_id,
                                &recipient,
                                &home_nest_id,
                                Some(peer_url),
                                rebind_power,
                            )
                            .await
                        {
                            tracing::warn!("register_foreign_channel_member: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("resolve home nest_id for foreign channel member: {e}")
                    }
                }
            }

            return Ok(WelcomeDeliverReply {
                inbox_id: 0,
                extra: std::collections::BTreeMap::new(),
            });
        }

        let body = Bytes::from(req.welcome_bytes.clone());

        // Resolve the sharer's handle for the recipient's "Shared by ‹handle›"
        // surface (folders.md § Sharing). This is the SAME-NEST branch (a
        // cross-nest relay took the early return above with `shared_by = None`),
        // so the caller is a local `users` row; resolve only when a sharer was
        // stamped (folder welcomes). Cosmetic + best-effort: a lookup error or
        // a handle-less caller leaves it `None` and the recipient falls back to
        // the shortened `shared_by` hex — it must never fail an otherwise-good
        // delivery. Mirrors the owner_handle resolve in `folder_handlers::list`
        // (`get_handle` → filter-empty). Only same-nest Folder welcomes stamp
        // `shared_by`, so DM/group/scheduling skip the DB round-trip.
        let shared_by_handle = if shared_by.is_some() {
            match state.db.get_handle(caller_id).await {
                Ok(h) => h.filter(|h| !h.is_empty()),
                Err(e) => {
                    tracing::warn!("resolve shared_by handle for folder welcome: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Same-nest Welcome → canonical DAG-CBOR inbox envelope (layer 1).
        // The envelope carries the channel metadata (id / type / group) that
        // previously rode ONLY on the best-effort push event — so the durable
        // drain backstop fully recovers a missed push (same-nest welcomes used
        // to store raw bytes, losing this metadata). `nest_url` is None
        // (same nest). The envelope is what gets stored, blob and inline.
        let inbox_bytes = Bytes::from(
            fauna_protocol::inbox::InboxEnvelope::welcome(&fauna_protocol::inbox::WelcomeInbox {
                welcome_bytes: body.to_vec(),
                channel_id: Some(req.channel_id.clone()),
                nest_url: None,
                channel_type: Some(channel_type.clone()),
                group_id: group_id.clone(),
                shared_by: shared_by.clone(),
                shared_by_handle: shared_by_handle.clone(),
                // Same-nest: a bare handle means a local `users` row — the
                // pair's domain half is stamped only on a verified cross-nest
                // relay (`federation.md` § … *The cross-nest owner label*).
                shared_by_domain: None,
                set_name: set_name.clone(),
                // Uniform stamp, same reasoning as `set_name` — a same-nest
                // recipient could resolve the pair from `fauna.folders.list`
                // once its roster row lands (share_core writes it before the
                // Welcome delivers), but one envelope shape serving both
                // planes is simpler than branching. Path-sealing S5c-2.
                set_name_sealed: set_name_sealed.clone(),
                set_name_hash: set_name_hash.clone(),
                // Uniform stamp — a same-nest recipient could resolve its own
                // grant from `fauna.folders.list` (the nest projects `access`
                // onto the member summary), so this is redundant there rather
                // than load-bearing. Stamped anyway so ONE envelope shape
                // serves both planes and the accept path never branches.
                access: access.clone(),
                // Same-nest (`nest_url: None`) → no foreign byte plane and the
                // recipient reads its cadence from `fauna.folders.list`, so the
                // cross-nest trust root + cadence are not carried here.
                home_nest_actor_id: None,
                residency: None,
                extra: Default::default(),
            })
            .and_then(|e| e.to_canonical_bytes())
            .map_err(|e| {
                tracing::error!("welcome inbox envelope encode error: {e}");
                internal("storage error")
            })?,
        );

        // Leg parity with the federation Welcome's F10 and `inbox.deliver`: under
        // enforced tier quotas the envelope is checked against, and charged to,
        // the recipient's inbox quota — checked before the spill, so a refused
        // Welcome stores nothing. Unenforced, it lands uncharged, and its link
        // records that, so its ack refunds nothing.
        let enforce_quota = *state.enforce_tier_quotas.read().await;
        if enforce_quota && let Err(e) = state.db.check_quota(&recipient, inbox_bytes.len()).await {
            return Err(forbidden(&format!("inbox quota: {e}")));
        }

        let blob_hash_welcome = if let Some(ps) = &state.payload_store {
            match ps.store(&inbox_bytes).await {
                Ok((_, hash)) => hash.map(|h| h.digest()),
                Err(e) => {
                    tracing::error!("welcome payload_store error: {e}");
                    return Err(internal("payload_store error"));
                }
            }
        } else {
            None
        };

        let pushed = if enforce_quota {
            state
                .db
                .push_inbox_with_quota(&recipient, &inbox_bytes, blob_hash_welcome.as_ref())
                .await
        } else {
            state
                .db
                .push_inbox(&recipient, &inbox_bytes, blob_hash_welcome.as_ref())
                .await
        };
        let inbox_id = pushed.map_err(|e| {
            tracing::error!("welcome push_inbox error: {e}");
            internal("storage error")
        })?;

        state.ws.notify_push(
            &recipient,
            fauna_protocol::PushEvent::Welcome(fauna_protocol::push_events::WelcomePayload {
                welcome_bytes: body.to_vec(),
                channel_id: Some(req.channel_id.clone()),
                nest_url: None,
                channel_type: Some(channel_type),
                group_id,
                shared_by,
                shared_by_handle,
                shared_by_domain: None,
                set_name,
                set_name_sealed,
                set_name_hash,
                access,
                home_nest_actor_id: None,
                residency: None,
                extra: std::collections::BTreeMap::new(),
            }),
        );

        // Best-effort APNS/FCM for offline recipients (mirrors the
        // HTTP twin). Errors must not block delivery.
        crate::push::dispatch_offline_push(
            state,
            &recipient,
            "Group invite",
            "You have been invited to a group",
            "/app/groups",
        );

        // Auto-register the recipient on the channel so subsequent
        // ciphertext fetches resolve. Mirrors the HTTP twin's
        // `register_actor_channel` on Welcome receipt. Gated on the
        // authenticated `caller_id`: on a claimed folder channel only the
        // claimant (the owning sharer) may register a recipient, so an evicted
        // member cannot self-address a Welcome to re-insert
        // themselves. A legit share passes — `share_core` claims the
        // channel to the owner before delivering the Welcome, so
        // `caller_id == claimant`. Unclaimed DM/group/scheduling channels are
        // unaffected.
        //
        // The claimant's Welcome onto its own folder channel is also the leg
        // that ends a retired member's seat: where the recipient succeeded a
        // member seated here, the same write retires that seat and carries its
        // grant (`writer-signed-change-records.md` ruling (8)(j)(2)). A
        // non-claimant's Welcome registers nobody there and so carries
        // nothing; an unclaimed channel holds no grant and keeps the plain
        // register.
        if is_the_claimant(folder_claim, caller_id) {
            match state
                .db
                .register_successor_carrying_seat(&recipient, &channel_id)
                .await
            {
                Ok(carried) if carried.seats_retired > 0 => tracing::info!(
                    target: "recovery",
                    channel = %hex::encode(channel_id),
                    seats_retired = carried.seats_retired,
                    grants_carried = carried.grants,
                    "the successor's Welcome retired its predecessor's seat"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("welcome register_actor_channel: {e}"),
            }
        } else {
            register_actor_channel_gated_with_claim(
                &state.db,
                &channel_id,
                folder_claim,
                caller_id,
                &recipient,
                "welcome register_actor_channel",
            )
            .await;
        }

        Ok(WelcomeDeliverReply {
            inbox_id,
            extra: std::collections::BTreeMap::new(),
        })
    }
}

/// The recipient-side reach gate every **conversation initiation** passes —
/// the floor, then the recipient's own inbox mode.
///
/// Two doors initiate a conversation with someone who is not already in it:
/// a first-reach `welcome.deliver`, and a room invite
/// (`conversation-rooms.md` § Join rules and invites — "an invite is
/// initiation, and initiation is what the recipient's inbox mode mediates;
/// the group-Welcome gate there applies unchanged"). This function is that
/// gate, so "unchanged" is literal rather than a copy that drifts.
///
/// A refusal is deliberately the generic `forbidden`: telling an arbitrary
/// sender "this account is supervised" would disclose the ward's status to
/// the whole network, and the same opaque refusal covers `contacts_only`, so
/// a sender cannot distinguish a policy from supervision. Its i18n string
/// already points the sender at the contact request, which is the remedy for
/// every refusing cause.
///
/// `consult_inbox_mode` is the caller's to decide, and it is **false on a
/// cross-nest leg originated here**: the inbox mode is the RECIPIENT'S OWN
/// nest's fact to enforce, and on a relay leg this nest is the *sender's* —
/// its view of a remote recipient's mode is the empty default, not their
/// policy. The receiving nest's ingest keeps its own verdicts: a Welcome's
/// arrives unsigned and takes the unauthenticated floor there, while a
/// **room invite** arrives as the inviter's signed act, so the invitee's own
/// nest runs this very gate over it with `origin = Federation` and the
/// inviter as the caller (`federation_handlers::room_invite_deliver_handler`,
/// `conversation-rooms.md` § Join rules and invites → *A cross-nest
/// invitation*) — the one cross-nest initiation whose reach policy is
/// enforced in full. Cross-nest mode enforcement for the unsigned kinds stays
/// a declared follow-on (`direct-messages.md` § Reach policy), blocked on the
/// contact model learning home-nests. The Welcome path additionally passes
/// `false` for the kinds that do not consult it at all.
///
/// `origin` is where the initiation arrived from — `Local` for a caller this
/// nest authenticated, `Federation` for one a peer nest relayed — which is
/// what the floor's `federation_contact` pillar turns on.
pub(crate) async fn conversation_initiation_reach_gate(
    state: &Arc<AppState>,
    recipient: &[u8; 32],
    caller_id: &[u8; 32],
    consult_inbox_mode: bool,
    origin: ArrivalOrigin,
) -> Result<(), RpcError> {
    match crate::routes::reach_floor(state, recipient, caller_id, origin).await {
        Ok((ReachVerdict::Proceed, status)) => {
            // The floor imposes nothing; the recipient's **inbox mode** is
            // the "caller's own routing" `ReachVerdict`'s contract hands
            // back — and for a conversation-shaped initiation that routing
            // is the shared `dm_initiation_mode_verdict`
            // (`direct-messages.md` § Reach policy: the FAQ's "the recipient
            // must accept before DMs flow" default, enforced here).
            if consult_inbox_mode {
                let mode = state.db.get_inbox_mode(recipient).await.map_err(|e| {
                    tracing::error!("initiation get_inbox_mode error: {e}");
                    internal("storage error")
                })?;
                // Reject-don't-guess on an unknown stored token — the knock
                // path's own unknown-mode arm (`deliver_inbox_payload_core`).
                let Some(mode) = InboxMode::from_wire(&mode) else {
                    return Err(forbidden("recipient is not accepting new conversations"));
                };
                match dm_initiation_mode_verdict(status, mode) {
                    ReachVerdict::Proceed => {}
                    ReachVerdict::Knock | ReachVerdict::Suppress => {
                        return Err(forbidden("recipient is not accepting new conversations"));
                    }
                }
            }
            Ok(())
        }
        Ok((ReachVerdict::Knock | ReachVerdict::Suppress, _)) => {
            // A Welcome carries no `ContactRequest`, and neither does a room
            // invite, so there is nothing to knock with — and inventing a
            // second pending store for held initiations is exactly what
            // `family-safety.md` § Don't do these forbids. The sender's path
            // to this recipient is the contact request (`fauna.inbox.send`),
            // which knocks into the guardian's queue; once approved, the
            // initiation goes through.
            Err(forbidden("recipient is not accepting new conversations"))
        }
        Err(()) => Err(internal("storage error")),
    }
}

// ── fauna.conversations.room.create ────────────────────────────

/// The community class's **birth ceremony**.
///
/// Three things are established here, and each is what some later door
/// depends on (`conversation-rooms.md` § The room, § The home nest, § Don't
/// do these):
///
/// 1. **The id commits to the founder.** `room_id =
///    derive_room_id(owner, salt)`, so a room id is not a 32-byte name
///    anyone may claim but a commitment to whose key founded it. The caller
///    does not get to choose it, and the nest re-derives rather than trusts.
/// 2. **The founding roster seats the home nest.** The ceremony writes two
///    principals: the creator as the room's one `owner`, and this nest as an
///    ordinary `member` of kind `nest`. The class is then DERIVED from that
///    member set by the ordered rule (§ The three classes) and lands on
///    `community` — no class word came off the wire. This is § Don't do
///    these' first bullet read literally: there is no "make this room
///    readable by the nest" toggle; the nest is a member, visibly, from the
///    room's first instant. Crucially it is not a *promotion* either — the
///    bullet's real target is granting a nest a read over a transcript its
///    members already sealed to each other, and a room born this way has no
///    such transcript.
/// 3. **The room becomes floor-authoritative.** Storing the birth salt is
///    what lets [`RoomRecord::is_floor_authoritative`] tell this room from
///    one that first appeared through the mirror door — which is how the
///    report door's declared bootstrap bound is closed, below.
///
/// What is deliberately NOT here: a class field, a home-nest field, a
/// members list. Class is derived; home is where the ceremony ran ("a room
/// is born on its creating member's home nest"); membership past the two
/// founders is the invite door's, a later slice.
fn room_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.create").await?;
            let req: RoomCreateRequest = decode(&payload).map_err(malformed)?;
            let salt = parse_32_bytes(&req.salt)
                .ok_or_else(|| invalid_params("invalid salt hex — 32 bytes expected"))?;
            // A birth salt without the binding mark is refused before anything
            // else is read: it is the one lever a non-conforming founder has to
            // mint a room whose id reads as predating the room signature, and
            // the room this nest would then be home to is exactly the one a
            // splice of another room's versions targets. The shared birth judge
            // below refuses it too; this is the door's own typed answer.
            if !fauna_mls::room_policy::is_binding_birth_salt(&salt) {
                return Err(invalid_params(
                    "birth salt refused: a community room's salt opens with the binding mark",
                ));
            }

            // A non-empty wrap target must be one a mint can actually seal
            // to — empty is the ordinary "not keyed yet" founder.
            if !req.reception_pubkey.is_empty() {
                reject_invalid_reception_key(&req.reception_pubkey)?;
            }

            let signed: fauna_mls::room_policy::SignedRoomPolicy =
                fauna_protocol::decode_strict(&req.policy)
                    .map_err(|e| invalid_params(&format!("malformed room policy: {e}")))?;

            // The signature proves the bytes are the signer's, and
            // `verify_signature_community` pairs it with the structural
            // check the community class owns — which, unlike the end-to-end
            // one, admits the `request` join rule (§ Join rules and invites:
            // `request` is "community rooms only", and this ceremony founds
            // exactly those).
            //
            // The birth record is the creating principal's OWN act, twice
            // over: the policy's owner must be its signer (nobody signs a
            // room into existence under somebody else's name), and the
            // caller must be that owner (a third party does not found a room
            // on the owner's behalf — § The home nest, "a room is born on
            // its CREATING member's home nest"). The two are separate
            // refusals because they fail for different reasons: the first is
            // a malformed record, the second an unauthorized caller.
            //
            // The record's own half — signed by the owner it names, version
            // 1, no admins (at birth the only user principal on the floor is
            // the owner, so an admin set would name ranks nobody holds) — is
            // the shared judge's, the very check a member later re-runs to
            // anchor the room's policy chain to its id.
            let room_id = fauna_mls::room_policy::verify_community_birth(&signed, &salt)
                .map_err(|e| invalid_params(&format!("birth record refused: {e}")))?;
            if signed.policy.owner.0 != actor_id {
                return Err(permission_denied(
                    "a room is founded by its own creating principal",
                ));
            }

            // The nest's principal id on the roster is exactly the `nest_id`
            // it publishes on `fauna.nest.info` (`discovery_core.rs` reads
            // the same `nest_identity`), so a member can recognise its home
            // nest's roster row against the node info it already fetched —
            // and pick up the `room_read_pubkey` published beside it, which
            // is the wrap target a community room's generation bundle needs
            // (`key-material-hierarchy.md` § Audience: deployment
            // infrastructure → *Room-read keypair*).
            let nest_principal = state.nest_identity.public_key_bytes();
            // The nest seats itself with its OWN room-read public key rather
            // than one the caller supplied: it is the nest's key, the caller
            // has no business naming it, and a founder that could name it
            // would choose a wrap target it holds the secret for and read the
            // room as the nest. A lookup, never a mint: the keypair is minted
            // at boot, so a nest missing the row refuses the ceremony with
            // `NotProvisioned` rather than sealing a fresh row under whatever
            // seed this serving generation happens to hold.
            //
            // A nest with no deployment signing key seats itself with **no**
            // wrap target rather than refusing the ceremony: it is still a
            // member — the class is the member set (rule 1) — it simply
            // cannot be a reader, which is the same honest answer
            // `fauna.nest.info` gives by omitting `room_read_pubkey`.
            let nest_reception = match state.nest_signing_key.as_ref().map(|k| k.to_bytes()) {
                Some(seed) => {
                    let db = state.db.clone();
                    tokio::task::spawn_blocking(move || {
                        crate::room_read_key::public_key(&db.conn_blocking(), &seed)
                    })
                    .await
                    .map_err(|e| internal(format!("room-read key task: {e}")))?
                    .map_err(|e| internal(format!("room-read key: {e}")))?
                }
                None => Vec::new(),
            };
            let members = vec![
                crate::db::rooms::ReportedMember {
                    principal_id: actor_id,
                    principal_kind: "user".to_string(),
                    role: Some("owner".to_string()),
                    home_node_url: String::new(),
                    reception_pubkey: req.reception_pubkey.clone(),
                },
                crate::db::rooms::ReportedMember {
                    principal_id: nest_principal,
                    principal_kind: "nest".to_string(),
                    // The nest READS; it never invites, removes, renames or
                    // mints (§ Don't do these — "don't put the home nest in
                    // a community room's key authority set"). `member` is
                    // the honest floor role for a principal that holds no
                    // operation in the roles table.
                    role: Some("member".to_string()),
                    home_node_url: String::new(),
                    reception_pubkey: nest_reception,
                },
            ];

            // Derived, not chosen: the ordered rule over the kinds seated
            // above (§ The three classes). Spelled as the derivation rather
            // than as the constant `"community"` so that the day a ceremony
            // seats a different member set, the stored class follows the
            // members instead of a literal somebody forgot.
            let class = room_class_of(members.iter().map(|m| m.principal_kind.as_str()));

            let founded = state
                .db
                .found_room(
                    &room_id,
                    class,
                    &actor_id,
                    signed.policy.version,
                    &req.policy,
                    &salt,
                    &members,
                )
                .await
                .map_err(|e| {
                    tracing::error!("found_room: {e}");
                    internal("storage error")
                })?;
            if !founded {
                return Err(permission_denied(
                    "that room id already names a room founded by another principal",
                ));
            }

            // Seat the founder on the channel's ROUTING roster too — the
            // fan-out mechanism the nest may mutate without any key
            // (§ The floor roster → *What the floor roster is not*). A
            // founder that never sent would otherwise be unreachable by its
            // own room's fan-out until its first send registered the row.
            state
                .db
                .register_actor_channel(&actor_id, &room_id)
                .await
                .map_err(|e| internal(format!("routing roster: {e}")))?;

            encode_reply(&RoomCreateReply {
                room_id: hex::encode(room_id),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

/// The ordered class derivation of `conversation-rooms.md` § The three
/// classes, over the principal kinds of a member set: any bridge or MTA
/// member ⇒ transport-only; else the home nest a member ⇒ community; else
/// end-to-end.
///
/// The nest's copy of `fauna_conversations::room::derive_room_class`, which
/// is the client-side authority and works on that crate's `PrincipalKind`
/// enum. This one reads the storage vocabulary the roster tables use
/// (`db/rooms.rs`), so the two agree on the rule without the nest taking a
/// dependency on the app-side render types.
fn room_class_of<'a>(kinds: impl IntoIterator<Item = &'a str>) -> &'static str {
    let mut has_nest = false;
    for kind in kinds {
        match kind {
            "bridge" => return "transport_only",
            "nest" => has_nest = true,
            _ => {}
        }
    }
    if has_nest { "community" } else { "end_to_end" }
}

// ── the reception-key validator ──────────────────────
//
// A stored NON-EMPTY `room_members.reception_pubkey` is always FIPS-203-valid
// (`community-rooms.md` § Implementation status today, *A reception key is
// FIPS-203-valid before it is ever stored*): every writer below refuses a
// non-empty key that fails `fauna_mls::wrapped_blob::XWingPublicKey::
// parse_and_validate` — the same check `parse_target` runs before a mint
// seals to it — so a length-only-checked key can no longer reach storage and
// freeze every later mint over the seat that holds it. Empty stays legal at
// `room.create` and `room.accept_invite` — "no wrap target yet" is the
// member top-up's own state, not a malformed key.

/// Refuse `bytes` if it is a non-empty reception key that is malformed or
/// fails FIPS 203 validation. Called unconditionally at `set_reception_key`
/// (a key is never optional there) and only when `bytes` is non-empty at
/// `room.create`/`room.accept_invite` (where an empty key is the ordinary
/// "not keyed yet" seat).
fn reject_invalid_reception_key(bytes: &[u8]) -> Result<(), RpcError> {
    fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate(bytes)
        .map(|_| ())
        .map_err(|e| invalid_params(&format!("reception_pubkey: {e}")))
}

/// Whether a *stored* reception key is one a mint can actually wrap to:
/// non-empty and FIPS-203-valid. Every writer refuses an invalid non-empty
/// key going forward, but a room sealed before this fix could still hold one
/// — this treats it exactly like no key at all, so the roster coverage rule
/// skips it (rather than freezing the mint) and the roster read reports it
/// absent, which is what makes `heal_own_room_seat`'s ordinary "not keyed
/// with my current key" path repair it automatically, with no separate sweep
/// over already-poisoned rows.
fn usable_reception_key(bytes: &[u8]) -> bool {
    !bytes.is_empty() && fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate(bytes).is_ok()
}

// ── the community floor's role gate ────────────────────────────

/// The room's stored policy, decoded — the record the floor applies
/// (`conversation-rooms.md` § Roles and authorization → *Community rooms —
/// enforced at the floor*: "the same policy is stored on the home nest
/// beside the roster, owner-signed, and the nest applies the table above on
/// every write before it stores or fans out").
async fn stored_room_policy(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
) -> Result<fauna_mls::room_policy::SignedRoomPolicy, RpcError> {
    let blob = state
        .db
        .get_room_policy_blob(room_id)
        .await
        .map_err(|e| internal(format!("room policy read: {e}")))?
        .ok_or_else(|| invalid_params("this room carries no policy — it was not founded here"))?;
    fauna_protocol::decode_strict(&blob)
        .map_err(|e| internal(format!("stored room policy does not decode: {e}")))
}

/// The caller's live role on a room's floor, refusing a non-member.
///
/// Membership first, rank second: a caller absent from the roster is
/// `permission_denied` for *not being there*, which is a different fact
/// from holding too low a rank, and conflating them would tell a stranger
/// their rank was the problem.
async fn floor_role(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    actor_id: &[u8; 32],
) -> Result<fauna_mls::room_policy::RoomRole, RpcError> {
    optional_floor_role(state, room_id, actor_id)
        .await?
        .ok_or_else(|| permission_denied("not a member of this room"))
}

/// The floor's judgment of a **floor delete record**, before anything is
/// stored (`conversation-rooms.md` § Roles and authorization → *Delete any
/// message — the mechanism* → *Community rooms*) — four checks, all from the
/// record alone:
///
/// 1. the signature verifies, for this room;
/// 2. the author is the authenticated caller — nobody files a delete under
///    another principal's name, signed or not;
/// 3. the named policy version is the version the floor holds — a record made
///    under a superseded policy is refused here, which is what keeps a demoted
///    admin from acting under the version that still named them;
/// 4. the author's floor seat is owner or admin. The seat, not the policy's
///    name list: the succession ceremony hands a predecessor's seat to its
///    successor, so the seat is where "through the succession chain" already
///    resolves on this nest ([`floor_designee`]).
///
/// The target's existence is deliberately **not** checked: the tombstone is
/// cooperative, a record naming a position nothing sits at tombstones nothing,
/// and looking the target up would be a step toward judging by content.
async fn judge_floor_delete(
    state: &Arc<AppState>,
    room: &crate::db::rooms::RoomRecord,
    room_id: &[u8; 32],
    caller: &[u8; 32],
    record: &[u8],
) -> Result<(), RpcError> {
    use fauna_mls::room_policy::{RoomRole, SignedRoomFloorDelete};
    let signed = SignedRoomFloorDelete::from_bytes(record)
        .map_err(|e| invalid_params(&format!("malformed floor delete record: {e}")))?;
    signed
        .verify(room_id)
        .map_err(|e| invalid_params(&format!("floor delete record does not verify: {e}")))?;
    if &signed.record.author.0 != caller {
        return Err(permission_denied(
            "a floor delete record is filed by the author it names",
        ));
    }
    if room.policy_version != Some(signed.record.policy_version) {
        return Err(permission_denied(
            "this floor delete record names a policy version the floor does not hold",
        ));
    }
    match floor_role(state, room_id, caller).await? {
        RoomRole::Owner | RoomRole::Admin => Ok(()),
        RoomRole::Member => Err(permission_denied(
            "deleting another member's message takes the room's owner or an admin",
        )),
    }
}

/// The caller's live role on a room's floor, or `None` when it holds no live
/// seat — [`floor_role`] without the refusal.
///
/// The `None` arm is for the doors whose *whole point* is that the seat may
/// already be gone: an idempotent departure has to be able to tell "you are
/// not on this floor" (a converged success) from "this stored role is a word
/// I do not understand" (an `internal`), and an `Err` that folds the two
/// together cannot.
async fn optional_floor_role(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    actor_id: &[u8; 32],
) -> Result<Option<fauna_mls::room_policy::RoomRole>, RpcError> {
    use fauna_mls::room_policy::RoomRole;
    let Some(role) = state
        .db
        .get_room_member_role(room_id, actor_id)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?
    else {
        return Ok(None);
    };
    match role.as_str() {
        "owner" => Ok(Some(RoomRole::Owner)),
        "admin" => Ok(Some(RoomRole::Admin)),
        "member" => Ok(Some(RoomRole::Member)),
        // A policy-less room's members carry no role at all, and the CHECK
        // constraint admits no other word — so this arm is a stored row the
        // vocabulary does not cover. Refuse rather than guess: a role the
        // nest does not understand must not be silently demoted to `member`,
        // which would be a quiet privilege change.
        other => Err(internal(format!("unknown stored room role {other:?}"))),
    }
}

/// A room founded by the create ceremony, refusing every other provenance.
///
/// The membership doors below write the floor **as the authority**, which is
/// only the arrangement a ceremony-born room has. An end-to-end room's
/// membership is its MLS group's, reaching the nest through
/// `room.roster_report`; letting these doors write it would put a second
/// authority on one room — the mirror image of the report door's gate 0.
async fn floor_authoritative_room(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
) -> Result<crate::db::rooms::RoomRecord, RpcError> {
    let room = state
        .db
        .get_room(room_id)
        .await
        .map_err(|e| internal(format!("room read: {e}")))?
        .ok_or_else(|| invalid_params("no such room"))?;
    if !room.is_floor_authoritative() {
        return Err(permission_denied(
            "this room's membership authority is its MLS group, not its floor",
        ));
    }
    Ok(room)
}

/// Every identity a room policy's name stands for on this nest: the name
/// itself, then each identity that has succeeded it, oldest first.
async fn succession_line(
    state: &Arc<AppState>,
    name: &[u8; 32],
) -> Result<Vec<[u8; 32]>, RpcError> {
    let path = state
        .db
        .succession_path(name)
        .await
        .map_err(|e| internal(format!("succession path: {e}")))?;
    let mut line = vec![*name];
    line.extend(
        path.iter()
            .filter_map(|hop| <[u8; 32]>::try_from(hop.new_actor_id.as_slice()).ok()),
    );
    Ok(line)
}

/// Whom a room policy's name designates on the floor today, with that seat's
/// role — `(name, None)` when nobody along its succession line holds a live
/// seat.
///
/// The community class stores the same signed policy the end-to-end class
/// carries in its group context, and the two classes must answer "who is this
/// admin" alike. The end-to-end class resolves every name through the
/// succession chain in the policy extension (`RoomPolicyExtension::role_of`),
/// so "an admin's successor is an admin, and no policy rewrite is needed for
/// the inheritance" (`conversation-rooms.md` § Roles and authorization →
/// *Successions inside a governed room*). The nest cannot rewrite a signed
/// policy — it can only refuse one — while the succession ceremony hands the
/// predecessor's floor seat to the successor. So a policy signed before the
/// ceremony keeps naming an identity that no longer holds a seat, and every
/// door that matches a policy name against a seat goes through here instead of
/// comparing bytes.
///
/// The NEWEST live seat along the line wins. After a ceremony on this nest
/// that is the successor, whose seat the ceremony wrote beside the absorbed
/// predecessor's; for a member homed on another nest, whose seat nothing here
/// hands over, it is still the predecessor — so a succession this nest only
/// learned from a peer never makes a live seat unreachable by its own name.
async fn floor_designee(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    name: &[u8; 32],
) -> Result<([u8; 32], Option<String>), RpcError> {
    let line = succession_line(state, name).await?;
    for principal in line.iter().rev() {
        let seated = state
            .db
            .get_room_member_role(room_id, principal)
            .await
            .map_err(|e| internal(format!("floor roster: {e}")))?;
        if seated.is_some() {
            return Ok((*principal, seated));
        }
    }
    Ok((*name, None))
}

/// May `inviter` invite `invitee` into this room at `role`, as the floor and
/// the signed policy stand **right now**?
///
/// One judgement, asked by two doors (`conversation-rooms.md` § Join rules
/// and invites → *An invitation is a standing offer*): the invite door asks it
/// of its caller, and the accept door asks it again of the invitation's
/// recorded inviter, because accepting is the moment the inviter's authority
/// is exercised. Whatever this function tests, both doors test — a rule added
/// to one of them alone is an invitation that outlives the authority behind
/// it.
///
/// The outer `Err` means the judgement could not be made (a storage fault);
/// the inner one is the verdict. The accept door lapses an invitation on a
/// verdict and never on a fault.
///
/// **The inviter is judged by its own seat, never through its succession
/// line.** The ceremony absorbs a predecessor's seat, so an identity since
/// succeeded holds none and everything it left pending lapses — which is the
/// point: those are the invitations a seed thief could have issued, and
/// resolving the inviter to its successor would pass them on the recovered
/// owner's rank. (For the *caller* of the invite door the two readings agree:
/// an authenticated caller is its own newest identity.)
async fn judge_room_invitation(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    inviter: &[u8; 32],
    invitee: &[u8; 32],
    role: fauna_mls::room_policy::RoomRole,
) -> Result<Result<(), RpcError>, RpcError> {
    use fauna_mls::room_policy::{JoinRule, RoomRole};
    let Some(inviter_role) = optional_floor_role(state, room_id, inviter).await? else {
        return Ok(Err(permission_denied("not a member of this room")));
    };
    let policy = stored_room_policy(state, room_id).await?;

    // § Join rules and invites, read off the room's own policy: under
    // `invite` (and under `request`, whose knock door is a later slice) the
    // owner and admins invite; under `member-invite` any member does. Exactly
    // the `judge_commit` rule the end-to-end class applies to an Add, so one
    // table governs both classes.
    let may_invite =
        inviter_role.is_admin_or_owner() || policy.policy.join_rule == JoinRule::MemberInvite;
    if !may_invite {
        return Ok(Err(permission_denied(
            "this room's join rule reserves invitations to its owner and admins",
        )));
    }

    // An ADMIN invitation is admissible only when the room's current signed
    // policy ALREADY names the invitee in its admin set.
    //
    // The nest cannot author a policy (rule 6), so seating an admin on the
    // floor whose admin set the policy does not name would make the nest
    // enforce a rank the policy members verify does not grant — the exact
    // divergence rule 6 exists to prevent, since members render roles from
    // the owner-signed policy and every nest-side gate reads the roster's
    // projection of it. The owner's route is therefore: `set_policy` naming
    // the future admin, then the invitation. That keeps admin appointment a
    // single owner-signed act rather than two doors that can disagree.
    //
    // A name counts through its succession line ([`floor_designee`]'s
    // reasoning): a policy naming an identity since succeeded here names its
    // successor too.
    if role == RoomRole::Admin {
        let mut named_admin = false;
        for admin in &policy.policy.admins {
            if succession_line(state, &admin.0).await?.contains(invitee) {
                named_admin = true;
                break;
            }
        }
        if !named_admin {
            return Ok(Err(invalid_params(
                "name the invitee in the room's admin set first — an admin rank comes from the owner-signed policy, not from an invitation",
            )));
        }
    }
    Ok(Ok(()))
}

// ── fauna.conversations.room.invite ────────────────────────────

/// The invite door's **first act**, shared by every door an inviter's signed
/// act arrives at — the same-nest door, its relayed twin on the inviter's own
/// nest, and the federated issue door on the room's home: the record decodes,
/// its signature verifies, and its signer IS `signer` — the caller this nest
/// authenticated, or the requesting actor a relaying nest authenticated and
/// the home binds the act to (`conversation-rooms.md` § Join rules and invites
/// → *A cross-nest invitation*, the foreign-inviter leg). The same double
/// bond the birth record carries: a principal may not put an invitation in
/// another's mouth, on any hop. Answers the verified record and the 32-byte
/// room it names.
pub(crate) fn verify_room_invite_act(
    invite: &[u8],
    signer: &[u8; 32],
) -> Result<(fauna_mls::room_policy::SignedRoomInvite, [u8; 32]), RpcError> {
    let signed: fauna_mls::room_policy::SignedRoomInvite = fauna_protocol::decode_strict(invite)
        .map_err(|e| invalid_params(&format!("malformed room invite: {e}")))?;
    signed
        .verify_signature()
        .map_err(|e| invalid_params(&format!("room invite does not verify: {e}")))?;
    if signed.inviter.0 != *signer {
        return Err(permission_denied("an invitation is the inviter's own act"));
    }
    let room_id: [u8; 32] = signed
        .invite
        .room_id
        .as_slice()
        .try_into()
        .map_err(|_| invalid_params("a room invite names a 32-byte room"))?;
    Ok((signed, room_id))
}

/// Whether a base URL an inviter derived from a handle's domain names THIS
/// nest — its claimed identity domain, spelled as the client's `peer_nest_url`
/// derivation spells a node (`https://<domain>`), compared host-wise and
/// case-insensitively. A domainless box names nothing: it cannot be reached
/// by name, so no inviter could have derived it.
fn invitee_node_names_this_nest(state: &AppState, node: &str) -> bool {
    let Some(domain) = state.handle_domain_if_set() else {
        return false;
    };
    let host = node
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    !host.is_empty() && host.eq_ignore_ascii_case(domain.trim_end_matches('/'))
}

/// The invitation's **body** — everything past binding the signed act to its
/// inviter ([`verify_room_invite_act`]) — shared by the same-nest door below
/// and the federated issue door
/// (`federation_handlers::room_invite_issue_handler`), so an inviter homed
/// elsewhere is judged by exactly the rules an inviter homed here is
/// (`conversation-rooms.md` § Join rules and invites → *A cross-nest
/// invitation*, the foreign-inviter leg): the room must be
/// floor-authoritative, the inviter's seat and the join rule
/// ([`judge_room_invitation`]), the already-a-member refusal, then the
/// delivery.
///
/// **Three deliveries, one door.** `invitee_node` is the invitee's home as
/// the inviter knows it, and it picks the arm: a node naming another nest is
/// [`room_invite_deliver_abroad`]'s push, which lets the invitee's own nest
/// run the invitee's reach policy; an empty node — or one naming THIS nest —
/// is the same-nest delivery through the inbox plane under the invitee's
/// reach policy here. The self-naming arm exists for the relayed twin: a
/// foreign inviter's client derives an invitee's node from the handle's
/// domain, and for an invitee homed on the room's home that domain is this
/// nest's own, which the same-nest door's callers blank before it ever sees
/// them. A nest never dials itself.
///
/// `origin` is where the inviter's act arrived from — `Local` for a caller
/// this nest authenticated, `Federation` for one a peer nest relayed — and
/// it is the same-nest arm's reach-gate input: an invitee homed here judges a
/// foreign inviter's knock by the floor's `federation_contact` pillar, as it
/// would that inviter's first-reach Welcome.
pub(crate) async fn room_invite_apply(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    inviter: &[u8; 32],
    signed: &fauna_mls::room_policy::SignedRoomInvite,
    signed_bytes: &[u8],
    invitee_node: &str,
    origin: ArrivalOrigin,
) -> Result<RoomInviteReply, RpcError> {
    use fauna_mls::room_policy::RoomRole;
    let invitee = signed.invite.invitee.0;
    if invitee == *inviter {
        return Err(invalid_params("an inviter does not invite itself"));
    }

    floor_authoritative_room(state, room_id).await?;
    // (An invitation naming the OWNER role never reaches here:
    // `RoomInvite::validate` refuses it, and `verify_signature` ran it in
    // `verify_room_invite_act`. The rule has one home, in the record's own
    // validator, rather than a copy per door.)
    judge_room_invitation(state, room_id, inviter, &invitee, signed.invite.role).await??;

    // Already on the floor? Say so rather than opening an invitation
    // that would re-write a live member's role on acceptance — a
    // rank change is not an invitation.
    if state
        .db
        .is_room_member(room_id, &invitee)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?
    {
        return Err(invalid_params("that principal is already a member"));
    }

    // Exhaustive, not a catch-all: `RoomInvite::validate` already
    // refuses `Owner` (checked by `verify_signature` above), so this
    // covers the two roles an invitation can actually carry — and a
    // future `RoomRole` variant becomes a compile error here instead
    // of silently downgrading to "member".
    let role_word = match signed.invite.role {
        RoomRole::Admin => "admin",
        RoomRole::Member => "member",
        RoomRole::Owner => {
            unreachable!("RoomInvite::validate refuses Owner before this point is reached")
        }
    };
    let reply = RoomInviteReply {
        role: role_word.to_string(),
        extra: std::collections::BTreeMap::new(),
    };

    // An invitee homed on ANOTHER nest: the judgement above is the
    // whole of what this nest decides, and the knock is delivered to
    // the invitee's own nest, which runs the invitee's reach policy
    // itself (§ Join rules and invites → *A cross-nest invitation*).
    let invitee_node = invitee_node.trim().trim_end_matches('/');
    if !invitee_node.is_empty() && !invitee_node_names_this_nest(state, invitee_node) {
        room_invite_deliver_abroad(
            state,
            room_id,
            inviter,
            &invitee,
            role_word,
            invitee_node,
            signed_bytes,
        )
        .await?;
        return Ok(reply);
    }

    // The invitee's own reach policy — an invite is initiation, and
    // "the group-Welcome gate there applies unchanged"
    // (§ Join rules and invites). This is the same-nest leg, so the
    // invitee's inbox mode is this nest's fact to consult.
    conversation_initiation_reach_gate(state, &invitee, inviter, true, origin).await?;

    // The invitation reaches the invitee through the **inbox plane**
    // (`conversation-rooms.md` § Join rules and invites), carrying the
    // inviter's signed act verbatim — the record names the room, and it
    // is the only way an invitee ever learns the id to accept with.
    // Recorded and delivered in one transaction so the accept door's
    // row and the knock the invitee sees cannot diverge.
    //
    // `room_node` stays absent: an invitation is admissible here only
    // for a room this nest homes, so the invitee's next hop is the nest
    // it is already talking to — and the recorded node URL is blank for
    // the same reason, whatever spelling of this nest the inviter used.
    let inbox_bytes = fauna_protocol::inbox::InboxEnvelope::room_invite(
        &fauna_protocol::inbox::RoomInviteInbox {
            signed_invite: signed_bytes.to_vec(),
            room_node: None,
            extra: std::collections::BTreeMap::new(),
        },
    )
    .and_then(|env| env.to_canonical_bytes())
    .map_err(|e| internal(format!("encode room invite envelope: {e}")))?;
    let recorded = state
        .db
        .record_room_invite_and_deliver(
            room_id,
            &invitee,
            inviter,
            role_word,
            "",
            signed_bytes,
            &inbox_bytes,
            *state.enforce_tier_quotas.read().await,
        )
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("exceeds invitee's inbox quota") {
                return forbidden(&format!("invitee's inbox is full: {msg}"));
            }
            tracing::error!("record_room_invite_and_deliver: {e}");
            internal("storage error")
        })?;
    if !recorded {
        return Err(invalid_params("that invitation was already accepted"));
    }

    crate::push::dispatch_offline_push(
        state,
        &invitee,
        "Room invite",
        "You have been invited to a room",
        "/app/conversations",
    );

    Ok(reply)
}

fn room_invite_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.invite").await?;
            let req: RoomInviteRequest = decode(&payload).map_err(malformed)?;
            // The signature must be the inviter's, and the inviter must be
            // the caller. A third party may not relay somebody else's
            // invitation into this nest: an inviter homed elsewhere issues
            // through its OWN nest (`room.invite_remote`), which binds the
            // act to the member it authenticated.
            let (signed, room_id) = verify_room_invite_act(&req.invite, &actor_id)?;
            let reply = room_invite_apply(
                &state,
                &room_id,
                &actor_id,
                &signed,
                &req.invite,
                &req.invitee_node,
                ArrivalOrigin::Local,
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

// ── fauna.conversations.room.invite_remote ─────────────────────

/// A **foreign member's** invitation, relayed by its own home nest — the
/// issuing twin of [`room_accept_invite_remote_handler`], and the leg that
/// gives `member-invite` its plain meaning for a member homed elsewhere.
///
/// § Roles and authorization grants *invite* to any member under
/// `member-invite` with no homing carve-out, and § The home nest has a member
/// on a foreign nest reach the room "only through their own home nest, which
/// originates the leg to the room's home". This nest is not that home: it
/// holds no room record, so its same-nest door answers "no such room" —
/// which is why the relay is a distinct kind (`RoomInviteRemoteRequest`).
///
/// **This nest keeps nothing and decides nothing about the room.** What it
/// does is the invite door's own first act — the record verifies and its
/// signer is the caller ([`verify_room_invite_act`]) — so it never relays
/// somebody else's invitation under its member's name; the join-rule
/// judgement, the already-a-member refusal and the delivery are the home's
/// (`federation_handlers::room_invite_issue_handler`), keyed on the actor id
/// this nest is authenticated for and gated on that actor's foreign-member
/// binding there. `invitee_node` rides through as the inviter spelled it:
/// empty for an invitee homed HERE, which the home resolves to this nest's
/// verified identity rather than to anything this nest declares.
fn room_invite_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.invite_remote").await?;
            let req: RoomInviteRemoteRequest = decode(&payload).map_err(malformed)?;
            verify_room_invite_act(&req.invite, &actor_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest invitations use room.invite)",
                ));
            }

            match crate::federation_pool::originate_room_invite_issue(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                req.invite,
                req.invitee_node,
            )
            .await
            {
                Ok(Ok(ack)) => encode_reply(&ack),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room invite relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room invite issue (invite_remote): {pool_err}");
                    Err(internal("federation room invite issue failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.room.invite — the cross-nest leg ───────

/// Deliver an invitation to an invitee homed on **another nest** — the
/// room home's half of `conversation-rooms.md` § Join rules and invites →
/// *A cross-nest invitation*, run by `room.invite` once the invitation has
/// been judged exactly as a same-nest one is.
///
/// Three acts, in an order chosen so that every failure leaves a state a
/// retry converges:
///
/// 1. **Resolve the invitee's nest to its verified identity** — the
///    federation dial's handshake, never the inviter's declaration — because
///    that identity is the gate the relayed accept will run behind: only the
///    nest the invitation was delivered to may seat this invitee. A nest that
///    cannot be reached is a refusal here rather than a row nobody can act on.
/// 2. **Record the row, with no local envelope**
///    (`record_room_invite_for_foreign_delivery`): the knock lands in the
///    invitee's own inbox on their nest, and the row is what the accept door
///    reads — recorded FIRST so a delivered knock always has a row behind it.
/// 3. **Originate `fauna.federation.conversation.room.invite`.** The
///    invitee's nest verifies the signed act, runs the invitee's own reach
///    policy against the inviter, binds the room's home URL to this nest's
///    verified identity, and delivers the knock. Its refusal is the inviter's
///    answer, as a same-nest reach refusal is — and consumes the row, so no
///    invitation stands that its invitee was never told of.
///
/// This nest's own reach gate is deliberately NOT run for a foreign invitee:
/// the invitee's inbox mode and contact edges are their nest's facts, and the
/// same-nest gate's view of them here is the empty default.
async fn room_invite_deliver_abroad(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    inviter: &[u8; 32],
    invitee: &[u8; 32],
    role_word: &str,
    invitee_node: &str,
    signed_invite: &[u8],
) -> Result<(), RpcError> {
    let peer_url = fauna_core::resolve::resolve_full_url(invitee_node).await;
    let invitee_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(&peer_url)
        .await
        .map_err(|e| {
            tracing::warn!("resolve the invitee's home nest ({peer_url}): {e}");
            invalid_params("the invitee's home nest could not be reached")
        })?;
    let recorded = state
        .db
        .record_room_invite_for_foreign_delivery(
            room_id,
            invitee,
            inviter,
            role_word,
            invitee_node,
            signed_invite,
            &invitee_nest_id,
        )
        .await
        .map_err(|e| {
            tracing::error!("record_room_invite_for_foreign_delivery: {e}");
            internal("storage error")
        })?;
    if !recorded {
        return Err(invalid_params("that invitation was already accepted"));
    }

    // Claim-refreshed identity domain, the Welcome relay's own choice: a
    // provisioned box boots domainless and would hand the peer no origin
    // URL at all until a restart. The invitee's nest honours it only against
    // this nest's verified identity (`resolve_origin_home_url`).
    let origin_nest_url = state.handle_domain_if_set().map(|d| format!("https://{d}"));
    let delivered = crate::federation_pool::originate_room_invite_deliver(
        &state.federation_pool,
        state,
        &peer_url,
        crate::federation_handlers::FedRoomInviteRequest {
            recipient_actor_id: hex::encode(invitee),
            signed_invite: signed_invite.to_vec(),
            origin_nest_url,
        },
    )
    .await;
    let failure = match delivered {
        Ok(Ok(_inbox_id)) => return Ok(()),
        Ok(Err(peer_err)) => {
            crate::rpc_errors::map_peer_relay_error(peer_err, "the cross-nest room invite")
        }
        Err(pool_err) => {
            tracing::error!("federation room invite delivery ({peer_url}): {pool_err}");
            internal("federation room invite delivery failed")
        }
    };
    // Consume what step 2 recorded — the invitee was never told, so nothing
    // may stand for them to accept. A failure here is logged, not returned:
    // the delivery's own refusal is the answer the inviter needs, and the
    // orphaned row lapses on its own the moment anyone tries it.
    if let Err(e) = state
        .db
        .consume_pending_room_invite(room_id, invitee, Some(inviter))
        .await
    {
        tracing::error!("consume an undelivered cross-nest invitation: {e}");
    }
    Err(failure)
}

// ── fauna.conversations.room.accept_invite ─────────────────────

/// The acceptance's **body** — what seats the invitee — shared by the
/// same-nest door below and the federated twin
/// ([`room_accept_relayed`]), so a member homed elsewhere is judged and
/// seated by exactly the rules a member homed here is.
///
/// An invitation is a standing offer, and accepting it is when the inviter's
/// authority is exercised — so the invite door's own judgement runs again,
/// against the floor and the policy as they stand now (§ Join rules and
/// invites → *An invitation is a standing offer*). One that no longer passes
/// LAPSES: the row, its envelope and the envelope's quota charge go in one
/// act, so the invitee is not left holding a knock this door will refuse for
/// ever. Whoever holds the authority today may invite again.
///
/// The seating is bound to the policy version the judgement read
/// (`accept_room_invite`'s compare-and-swap). A policy landing in between
/// sends this round again; a room re-signing its policy faster than this door
/// can judge it is not a state worth a fourth try.
///
/// Answers the role word now held.
async fn room_accept_apply(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    invitee: &[u8; 32],
    reception_pubkey: &[u8],
) -> Result<String, RpcError> {
    // A non-empty wrap target must be one a mint can actually seal to —
    // empty is the ordinary "not keyed yet" seat.
    if !reception_pubkey.is_empty() {
        reject_invalid_reception_key(reception_pubkey)?;
    }
    for _ in 0..3 {
        let room = floor_authoritative_room(state, room_id).await?;

        // Only the invitee accepts, and only its OWN pending invitation: the
        // caller is the key, so there is nothing to name and nothing to spoof.
        let Some(pending) = state
            .db
            .get_pending_room_invite(room_id, invitee)
            .await
            .map_err(|e| internal(format!("invite read: {e}")))?
        else {
            return Err(permission_denied("no invitation to this room is pending"));
        };
        let role = match pending.role.as_str() {
            "admin" => fauna_mls::room_policy::RoomRole::Admin,
            "member" => fauna_mls::room_policy::RoomRole::Member,
            other => {
                return Err(internal(format!(
                    "a stored invitation carries a role outside the vocabulary: {other}"
                )));
            }
        };
        if let Err(verdict) =
            judge_room_invitation(state, room_id, &pending.inviter_id, invitee, role).await?
        {
            state
                .db
                .consume_pending_room_invite(room_id, invitee, Some(&pending.inviter_id))
                .await
                .map_err(|e| internal(format!("lapse invite: {e}")))?;
            tracing::info!(
                room = %hex::encode(room_id),
                "a pending room invitation lapsed at accept: {}",
                verdict.code
            );
            return Err(permission_denied(
                "this invitation has lapsed — whoever issued it could not issue it today; ask to be invited again",
            ));
        }

        use crate::db::rooms::RoomInviteAccept;
        match state
            .db
            .accept_room_invite(room_id, invitee, reception_pubkey, room.policy_version)
            .await
            .map_err(|e| {
                tracing::error!("accept_room_invite: {e}");
                internal("storage error")
            })? {
            RoomInviteAccept::Seated => return Ok(pending.role),
            // Lost a race with a removal or a concurrent accept.
            RoomInviteAccept::NotPending => {
                return Err(permission_denied("no invitation to this room is pending"));
            }
            RoomInviteAccept::PolicyMoved => continue,
        }
    }
    Err(internal(
        "the room's policy kept changing while the invitation was judged",
    ))
}

/// The federated accept's body — `fauna.federation.conversation.room.accept`
/// arriving at the room's home from the invitee's own nest
/// (`conversation-rooms.md` § Join rules and invites → *A cross-nest
/// invitation*; the door is `federation_handlers::room_accept_handler`).
///
/// **Its gate is the invitation, not the foreign-member binding.** Every
/// other relayed room door runs behind `require_foreign_member`, but here no
/// binding exists yet — and writing one at delivery time would open the
/// binding-only doors (the relayed roster read above all) to an invitee who
/// never accepted, which the same-nest twin refuses as "membership is not
/// public". So the gate is what the home recorded when it delivered the
/// invitation: the row for `(room, requester)` must name the calling nest's
/// **verified** identity as the invitee's home. A nest the invitation did not
/// go to is refused whatever it claims about its member.
///
/// Behind the gate: **seat first, bind second.** The seating is
/// [`room_accept_apply`], the same-nest body verbatim; the binding is the
/// `channel_foreign_members` row every relayed door serves off, inserted with
/// `InsertOnly` power — the Welcome relay's own arm, so a binding a removal
/// purged is re-insertable and a live one is never moved by an accept. Seat
/// before bind because a seat without relayed reach is the tighter half-state:
/// the member reads nothing until a retry binds it, whereas a binding without
/// a seat would serve the room's floor to a principal not on it.
///
/// **Idempotent, for the §4.D re-send** the relay adds: a requester whose
/// invitation from this nest is already ACCEPTED and who holds a live seat is
/// answered with that seat's role and its binding re-asserted — the
/// half-landed "seated, not yet bound" state converges here too. A consumed
/// or absent invitation, or an accepted one whose seat has since been
/// removed (`unseat_room_member` clears the row), is refused as the
/// same-nest door refuses a stale accept.
///
/// No routing-roster row is written for a foreign member, deliberately: the
/// fan-out reaches same-nest sockets, and a member homed elsewhere learns of
/// new traffic by polling through its own nest (`community-rooms.md`
/// § Implementation status today → *A foreign member … learns of a new
/// message only by polling*).
pub(crate) async fn room_accept_relayed(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    requester: &[u8; 32],
    origin_nest_id: &[u8; 32],
    reception_pubkey: &[u8],
) -> Result<RoomAcceptInviteReply, RpcError> {
    let Some(binding) = state
        .db
        .room_invite_home_binding(room_id, requester)
        .await
        .map_err(|e| internal(format!("invite read: {e}")))?
    else {
        return Err(permission_denied("no invitation to this room is pending"));
    };
    if binding.invitee_nest_id != Some(*origin_nest_id) {
        return Err(forbidden(
            "this invitation was not delivered to the requesting nest",
        ));
    }
    let role = if binding.accepted {
        use fauna_mls::room_policy::RoomRole;
        match optional_floor_role(state, room_id, requester).await? {
            Some(RoomRole::Owner) => "owner".to_string(),
            Some(RoomRole::Admin) => "admin".to_string(),
            Some(RoomRole::Member) => "member".to_string(),
            None => return Err(permission_denied("no invitation to this room is pending")),
        }
    } else {
        room_accept_apply(state, room_id, requester, reception_pubkey).await?
    };
    let nest_url = Some(binding.invitee_node_url.as_str()).filter(|u| !u.is_empty());
    state
        .db
        .register_foreign_channel_member(
            room_id,
            requester,
            origin_nest_id,
            nest_url,
            crate::db::channels::RebindPower::InsertOnly,
        )
        .await
        .map_err(|e| {
            tracing::error!("register_foreign_channel_member (room accept): {e}");
            internal("storage error")
        })?;
    Ok(RoomAcceptInviteReply {
        role,
        extra: std::collections::BTreeMap::new(),
    })
}

fn room_accept_invite_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.accept_invite").await?;
            let req: RoomAcceptInviteRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            let accepted_role =
                room_accept_apply(&state, &room_id, &actor_id, &req.reception_pubkey).await?;

            // Seat the new member on the channel's ROUTING roster as well —
            // the fan-out mechanism (§ The floor roster → *What the floor
            // roster is not*). Membership is the floor's; reachability is
            // this row's, and a member the fan-out cannot reach would never
            // see the room's traffic.
            state
                .db
                .register_actor_channel(&actor_id, &room_id)
                .await
                .map_err(|e| internal(format!("routing roster: {e}")))?;

            encode_reply(&RoomAcceptInviteReply {
                role: accepted_role,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.accept_invite_remote ──────────────

/// A **foreign invitee's** acceptance, relayed by its own home nest — the
/// seating twin of [`room_leave_remote_handler`].
///
/// § The home nest has a member on a foreign nest reach the room "only
/// through their own home nest, which originates the leg to the room's
/// home", and an acceptance is that member's first act on the room. This
/// nest is not the home: it holds no room record and no invitation row (the
/// knock it delivered to its member carries the home's URL as `room_node`),
/// so its same-nest door answers "no invitation pending" — which is why the
/// relay is a distinct kind (`RoomAcceptInviteRemoteRequest`).
///
/// **This nest keeps nothing and decides nothing.** The wrap target is
/// validated for shape here, as every door validates its own input; the
/// judgement, the seating and the binding are the home's
/// (`room_accept_relayed`), keyed on the actor id this nest is authenticated
/// for, so a nest can seat only its own members. The client acks the knock
/// itself once the home's ack comes back (`FaunaMlsBackend::accept_room_invitation`).
fn room_accept_invite_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.accept_invite_remote",
            )
            .await?;
            let req: RoomAcceptInviteRemoteRequest = decode(&payload).map_err(malformed)?;
            parse_room_id(&req.room_id)?;
            if !req.reception_pubkey.is_empty() {
                reject_invalid_reception_key(&req.reception_pubkey)?;
            }
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest acceptances use room.accept_invite)",
                ));
            }
            match crate::federation_pool::originate_room_accept(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
                req.reception_pubkey,
            )
            .await
            {
                Ok(Ok(ack)) => encode_reply(&ack),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room accept relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room accept (accept_invite_remote): {pool_err}");
                    Err(internal("federation room accept failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.room.list_invites / revoke_invite ──────

/// Whose pending invitations `caller` is served — and may therefore withdraw
/// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
/// are visible to whoever may withdraw them*). ONE predicate for both doors,
/// so nobody is shown an invitation it cannot act on and nobody acts on one
/// it was not shown:
///
/// - `Ok(None)` — the owner or an admin: every pending invitation;
/// - `Ok(Some(caller))` — any other seated member: the ones it issued;
/// - `Err` — a caller off the floor: who a room has invited is not public.
///
/// The caller is read by its own seat, [`judge_room_invitation`]'s reading.
async fn pending_invite_scope(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    caller: &[u8; 32],
) -> Result<Option<[u8; 32]>, RpcError> {
    let role = floor_role(state, room_id, caller).await?;
    Ok((!role.is_admin_or_owner()).then_some(*caller))
}

fn room_list_invites_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.list_invites").await?;
            let req: RoomListInvitesRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            floor_authoritative_room(&state, &room_id).await?;
            let issued_by = pending_invite_scope(&state, &room_id, &actor_id).await?;

            let pending = state
                .db
                .pending_invites_for_room(&room_id, issued_by.as_ref())
                .await
                .map_err(|e| internal(format!("pending invites: {e}")))?;
            let domain = state.handle_domain();
            let mut invites = Vec::with_capacity(pending.len());
            for row in pending {
                // The accept door's own judgement, run now — never a second
                // rule that could drift from it. A role outside the
                // vocabulary is a row the accept door refuses too.
                let still_acceptable = match row.role.as_str() {
                    "admin" => Some(fauna_mls::room_policy::RoomRole::Admin),
                    "member" => Some(fauna_mls::room_policy::RoomRole::Member),
                    _ => None,
                };
                let still_acceptable = match still_acceptable {
                    Some(role) => judge_room_invitation(
                        &state,
                        &room_id,
                        &row.inviter_id,
                        &row.invitee_id,
                        role,
                    )
                    .await?
                    .is_ok(),
                    None => false,
                };
                let named = row.invitee_handle.is_some() || row.inviter_handle.is_some();
                invites.push(RoomPendingInviteWire {
                    invitee: hex::encode(row.invitee_id),
                    invitee_handle: row.invitee_handle,
                    inviter: hex::encode(row.inviter_id),
                    inviter_handle: row.inviter_handle,
                    domain: named.then(|| domain.clone()),
                    role: row.role,
                    invited_at: row.invited_at,
                    still_acceptable,
                    extra: std::collections::BTreeMap::new(),
                });
            }
            encode_reply(&RoomListInvitesReply {
                invites,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

fn room_revoke_invite_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.revoke_invite").await?;
            let req: RoomRevokeInviteRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            let invitee = parse_room_id(&req.invitee)
                .map_err(|_| invalid_params("invitee must be a 32-byte hex actor id"))?;
            floor_authoritative_room(&state, &room_id).await?;
            let issued_by = pending_invite_scope(&state, &room_id, &actor_id).await?;

            // The lapse's own writer: row, envelope and quota charge leave as
            // one act, and the invitee is told nothing. For a plain member the
            // scope rides INTO the transaction, so somebody else's invitation
            // is — to it — not there: answered `false`, indistinguishable
            // from none, and nothing consumed.
            let revoked = state
                .db
                .consume_pending_room_invite(&room_id, &invitee, issued_by.as_ref())
                .await
                .map_err(|e| {
                    tracing::error!("consume_pending_room_invite: {e}");
                    internal("storage error")
                })?;
            encode_reply(&RoomRevokeInviteReply {
                revoked,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.remove ────────────────────────────

fn room_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            use fauna_mls::room_policy::RoomRole;
            require_permission(&state, &actor_id, "fauna.conversations.room.remove").await?;
            let req: RoomRemoveRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            let target = parse_actor_id(&req.principal)?;
            floor_authoritative_room(&state, &room_id).await?;

            if target == actor_id {
                return Err(invalid_params(
                    "leaving is its own door — fauna.conversations.room.leave",
                ));
            }

            let role = floor_role(&state, &room_id, &actor_id).await?;
            if !role.is_admin_or_owner() {
                return Err(permission_denied("only an owner or admin removes a member"));
            }

            // The TARGET's role — read through the same helper, but its
            // "not a member" refusal is about the target, not the caller, so
            // it is re-worded rather than passed through: a remover told
            // "not a member of this room" would read it as a verdict on
            // itself and go looking for the wrong problem.
            let target_role = floor_role(&state, &room_id, &target)
                .await
                .map_err(|_| invalid_params("that principal is not a live member"))?;
            if target_role == RoomRole::Owner {
                return Err(permission_denied(
                    "the owner's membership is not removable until ownership is transferred",
                ));
            }

            // The home nest is a member too, and the materialization grant's
            // REVOKE is "revocable by **rotating it out**" (§ The three
            // classes, the community key model, reason 1) — which is a
            // generation mint whose wrap set omits the nest's roster entry,
            // not a roster removal. That door is
            // `fauna.conversations.room.publish_generation`, and it deletes
            // the derived views in the same act.
            //
            // Removal stays refused here, and now for a sharper reason than
            // "unbuilt" (ruled at the sealing build, 2026-09-09): a room's
            // class is a function of its member set (§ Architectural rules,
            // rule 1), so unseating the home nest would derive the room back
            // to `end_to_end` while its log stays sealed under the
            // recipient-set scheme rather than MLS — a room whose stored
            // class names a key model it does not use. Rotation revokes the
            // read without touching the member set, which is exactly what
            // the grant language asks for and leaves nothing incoherent.
            let target_row = state
                .db
                .list_floor_roster(&room_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?
                .into_iter()
                .find(|m| m.principal_id == target);
            if target_row.is_some_and(|m| m.principal_kind != "user") {
                return Err(permission_denied(
                    "the home nest's read is revoked by rotating the room generation without its \
                     wrap, not by removing it from the floor",
                ));
            }

            // S8, the shape the folder plane already holds: **the fetch
            // authorization must die with the membership**
            // (`../architecture/federation.md` § Federation residue surface —
            // `members.evict` purges the foreign row). A foreign member's
            // admission to this room IS its `channel_foreign_members` row, and
            // the relayed doors gated on that row *alone* — the roster read,
            // the attachment write-token mint, `channel.actors` — would
            // otherwise outlive the seat: a removed member's home nest would
            // keep passing `require_foreign_member` and keep being served the
            // room's LIVE floor, every later join's `handle@domain`, roles, the
            // signed policy and each member's reception key, which is exactly
            // the read the same-nest twin refuses as "membership is not
            // public". Their binding-only gate is ratified and correct
            // (federation.md, the `conversation.roster.fetch` row: stacking
            // `is_room_member` on it would deny the read to the newest-seated
            // member, and "nothing is disclosed by that choice: an ADMITTED
            // channel member already reads every member's actor id off the MLS
            // ratchet tree") — the load-bearing word being *admitted*, so it is
            // admission a removal has to end, not the gate to re-shape.
            //
            // The rotation the client performs after this door covers the
            // CIPHERTEXT residue (§ The three classes -> *Community*, reason
            // 4); it covers no plaintext door, so the binding is what has to go.
            //
            // **Before the unseat, deliberately** — the inverse of the client's
            // unseat-then-mint order, and for the inverse reason. The mint must
            // follow because coverage is judged against the floor as it stands;
            // this purge reads no floor, so ordering it first makes every
            // failure mode fail CLOSED: a purge that fails changes nothing and
            // the caller retries cleanly, and a purge that lands over a failed
            // unseat leaves a seat with no relayed reach — tighter than it was,
            // never leakier — which the same retry converges (both halves are
            // idempotent). Unseat-first would leave the one residue this fix
            // exists to remove: off the floor, still reading the live roster.
            // A local target holds no row, so this is a no-op for one.
            state
                .db
                .remove_foreign_channel_member(&room_id, &target)
                .await
                .map_err(|e| {
                    tracing::error!("remove_foreign_channel_member: {e}");
                    internal("storage error")
                })?;

            let removed = state
                .db
                .unseat_room_member(&room_id, &target)
                .await
                .map_err(|e| {
                    tracing::error!("unseat_room_member: {e}");
                    internal("storage error")
                })?;
            if !removed {
                return Err(invalid_params("that principal is not a live member"));
            }

            encode_reply(&RoomRemoveReply {
                members: live_floor_count(&state, &room_id).await?,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.leave ─────────────────────────────

/// What one run of [`room_leave_apply`] did.
pub(crate) struct RoomLeaveOutcome {
    /// Whether this call took the caller off the floor. `false` means they
    /// held no live seat — which the client-facing doors refuse loudly and the
    /// federated one reads as a converged success.
    pub(crate) unseated: bool,
    /// Whether this call purged a `channel_foreign_members` binding. Always
    /// `false` for a member homed on this nest, which holds none.
    pub(crate) binding_purged: bool,
    /// How many principals the floor holds live afterwards.
    pub(crate) members: u32,
}

/// A principal's departure from a room homed **here** — the one body every
/// leave door runs: the same-nest [`room_leave_handler`], its relayed twin's
/// far end (`federation_handlers::room_leave_handler`), and the generic
/// `fauna.federation.channel.leave` when the channel it names is a room.
///
/// **Two acts, not one**, exactly as `room.remove` has them
/// (`community-rooms.md` § Implementation status today → *A removal severs*):
/// the seat comes off the floor, and the foreign member's
/// `channel_foreign_members` binding — its admission to the relayed doors
/// gated on that row *alone* (the roster read, the write-token mint,
/// `channel.actors`) — is purged with it. A departure that dropped only one of
/// the two leaves a residue: seat-without-binding is a ghost the floor never
/// converges past, whose reception key every later generation mint is *obliged*
/// to wrap (the roster-coverage gate) and whose presence refuses the
/// re-admission that would otherwise heal it; binding-without-seat is the
/// reverse, a departed member still served the room's live floor.
///
/// **The purge lands BEFORE the unseat, deliberately** — `room.remove`'s
/// ratified order and its reasoning verbatim: the purge reads no floor, so
/// ordering it first makes every failure mode fail closed. A purge that fails
/// changes nothing; a purge that lands over a failed unseat leaves a seat with
/// no relayed reach — tighter than it was, never leakier. The one way this
/// door's residue differs from the removal's is that a *self*-leave's own
/// authorization is the binding it just purged, so a retry of this door after
/// a half-landed call is refused at the gate rather than converging; what
/// converges it is `room.remove` from any remaining owner or admin, the door
/// that reads a rank rather than a binding.
///
/// **No rotation, and that is a reason rather than an omission**
/// (`community-rooms.md` § Implementation status today, the leave paragraph): a
/// leaver holds no mint authority, and a mint it could build would still wrap
/// to itself, since coverage is judged against the floor and it is on the floor
/// until the unseat lands. A room that wants a departed member sealed out of
/// *new* traffic rotates from a remaining owner or admin.
///
/// **Both classes, one self-scoped act** (`conversation-rooms.md` § Roles and
/// authorization → *Leaving — the mechanism*, amended 2026-09-23). An
/// end-to-end room's floor is a member-reported mirror, and this is the one
/// floor door such a room admits: a member's own absence is the one thing
/// about the mirror it may assert on nobody else's word, and the door writes
/// nothing else — no one else's row, no position, no owner. On that class the
/// seat moves **alone**: the binding is not purged, because the class severs
/// relayed reach by reading the floor rather than by purging
/// (`federation_handlers::refuse_removed_room_member`), and a purge
/// there would leave a member still inside the MLS group no re-Welcome to be
/// re-admitted by. The door replaced the end-to-end class's departure by final
/// roster report, which named the roster minus the leaver from the leaver's own
/// group view at no position — taken wholesale, so a leaver behind the newest
/// membership commit rolled the floor back to its stale view.
pub(crate) async fn room_leave_apply(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    actor_id: &[u8; 32],
) -> Result<RoomLeaveOutcome, RpcError> {
    use fauna_mls::room_policy::RoomRole;
    let room = state
        .db
        .get_room(room_id)
        .await
        .map_err(|e| internal(format!("room read: {e}")))?
        .ok_or_else(|| invalid_params("no such room"))?;

    // The rank is read BEFORE either write: the owner's refusal has to come
    // before anything is severed, or a refused departure would still have cut
    // the owner's own relayed reach.
    if optional_floor_role(state, room_id, actor_id).await? == Some(RoomRole::Owner) {
        // An owner-less roster would strand invite and remove forever
        // (§ Roles and authorization, Mechanism B's rule kept), so the
        // owner's exit is a transfer followed by a leave, never a leave
        // alone. Unreachable through the relayed doors by construction — a
        // room is born on its owner's home and a transfer to a member homed
        // elsewhere re-homes it (§ The home nest), so a room's owner is never
        // one of its foreign members — and kept here anyway, because an
        // invariant this load-bearing is not left resting on a second
        // document's argument.
        return Err(permission_denied(
            "transfer ownership before leaving — a room is never owner-less",
        ));
    }

    let binding_purged = if room.is_floor_authoritative() {
        state
            .db
            .remove_foreign_channel_member(room_id, actor_id)
            .await
            .map_err(|e| {
                tracing::error!("remove_foreign_channel_member: {e}");
                internal("storage error")
            })?
    } else {
        false
    };

    let unseated = state
        .db
        .unseat_room_member(room_id, actor_id)
        .await
        .map_err(|e| {
            tracing::error!("unseat_room_member: {e}");
            internal("storage error")
        })?;

    Ok(RoomLeaveOutcome {
        unseated,
        binding_purged,
        members: live_floor_count(state, room_id).await?,
    })
}

fn room_leave_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.leave").await?;
            let req: RoomLeaveRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;

            let outcome = room_leave_apply(&state, &room_id, &actor_id).await?;
            if !outcome.unseated {
                // No live seat. A caller the floor has seated and since stamped
                // departed is answered as a converged departure — its own
                // earlier leave whose reply was lost, or a removal that beat it
                // there; either way the postcondition "off this floor" holds,
                // and refusing would tell a user who has left that they could
                // not, on every retry, for ever. The relayed twin has always
                // answered this way. A caller the floor never seated is still
                // refused, in the words the rest of the room plane's doors use.
                let departed = state
                    .db
                    .room_member_removed(&room_id, &actor_id)
                    .await
                    .map_err(|e| internal(format!("floor roster: {e}")))?;
                if !departed {
                    return Err(permission_denied("not a member of this room"));
                }
            }

            encode_reply(&RoomLeaveReply {
                members: outcome.members,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.leave_remote ──────────────────────

/// A **foreign member's** departure, relayed by its own home nest — the
/// self-scoped twin of [`room_roster_report_remote_handler`].
///
/// § Roles and authorization grants *leave (remove self)* with no homing
/// carve-out, and § The home nest has a member on a foreign nest reach the
/// room "only through their own home nest, which originates the leg to the
/// room's home". This nest is not that home: it holds no room record, so its
/// same-nest door answers "no such room", and the generic
/// `fauna.federation.channel.leave` the member could otherwise reach drops the
/// relay binding without touching the floor — a departure that leaves the seat
/// behind.
///
/// **This nest keeps nothing and decides nothing.** It forwards the room
/// home's ack; the home resolves the leaver's standing itself, from the actor
/// id this nest is authenticated for ([`crate::federation_handlers`]'
/// `require_foreign_member`), so a nest can retire only its own members' seats.
fn room_leave_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.leave_remote").await?;
            let req: RoomLeaveRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id shape locally; the room's home nest is
            // authoritative for the membership this retires.
            parse_room_id(&req.room_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest departures use room.leave)",
                ));
            }

            match crate::federation_pool::originate_room_leave(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
            )
            .await
            {
                Ok(Ok(ack)) => encode_reply(&ack),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room leave relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room leave (leave_remote): {pool_err}");
                    Err(internal("federation room leave failed"))
                }
            }
        })
    })
}

/// How many principals a room's floor holds live — the same number the
/// report door's ack carries, so every membership door answers in one
/// currency.
async fn live_floor_count(state: &Arc<AppState>, room_id: &[u8; 32]) -> Result<u32, RpcError> {
    Ok(state
        .db
        .list_floor_roster(room_id)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?
        .len() as u32)
}

// ── fauna.conversations.room.set_policy ────────────────────────

/// The roles table applied to a policy offered to either governance door —
/// **the shared judge** (`fauna_mls::room_policy::judge_community_policy_step`),
/// the same one a member runs over the retained versions before it lets one
/// grant anybody a rank (`conversation-rooms.md` § Roles and authorization →
/// *Delete any message — the mechanism* → *Members verify what they paint*).
/// One judge on both sides is what keeps the floor from admitting a version
/// its members would then refuse to follow.
///
/// It covers the signature — **for this room**, the room signature every
/// community room requires above version 1
/// ([`fauna_mls::room_policy::RoomBinding`]), so a version the caller signed
/// in another room it owns, or one carrying no room signature, is refused here
/// exactly as a member refuses it — the strict `stored + 1` ratchet (`>` alone would
/// let a caller skip versions and strand a member that had read version N+1
/// from a peer; equality is what makes the sequence a chain), and rule 6 read
/// exactly — "the owner for the owner and the admin set, owner or admin for
/// the rest", by the signer's rank in the PREVIOUS version. What this nest
/// adds is the one input the judge cannot compute: whom each name designates
/// on the floor ([`floor_designee`]), so an admin's successor carrying the
/// admin set verbatim, or re-naming itself in it, changes nobody's rank.
async fn judge_policy_step(
    state: &Arc<AppState>,
    room: &crate::db::rooms::RoomRecord,
    stored: &fauna_mls::room_policy::SignedRoomPolicy,
    offered: &fauna_mls::room_policy::SignedRoomPolicy,
) -> Result<(), RpcError> {
    use fauna_mls::room_policy::PolicyStepRefusal;
    let room_id = &room.room_id;
    let binding = fauna_mls::room_policy::RoomBinding::new(*room_id);
    let mut seats = std::collections::HashMap::new();
    for policy in [&stored.policy, &offered.policy] {
        for name in std::iter::once(&policy.owner).chain(&policy.admins) {
            if !seats.contains_key(name) {
                let (designee, _) = floor_designee(state, room_id, &name.0).await?;
                seats.insert(*name, fauna_core::identity::ActorId(designee));
            }
        }
    }
    let designee = |name: &fauna_core::identity::ActorId| seats.get(name).copied().unwrap_or(*name);
    fauna_mls::room_policy::judge_community_policy_step(
        &stored.policy,
        offered,
        &binding,
        &designee,
    )
    .map_err(|refusal| match refusal {
        PolicyStepRefusal::Malformed(_) | PolicyStepRefusal::Version { .. } => {
            invalid_params(&refusal.to_string())
        }
        PolicyStepRefusal::Rank(_) => permission_denied(&refusal.to_string()),
    })
}

/// Decode and verify a policy offered to the `set_policy` door: the shared
/// judge admits the change ([`judge_policy_step`]), the signature is the
/// caller's, and the admin set names only live user members.
///
/// Returns the verified policy, the stored one it replaces, and the offered
/// admin set as the floor principals its names designate ([`floor_designee`])
/// — the set the roster's roles are reconciled to.
async fn verified_policy_change(
    state: &Arc<AppState>,
    room: &crate::db::rooms::RoomRecord,
    actor_id: &[u8; 32],
    blob: &[u8],
) -> Result<
    (
        fauna_mls::room_policy::SignedRoomPolicy,
        fauna_mls::room_policy::SignedRoomPolicy,
        Vec<[u8; 32]>,
    ),
    RpcError,
> {
    let signed: fauna_mls::room_policy::SignedRoomPolicy = fauna_protocol::decode_strict(blob)
        .map_err(|e| invalid_params(&format!("malformed room policy: {e}")))?;
    let room_id = &room.room_id;
    let stored = stored_room_policy(state, room_id).await?;
    judge_policy_step(state, room, &stored, &signed).await?;
    // The signer must be the caller: "the nest stores it and cannot author
    // it" cuts both ways — it also may not accept one principal's policy
    // relayed by another, since the judge's rank check is about the signer.
    if signed.signer.0 != *actor_id {
        return Err(permission_denied(
            "a policy change is signed by the principal making it",
        ));
    }

    // An admin who is not a member is a rank nobody holds — and a roster the
    // policy cannot be projected onto is a floor whose gates disagree with
    // what members render. Each name is matched through its succession line,
    // so a policy signed before an admin's succession still names a seat.
    let mut admins = Vec::with_capacity(signed.policy.admins.len());
    for admin in &signed.policy.admins {
        let (designee, seated) = floor_designee(state, room_id, &admin.0).await?;
        match seated.as_deref() {
            Some("owner") => {
                // Unreachable via `RoomPolicy::validate` (the owner is never
                // in its own admin set) unless the policy is transferring
                // ownership, where the outgoing owner may be named an admin
                // — that case is handled by the transfer door, which checks
                // the admin set against the roster AFTER the swap.
                return Err(invalid_params(
                    "the room's owner is not a member of its own admin set",
                ));
            }
            Some(_) => admins.push(designee),
            None => {
                // Most often a deleted admin's name, which stays in the
                // signed policy until the owner — the one principal the roles
                // table lets change the admin set — re-signs without it. Say
                // so, rather than leave every other admin guessing why their
                // edit bounced.
                return Err(invalid_params(
                    "an admin set names a principal that is not a live member of the room — the owner re-signs the policy without it",
                ));
            }
        }
    }
    Ok((signed, stored, admins))
}

fn room_set_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.set_policy").await?;
            let req: RoomSetPolicyRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            let room = floor_authoritative_room(&state, &room_id).await?;

            let role = floor_role(&state, &room_id, &actor_id).await?;
            let (signed, stored, admins) =
                verified_policy_change(&state, &room, &actor_id, &req.policy).await?;

            // Ownership is its own operation in § Roles, and it moves the
            // roster row and the room's home as well as the bytes — so this
            // door refuses it by name rather than half-applying it. The two
            // owner names are compared as the seats they designate: after an
            // owner's succession the stored policy names the predecessor and
            // the successor's first re-sign names itself — the same owner,
            // not a transfer.
            let (offered_owner, _) =
                floor_designee(&state, &room_id, &signed.policy.owner.0).await?;
            let (stored_owner, _) =
                floor_designee(&state, &room_id, &stored.policy.owner.0).await?;
            if offered_owner != stored_owner {
                return Err(invalid_params(
                    "ownership moves through fauna.conversations.room.transfer_ownership",
                ));
            }

            // Rule 6 — who may change which field — is the shared judge's,
            // already applied ([`judge_policy_step`]) by the signer's rank in
            // the stored version. The floor's own role column is that
            // version's projection, so a caller it seats as a plain member
            // holds no rank the judge could have found either; refusing on it
            // here keeps a roster that ever drifted from its policy failing
            // closed rather than open.
            if !role.is_admin_or_owner() {
                return Err(permission_denied(
                    "only an owner or admin sets a room's policy",
                ));
            }

            let applied = state
                .db
                .set_room_policy(
                    &room_id,
                    stored.policy.version,
                    signed.policy.version,
                    &req.policy,
                    &admins,
                )
                .await
                .map_err(|e| {
                    tracing::error!("set_room_policy: {e}");
                    internal("storage error")
                })?;
            if !applied {
                // The ratchet moved under us between the read and the write.
                return Err(rate_limited());
            }

            encode_reply(&RoomSetPolicyReply {
                policy_version: signed.policy.version,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.set_labelers ──────────────────────

use fauna_mls::room_policy::ROOM_LABELER_KINDS;

fn room_set_labelers_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.set_labelers").await?;
            let req: RoomSetLabelersRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            // A room this nest reads only by its members' grant, which means a
            // ceremony-born one: an end-to-end room's nest reads nothing, so a
            // set stored there would be a promise the class cannot keep.
            floor_authoritative_room(&state, &room_id).await?;
            let role = floor_role(&state, &room_id, &actor_id).await?;

            let signed: fauna_mls::room_policy::SignedRoomLabelers =
                fauna_protocol::decode_strict(&req.labelers)
                    .map_err(|e| invalid_params(&format!("malformed labeler set: {e}")))?;
            signed
                .verify_signature()
                .map_err(|e| invalid_params(&format!("labeler set does not verify: {e}")))?;
            // "The nest stores it and cannot author it" — nor accept one
            // principal's set relayed by another, since the role check below
            // is about the signer.
            if signed.signer.0 != actor_id {
                return Err(permission_denied(
                    "a labeler set is signed by the principal making it",
                ));
            }
            // Rule 6, "owner or admin for the rest": what reads a room is not a
            // plain member's to choose for everyone in it.
            if !role.is_admin_or_owner() {
                return Err(permission_denied(
                    "only an owner or admin names a room's labelers",
                ));
            }
            if signed.labelers.room_id.as_slice() != room_id.as_slice() {
                return Err(invalid_params(
                    "this labeler set was signed for another room",
                ));
            }

            let (stored_version, _) = state
                .db
                .get_room_labelers(&room_id)
                .await
                .map_err(|e| internal(format!("room labeler set read: {e}")))?;
            if signed.labelers.version != stored_version + 1 {
                return Err(invalid_params(&format!(
                    "a labeler set is exactly one version on: this room is at {stored_version}, \
                     the offer is {}",
                    signed.labelers.version
                )));
            }

            // Only what this nest would actually run. A published `wasm` or
            // `text-model` artifact is transparent by construction — anyone can
            // inspect it. An id the registry does not hold names nothing
            // transparent: a user's own (tier-1) model is sealed under its
            // owner's key and never published here, so this is also the door
            // that keeps one from ever being named (`content-scoring.md` § The
            // placement matrix — "never a tier-1 model").
            for id in &signed.labelers.labelers {
                let record = state
                    .db
                    .get_labeler(&id.0)
                    .await
                    .map_err(|e| internal(format!("labeler registry read: {e}")))?;
                match record {
                    Some(r) if ROOM_LABELER_KINDS.contains(&r.artifact_kind.as_str()) => {}
                    Some(r) => {
                        return Err(invalid_params(&format!(
                            "labeler {} is a `{}` artifact — a room names only `wasm` or \
                             `text-model` labelers",
                            hex::encode(id.0),
                            r.artifact_kind
                        )));
                    }
                    None => {
                        return Err(invalid_params(&format!(
                            "labeler {} is not a published transparent labeler on this nest",
                            hex::encode(id.0)
                        )));
                    }
                }
            }

            let applied = state
                .db
                .set_room_labelers(
                    &room_id,
                    stored_version,
                    signed.labelers.version,
                    &req.labelers,
                )
                .await
                .map_err(|e| {
                    tracing::error!("set_room_labelers: {e}");
                    internal("storage error")
                })?;
            if !applied {
                // The ratchet moved under us between the read and the write.
                return Err(rate_limited());
            }
            encode_reply(&RoomSetLabelersReply {
                labelers_version: signed.labelers.version,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.transfer_ownership ────────────────

fn room_transfer_ownership_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            use fauna_mls::room_policy::RoomRole;
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.transfer_ownership",
            )
            .await?;
            let req: RoomTransferOwnershipRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            let room = floor_authoritative_room(&state, &room_id).await?;

            let role = floor_role(&state, &room_id, &actor_id).await?;
            if role != RoomRole::Owner {
                return Err(permission_denied("only the owner transfers ownership"));
            }

            let signed: fauna_mls::room_policy::SignedRoomPolicy =
                fauna_protocol::decode_strict(&req.policy)
                    .map_err(|e| invalid_params(&format!("malformed room policy: {e}")))?;
            // Signed by the OUTGOING owner: the signer's role in the
            // previous version decides whether it may have changed the owner
            // field, and at signing time that is still the owner — which is
            // the shared judge's rule ([`judge_policy_step`]), so a member
            // walking the retained versions admits this hand-over by the same
            // check that admitted it here.
            let stored = stored_room_policy(&state, &room_id).await?;
            judge_policy_step(&state, &room, &stored, &signed).await?;
            if signed.signer.0 != actor_id {
                return Err(permission_denied(
                    "a transfer is signed by the outgoing owner",
                ));
            }

            // The incoming owner as the floor principal its name designates
            // ([`floor_designee`]): an offer naming a member since succeeded
            // here hands the room to the successor holding that seat.
            let (new_owner, _) = floor_designee(&state, &room_id, &signed.policy.owner.0).await?;
            if new_owner == actor_id {
                return Err(invalid_params(
                    "a transfer names a new owner — this policy keeps the old one",
                ));
            }

            // An owner-less room is unrepresentable, so the incoming owner
            // must already be there. It must also be a USER: the home nest
            // holds no operation in the roles table, and a room owned by the
            // nest that reads it would put the key authority in exactly the
            // place § Don't do these keeps it out of.
            let incoming = state
                .db
                .list_floor_roster(&room_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?
                .into_iter()
                .find(|m| m.principal_id == new_owner)
                .ok_or_else(|| {
                    invalid_params("the incoming owner is not a live member of this room")
                })?;
            if incoming.principal_kind != "user" {
                return Err(invalid_params(
                    "a room is owned by a user principal, never by its home nest or a bridge",
                ));
            }
            // ⚠ DECLARED BOUND — re-homing is unbuilt. § The home nest says
            // a transfer to a member homed elsewhere re-homes the room, by a
            // signed room-succession record the old home publishes to every
            // member's home nest, with the log moving by the segment backup
            // protocol. None of that exists, and a transfer that silently
            // left the room homed here would leave the new owner's nest
            // believing it homes a room it does not. Refuse until the
            // ceremony exists, rather than half-transferring.
            if !incoming.home_node_url.is_empty() {
                return Err(permission_denied(
                    "transferring to an owner homed on another nest needs the re-homing ceremony, which is not built",
                ));
            }

            // The admin set is checked against the roster AFTER the swap:
            // the outgoing owner may legitimately be named an admin by the
            // very policy that demotes them, and every other name must be a
            // live user member. Names are matched as the seats they designate.
            let mut admins = Vec::with_capacity(signed.policy.admins.len());
            for admin in &signed.policy.admins {
                let (designee, seated) = floor_designee(&state, &room_id, &admin.0).await?;
                if designee != actor_id && seated.is_none() {
                    return Err(invalid_params(
                        "an admin set names a principal that is not a live member of the room — the owner re-signs the policy without it",
                    ));
                }
                admins.push(designee);
            }

            let applied = state
                .db
                .transfer_room_ownership(
                    &room_id,
                    stored.policy.version,
                    signed.policy.version,
                    &req.policy,
                    &actor_id,
                    &new_owner,
                    &admins,
                )
                .await
                .map_err(|e| {
                    tracing::error!("transfer_room_ownership: {e}");
                    internal("storage error")
                })?;
            if !applied {
                return Err(rate_limited());
            }

            encode_reply(&RoomTransferOwnershipReply {
                owner: hex::encode(new_owner),
                policy_version: signed.policy.version,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── the sealing plane: generations and their wraps ─────────────

/// Build the home nest's derived view of one community-room message.
///
/// The whole of the community class's *read*: a `RoomSealed` envelope, opened
/// with the wrap the room's members minted to this nest, its text folded into
/// the shared FTS corpus under a per-room class so the revoke can delete
/// exactly this room's rows (`CacheDb::room_view_schema`).
///
/// Every failure is silent and view-less: this runs after a send has already
/// been stored, and the send is the member's act, not the nest's. Refusing it
/// because the nest could not index it would make a member's message
/// contingent on a grant that is theirs to withdraw.
async fn index_room_message(state: &Arc<AppState>, channel_id: &[u8; 32], seq: i64, body: &[u8]) {
    let Ok(ChannelEnvelope::RoomSealed {
        generation,
        ciphertext,
    }) = ChannelEnvelope::from_bytes(body)
    else {
        return;
    };
    let Ok(generation) = <[u8; 32]>::try_from(generation.as_slice()) else {
        return;
    };
    let key = match nest_room_tip_key(state, channel_id).await {
        Ok(Some((key, tip))) if tip == generation => key,
        // Sealed under something other than the tip — an older generation the
        // nest may still hold a wrap for, or one it never did. Either way the
        // nest builds nothing: once the members have rotated it out, a
        // straggler under an old generation must not resurrect a view the
        // revoke deleted.
        Ok(_) => return,
        Err(e) => {
            tracing::warn!("room view: {e:?}");
            return;
        }
    };
    // Opens AND verifies the author's signature over `(room, generation,
    // author, stamp, body)` — the community class's attribution, since a
    // generation key every member holds authenticates nobody
    // (`fauna_mls::room_message`). A refusal here is a member sealing under
    // another member's name, so it never reaches the corpus.
    let signed = match fauna_mls::room_message::open_room_message(
        &key,
        channel_id,
        &generation,
        &ciphertext,
    ) {
        Ok(signed) => signed,
        Err(e) => {
            tracing::warn!("room view: a RoomSealed envelope under the tip did not open: {e}");
            return;
        }
    };
    // A signature proves *who* authored the bytes, never that they were
    // entitled to (`room_message` module doc → *What a verified signature does
    // NOT establish*). The floor roster is the membership authority
    // (`conversation-rooms.md` § The floor roster), so a non-member's — or a
    // removed member's — well-signed message builds no view: the derived
    // corpus must contain exactly what the room's own members wrote.
    let authored_by_a_member = state
        .db
        .list_floor_roster(channel_id)
        .await
        .map(|roster| {
            roster
                .iter()
                .any(|m| m.principal_id == signed.core.author.0)
        })
        .unwrap_or(false);
    if !authored_by_a_member {
        tracing::warn!("room view: a RoomSealed envelope was authored by a non-member");
        return;
    }
    // Control traffic is not content and never reaches a derived view.
    let Some(input) = fauna_mls::room_message::labeler_input(&signed) else {
        return;
    };
    // Purpose 1, search — over what a member typed, the message or its
    // caption. An attachment's filenames ride the same body, but indexing the
    // reference metadata would put bytes in the corpus that no member typed.
    if let Some(text) = input.text.as_deref() {
        index_room_text(state, channel_id, seq, text, &generation).await;
    }
    // Purpose 2, labels — the transparent labelers the room names, in this
    // same act, so a verdict exists before the fan-out that follows it. The
    // attachments' references ride with it: a labeler that declared it reads
    // their bytes, opened under the same generation the message just opened
    // under (*What the read covers*: "attachment bytes included").
    let attachments = match &signed.core.body {
        fauna_mls::types::ChannelMessageBody::Attachments { attachments, .. } => {
            attachments.as_slice()
        }
        _ => &[],
    };
    label_room_message(
        state,
        channel_id,
        seq,
        input,
        attachments,
        &key,
        &generation,
    )
    .await;
}

/// Where one item's attachment bytes come from, for the labelers that
/// declared they read them (`content-moderation-and-ranking.md` § Tier-3 →
/// *The attachment facet*). The two positions that hold a community room's
/// plaintext seal their media differently, and this is the whole of that
/// difference: the bounding rules are one shared loop's
/// ([`fauna_labeler::build_attachment_facet`]), and only the key model varies.
///
/// Opened once per item, whatever the number of declaring labelers, and
/// dropped with the pass that called for it: no derived view carries bytes
/// (`community-rooms.md` § The three classes → *Forbidden*).
pub(crate) enum RoomFacetSource<'a> {
    /// A room **message**'s attachments: sealed as the second per-kind content
    /// kind off the same wrap the pass already opened the message with
    /// (`community-rooms.md` § The three classes → *Attachments — the second
    /// content kind*), so a blob sealed under anything else is withheld, not
    /// resurrected.
    Message {
        attachments: &'a [fauna_mls::types::ChannelAttachment],
        key: &'a fauna_core::crypto::GenerationKey,
        generation: &'a [u8; 32],
        /// The record this item IS — `(channel, seq)`, the key
        /// `conv_attachment_refs` holds the sender's own list of sealed
        /// addresses under. The facet reads it to refuse a blob the record
        /// never pinned.
        room_id: &'a [u8; 32],
        seq: i64,
    },
    /// A room-restricted **post**'s media: sealed under the post's own
    /// per-post key — `derive_post_key(room_post_base_key(tip), seal_id)`, the
    /// very key the pass opened the body with (`../ui/feed.md` § Encryption at
    /// rest → *Room-restricted — the ruling*, ruling 4) — never as room
    /// attachments.
    Post {
        media: &'a [fauna_core::data::MediaItem],
        per_post_key: &'a [u8; 32],
    },
}

impl RoomFacetSource<'_> {
    /// Whether this item carries anything a declaring labeler could be handed.
    /// An item with no attachments is its own arm's empty slice — no bytes are
    /// opened for it, and no third variant is needed to say so.
    fn is_empty(&self) -> bool {
        match self {
            Self::Message { attachments, .. } => attachments.is_empty(),
            Self::Post { media, .. } => media.is_empty(),
        }
    }

    /// The facet itself — each item's opened plaintext in the author's order,
    /// or its declared metadata with empty bytes where the host withholds it.
    async fn open(
        &self,
        state: &Arc<AppState>,
    ) -> Vec<fauna_core::scoring::LabelerAttachmentInput> {
        use fauna_labeler::build_attachment_facet;

        let candidates = self.candidates();
        if candidates.len() > fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES {
            // The author writes this list inside the sealed body, so no
            // send-time check bounds it; past the ceiling the loop fetches and
            // opens nothing, and the metadata rides withheld. A list longer
            // than the record could have pinned is the abuse shape worth a
            // line (`MAX_ATTACHMENT_REFS_PER_RECORD`).
            tracing::warn!(
                "room labels: an item names {} attachments, past the host's per-item \
                 ceiling of {}; the remainder ride as declared metadata, unread",
                candidates.len(),
                fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES,
            );
        }
        let store = state
            .backup_service
            .as_ref()
            .map(|svc| svc.local_blob_store());
        let Some(store) = store else {
            // No blob store: every candidate rides as declared metadata.
            return build_attachment_facet(
                &candidates,
                |_| async { None },
                |_| async { None },
                |_, _| async { None },
            )
            .await;
        };
        // One probe for the whole item, shared by both arms — the two key
        // models differ, what a stored seal weighs does not, so this stays on
        // the side of the split that does not vary (the doc's "one loop
        // applies the bounds for both").
        let sealed_sizes = sealed_sizes_for(state, &candidates).await;
        let sealed_size = |i: usize| {
            let sealed_sizes = &sealed_sizes;
            let address = candidates.get(i).and_then(|c| c.address);
            async move { address.and_then(|a| sealed_sizes.get(&a).copied()) }
        };
        // And one open for the whole item, shared by both arms for the same
        // reason: the key model varies ([`FacetKey`]), where the decrypt runs
        // does not. With a single call site of [`open_off_the_worker`], no arm
        // can move its decrypt back onto the async worker alone — and the tests
        // that drive this source watch the thread the decrypt itself ran on.
        let facet_key = std::sync::Arc::new(self.facet_key());
        let open = move |_: usize, sealed: Vec<u8>| {
            let facet_key = std::sync::Arc::clone(&facet_key);
            async move { open_off_the_worker(move || facet_key.open(&sealed)).await }
        };
        match self {
            Self::Message {
                attachments,
                room_id,
                seq,
                ..
            } => {
                // What this nest itself recorded the record as pinning
                // (`conv_attachment_refs`, the conversation kind's
                // blob-reachability floor). The inner list lives inside the
                // sealed body, so nothing at send time bounds it; the refs
                // row is the one statement about this message's attachments
                // that the nest made in plaintext, capped at
                // `MAX_ATTACHMENT_REFS_PER_RECORD`.
                //
                // ⚠ **Empty means "no record to check against", never
                // "nothing was pinned".** `attachment_refs` is an additive
                // wire field a sender may omit, so a message from a sender that
                // omits it records none (as does a refs read fault) — and a naive "absent ⇒ withhold"
                // would blank the bytes for exactly the honest case the facet
                // exists to serve.
                let pinned: std::collections::BTreeSet<[u8; 32]> = state
                    .db
                    .conv_attachment_refs(room_id, *seq)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "room labels: reading the record's attachment refs \
                                        failed: {e}; the facet is bounded by its ceilings alone"
                        );
                        Vec::new()
                    })
                    .into_iter()
                    .collect();
                build_attachment_facet(
                    &candidates,
                    sealed_size,
                    |i| {
                        let store = &store;
                        let pinned = &pinned;
                        let cid = &attachments[i].sealed_cid;
                        async move {
                            if !pinned.is_empty() && !pinned.contains(&cid.digest()) {
                                // Named inside the sealed body but absent from
                                // the record's own refs: this nest never
                                // recorded the message as pinning it, so it is
                                // not read.
                                tracing::warn!(
                                    "room labels: an attachment the record never pinned; withheld \
                                     unread"
                                );
                                return None;
                            }
                            fetched_blob(store.get(cid).await)
                        }
                    },
                    open,
                )
                .await
            }
            Self::Post { media, .. } => {
                build_attachment_facet(
                    &candidates,
                    sealed_size,
                    |i| {
                        let store = &store;
                        let hash = &media[i].blob_hash;
                        async move { fetched_blob(store.get(hash).await) }
                    },
                    open,
                )
                .await
            }
        }
    }

    /// The key model this item's attachments were sealed under, owned: the
    /// decrypt runs on the blocking pool, and a blocking task must OWN what it
    /// opens with.
    ///
    /// `GenerationKey` is deliberately not `Clone` (key custody —
    /// `fauna_core::crypto`), so this makes the one copy of it the facet holds;
    /// [`Self::open`] wraps that copy in a single `Arc` and each decrypt clones
    /// the handle rather than the key — no per-candidate key material, one
    /// zeroize when the pass ends.
    fn facet_key(&self) -> FacetKey {
        match self {
            Self::Message {
                key, generation, ..
            } => FacetKey::Message {
                key: fauna_core::crypto::GenerationKey::from_bytes(*key.as_bytes()),
                generation: **generation,
            },
            Self::Post { per_post_key, .. } => FacetKey::Post {
                per_post_key: **per_post_key,
            },
        }
    }

    /// What each item declares about itself — what rides the facet when the
    /// bytes are withheld.
    fn candidates(&self) -> Vec<fauna_labeler::FacetCandidate> {
        use fauna_labeler::FacetCandidate;
        match self {
            Self::Message { attachments, .. } => attachments
                .iter()
                .map(|att| FacetCandidate {
                    mime_type: att.mime_type.clone(),
                    size_bytes: att.size_bytes,
                    // The sealed address, which is what `fetch` reads by —
                    // so the loop can serve sixty-four entries naming one
                    // upload from one read.
                    address: Some(att.sealed_cid.digest()),
                })
                .collect(),
            Self::Post { media, .. } => media
                .iter()
                .map(|item| FacetCandidate {
                    mime_type: item.media_type.clone(),
                    size_bytes: item.size_bytes,
                    address: Some(item.blob_hash.digest()),
                })
                .collect(),
        }
    }
}

/// How one item's attachments open — the only thing the facet's two positions
/// vary ([`RoomFacetSource`]); what bounds the work, and where the decrypt
/// runs, is shared.
enum FacetKey {
    /// A room message's attachments: the second per-kind content kind, off the
    /// generation the message itself opened under.
    Message {
        key: fauna_core::crypto::GenerationKey,
        generation: [u8; 32],
    },
    /// A room-restricted post's media: the post's own per-post key.
    Post { per_post_key: [u8; 32] },
}

impl FacetKey {
    /// Open one sealed attachment, or `None` with the reason logged. An AEAD
    /// over up to [`fauna_labeler::LABELER_ATTACHMENT_BYTES_MAX`] — CPU-bound,
    /// so its one caller runs it through [`open_off_the_worker`].
    fn open(&self, sealed: &[u8]) -> Option<Vec<u8>> {
        // Where the decrypt ran, for the tests that drive the real source: a
        // thread id taken here, beside the AEAD, is what an edit that inlined
        // the call — or bypassed this method for the crypto directly — cannot
        // keep looking right.
        #[cfg(test)]
        tests::facet_decrypts::record(sealed);
        match self {
            Self::Message { key, generation } => {
                fauna_mls::room_message::open_room_attachment(key, generation, sealed)
                    .map_err(|e| {
                        tracing::warn!(
                            "room labels: an attachment did not open under the tip: {e}; withheld"
                        );
                    })
                    .ok()
            }
            Self::Post { per_post_key } => {
                fauna_core::subscription::crypto::decrypt_content(per_post_key, sealed)
                    .map_err(|e| {
                        tracing::warn!(
                            "room labels: a post's media did not open under its per-post key: \
                             {e}; withheld"
                        );
                    })
                    .ok()
            }
        }
    }
}

/// The nest's own measurement of each candidate's **sealed** length — the
/// metadata row it wrote when it received the upload
/// (`db::blobs::get_blob_sizes`, one batched query rather than N point
/// reads).
///
/// **This is what makes a pre-read bound possible, and why it is admissible
/// where [`fauna_labeler::FacetCandidate::size_bytes`] is not.** That number
/// is the author's own declaration, written inside a sealed body no send-time
/// check can bound; this one is what this nest measured and stored. A sealed
/// length is its plaintext's plus framing under every position's key model,
/// so the facet loop's two sealed-length refusals can both be decided from it
/// *before* the blob is read — which is the whole of rule (4)'s "a candidate
/// that cannot be admitted is never fetched and never decrypted"
/// (`content-moderation-and-ranking.md` § Tier-3 → *The attachment facet*).
/// Without it, an author naming 64 distinct pinned uploads that each declare
/// a few bytes over blobs at the upload ceiling was charged 64 full reads per
/// message, inline in the send path.
///
/// ⚠ **An absent answer means "no answer", never "refuse".** A blob may be
/// stored with no metadata row behind it, and a failed query is not evidence
/// about any blob — either way the loop falls through to the read it did
/// before this existed, and the honest facet survives. Same trap, and the
/// same resolution, as the empty-refs rule beside it.
///
/// Only the candidates the loop could still read are asked about: past the
/// per-item candidate ceiling nothing is fetched, so nothing there is worth a
/// row in the query either, and the addresses are deduped because that is
/// what the loop itself does with them.
async fn sealed_sizes_for(
    state: &Arc<AppState>,
    candidates: &[fauna_labeler::FacetCandidate],
) -> std::collections::HashMap<[u8; 32], u64> {
    let addresses: Vec<[u8; 32]> = candidates
        .iter()
        .take(fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES)
        .filter_map(|c| c.address)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if addresses.is_empty() {
        return std::collections::HashMap::new();
    }
    match state.db.get_blob_sizes(&addresses).await {
        Ok(sizes) => sizes
            .into_iter()
            // A stored size that will not fit a `u64` is not an answer. The
            // column is signed; a negative there would be nonsense, and
            // "no answer" is the safe reading of nonsense here because it
            // costs a read rather than a wrong withholding.
            .filter_map(|(hash, n)| u64::try_from(n).ok().map(|n| (hash, n)))
            .collect(),
        Err(e) => {
            tracing::warn!(
                "room labels: reading the candidates' stored sizes failed: {e}; the \
                 facet is bounded by its post-read ceilings alone"
            );
            std::collections::HashMap::new()
        }
    }
}

/// Run one attachment's decrypt **off the async worker**.
///
/// An AEAD over up to [`fauna_labeler::LABELER_ATTACHMENT_BYTES_MAX`] is
/// CPU-bound work of the same class as the `wasm` run that follows it, and
/// the facet is built inline in the send path — so doing it on the worker
/// stalls every other connection sharing that thread, and a hostile item
/// (many candidates, none of which open) stalls it repeatedly. `spawn_blocking`
/// puts it on the blocking pool instead, where a slow open costs one pool
/// thread. The failure of a blocking task is the same withholding as a failed
/// open: the facet is built either way, never half-built.
///
/// Deliberately NOT `block_in_place`, which panics on a current-thread runtime
/// — which is what the tests run on.
async fn open_off_the_worker<F>(open: F) -> Option<Vec<u8>>
where
    F: FnOnce() -> Option<Vec<u8>> + Send + 'static,
{
    match tokio::task::spawn_blocking(open).await {
        Ok(opened) => opened,
        Err(e) => {
            tracing::warn!("room labels: an attachment's open task did not finish: {e}; withheld");
            None
        }
    }
}

/// One blob-store read as the facet loop wants it: the bytes, or `None` with
/// the reason logged — a blob this nest does not hold is an ordinary state
/// (a relayed item whose bytes never arrived), not an error.
fn fetched_blob(read: anyhow::Result<Option<Vec<u8>>>) -> Option<Vec<u8>> {
    match read {
        Ok(Some(sealed)) => Some(sealed),
        Ok(None) => {
            tracing::warn!("room labels: an attachment's blob is not stored here; withheld");
            None
        }
        Err(e) => {
            tracing::warn!("room labels: reading an attachment's blob failed: {e}; withheld");
            None
        }
    }
}

/// Purpose 1 of the home nest's read: fold one message's text into the room's
/// search corpus and record where it came from.
async fn index_room_text(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    seq: i64,
    text: &str,
    generation: &[u8; 32],
) {
    let schema = crate::db::CacheDb::room_view_schema(channel_id);
    let doc_id = seq.to_string();
    if let Err(e) = state
        .db
        .index_document(
            &schema,
            &doc_id,
            "",
            text,
            "",
            "",
            crate::db::now_epoch_micros(),
        )
        .await
    {
        tracing::warn!("room view: indexing failed: {e}");
        return;
    }
    // The half that makes the corpus answerable: an FTS row is keyed by a
    // one-way hash of `(schema, seq)`, so a hit can name the message it came
    // from only if the nest wrote the map down when it indexed it
    // (`fauna.conversations.room.search`). Recorded after the index and never
    // before, so the map can only ever be a subset of what is searchable —
    // a seq the corpus does not hold is a hit nobody can produce, whereas a
    // hit the map cannot resolve is a message the door silently drops.
    //
    // The generation rides with it because the door serves a position only to
    // a member holding that generation's wrap: the map row is where
    // "which key opens this position" is written down, and nothing later can
    // recover it — the sealed record has it, but the door reads the map.
    if let Err(e) = state
        .db
        .record_room_message_view(
            channel_id,
            seq,
            &crate::db::content_id_for_document(&schema, &doc_id),
            generation,
        )
        .await
    {
        tracing::warn!("room view: recording the seq map failed: {e}");
    }
}

/// Purpose 2 of the home nest's read: run the transparent labelers the room's
/// signed set names over one message the nest just opened, and write what they
/// derive onto the bus (`conversation-rooms.md` § The three classes → *What
/// the home nest does with its read*; placement `content-scoring.md` § The
/// placement matrix, the *capability-holder* row).
///
/// Only the room's own choice runs — a labeler being published on this nest is
/// no licence to run it over anybody's room — and only the shared pure scorers
/// run it (§ Don't do these: never fork a scorer by execution position). Best
/// effort like the rest of the act that called it: a labeler that fails, runs
/// out of fuel, or has left the registry since it was named contributes
/// nothing, and the member's send stands either way.
async fn label_room_message(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    seq: i64,
    input: fauna_core::scoring::LabelerPostInput,
    attachments: &[fauna_mls::types::ChannelAttachment],
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
) {
    let Some((labels, scores)) = run_room_labelers(
        state,
        room_id,
        input,
        RoomFacetSource::Message {
            attachments,
            key,
            generation,
            room_id,
            seq,
        },
    )
    .await
    else {
        return;
    };
    if let Err(e) = state
        .db
        .record_room_message_bus(
            room_id,
            seq,
            &labels,
            &scores,
            &state.nest_identity.public_key_bytes(),
        )
        .await
    {
        tracing::warn!("room labels: recording the bus rows failed: {e}");
    }
}

/// Run the transparent labelers `room_id`'s signed set names over one piece of
/// content the nest just opened — a message (purpose 2) or a room-restricted
/// post (purpose 3, `room_post_view::index_room_post`) — and reduce what they
/// derive to bus rows. One home for the set read, the registry lookup and the
/// blocking scorer run, so the two passes can never disagree on *what* runs;
/// which table the rows land in is the caller's.
///
/// `facet` says where this item's attachment bytes come from for a labeler
/// that declared it reads them ([`RoomFacetSource`] — a message's attachments
/// under its generation, a post's media under its per-post key); a caller with
/// none passes its own arm's empty slice and no bytes are ever opened.
///
/// `None` when nothing ran: no set, a set naming nothing still published, or
/// a scoring task that did not finish — every one best effort, like the act
/// that called it.
pub(crate) async fn run_room_labelers(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    input: fauna_core::scoring::LabelerPostInput,
    facet: RoomFacetSource<'_>,
) -> Option<(
    Vec<crate::db::room_labels::RoomLabel>,
    Vec<fauna_core::scoring::ScoreEntry>,
)> {
    let blob = match state.db.get_room_labelers(room_id).await {
        Ok((_, Some(blob))) => blob,
        Ok((_, None)) => return None,
        Err(e) => {
            tracing::warn!("room labels: reading the room's labeler set failed: {e}");
            return None;
        }
    };
    let set: fauna_mls::room_policy::SignedRoomLabelers = match fauna_protocol::decode_strict(&blob)
    {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!("room labels: the stored labeler set does not decode: {e}");
            return None;
        }
    };
    let mut records = Vec::with_capacity(set.labelers.labelers.len());
    for id in &set.labelers.labelers {
        match state.db.get_labeler(&id.0).await {
            Ok(Some(record)) => records.push(record),
            Ok(None) => tracing::warn!(
                "room labels: labeler {} is named by a room but no longer published here",
                hex::encode(id.0)
            ),
            Err(e) => tracing::warn!("room labels: labeler registry read failed: {e}"),
        }
    }
    if records.is_empty() {
        return None;
    }
    // The attachment bytes are opened only when a named `wasm` labeler
    // declared it reads them — the declaration is in its signed metadata —
    // and once for all of them.
    let facet = if !facet.is_empty() && records.iter().any(labeler_declares_attachment_bytes) {
        facet.open(state).await
    } else {
        Vec::new()
    };
    // A `wasm` run is CPU-bound for up to its fuel ceiling, so it leaves the
    // async executor rather than stall every other connection behind it.
    let scored = tokio::task::spawn_blocking(move || {
        score_with_room_labelers(&records, &input, &facet)
        // `facet` drops here, with the pass: nothing keeps the bytes.
    })
    .await;
    match scored {
        Ok(scored) => Some(scored),
        Err(e) => {
            tracing::warn!("room labels: the scoring task did not finish: {e}");
            None
        }
    }
}

/// Does this published `wasm` labeler declare that it reads attachment bytes
/// (`LabelerInput::needs_attachment_bytes`)? Read off the signed metadata the
/// registry stored; a record that does not decode declares nothing.
fn labeler_declares_attachment_bytes(record: &crate::db::labelers::LabelerRecord) -> bool {
    record.artifact_kind == fauna_core::scoring::artifact_kind::WASM
        && fauna_core::encoding::canonical_decode::<fauna_core::scoring::AlgorithmLabeler>(
            &record.metadata_blob,
        )
        .map(|m| m.input_schema.needs_attachment_bytes)
        .unwrap_or(false)
}

/// Run each named labeler over one message and reduce what it says to bus
/// rows: one tier-3 factor row per labeler that ran, and — for a `wasm`
/// labeler — its verdicts in the canonical five as category rows.
///
/// A `text-model` scores without naming a category, so it lands on the factor
/// plane only; so does a `wasm` label outside the canonical five, which no
/// labeler may put on the category plane (`moderation.md` § Categories &
/// enforcement) — it still counts toward its labeler's score, which is the
/// labeler's own output. A `text-model` at a tokenizer version this build does
/// not implement stays **inert**, the subscriber's client's rule exactly: a
/// silent mis-score is the one outcome that version exists to prevent.
fn score_with_room_labelers(
    records: &[crate::db::labelers::LabelerRecord],
    input: &fauna_core::scoring::LabelerPostInput,
    facet: &[fauna_core::scoring::LabelerAttachmentInput],
) -> (
    Vec<crate::db::room_labels::RoomLabel>,
    Vec<fauna_core::scoring::ScoreEntry>,
) {
    use fauna_core::content_category::ContentCategory;
    use fauna_core::scoring::{
        ScoreEntry, TIER_COMMUNITY, artifact_kind, labels_to_score_entry,
        text_model_version_supported, validate_text_model_artifact,
    };
    let mut labels = Vec::new();
    let mut scores = Vec::new();
    for record in records {
        let Ok(labeler_id) = <[u8; 32]>::try_from(record.labeler_id.as_slice()) else {
            continue;
        };
        let version = u32::try_from(record.version).unwrap_or(u32::MAX);
        match record.artifact_kind.as_str() {
            artifact_kind::WASM => {
                // The facet reaches only a module whose signed metadata
                // declared it — the shared boundary reads the declaration and
                // encodes the shape the module reads, so an undeclaring
                // module gets the v1 bytes it always did.
                // The set names the labeler by id; the shared boundary pins
                // the stored artifact to that id, so a swapped registry row
                // cannot run another publisher's module under this name.
                let out = match fauna_labeler::run_published_labeler_with_attachments(
                    &record.metadata_blob,
                    &record.wasm_bytes,
                    &fauna_core::identity::ActorId(labeler_id),
                    input,
                    facet,
                ) {
                    Ok(out) => out,
                    Err(e) => {
                        tracing::warn!(
                            "room labels: labeler {} did not run: {e:#}",
                            hex::encode(labeler_id)
                        );
                        continue;
                    }
                };
                scores.push(labels_to_score_entry(&out, record.factor.clone(), version));
                for label in out {
                    // A module's confidence is untrusted: NaN never lands, and
                    // the rest is held to the column's documented [0, 1].
                    if !label.confidence.is_finite()
                        || ContentCategory::from_wire(&label.category) == ContentCategory::Other
                    {
                        continue;
                    }
                    labels.push(crate::db::room_labels::RoomLabel {
                        category: label.category,
                        confidence: label.confidence.clamp(0.0, 1.0),
                        labeler_id,
                        labeler_version: record.version,
                    });
                }
            }
            artifact_kind::TEXT_MODEL => {
                // A model over no text says nothing — an attachment with no
                // caption is left to the labelers that read its metadata.
                let Some(text) = input.text.as_deref() else {
                    continue;
                };
                let artifact = match validate_text_model_artifact(&record.wasm_bytes) {
                    Ok(artifact) => artifact,
                    Err(e) => {
                        tracing::warn!(
                            "room labels: text-model {} does not validate: {e}",
                            hex::encode(labeler_id)
                        );
                        continue;
                    }
                };
                if !text_model_version_supported(artifact.version) {
                    continue;
                }
                let model = fauna_text_model::publish::PublishedTextModel::new(
                    artifact.more_docs,
                    artifact.less_docs,
                    artifact
                        .ngrams
                        .into_iter()
                        .map(|n| (n.ngram, n.more, n.less)),
                );
                scores.push(ScoreEntry {
                    factor: record.factor.clone(),
                    score: model.damped_score(text),
                    tier: TIER_COMMUNITY,
                    scorer_version: version,
                });
            }
            // Named while it was a scoring kind, republished since as one that
            // cannot score a message.
            _ => {}
        }
    }
    (labels, scores)
}

/// This nest's own live roster entry on a room's floor, when it has one —
/// `(entry_id, reception_pubkey)`.
///
/// Everything the nest does with its read goes through here, so "the nest
/// reads a room only while its membership says so" is one fact in one place
/// rather than a condition each caller remembers. `None` covers all three
/// honest absences: the nest is not on this floor, it was seated before the
/// sealing plane existed, or it holds no room-read key.
async fn nest_room_entry(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
) -> Result<Option<[u8; 32]>, RpcError> {
    let nest_principal = state.nest_identity.public_key_bytes();
    let row = state
        .db
        .list_floor_roster(room_id)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?
        .into_iter()
        .find(|m| m.principal_id == nest_principal && m.principal_kind == "nest");
    Ok(row
        .and_then(|m| m.entry_id)
        .and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok()))
}

/// Open the room's tip generation as the home nest, or `None` when this nest
/// holds no wrap for it — the state a **revoke** leaves behind.
///
/// Deliberately keyed on the **tip** rather than on the generation an
/// individual envelope names. Once the members have rotated the nest out, the
/// nest builds no more views for the room even from a message somebody seals
/// under an older generation it still holds a wrap for; otherwise a revoke
/// would delete the views and then quietly let one resurrect.
///
/// Shared with the room-restricted post pass (`crate::room_post_view`), which
/// is bound by exactly the same rule.
pub(crate) async fn nest_room_tip_key(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
) -> Result<Option<(fauna_core::crypto::GenerationKey, [u8; 32])>, RpcError> {
    let Some(entry_id) = nest_room_entry(state, room_id).await? else {
        return Ok(None);
    };
    let Some(tip) = state
        .db
        .room_generation_tip(room_id)
        .await
        .map_err(|e| internal(format!("generation tip: {e}")))?
    else {
        return Ok(None);
    };
    let Some(wrap) = state
        .db
        .get_room_generation_wrap(room_id, &tip.generation_id, &entry_id)
        .await
        .map_err(|e| internal(format!("generation wrap: {e}")))?
    else {
        return Ok(None);
    };
    let Some(seed) = state.nest_signing_key.as_ref().map(|k| k.to_bytes()) else {
        return Ok(None);
    };
    let db = state.db.clone();
    let record = tokio::task::spawn_blocking(move || {
        crate::room_read_key::room_read_key(&db.conn_blocking(), &seed)
    })
    .await
    .map_err(|e| internal(format!("room-read key task: {e}")))?
    .map_err(|e| internal(format!("room-read key: {e}")))?;
    let keypair = record
        .record
        .keypair()
        .map_err(|e| internal(format!("room-read keypair: {e}")))?;
    let commitment = <[u8; 32]>::try_from(tip.key_commitment.as_slice())
        .map_err(|_| internal("stored key commitment is not 32 bytes"))?;
    // A wrap that does not open is NOT an error the caller hears about: it is
    // the nest's own read failing, and the send it rode in on is a member's
    // legitimate act either way. The room simply gets no derived view.
    match fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry(
        &wrap,
        &keypair.secret,
        &tip.generation_id,
        &entry_id,
        &commitment,
    ) {
        Ok(key) => Ok(Some((key, tip.generation_id))),
        Err(e) => {
            tracing::warn!("home nest could not open its room generation wrap: {e}");
            Ok(None)
        }
    }
}

// ── fauna.conversations.room.publish_generation ────────────────

fn room_publish_generation_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.publish_generation",
            )
            .await?;
            let req: RoomPublishGenerationRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            floor_authoritative_room(&state, &room_id).await?;

            // Key authority is the room's owner and admins, never the nest
            // (§ Don't do these). This is the room plane's fill of the
            // scheme's declared authority seam — the floor's ranks stand in
            // for "the initiator's device fleet", exactly as T19 stands the
            // box's admin set in.
            let role = floor_role(&state, &room_id, &actor_id).await?;
            if !role.is_admin_or_owner() {
                return Err(permission_denied(
                    "only an owner or admin mints a room generation",
                ));
            }

            let record: fauna_core::group_generation::GroupGenerationMintRecord =
                fauna_protocol::codec::decode_strict(&req.mint)
                    .map_err(|e| invalid_params(&format!("malformed generation mint: {e}")))?;
            let fauna_core::group_generation::GroupGenerationMintRecord::Minted {
                core,
                minter_sig,
                wraps,
                ..
            } = &record
            else {
                return Err(invalid_params(
                    "a room generation is published as a Minted record",
                ));
            };

            // The minter is bound to the authenticated caller, the same way
            // `room.invite` binds an invitation's signer: the floor knows
            // actors, so an actor-signed mint is what its ranks can judge.
            if core.minter != actor_id {
                return Err(permission_denied(
                    "a room generation's minter must be the calling principal",
                ));
            }
            let generation_id = fauna_core::group_generation::group_generation_id(core)
                .map_err(|e| invalid_params(&format!("mint core does not encode: {e}")))?;
            fauna_core::group_generation::verify_group_mint_minter_sig(
                &core.minter,
                &generation_id,
                minter_sig,
            )
            .map_err(|e| {
                permission_denied(&format!("the mint's minter signature does not verify: {e}"))
            })?;

            // The chain is a strict ratchet on the room's tip: a mint must
            // name the generation it replaces, so two concurrent rotations
            // cannot both land and leave the room with a fork the nest would
            // have to resolve — the arbiter this plane deliberately has not
            // got. A room's first mint names no parent.
            let tip = state
                .db
                .room_generation_tip(&room_id)
                .await
                .map_err(|e| internal(format!("generation tip: {e}")))?;
            let expected_parent = tip.as_ref().map(|t| t.generation_id);
            let named_parent = match core.parents.as_slice() {
                [] => None,
                [one] => Some(*one),
                _ => {
                    return Err(invalid_params(
                        "a room generation names exactly one parent — its predecessor",
                    ));
                }
            };
            if named_parent != expected_parent {
                return Err(invalid_params(
                    "this mint does not name the room's current generation as its parent",
                ));
            }

            // **Roster coverage** — the scheme's admissibility rule with the
            // escrow arm replaced (`account-data-taxonomy.md` § The
            // recipient-set scheme, delta (ii)): every live floor principal
            // that has a wrap target must have a wrap, or somebody the room
            // says is a member holds ciphertext they cannot open. A member
            // with no reception key yet is not coverable and is skipped —
            // the state the scheme's member top-up heals.
            //
            // ⚠ **The home nest is deliberately outside the coverage
            // requirement** (ruled at the sealing build, 2026-09-09, after the
            // conformance suite caught the two rules contradicting each
            // other). Coverage and revoke are otherwise mutually exclusive:
            // the nest is a live floor member, so requiring a wrap for it
            // would make "revocable by rotating it out" — the ratified shape
            // of the nest's readable position (`conversation-rooms.md` § The
            // three classes → *Community*, reason 1) — unexpressible, and the
            // grant would be irrevocable in practice.
            //
            // The asymmetry is the class's own: coverage exists so that
            // nobody the room says is a MEMBER holds ciphertext they cannot
            // open, and a member who cannot read is broken. The home nest is
            // a **reader recipient, never in the authority set** (§ Don't do
            // these), so a nest that cannot read is not broken — it is the
            // members having withdrawn a grant that was theirs to give. The
            // withdrawal is never silent: the reply says `nest_read_revoked`
            // and the views go with it.
            let roster = state
                .db
                .list_floor_roster(&room_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?;
            let wrapped: std::collections::BTreeSet<[u8; 32]> =
                wraps.iter().map(|w| w.entry_id).collect();
            let mut covered = 0u64;
            let mut nest_covered = false;
            let nest_principal = state.nest_identity.public_key_bytes();
            for m in &roster {
                let (Some(entry), Some(recv)) = (m.entry_id.as_ref(), m.reception_pubkey.as_ref())
                else {
                    continue;
                };
                // A stored key that fails validation is exactly as
                // uncoverable as no key at all — never a coverage
                // failure that freezes the mint, because no writer can put a
                // fresh invalid key here going forward and this member's own
                // door heals an old one the moment it is asked to.
                if !usable_reception_key(recv) {
                    continue;
                }
                let Ok(entry) = <[u8; 32]>::try_from(entry.as_slice()) else {
                    continue;
                };
                let is_home_nest = m.principal_id == nest_principal;
                if !wrapped.contains(&entry) {
                    if is_home_nest {
                        // The revoke. Not a coverage failure — see above.
                        continue;
                    }
                    return Err(invalid_params(
                        "this mint does not wrap to every live member of the room's floor",
                    ));
                }
                covered += 1;
                if is_home_nest {
                    nest_covered = true;
                }
            }

            let row = crate::db::rooms::RoomGenerationRow {
                generation_id,
                parent_id: named_parent.map(|p| p.to_vec()),
                key_commitment: core.key_commitment.to_vec(),
                minted_by: actor_id.to_vec(),
                mint_blob: req.mint.clone(),
                minted_at_ms: core.minted_at_ms,
            };
            let wrap_rows: Vec<([u8; 32], Vec<u8>)> =
                wraps.iter().map(|w| (w.entry_id, w.wrap.clone())).collect();
            // The insert re-checks the ratchet under its own lock, closing the
            // window between the tip read above and this write: a concurrent
            // rotation that landed in between makes this mint a second child
            // of the old tip, so it is refused exactly as a stale mint is.
            match state
                .db
                .insert_room_generation(&room_id, &row, &wrap_rows)
                .await
                .map_err(|e| {
                    tracing::error!("insert_room_generation: {e}");
                    internal("storage error")
                })? {
                crate::db::rooms::RoomGenerationInsert::Stored => {}
                crate::db::rooms::RoomGenerationInsert::NotTheTip => {
                    return Err(invalid_params(
                        "this mint does not name the room's current generation as its parent",
                    ));
                }
            }

            // The REVOKE arm. A mint that leaves the home nest's entry out of
            // its wrap set is the members withdrawing the materialization
            // grant (`principles.md` § The user always controls their data —
            // "revoke deletes the views"), so the views go in the same act
            // rather than at some later sweep the user cannot observe.
            let nest_seated = nest_room_entry(&state, &room_id).await?.is_some();
            let revoked = nest_seated && !nest_covered;
            if revoked {
                let gone = state
                    .db
                    .purge_room_derived_views(&room_id)
                    .await
                    .map_err(|e| internal(format!("purge derived views: {e}")))?;
                tracing::info!(
                    "room read grant revoked by rotation — {gone} derived view row(s) deleted"
                );
            }

            encode_reply(&RoomPublishGenerationReply {
                generation_id: hex::encode(generation_id),
                covered,
                nest_read_revoked: revoked,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.backfill_generations ──────────────

/// The generations a room's history policy authorizes backfilling to a
/// newcomer, newest-relevant last — the ratified rule in one place.
///
/// `Full` authorizes the whole **retained bundle** ("a newcomer receives the
/// room's history", § History for joiners), which is the scheme's own move
/// for an add (`account-data-taxonomy.md` § The recipient-set scheme →
/// *Mint triggers*: "an **add** wraps the **retained generation bundle** —
/// tip *and* the generations still covering live content").
///
/// `None` — every room's default — authorizes the **tip alone**. The policy
/// says a newcomer "sees the room from their admission; nothing before", and
/// every earlier generation is exactly that history. The tip is not history:
/// coverage is checked at MINT time over the floor as it stood then, so a
/// member seated afterwards holds no wrap for the tip either and cannot read
/// the room's *new* messages until something covers it. Withholding the tip
/// would not enforce a history policy, it would leave a newcomer unable to
/// read the room at all — the admission/mint race the scheme's member top-up
/// exists to heal.
fn authorized_backfill_set(
    policy: &fauna_mls::room_policy::RoomPolicy,
    generations: &[crate::db::rooms::RoomGenerationRow],
) -> std::collections::BTreeSet<[u8; 32]> {
    if policy.history_policy == fauna_mls::room_policy::HistoryPolicy::Full {
        generations.iter().map(|g| g.generation_id).collect()
    } else {
        generations
            .last()
            .map(|tip| std::iter::once(tip.generation_id).collect())
            .unwrap_or_default()
    }
}

/// `fauna.conversations.room.backfill_generations` — cover a newly-seated
/// member with the generations its room's history policy allows.
///
/// The **add** half of the recipient-set scheme's mint triggers: a remove
/// mints (the severance rotation, `room.publish_generation`), an add does
/// not — it wraps what already exists to the new entry. So this door can
/// never change the room's tip, and it stores nothing until every wrap in
/// the batch is authorized.
///
/// **Adds coverage, never replaces it.** A wrap already stored for a
/// `(generation, entry)` is left untouched — no sanctioned path re-seals a
/// wrap already at an entry (`community-rooms.md` § Implementation status
/// today → *A seat gains or rotates its wrap target*, rule (b): "the wraps
/// already stored at the entry stay openable and nothing is re-sealed"), so
/// this door has no re-key act to perform. A repeat backfill over existing
/// coverage is a no-op, not an error.
///
/// Five checks, and no more:
///
/// 1. **A floor-authoritative room** — the community class. An end-to-end
///    room's history slice is a member-produced state replica, not a wrap
///    (§ History for joiners), and its nest stores no key material at all.
/// 2. **The caller holds `owner` or `admin`** — the same rank the mint takes.
///    The act hands out a generation key, so it is key authority, and key
///    authority is the room's owner and admins, never a plain member and
///    never the nest (§ Don't do these).
/// 3. **The target is a live floor member with a wrap target**, and is a
///    **user** principal. Wrapping to a stranger would hand the room's key to
///    a principal the room does not say is a member; wrapping to the *home
///    nest* is refused for a sharper reason — the nest's read is a grant the
///    members make at a mint and withdraw by rotating it out, so a backfill
///    that could re-wrap to the nest would let one admin restore a read the
///    members had just revoked, outside the single act the design gives them.
/// 4. **Each record verifies at its own cell** (`GroupTopupRecord::
///    verifies_at`) with the healer bound to the authenticated caller and the
///    target entry bound to the target's **current** roster entry. The
///    current-entry binding is what keeps a re-admission's fresh entry id
///    meaningful: a wrap minted for the seat a member was removed from must
///    not become openable at the seat it came back on.
/// 5. **The history policy authorizes the generation**
///    ([`authorized_backfill_set`]) — "in a community room the nest refuses
///    to store one" (§ History for joiners). This is the clause that makes
///    `history_policy` mean something on this class.
fn room_backfill_generations_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.backfill_generations",
            )
            .await?;
            let req: RoomBackfillGenerationsRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            floor_authoritative_room(&state, &room_id).await?;

            // (2) Key authority — the mint's rank, for the same reason.
            let role = floor_role(&state, &room_id, &actor_id).await?;
            if !role.is_admin_or_owner() {
                return Err(permission_denied(
                    "only an owner or admin backfills a room generation",
                ));
            }

            let target = parse_room_id(&req.target_actor_id)
                .map_err(|_| invalid_params("target_actor_id must be a 32-byte hex actor id"))?;

            // (3) A live floor member with a wrap target — and never the
            // home nest, whose read is a mint-time grant.
            let roster = state
                .db
                .list_floor_roster(&room_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?;
            let Some(row) = roster.iter().find(|m| m.principal_id == target) else {
                return Err(permission_denied(
                    "the backfill target is not a live member of this room's floor",
                ));
            };
            if row.principal_kind == "nest" {
                return Err(permission_denied(
                    "the home nest's read is a grant the members make at a mint and revoke by \
                     rotating it out — it is never restored by a backfill",
                ));
            }
            let (Some(entry), Some(recv)) = (row.entry_id.as_ref(), row.reception_pubkey.as_ref())
            else {
                return Err(invalid_params(
                    "the backfill target holds no wrap target — it cannot be covered yet",
                ));
            };
            // An invalid stored key is exactly as uncoverable as no key at
            // all — the same predicate the coverage rule and the
            // roster read apply to this column.
            if !usable_reception_key(recv) {
                return Err(invalid_params(
                    "the backfill target holds no wrap target — it cannot be covered yet",
                ));
            }
            let entry_id = <[u8; 32]>::try_from(entry.as_slice())
                .map_err(|_| internal("a stored roster entry id is not 32 bytes"))?;

            // (5)'s input: what the room retains, and what the policy lets
            // this newcomer see of it.
            let generations = state
                .db
                .list_room_generations(&room_id)
                .await
                .map_err(|e| internal(format!("generation list: {e}")))?;
            let policy = stored_room_policy(&state, &room_id).await?;
            let authorized = authorized_backfill_set(&policy.policy, &generations);

            if req.wraps.is_empty() {
                return Err(invalid_params("a backfill carries at least one wrap"));
            }

            // Verify the WHOLE batch before storing any of it: a partial
            // bundle would be a history slice nobody authorized, left behind
            // by a refusal.
            let mut rows: Vec<([u8; 32], Vec<u8>)> = Vec::with_capacity(req.wraps.len());
            for bytes in &req.wraps {
                let record: fauna_core::group_generation::GroupTopupRecord =
                    fauna_protocol::codec::decode_strict(bytes)
                        .map_err(|e| invalid_params(&format!("malformed top-up wrap: {e}")))?;
                let fauna_core::group_generation::GroupTopupRecord::Wrap {
                    generation_id,
                    target_entry,
                    healer,
                    wrap,
                    ..
                } = &record;

                // (4) The healer is the authenticated caller — the same
                // binding the mint puts on its minter, so the floor's ranks
                // judge the principal that actually signed.
                if healer != &actor_id {
                    return Err(permission_denied(
                        "a top-up wrap's healer must be the calling principal",
                    ));
                }
                // (4) …and the wrap is bound to the target's CURRENT entry.
                if target_entry != &entry_id {
                    return Err(invalid_params(
                        "a top-up wrap names a roster entry the target no longer holds",
                    ));
                }
                if !record.verifies_at(generation_id, &entry_id, &actor_id) {
                    return Err(permission_denied(
                        "the top-up wrap's healer signature does not verify",
                    ));
                }
                // The generation must exist — a wrap for one the room never
                // had is unreadable noise, and storing it would make the
                // read door answer with a generation the log never used.
                if !generations
                    .iter()
                    .any(|g| &g.generation_id == generation_id)
                {
                    return Err(invalid_params(
                        "a top-up wrap names a generation this room does not retain",
                    ));
                }
                // (5) The history policy.
                if !authorized.contains(generation_id) {
                    return Err(permission_denied(
                        "this room's history policy does not authorize backfilling that \
                         generation — under `none` a newcomer sees the room from its admission",
                    ));
                }
                rows.push((*generation_id, wrap.clone()));
            }

            let stored = state
                .db
                .backfill_room_generation_wraps(&room_id, &entry_id, &rows)
                .await
                .map_err(|e| {
                    tracing::error!("backfill_room_generation_wraps: {e}");
                    internal("storage error")
                })?;

            encode_reply(&RoomBackfillGenerationsReply {
                stored: stored as u64,
                retained: generations.len() as u64,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.set_reception_key ─────────────────

/// `fauna.conversations.room.set_reception_key` — a live member supplies, or
/// rotates, the wrap target of the seat it already holds
/// (`community-rooms.md` § Implementation status today, *A seat gains or
/// rotates its wrap target*).
///
/// Every other writer of `room_members.reception_pubkey` is a seating act.
/// The seats that exist without a key — a succession's successor, which the
/// ceremony deliberately seats keyless, and a member seated before the sealing
/// plane — governed the room and opened nothing minted after them: coverage
/// skips a keyless seat and the backfill refuses one. This is the door that
/// makes such a seat keyable, and the door a key rotation lands on.
///
/// Four checks:
///
/// 1. **A floor-authoritative room** — the community class; a mirror room has
///    no generations and its rows carry no wrap target.
/// 2. **The key parses AND validates as an X-Wing public key** —
///    the one shape every wrap seals to, checked by the same
///    [`fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate`] a mint's
///    seal runs. A key that only *parses* but fails FIPS 203 would still make
///    the seat *coverable* and every mint over it unopenable — worse than
///    keyless, and worse than a length check alone catches.
/// 3. **The caller holds a live USER seat** — [`CacheDb::set_room_member_reception_key`]
///    binds only that. Nobody sets another principal's key: the nest's read is
///    a mint-time grant, a bridge is seated by its enrollment.
/// 4. Nothing about rank — every member owns its own wrap target, as it does
///    at acceptance.
///
/// **A key already set is replaced, never refused.** The scheme's wrap target
/// is the member's *current* reception key, rotated on its own fleet events
/// (`account-data-taxonomy.md` § The recipient-set scheme → *Severance, per
/// axis*); the entry survives the rotation, for the reason the db method
/// gives. The door mints nothing — what the seat still owes is answered:
/// `uncovered_tip` names the room's current generation when this seat holds
/// no wrap for it, because `room.generations` would show such a seat nothing
/// and a mint has to name that tip as its parent.
fn room_set_reception_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.set_reception_key",
            )
            .await?;
            let req: RoomSetReceptionKeyRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            floor_authoritative_room(&state, &room_id).await?;

            // (2) The wrap target's one shape — never empty here (unlike
            // `room.create`/`accept_invite`, this door's whole point is
            // supplying a key).
            reject_invalid_reception_key(&req.reception_pubkey)?;

            // (3) The caller's own live user seat.
            let bound = state
                .db
                .set_room_member_reception_key(&room_id, &actor_id, &req.reception_pubkey)
                .await
                .map_err(|e| {
                    tracing::error!("set_room_member_reception_key: {e}");
                    internal("storage error")
                })?;
            let Some(bound) = bound else {
                return Err(permission_denied(
                    "only a live user member sets a wrap target, and only for its own seat",
                ));
            };

            // What the seat still owes — the tip it holds no wrap for.
            let tip = state
                .db
                .room_generation_tip(&room_id)
                .await
                .map_err(|e| internal(format!("generation tip: {e}")))?;
            let uncovered_tip = match tip {
                Some(tip) => {
                    let covered = state
                        .db
                        .get_room_generation_wrap(&room_id, &tip.generation_id, &bound.entry_id)
                        .await
                        .map_err(|e| internal(format!("generation wrap: {e}")))?
                        .is_some();
                    (!covered).then(|| hex::encode(tip.generation_id))
                }
                None => None,
            };

            encode_reply(&RoomSetReceptionKeyReply {
                entry_id: hex::encode(bound.entry_id),
                rotated: bound.rotated,
                uncovered_tip,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.generations ───────────────────────

/// The generations **one principal** holds a wrap for, as the read doors
/// serve them.
///
/// The whole of a room's key read, in one place, because it has two doors:
/// the same-nest `fauna.conversations.room.generations` below, and the
/// relayed `fauna.federation.conversation.generations.fetch` a foreign
/// member's own nest originates on its behalf
/// (`conversation-rooms.md` § The home nest — "a member on a foreign nest
/// reaches the room only through their own home nest"). Both must serve
/// exactly the caller's own wraps and nothing else, so they share the
/// resolution rather than each remembering the rule.
///
/// ⚠ **`principal` names the reader, and the entry is derived from it here.**
/// Neither door lets a caller name a roster *entry*: the entry is looked up
/// from the floor by the principal the door authenticated. That is what stops
/// a relaying nest asking for a wrap it is not the recipient of — including
/// substituting **itself**, whose own room-read entry it could otherwise
/// name. A nest that relays for its member carries ciphertext and no wrap
/// (§ The home nest), and this is where that stays true.
async fn room_generations_for_principal(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    principal: &[u8; 32],
) -> Result<Vec<RoomGenerationWire>, RpcError> {
    // A live floor member, of any rank — reading the room is what
    // every member does, and the roles table gates minting, not
    // holding a key. A caller off the floor gets the membership
    // refusal, not an empty list: silence would read as "this room
    // has no generations".
    let Some(entry_id) = state
        .db
        .list_floor_roster(room_id)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?
        .into_iter()
        .find(|m| &m.principal_id == principal)
        .and_then(|m| m.entry_id)
        .and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
    else {
        return Err(permission_denied("not a member of this room"));
    };

    let all = state
        .db
        .list_room_generations(room_id)
        .await
        .map_err(|e| internal(format!("generation list: {e}")))?;
    let tip = all.last().map(|g| g.generation_id);
    let mut out = Vec::new();
    for g in &all {
        // Only the reader's OWN wrap is served. A member that holds
        // no wrap for a generation simply does not see that
        // generation — which is exactly what severance looks like
        // from inside, and what the retained bundle an add is wrapped
        // looks like too.
        let Some(wrap) = state
            .db
            .get_room_generation_wrap(room_id, &g.generation_id, &entry_id)
            .await
            .map_err(|e| internal(format!("generation wrap: {e}")))?
        else {
            continue;
        };
        out.push(RoomGenerationWire {
            generation_id: hex::encode(g.generation_id),
            key_commitment: g.key_commitment.clone(),
            wrap,
            entry_id: hex::encode(entry_id),
            minted_at_ms: g.minted_at_ms,
            is_tip: tip == Some(g.generation_id),
            extra: std::collections::BTreeMap::new(),
        });
    }
    Ok(out)
}

/// The relayed twin's entry point: the same read, for a foreign member whose
/// own home nest has already been bound to it by the federation gate.
///
/// Lives here rather than in `federation_handlers` so that the room plane's
/// "serve only your own wraps" rule has exactly one implementation; the
/// federation side owns the *authorization* (`require_foreign_member`) and
/// nothing about what a member may read.
pub(crate) async fn room_generations_relayed(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    member: &[u8; 32],
) -> Result<Vec<RoomGenerationWire>, RpcError> {
    floor_authoritative_room(state, room_id).await?;
    room_generations_for_principal(state, room_id, member).await
}

fn room_generations_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.generations").await?;
            let req: RoomGenerationsRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            floor_authoritative_room(&state, &room_id).await?;

            encode_reply(&RoomGenerationsReply {
                generations: room_generations_for_principal(&state, &room_id, &actor_id).await?,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.conversations.room.search ────────────────────────────

/// Search a community room through the derived view its home nest built.
///
/// This is the read position's **first purpose made reachable**
/// (`conversation-rooms.md` § The three classes → *What the home nest does
/// with its read*): the nest has indexed every message it could open since
/// the sealing plane landed, and until this door there was nothing that could
/// ask the corpus a question — the index existed and served nobody.
///
/// Why the nest and not the member's own device: the community class is the
/// **unbounded** one. A member's client holds whatever slice of the log it
/// has fetched, which for a large community is a vanishing fraction, so
/// searching the room is precisely the work "that needs a reader the members
/// are not". Every other class searches client-side and this door refuses
/// them, because for them the members ARE the readers.
///
/// ⚠ **It answers `where`, never `what` — and only where the caller could
/// have read anyway.** The hits carry a log position and a rank and no text —
/// see [`fauna_protocol::conversations::RoomSearchHit`] for the reason: the
/// nest indexes under the **tip**, but a member holds a wrap only for the
/// generations minted while it sat on the floor, so a snippet would hand a
/// newly-seated member exactly the plaintext the sealing plane withholds
/// from it.
///
/// The same reasoning binds the POSITIONS, and that is the second
/// half of the rule: a position is an answer too, so serving one for a
/// message the caller can never open would make this door a chosen-plaintext
/// oracle over exactly the history the room's `history_policy` withholds —
/// one probe per term, answered by *where*. So every hit is bounded by the
/// caller's own wraps ([`wrap_bounded_page`]): a position it could not open
/// is a position it is not told about. A member backfilled the room's history
/// under `full` holds those generations and sees those hits; a member seated
/// under `none` sees the room from its admission, here as everywhere else.
///
/// A caller that sets `include_posts` also meets the room-restricted **posts**
/// the reception pass indexed (`crate::room_post_view`) — the same rules: a
/// post id and a rank, never the post's text, and only for a post the caller
/// holds the generation's wrap for.
fn room_search_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.search").await?;
            let req: RoomSearchRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;
            // A community room only: the floor is the membership authority
            // here, and it is the only class whose log the nest can read at
            // all. An end-to-end room's search is its members' own work.
            floor_authoritative_room(&state, &room_id).await?;

            // A live floor member of ANY rank. The roles table gates minting
            // and membership, never reading — and the refusal is explicit
            // rather than an empty list, for `room_generations_for_principal`'s
            // reason: silence would read as "this room holds nothing".
            let Some(seat) = state
                .db
                .list_floor_roster(&room_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?
                .into_iter()
                .find(|m| m.principal_id == actor_id)
            else {
                return Err(permission_denied("not a member of this room"));
            };

            if req.query.trim().is_empty() {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "conversations",
                    "query is required",
                ));
            }
            let limit = req
                .limit
                .unwrap_or(fauna_protocol::conversations::ROOM_SEARCH_DEFAULT_LIMIT)
                .clamp(1, fauna_protocol::conversations::ROOM_SEARCH_MAX_LIMIT)
                as usize;

            // ⚠ **The reader's roster ENTRY is what bounds the page**, and it
            // is derived here from the principal the door authenticated —
            // never named by the caller — for `room_generations_for_principal`'s
            // reason. A member holding no wrap target yet (the admission/mint
            // race, before the top-up heals it) holds no wrap for anything,
            // so its page is empty rather than refused: it IS a member, and
            // there is nothing in the corpus it could open.
            let Some(entry_id) = seat
                .entry_id
                .and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
            else {
                return encode_reply(&fauna_protocol::conversations::RoomSearchReply {
                    hits: Vec::new(),
                    extra: std::collections::BTreeMap::new(),
                });
            };

            // (BM25, hit): FTS5's BM25 is lower-is-better, and it is computed
            // over the whole corpus whichever class a query filters to, so a
            // message and a post match rank on one scale and merge by it.
            let mut ranked =
                wrap_bounded_page(&state, &room_id, &entry_id, &req.query, limit, false).await?;

            // Posts only for a caller that asked: a post hit carries no
            // `seq`, which a caller built before post hits requires
            // (`RoomSearchRequest::include_posts`). Their own class, so the
            // message page above is exactly what it always was.
            if req.include_posts {
                ranked.extend(
                    wrap_bounded_page(&state, &room_id, &entry_id, &req.query, limit, true).await?,
                );
                ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
                ranked.truncate(limit);
            }
            let hits = ranked.into_iter().map(|(_, hit)| hit).collect();

            encode_reply(&fauna_protocol::conversations::RoomSearchReply {
                hits,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

/// A BM25 score as a room-search hit carries it: negated (higher = more
/// relevant) and scaled to fixed-point micro-units — `search::SearchResult`'s
/// own convention, because the dag-cbor wire forbids floats.
fn room_search_rank(bm25: f64) -> i64 {
    (-bm25 * 1_000_000.0).round() as i64
}

/// How many ranked corpus rows one `room.search` call may walk while looking
/// for hits the caller can open.
///
/// The bound exists because the wrap filter is a *drop*: a member holding a
/// wrap for one generation of a long-lived room can match thousands of rows it
/// may not be served, and without a cap each query would walk the whole room's
/// corpus. At the cap the page simply ends — the honest answer, since the door
/// has never promised that a page shorter than `limit` means the corpus is
/// exhausted.
const ROOM_SEARCH_SCAN_CAP: usize = 1_000;

/// One page of a community room's derived corpus, **bounded by the reader's
/// wraps** — messages, or the room-restricted posts when `posts`.
///
/// ⚠ **Over-fetches, and must.** `search_fts` applies its own `LIMIT` before
/// anything here can drop a hit, so filtering one `limit`-sized page in place
/// would hand a member whose wraps cover part of the corpus a SHORT page —
/// which reads as "there is nothing more", the one thing a filtered page must
/// never say falsely. So this walks the ranked corpus in batches until it has
/// a full page or the corpus runs out, bounded by [`ROOM_SEARCH_SCAN_CAP`].
///
/// The drop itself lives in the map lookup (`CacheDb::room_message_seqs_for_docs`,
/// `CacheDb::room_post_ids_for_docs`), so this function never holds a position
/// it may not serve.
async fn wrap_bounded_page(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    entry_id: &[u8; 32],
    query: &str,
    limit: usize,
    posts: bool,
) -> Result<Vec<(f64, fauna_protocol::conversations::RoomSearchHit)>, RpcError> {
    use fauna_protocol::conversations::{RoomSearchHit, RoomSearchHitKind};

    let schema = if posts {
        crate::db::CacheDb::room_post_view_schema(room_id)
    } else {
        crate::db::CacheDb::room_view_schema(room_id)
    };
    let mut out: Vec<(f64, RoomSearchHit)> = Vec::new();
    let mut scanned = 0usize;
    while out.len() < limit && scanned < ROOM_SEARCH_SCAN_CAP {
        let batch = (limit * 4).min(ROOM_SEARCH_SCAN_CAP - scanned);
        let found = state
            .db
            .search_fts(
                query,
                Some(&schema),
                None,
                None,
                batch as i64,
                scanned as i64,
            )
            .await
            .map_err(|e| internal(format!("room search: {e}")))?;
        let exhausted = found.len() < batch;
        scanned += found.len();
        // `search_fts` hands back the FTS document key as hex and a snippet of
        // the body. The snippet is deliberately dropped here and never enters
        // the reply.
        let doc_keys: Vec<Vec<u8>> = found
            .iter()
            .map(|r| hex::decode(&r.content_id).unwrap_or_default())
            .collect();
        if posts {
            let ids = state
                .db
                .room_post_ids_for_docs(room_id, &doc_keys, entry_id)
                .await
                .map_err(|e| internal(format!("room post search map: {e}")))?;
            out.extend(found.iter().zip(ids).filter_map(|(r, post)| {
                Some((
                    r.rank,
                    RoomSearchHit {
                        seq: None,
                        rank: room_search_rank(r.rank),
                        kind: RoomSearchHitKind::Post,
                        post_id: Some(hex::encode(post?)),
                        extra: std::collections::BTreeMap::new(),
                    },
                ))
            }));
        } else {
            let seqs = state
                .db
                .room_message_seqs_for_docs(room_id, &doc_keys, entry_id)
                .await
                .map_err(|e| internal(format!("room search map: {e}")))?;
            out.extend(found.iter().zip(seqs).filter_map(|(r, seq)| {
                Some((
                    r.rank,
                    RoomSearchHit {
                        seq: Some(seq?),
                        rank: room_search_rank(r.rank),
                        kind: RoomSearchHitKind::Message,
                        post_id: None,
                        extra: std::collections::BTreeMap::new(),
                    },
                ))
            }));
        }
        if exhausted {
            break;
        }
    }
    out.truncate(limit);
    Ok(out)
}

// ── fauna.conversations.room.generations_remote ────────────────

/// A **foreign member's** generation read, relayed by its own home nest.
///
/// The room is homed elsewhere, so the member cannot ask the room's home nest
/// directly — "a member on a foreign nest reaches the room only through their
/// own home nest, which originates the leg to the room's home over the
/// nest↔nest channel" (`conversation-rooms.md` § The home nest). This is that
/// leg's client-facing half, the `channel.actors_remote` / `send_remote`
/// shape.
///
/// **This nest keeps nothing.** It forwards the room home's answer and does
/// not store the wraps: a relaying nest "never holds a wrap, so a community
/// room's readable views exist on the home nest alone" (same §). The wraps it
/// passes through are sealed to the member's own reception key, so they are
/// opaque to it even in transit — which is why relaying them needs no new
/// trust in the relay.
///
/// The room home resolves *which* wraps by the requesting actor, never by an
/// entry this nest names ([`room_generations_for_principal`]), so this nest
/// cannot substitute itself as the recipient.
fn room_generations_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.generations_remote",
            )
            .await?;
            let req: RoomGenerationsRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id shape locally; the room's home nest is
            // authoritative for membership and for the wraps themselves.
            parse_room_id(&req.room_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest reads use room.generations)",
                ));
            }

            match crate::federation_pool::originate_room_generations(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
            )
            .await
            {
                Ok(Ok(generations)) => encode_reply(&RoomGenerationsReply {
                    generations,
                    extra: std::collections::BTreeMap::new(),
                }),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room generation read relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room generations (generations_remote): {pool_err}");
                    Err(internal("federation room generation read failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.room.list_roster ───────────────────────

fn room_list_roster_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.list_roster").await?;
            let req: RoomListRosterRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;

            // Membership is not public. A room id is unguessable
            // (content-derived from a birth record, or an MLS channel id),
            // but "unguessable" is a secret, and one leaked id must not turn
            // into the room's social record. `is_room_member` rather than
            // `get_room_member_role`: a policy-less room's members carry no role,
            // and a role-keyed gate would fail closed for exactly those.
            if !state
                .db
                .is_room_member(&room_id, &actor_id)
                .await
                .map_err(|e| internal(format!("floor roster: {e}")))?
            {
                return Err(permission_denied("not a member of this room"));
            }

            encode_reply(&room_roster_reply(&state, &room_id, req.at_policy_version).await?)
        })
    })
}

/// The floor-roster read's **shared core**: the room record, its live floor,
/// and the two handle joins — everything both doors serve, and neither door's
/// gate.
///
/// Both doors run this one body so that their answers cannot drift apart: the
/// same-nest `room.list_roster` after `is_room_member`, and the relayed
/// `fauna.federation.conversation.roster.fetch` after
/// [`crate::federation_handlers`]' `require_foreign_member`. That identity is
/// the point — a foreign member sees the same names a home member sees — and
/// it is what carries the id→handle announce to the one seat it could not
/// reach before, the foreign member's own device (`federation.md` § Cross-nest
/// shared folders + channel append, the id→handle bullet).
pub(crate) async fn room_roster_reply(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    at_policy_version: Option<u64>,
) -> Result<RoomListRosterReply, RpcError> {
    // A retained policy version, served by number: what a member judges a
    // floor delete record against (`conversation-rooms.md` § Roles and
    // authorization → *Delete any message — the mechanism* → *Community
    // rooms*). Behind the same gate as the current policy beside it — both
    // doors admit only a member, and a superseded policy is a document
    // every member of its day already held.
    let policy_at_version = match at_policy_version {
        Some(version) => state
            .db
            .get_room_policy_version(room_id, version)
            .await
            .map_err(|e| internal(format!("room policy history: {e}")))?,
        None => None,
    };
    let room = state
        .db
        .get_room(room_id)
        .await
        .map_err(|e| internal(format!("room read: {e}")))?
        .ok_or_else(|| internal("a member of a room with no record"))?;
    let roster = state
        .db
        .list_floor_roster(room_id)
        .await
        .map_err(|e| internal(format!("floor roster: {e}")))?;

    // Enrich each principal with the handle + domain this nest holds,
    // joined nest-side from its own `users` row — the
    // `fauna.contacts.list` shape verbatim (`ContactItem.handle`).
    // `RoomMemberRow.handle` is `Some(_)` iff the principal has a
    // local `users` row; `None` ⇒ a principal homed on another nest,
    // or a nest/bridge principal. The domain is this nest's live
    // handle domain — the same source `resolve_handle_core` reports
    // — and is meaningful only for a local principal.
    //
    // A principal homed on ANOTHER nest is named from the second
    // join, `RoomMemberRow.foreign_handle`/`foreign_domain`: the
    // `handle@domain` its own home nest announced on the member's
    // relayed drain, verified by this nest to resolve to that nest's
    // key, and stored on the binding row (`federation.md` § Cross-nest
    // shared folders + channel append, the id→handle bullet). `None`
    // until its home nest has announced one — then the member renders
    // as its elided actor id, the honest fallback.
    //
    // ⚠ Neither handle is ever read off the roster REPORT. For an
    // end-to-end room that report is a member-reported mirror
    // (`conversation-rooms.md` § The floor roster), so a reported
    // handle would be attacker-chosen; the local one is this nest's
    // own record and the foreign one its home nest's, domain-bound.
    // Both are display-only — `principal` is what every membership
    // decision keys on.
    let nest_domain: Option<String> = Some(state.handle_domain());
    let policy = state
        .db
        .get_room_policy_blob(room_id)
        .await
        .map_err(|e| internal(format!("room policy: {e}")))?;
    // Which slots the tip covers. The home nest's own row answers whether it
    // reads the room — the members' standing choice, which every member must be
    // able to see rather than only the one that minted it — and a user's row
    // answers whether an owner or admin still owes it a key-in. Slot ids only:
    // this reveals nothing the coverage rule does not already fix, since every
    // live member with a wrap target is covered except a newcomer not yet
    // keyed in and a nest whose grant was withdrawn.
    let tip_entries = state
        .db
        .room_tip_wrapped_entries(room_id)
        .await
        .map_err(|e| internal(format!("tip wraps: {e}")))?;
    // Which labelers read the room — the signed set, verbatim, for the reason
    // the policy rides here: members verify and render it, and the next change
    // is authored from it.
    let (_, labelers) = state
        .db
        .get_room_labelers(room_id)
        .await
        .map_err(|e| internal(format!("room labeler set: {e}")))?;
    Ok(RoomListRosterReply {
        members: roster
            .into_iter()
            .map(|m| {
                let is_local = m.handle.is_some();
                // An empty handle on a local user is "no usable handle".
                let (handle, domain) = if is_local {
                    (m.handle.filter(|h| !h.is_empty()), nest_domain.clone())
                } else {
                    match (m.foreign_handle, m.foreign_domain) {
                        (Some(h), Some(d)) if !h.is_empty() && !d.is_empty() => (Some(h), Some(d)),
                        _ => (None, None),
                    }
                };
                // The wrap-target pair. A mint is admissible only when its
                // wraps cover the Enrolled roster, so a minter must read the
                // roster and wrap to all of it — which it can only do if the
                // read carries what a wrap is addressed to. An empty OR
                // invalid reception key is "unkeyable" rather than
                // a target (the coverage check skips exactly those rows), so
                // it is reported as absent rather than as unwrappable bytes —
                // which is also what lets an honest member's own tend pass
                // notice it holds no *usable* key and re-supply one.
                let reception_pubkey = m.reception_pubkey.filter(|k| usable_reception_key(k));
                let tip_wrapped = match (&tip_entries, &m.entry_id) {
                    (Some(covered), Some(entry)) => Some(covered.contains(entry)),
                    _ => None,
                };
                RoomRosterMemberWire {
                    principal: hex::encode(m.principal_id),
                    kind: m.principal_kind,
                    role: m.role,
                    joined_at: m.joined_at,
                    handle,
                    domain,
                    entry_id: m.entry_id.map(hex::encode),
                    reception_pubkey,
                    tip_wrapped,
                    extra: std::collections::BTreeMap::new(),
                }
            })
            .collect(),
        class: room.class,
        policy_version: room.policy_version,
        // The signed policy the version names. Members render and verify it
        // for themselves — the roles above are only its projection — and a
        // policy change is authored from it, so serving the version without
        // the bytes left `set_policy` unusable: a replacement built without
        // them would silently reset the name, join rule and history policy.
        // Safe to serve here and nowhere cheaper: this door is already
        // admitted only to a live member, and a member is exactly who renders
        // a policy.
        policy: policy.map(serde_bytes::ByteBuf::from),
        labelers: labelers.map(serde_bytes::ByteBuf::from),
        policy_at_version: policy_at_version.map(serde_bytes::ByteBuf::from),
        // The other half of the birth record, for the member anchoring the
        // version it just asked for — so only on that read, which keeps the
        // roster read every poll makes exactly the size it was.
        birth_salt: at_policy_version
            .and(room.birth_salt)
            .map(serde_bytes::ByteBuf::from),
        extra: std::collections::BTreeMap::new(),
    })
}

// ── fauna.conversations.room.list_roster_remote ────────────────

/// A **foreign member's** floor-roster read, relayed by its own home nest.
///
/// A room's floor roster lives on its home nest and nowhere else, so this
/// nest — which is not that home — has no room record to read and its
/// same-nest door would answer `permission_denied` ("not a member of this
/// room") to a member who is perfectly well seated. "A member on a foreign
/// nest reaches the room only through their own home nest, which originates
/// the leg to the room's home" (`conversation-rooms.md` § The home nest);
/// this is that leg's client-facing half, the `channel.actors_remote` /
/// `generations_remote` shape.
///
/// **This nest keeps nothing.** It forwards the room home's answer and stores
/// no part of it: the names in it are the room's, read under the room home's
/// own membership gate, and a relay that cached them would be answering a
/// membership question it has no authority over the next time it was asked.
///
/// The room home resolves the requester's standing itself, from the actor id
/// this nest is authenticated for — it never takes this nest's word for who
/// is a member ([`crate::federation_handlers`]' `require_foreign_member`).
fn room_list_roster_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.list_roster_remote",
            )
            .await?;
            let req: RoomListRosterRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id shape locally; the room's home nest is
            // authoritative for membership and for the roster itself.
            parse_room_id(&req.room_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest reads use room.list_roster)",
                ));
            }

            match crate::federation_pool::originate_room_roster(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
                req.at_policy_version,
            )
            .await
            {
                Ok(Ok(reply)) => encode_reply(&reply),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room roster read relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation room roster (list_roster_remote): {pool_err}");
                    Err(internal("federation room roster read failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.room.roster_report_remote ──────────────

/// A **foreign member's** floor-roster report, relayed by its own home nest —
/// the write twin of [`room_list_roster_remote_handler`].
///
/// § The floor roster has the committing device report to the room's **home**
/// nest, and this nest is not that home: it holds no room record, and its
/// same-nest door would either refuse the report or — the reporter sitting on
/// this nest's routing roster after the relayed Welcome — bootstrap a stray
/// floor for a room it does not home. So the report rides the leg the member's
/// commit itself rode: "a member on a foreign nest reaches the room only
/// through their own home nest, which originates the leg to the room's home"
/// (`conversation-rooms.md` § The home nest).
///
/// **This nest keeps nothing.** It forwards the room home's ack and stores no
/// part of the roster: the membership record is the home's, and a relay that
/// mirrored it would be answering a membership question it has no authority
/// over. The room home resolves the reporter's standing itself, from the actor
/// id this nest is authenticated for ([`crate::federation_handlers`]'
/// `require_foreign_member`) — it never takes this nest's word for who is a
/// member, and this nest cannot report on a member's behalf.
fn room_roster_report_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.conversations.room.roster_report_remote",
            )
            .await?;
            let req: RoomRosterReportRemoteRequest = decode(&payload).map_err(malformed)?;
            // Validate the id shape locally; the room's home nest is
            // authoritative for membership, for the ratchet and for the
            // roster itself.
            parse_room_id(&req.room_id)?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_params(
                    "nest_url must name the room's home nest (same-nest reports use room.roster_report)",
                ));
            }

            match crate::federation_pool::originate_room_roster_report(
                &state.federation_pool,
                &state,
                peer_url,
                &hex::encode(actor_id),
                &req.room_id,
                req.members,
                req.policy_version,
                // The home's own log position — the member's commit rode this
                // same relay to the home's log — forwarded untouched: the home
                // bounds and orders it, as for a same-nest report.
                req.commit_seq,
            )
            .await
            {
                Ok(Ok(ack)) => encode_reply(&ack),
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest room roster report relay",
                )),
                Err(pool_err) => {
                    tracing::error!(
                        "federation room roster report (roster_report_remote): {pool_err}"
                    );
                    Err(internal("federation room roster report failed"))
                }
            }
        })
    })
}

// ── fauna.conversations.room.roster_report ─────────────────────

/// The three-role vocabulary of `conversation-rooms.md` § Roles and
/// authorization. Stored verbatim, so the CHECK constraint on
/// `room_members.role` and this list are the same closed set stated twice —
/// which is deliberate: a role the nest does not understand must be refused
/// at the door with a name, not turned into a constraint violation the
/// caller reads as `internal`.
const ROOM_ROLES: [&str; 3] = ["owner", "admin", "member"];

/// Validate one report and project it onto the storage vocabulary.
///
/// Every rule here is a property of a roster a real membership commit
/// produces, so a report failing one is a bug in the reporter — not a
/// membership fact the nest should mirror. The floor roster is read by the
/// custody serve door and the succession sweep, so a malformed roster
/// stored is a wrong answer given to both.
fn validate_room_roster(
    members: &[fauna_protocol::conversations::RoomRosterEntryWire],
) -> Result<Vec<crate::db::rooms::ReportedMember>, RpcError> {
    // A room with no members is not a room. Accepting an empty roster would
    // sever every consumer of it at once, and it is far more likely a bug in
    // the reporter than a membership fact.
    if members.is_empty() {
        return Err(invalid_params("a roster report names no members"));
    }

    let mut out: Vec<crate::db::rooms::ReportedMember> = Vec::with_capacity(members.len());
    let mut owners = 0usize;
    for m in members {
        let principal_id = parse_actor_id(&m.actor)?;
        if let Some(role) = m.role.as_deref()
            && !ROOM_ROLES.contains(&role)
        {
            return Err(invalid_params(&format!(
                "unknown room role {role:?} — the vocabulary is owner/admin/member"
            )));
        }
        // One entry per principal. Silently taking the last would let one
        // report mean two things, and the storage upsert would hide it.
        if out.iter().any(|p| p.principal_id == principal_id) {
            return Err(invalid_params(
                "a roster report names the same principal twice",
            ));
        }
        if m.role.as_deref() == Some("owner") {
            owners += 1;
        }
        out.push(crate::db::rooms::ReportedMember {
            principal_id,
            // The wire carries no principal kind, and a report comes from a
            // member device of an end-to-end room — whose member set is user
            // principals by construction (`conversation-rooms.md` § The three
            // classes). Deriving it here rather than trusting a field is what
            // keeps a report from asserting a kind that would change the
            // room's derived class: adding the home nest to a room is a
            // membership change with its own ceremony, never a word in a
            // mirror report.
            principal_kind: "user".to_string(),
            role: m.role.clone(),
            home_node_url: String::new(),
            // A mirror report carries no wrap target and needs none: an
            // end-to-end room has no generations, and reading there is MLS's
            // (§ The floor roster — the mirror "decides nothing about
            // confidentiality").
            reception_pubkey: Vec::new(),
        });
    }

    // "Exactly one owner" (§ Roles and authorization) — an owner-ambiguous
    // roster would give succession two targets and `remove` two
    // unremovable rows. Zero owners is the policy-less room, which carries no
    // roles at all; that case is checked below.
    if owners > 1 {
        return Err(invalid_params(
            "a roster report names more than one owner — a room has exactly one",
        ));
    }
    let roled = out.iter().filter(|m| m.role.is_some()).count();
    if roled != 0 && roled != out.len() {
        return Err(invalid_params(
            "a roster report mixes roled and role-less members — a room is governed or policy-less, not both",
        ));
    }
    if roled == out.len() && owners == 0 {
        return Err(invalid_params(
            "a governed room's roster report names no owner",
        ));
    }

    Ok(out)
}

fn room_roster_report_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.conversations.room.roster_report").await?;
            let req: RoomRosterReportRequest = decode(&payload).map_err(malformed)?;
            let room_id = parse_room_id(&req.room_id)?;

            // Gate 1 — the caller is on the channel's ROUTING roster, so it
            // is a live participant of this channel on this nest rather than
            // a caller that guessed a channel id.
            //
            // ⚠ This is emphatically NOT the membership authority — a
            // `channel.send` self-registers the row, so it proves knowledge
            // of the channel id and nothing more
            // (`conversation-rooms.md` § The floor roster → *What the floor
            // roster is not*). It is a cheap liveness gate stacked under gate
            // 2, and it is the reason a stranger cannot mint floor rosters —
            // and with them room records — for arbitrary 32-byte ids.
            if !state
                .db
                .is_actor_in_channel(&actor_id, &room_id)
                .await
                .map_err(|e| internal(format!("routing roster: {e}")))?
            {
                return Err(permission_denied(
                    "the reporter is not on this channel's routing roster",
                ));
            }

            encode_reply(
                &room_roster_report_apply(
                    &state,
                    &room_id,
                    &actor_id,
                    &req.members,
                    req.policy_version,
                    req.commit_seq,
                )
                .await?,
            )
        })
    })
}

/// Apply one floor-roster report to `room_id` — everything past a door's own
/// gate 1, shared by the two doors a report can arrive through so that they
/// cannot drift: the same-nest `room.roster_report` above, after its
/// routing-roster gate, and the relayed
/// `fauna.federation.conversation.roster.report`
/// ([`crate::federation_handlers`]), after `require_foreign_member` — the
/// `channel.fetch` gate, strictly stronger than the routing check, since the
/// `channel_foreign_members` row is this nest's own record of the Welcome it
/// relayed where a routing row is `channel.send`-self-registered.
///
/// In here: the roster validation, gate 0's provenance refusal of a
/// floor-authoritative room, the gate 2 ratchet against the STORED roster, the
/// commit-order guard, and the wholesale replace. `reporter` is the actor the
/// door authenticated — never anything named inside the report.
pub(crate) async fn room_roster_report_apply(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    reporter: &[u8; 32],
    members: &[RoomRosterEntryWire],
    policy_version: Option<u64>,
    commit_seq: Option<i64>,
) -> Result<RoomRosterReportReply, RpcError> {
    let members = validate_room_roster(members)?;

    // Gate 2 — the **ratchet**, which is what makes the floor roster
    // the *non-self-assertable* membership record the custody serve
    // door was declared to wait for
    // (`account-replica-posture.md` § Shared-audience carve-out).
    //
    // *Report, never guess* means a device reports its own room's
    // membership, and the reporter of a membership commit is by
    // construction a member of the roster that commit REPLACED —
    // true of an add, of a remove, and of a self-leave alike. So
    // once a room has a floor roster, only a **live member of the
    // stored roster** may replace it.
    //
    // Naming yourself in the new roster is deliberately NOT
    // sufficient: that is the self-assertion the whole record exists
    // to exclude. Its concrete shape is a departed member walking
    // back in — the routing row deliberately survives a departure
    // (`conformance_custody_nest_door_client.rs`), so without the
    // ratchet an evicted member could re-seat itself on the roster
    // and re-open the custody serve door over the room's conv scope.
    // That is finding 's attack in a new coat, and it is the
    // reason the check reads the STORED roster rather than the
    // reported one. It is the same shape as the Welcome plane's own
    // `register_actor_channel_gated_with_claim`, which refuses an
    // evicted member's self-addressed re-insertion.
    //
    // Conversely a departing member's final report IS admitted: it
    // is still live in the stored roster as it reports, and refusing
    // it would leave the departure invisible until some other member
    // happened to commit — with the custody door serving the
    // leaver's grants meanwhile.
    //
    // ⚠ DECLARED BOUND — the bootstrap, now bounded by gate 0. A
    // room with no stored roster has nothing to ratchet against, so
    // its FIRST report is admitted from any actor on the channel's
    // routing roster that names itself. That is self-assertable, and
    // the routing roster is not membership (gate 1). What stands
    // between an attacker and a minted floor roster is knowledge of
    // the 32-byte channel id plus winning a race against the room's
    // own first report, which a real room emits at its first
    // membership commit; and what it would win is the custody door's
    // admission to sealed bytes it cannot open. What it can NO
    // LONGER win, since the create ceremony landed, is a room that
    // was founded rather than reported: gate 0 above refuses those
    // outright, so the self-assertable window is now exactly "an
    // end-to-end room before its first report", where the MLS group
    // — not this table — decides every read. Recorded in
    // `conversation-rooms.md` § Implementation status today rather
    // than left for a reader to infer. The relayed door reaches this
    // same bound behind `require_foreign_member` — a Welcome-bound
    // member, never a routing-roster actor — so it does not widen
    // there.
    let room = state
        .db
        .get_room(room_id)
        .await
        .map_err(|e| internal(format!("room read: {e}")))?;

    // Gate 0 — PROVENANCE. This door is the *member-reported
    // mirror* of a membership authority the nest cannot read
    // (§ The floor roster → *End-to-end rooms*). A room founded by
    // the create ceremony has the opposite arrangement: its floor
    // IS the authority, written by the nest's own ceremonies. A
    // room has exactly one membership authority, so a report
    // against a floor-authoritative room is refused before either
    // gate below is consulted.
    //
    // ⚠ This is not a tidiness rule. Without it, gates 1 and 2 are
    // both satisfied by any ordinary member of a community room —
    // it is on the routing roster after one `channel.send`, and it
    // is a live member of the stored roster — so it could replace
    // that room's authoritative roster wholesale and name ITSELF
    // owner, `Removed`-absorbing the real one. The ratchet cannot
    // catch that: the attacker is exactly the live member the
    // ratchet admits. Provenance is what separates the two doors.
    if room.as_ref().is_some_and(|r| r.is_floor_authoritative()) {
        return Err(permission_denied(
            "this room's membership authority is its floor, not a member report",
        ));
    }

    let has_roster = room.is_some();
    let admitted = if has_roster {
        state
            .db
            .is_room_member(room_id, reporter)
            .await
            .map_err(|e| internal(format!("floor roster: {e}")))?
    } else {
        members.iter().any(|m| m.principal_id == *reporter)
    };
    if !admitted {
        return Err(permission_denied(
            "a roster report must come from a live member of the room it reports",
        ));
    }

    // "A room is never owner-less", judged against the STORED room
    // (`conversation-rooms.md` § Roles and authorization → *Leaving — the
    // mechanism*). `validate_room_roster` can only judge the report's own
    // shape, and a governed room's report from a device that could not read
    // the policy arrives role-less — indistinguishable there from a policy-less
    // room's — so it would be taken as policy-less, write `owner_id = NULL`, and
    // un-govern the floor: the room then renders owner-less and lets its owner
    // walk out, with no transfer. Any live member's modified client could do
    // the same on purpose. A floor that names no owner — a policy-less
    // room, a 1:1, a room still awaiting its birth report — keeps taking
    // role-less reports as before.
    if room.as_ref().is_some_and(|r| r.owner_id.is_some())
        && !members.iter().any(|m| m.role.as_deref() == Some("owner"))
    {
        return Err(invalid_params(
            "a roster report names no owner for a room whose floor names one — a room is never owner-less",
        ));
    }

    // The class is DERIVED from the member set, never stored as a
    // choice (`conversation-rooms.md` § Architectural rules, rule 1).
    // Every reported principal is a user principal (above), so the
    // derivation's ordered rule — any bridge ⇒ transport-only, else
    // the home nest a member ⇒ community, else end-to-end — lands on
    // end-to-end for every report this door accepts. The community
    // and bridged classes seat their non-user principals through
    // their own ceremonies, not through a mirror report.
    let class = "end_to_end";

    // The commit-order guard's BOUND (`conversation-rooms.md` § The floor
    // roster). A report names the log position of the commit it follows, and
    // the replace below drops one at or below the position the floor holds —
    // so without a bound, one member claiming an enormous position would
    // freeze the floor against every honest report after it. The bound is
    // something this nest observes rather than anything a client says: the
    // newest commit the room's log has carried, the same high-water mark the
    // device-owned-epoch gate reads (`channel_commit_watermark`, advanced
    // under the conv seq lock whenever a `ChannelEnvelope::Commit` lands —
    // including a foreign member's, which `fauna.federation.channel.append`
    // appends through the same `channel_send_core`). So the highest position
    // anyone can claim is the newest real commit, and the next honest commit
    // lands above it. A real position is at least 1 (conv seqs start there).
    //
    // After both gates, deliberately: ordering is not authorization, and a
    // fresh position must never buy a reporter the ratchet refused.
    if let Some(position) = commit_seq {
        let (newest, newest_sender) = state
            .db
            .channel_commit_watermark_with_sender(room_id)
            .await
            .map_err(|e| internal(format!("commit watermark: {e}")))?;
        if !(1..=newest).contains(&position) {
            return Err(invalid_params(
                "a roster report names a commit this room's log has not carried",
            ));
        }

        // The commit-order guard's AUTHORSHIP half (`conversation-rooms.md`
        // § The floor roster). The bound above asks *which* commit a report
        // names; this asks *whose* it was.
        //
        // ⚠ Ordering alone is not enough, and the gap is exactly the case the
        // guard exists for. The replace below drops a report at or below the
        // position the floor holds, so at every position the FIRST report to
        // land wins — and the one report the guard exists to protect, the
        // committing device's own, was not privileged over a bystander's.
        // Concretely: admin A removes M by the commit at seq N. Until A's
        // report lands, M is still a live member of the STORED floor (gate 2
        // admits it) and still on the routing roster (gate 1 — "the routing
        // row deliberately survives a departure", above), so M could report
        // position N with a roster that still names itself, win the race, and
        // leave A's honest report at N answered `Superseded` and written
        // nowhere. M then stayed seated until the NEXT membership or policy
        // commit's report — indefinitely, in a quiet group.
        //
        // The anchor was chosen because the nest observes it; the nest
        // observes the commit's sender at the same append, under the same
        // per-channel seq lock (`segments::conv::append_locked`). So a report
        // naming the newest commit's position must come from that commit's
        // sender. Both doors run this body, so the relayed report is bound to
        // the `requesting_actor_id` the home pinned at `require_foreign_member`
        // exactly as the same-nest one is bound to its authenticated caller
        // (`../../docs/goal/architecture/federation.md` § the room roster
        // report row).
        //
        // Declared residue, both fail-open by construction and both narrower
        // than the mirror's ratified trust in a live member's report:
        //   * a position BELOW the newest commit is unverifiable — the nest
        //     keeps one sender, the newest commit's. Admitting it costs
        //     nothing the ordering rule did not already allow: such a report
        //     is anchored lower than the honest one that follows, so it is
        //     replaced sooner, where a lie at the current position is the
        //     trust this door has always extended;
        //   * a watermark row written before schema 64 names no sender.
        //     Refusing those would break every existing room's next report
        //     until its next commit — a functional regression — so an unknown
        //     sender is admitted, and the next commit rewrites the pair.
        if position == newest
            && let Some(sender) = newest_sender
            && sender != *reporter
        {
            return Err(permission_denied(
                "a roster report at this commit's position must come from the device that committed it",
            ));
        }
    }

    let replaced = state
        .db
        .replace_floor_roster(room_id, class, "", policy_version, commit_seq, &members)
        .await
        .map_err(|e| {
            tracing::error!("replace_floor_roster: {e}");
            internal("storage error")
        })?;

    Ok(match replaced {
        crate::db::rooms::RosterReplace::Replaced { live } => RoomRosterReportReply {
            members: live,
            superseded_by: None,
            extra: std::collections::BTreeMap::new(),
        },
        // Not an error: the floor already holds the roster of a later commit,
        // which is at least as new as this one. The reporter learns which.
        crate::db::rooms::RosterReplace::Superseded { stored, live } => {
            tracing::debug!(
                room = %hex::encode(room_id),
                reported = ?commit_seq,
                stored,
                "a roster report older than the floor's was not applied"
            );
            RoomRosterReportReply {
                members: live,
                superseded_by: Some(stored),
                extra: std::collections::BTreeMap::new(),
            }
        }
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_conversations_handlers(b: &mut RpcRouterBuilder) {
    // Per-kind replay semantics + rationale: see
    // `KindRegistry::register_conversations_{channel,keypackage}_kinds`.
    b.add(
        "fauna.conversations.channel.send",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: channel_send_handler(),
        },
    );
    b.add(
        "fauna.conversations.channel.send_remote",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: channel_send_remote_handler(),
        },
    );
    b.add(
        KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET,
        RpcKindMeta {
            // A read-shaped mint (an in-memory short-TTL token, no row) — the
            // same posture + deadline as the folders twin `write_token.get`.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: blob_write_token_get_handler(),
        },
    );
    b.add(
        "fauna.conversations.channel.fetch",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: channel_fetch_handler(),
        },
    );
    b.add(
        "fauna.conversations.channel.list_for_actor",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: channel_list_for_actor_handler(),
        },
    );
    b.add(
        "fauna.conversations.channel.actors",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: channel_actors_handler(),
        },
    );
    b.add(
        "fauna.conversations.channel.actors_remote",
        RpcKindMeta {
            // A pure read like `actors`; 30 s covers the federation hop
            // (the `send_remote` precedent).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: channel_actors_remote_handler(),
        },
    );
    b.add(
        "fauna.conversations.keypackage.upload",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: keypackage_upload_handler(),
        },
    );
    b.add(
        "fauna.conversations.keypackage.fetch",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: keypackage_fetch_handler(),
        },
    );
    b.add(
        "fauna.conversations.keypackage.count",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: keypackage_count_handler(),
        },
    );
    b.add(
        "fauna.conversations.welcome.deliver",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: welcome_deliver_handler(),
        },
    );
    // The room plane's two membership doors and its roster read. `create`
    // is the community class's birth ceremony and `roster_report` the
    // end-to-end class's mirror; a room founded by one is refused by the
    // other (gate 0). Both are replay-safe — `create` derives its id from
    // the birth record and upserts, `roster_report` replaces a roster
    // wholesale — and neither fans anything out.
    b.add(
        "fauna.conversations.room.create",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_create_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.roster_report",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: room_roster_report_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.list_roster",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: room_list_roster_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.list_roster_remote",
        RpcKindMeta {
            // The same read, relayed to the room's home — a pure read on both
            // hops, with the peer round-trip's deadline (the
            // `channel.actors_remote` / `generations_remote` posture).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_list_roster_remote_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.roster_report_remote",
        RpcKindMeta {
            // The report, relayed to the room's home — a wholesale replace on
            // both hops, so replay-safe like `roster_report`; the peer
            // round-trip's deadline (the `list_roster_remote` posture).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_roster_report_remote_handler(),
        },
    );
    // The membership doors. `invite`, `accept_invite`, `remove` and `leave`
    // all forbid replay: each is externally visible (the invitee is
    // notified; a departure and a removal change who the room fans out to),
    // so a recovered connection must not auto-retry one. 30 s covers a
    // signature verification, a role read and a small transaction, plus the
    // invitee's reach-floor read.
    b.add(
        "fauna.conversations.room.invite",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_invite_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.accept_invite",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_accept_invite_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.list_invites",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: room_list_invites_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.revoke_invite",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_revoke_invite_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.remove",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_remove_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.leave",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_leave_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.leave_remote",
        RpcKindMeta {
            // The one relayed twin that does NOT inherit its door's flag:
            // the room home's federated leave is idempotent by construction
            // precisely so the §4.D auto re-send this relay adds cannot turn
            // a departure that landed into a reported failure
            // (`RoomLeaveRemoteRequest`).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_leave_remote_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.accept_invite_remote",
        RpcKindMeta {
            // `leave_remote`'s posture for the same reason: the room home's
            // federated accept is idempotent (`room_accept_relayed` answers a
            // member its recorded invitation already seated with its role),
            // so the §4.D re-send reports a seating that landed
            // (`RoomAcceptInviteRemoteRequest`).
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_accept_invite_remote_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.invite_remote",
        RpcKindMeta {
            // Inherits `room.invite`'s flag, unlike the two relayed twins
            // above: the room home's federated issue door records a row and
            // delivers a knock per call, so a re-sent frame would notify the
            // invitee twice. The inviter re-issues, and the pending
            // invitation is refreshed rather than duplicated
            // (`RoomInviteRemoteRequest`).
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_invite_remote_handler(),
        },
    );
    // The governance doors. Both forbid replay and both are refused by the
    // strict version ratchet on a replay regardless; the flag keeps a
    // recovered connection from producing the attempt.
    b.add(
        "fauna.conversations.room.set_policy",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_set_policy_handler(),
        },
    );
    // The labeler set is the policy's sibling record and is governed the same
    // way: externally visible (members render which labelers read the room),
    // refused on replay by its own strict ratchet.
    b.add(
        "fauna.conversations.room.set_labelers",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_set_labelers_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.transfer_ownership",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_transfer_ownership_handler(),
        },
    );
    // The sealing plane. A mint changes which key the room seals under and
    // can revoke the home nest's read, so it forbids replay — though the
    // parent ratchet refuses a replayed mint anyway, since its parent is no
    // longer the tip. Reading generations back is a pure read.
    b.add(
        "fauna.conversations.room.publish_generation",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_publish_generation_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.generations",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: room_generations_handler(),
        },
    );
    // The add's half of the scheme's mint triggers: covering a newcomer
    // hands out key material, so it forbids replay like the mint — though
    // unlike the mint it can never move the room's tip.
    b.add(
        "fauna.conversations.room.backfill_generations",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_backfill_generations_handler(),
        },
    );
    // A seat's own wrap target, after seating: a successor's keyless seat and a
    // member's rotation. Forbids replay because a
    // replayed older set would roll a rotated seat back to a retired key.
    b.add(
        "fauna.conversations.room.set_reception_key",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: room_set_reception_key_handler(),
        },
    );
    // The read position's first purpose, made reachable. A pure read of a
    // derived view — replayable, and cheap enough to keep the short
    // same-nest deadline.
    b.add(
        "fauna.conversations.room.search",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: room_search_handler(),
        },
    );
    b.add(
        "fauna.conversations.room.generations_remote",
        RpcKindMeta {
            // A relayed pure read — same posture as the `channel.actors_remote`
            // precedent, with the peer round-trip's deadline.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: room_generations_remote_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use fauna_protocol::encode_canonical;

    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;

    /// The facet's decrypt must not run on the async worker.
    ///
    /// An AEAD over up to 8 MiB is CPU-bound work of the same class as the
    /// `wasm` run that follows it, and the facet is built inline in the send
    /// path — so on the worker it stalls every other connection sharing that
    /// thread, repeatably, at the send rate. The observable is exact and
    /// carries no wall-clock: the closure runs on a DIFFERENT thread than the
    /// caller (`spawn_blocking` always dispatches to the blocking pool, on
    /// every runtime flavour), so a thread id is the whole assertion.
    #[tokio::test]
    async fn an_attachments_decrypt_runs_off_the_async_worker() {
        let here = std::thread::current().id();
        let opened = open_off_the_worker(move || {
            assert_ne!(
                std::thread::current().id(),
                here,
                "the decrypt ran on the caller's thread — the async worker"
            );
            Some(vec![7u8; 3])
        })
        .await;
        assert_eq!(opened, Some(vec![7u8; 3]));
    }

    /// A decrypt that panics withholds that one attachment and nothing else:
    /// the facet is built either way, never half-built, because a host that
    /// could not open a blob is exactly the case the facet already models.
    #[tokio::test]
    async fn a_decrypt_that_panics_withholds_its_attachment_and_no_more() {
        let opened = open_off_the_worker(|| panic!("a hostile blob")).await;
        assert_eq!(opened, None);
    }

    // The two tests above pin the helper. The two below pin its one caller,
    // and the facet's read bound with it, where the real `RoomFacetSource`
    // runs them: a facet alone cannot tell
    // "never read" from "read, then withheld", and a helper test cannot see a
    // caller that stopped using the helper.

    use crate::blob_store::BlobStoreBackend;
    use fauna_core::data::ContentHash;

    /// Every facet decrypt the real source runs, with the thread it ran on —
    /// recorded by [`FacetKey::open`], beside the AEAD itself. Keyed by the
    /// seal's content address, so tests running in parallel in this binary each
    /// read only their own seals.
    pub(crate) mod facet_decrypts {
        use std::sync::{Mutex, PoisonError};
        use std::thread::ThreadId;

        static SEEN: Mutex<Vec<([u8; 32], ThreadId)>> = Mutex::new(Vec::new());

        pub(crate) fn record(sealed: &[u8]) {
            let address = fauna_core::encoding::content_hash(sealed).digest();
            SEEN.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((address, std::thread::current().id()));
        }

        pub(crate) fn threads_for(address: &[u8; 32]) -> Vec<ThreadId> {
            SEEN.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|(seen, _)| seen == address)
                .map(|(_, thread)| *thread)
                .collect()
        }
    }

    /// A real [`DiskBlobStore`](crate::blob_store::DiskBlobStore) that counts
    /// its reads per content address — what rule (4)'s "never fetched" needs to
    /// be observable over the nest's own store.
    struct CountingBlobStore {
        inner: crate::blob_store::DiskBlobStore,
        reads: std::sync::Mutex<Vec<[u8; 32]>>,
    }

    impl CountingBlobStore {
        fn reads_of(&self, address: &ContentHash) -> usize {
            let address = address.digest();
            self.reads
                .lock()
                .unwrap()
                .iter()
                .filter(|read| **read == address)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl BlobStoreBackend for CountingBlobStore {
        async fn put(&self, hash: &ContentHash, data: &[u8]) -> anyhow::Result<()> {
            self.inner.put(hash, data).await
        }
        async fn get(&self, hash: &ContentHash) -> anyhow::Result<Option<Vec<u8>>> {
            self.reads.lock().unwrap().push(hash.digest());
            self.inner.get(hash).await
        }
        async fn exists(&self, hash: &ContentHash) -> anyhow::Result<bool> {
            self.inner.exists(hash).await
        }
        async fn exists_batch(&self, hashes: &[ContentHash]) -> anyhow::Result<Vec<bool>> {
            self.inner.exists_batch(hashes).await
        }
        async fn delete(&self, hash: &ContentHash) -> anyhow::Result<()> {
            self.inner.delete(hash).await
        }
        async fn usage_bytes(&self) -> anyhow::Result<u64> {
            self.inner.usage_bytes().await
        }
    }

    /// A nest whose local blob store counts its reads. The `TempDir` holds the
    /// store and must outlive the state.
    async fn nest_counting_blob_reads() -> (Arc<AppState>, Arc<CountingBlobStore>, tempfile::TempDir)
    {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let blobs = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(CountingBlobStore {
            inner: crate::blob_store::DiskBlobStore::new(blobs.path()).expect("disk blob store"),
            reads: Default::default(),
        });
        let backup = crate::backup::service::BackupService::with_local_store(
            db.clone(),
            blobs.path().to_path_buf(),
            store.clone(),
        );
        let mut state = AppState::for_test(db);
        state.backup_service = Some(Arc::new(backup));
        (Arc::new(state), store, blobs)
    }

    /// Store one seal the way an upload does; its content address.
    async fn stored(store: &CountingBlobStore, sealed: &[u8]) -> ContentHash {
        let address = fauna_core::encoding::content_hash(sealed);
        store.put(&address, sealed).await.expect("the seal uploads");
        address
    }

    /// Record `address` in the nest's blob metadata as past the per-attachment
    /// ceiling — the upload route's own measurement, which the probe believes —
    /// while the bytes behind it stay small. A host that reads the blob finds a
    /// seal that opens and fits, so only one that asked first withholds it
    /// unread (the conformance fixture
    /// `uploaded_room_picture_recorded_oversize` makes the same pairing).
    async fn recorded_oversize(state: &AppState, address: &ContentHash) {
        state
            .db
            .put_blob_metadata(
                &address.digest(),
                (fauna_labeler::LABELER_ATTACHMENT_BYTES_MAX
                    + fauna_labeler::SEALED_FRAMING_ALLOWANCE
                    + 1) as i64,
                "image/png",
                None,
                None,
            )
            .await
            .expect("the upload route records what it stored");
    }

    /// Rule (4) over the real source, for an item naming two attachments: first
    /// one the nest measured past the ceiling, then an honest one with no
    /// metadata row behind it. Reads the host's work, not only the facet.
    fn assert_only_the_admissible_work_was_done(
        facet: &[fauna_core::scoring::LabelerAttachmentInput],
        store: &CountingBlobStore,
        oversize: &ContentHash,
        honest: &ContentHash,
        honest_plaintext: &[u8],
    ) {
        assert_eq!(
            facet.len(),
            2,
            "both attachments ride, in the author's order"
        );
        assert_eq!(
            (
                store.reads_of(oversize),
                facet_decrypts::threads_for(&oversize.digest()).len()
            ),
            (0, 0),
            "an attachment the nest itself measured past the per-attachment ceiling is never \
             fetched and never decrypted"
        );
        assert!(facet[0].bytes.is_empty(), "and it rides withheld");
        assert_eq!(
            store.reads_of(honest),
            1,
            "the honest attachment beside it is read, once — an absent size is no answer"
        );
        assert_eq!(
            facet[1].bytes, honest_plaintext,
            "and handed to the module opened"
        );
        let decrypts = facet_decrypts::threads_for(&honest.digest());
        assert_eq!(
            decrypts.len(),
            1,
            "it is decrypted once, through the facet's open"
        );
        assert_ne!(
            decrypts[0],
            std::thread::current().id(),
            "the decrypt ran on the caller's thread — the async worker"
        );
    }

    /// The message arm: attachments sealed under the room's generation.
    /// Removing the pre-read probe, reading a blob before refusing it, or
    /// running the decrypt outside the facet's off-worker open each reddens
    /// this.
    #[tokio::test]
    async fn a_room_messages_facet_reads_and_opens_only_what_it_can_admit_off_the_worker() {
        let (state, store, _blobs) = nest_counting_blob_reads().await;
        let key = fauna_core::crypto::GenerationKey::from_bytes([0x61; 32]);
        let generation = [0x62u8; 32];
        let seal = |plaintext: &[u8]| {
            fauna_mls::room_message::seal_room_attachment(&key, &generation, plaintext)
                .expect("a member seals its attachment under the generation")
        };
        let oversize = stored(&store, &seal(b"a picture measured past the ceiling")).await;
        recorded_oversize(&state, &oversize).await;
        let honest_plaintext = b"an honest picture".to_vec();
        let honest = stored(&store, &seal(&honest_plaintext)).await;
        let attachment = |sealed_cid: ContentHash| fauna_mls::types::ChannelAttachment {
            blob_hash: sealed_cid,
            sealed_cid,
            filename: "picture.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 16,
            is_image: true,
            epoch: 0,
        };
        let attachments = [attachment(oversize), attachment(honest)];

        let facet = RoomFacetSource::Message {
            attachments: &attachments,
            key: &key,
            generation: &generation,
            // No stored record behind this item, so the pinned-refs cross-check
            // has nothing to check against: the probe is the only bound between
            // the oversize blob and a read.
            room_id: &[0x63; 32],
            seq: 1,
        }
        .open(&state)
        .await;

        assert_only_the_admissible_work_was_done(
            &facet,
            &store,
            &oversize,
            &honest,
            &honest_plaintext,
        );
    }

    /// The post arm: a room-restricted post's media, sealed under the post's
    /// own per-post key.
    #[tokio::test]
    async fn a_room_posts_facet_reads_and_opens_only_what_it_can_admit_off_the_worker() {
        let (state, store, _blobs) = nest_counting_blob_reads().await;
        let per_post_key = [0x64u8; 32];
        let seal = |plaintext: &[u8]| {
            fauna_core::subscription::crypto::encrypt_content(&per_post_key, plaintext)
        };
        let oversize = stored(&store, &seal(b"a post picture measured past the ceiling")).await;
        recorded_oversize(&state, &oversize).await;
        let honest_plaintext = b"an honest post picture".to_vec();
        let honest = stored(&store, &seal(&honest_plaintext)).await;
        let item = |blob_hash: ContentHash| fauna_core::data::MediaItem {
            blob_hash,
            media_type: "image/png".into(),
            size_bytes: 16,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let media = [item(oversize), item(honest)];

        let facet = RoomFacetSource::Post {
            media: &media,
            per_post_key: &per_post_key,
        }
        .open(&state)
        .await;

        assert_only_the_admissible_work_was_done(
            &facet,
            &store,
            &oversize,
            &honest,
            &honest_plaintext,
        );
    }

    /// Gate 1 (Plan 9): the `require_local_conv_serving` predicate — transparent
    /// without a backup row, refuses once the channel's `__conv/<hex>` reserved
    /// set is a custody copy.
    #[tokio::test]
    async fn require_local_conv_serving_gate() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let channel = [0x6Eu8; 32];

        // No backup row → transparent.
        require_local_conv_serving(&state, &channel).await.unwrap();

        // a custody copy → refused with the conv-namespaced error.
        let name = format!("__conv/{}", hex::encode(channel));
        db.create_folder_with_options(
            &name,
            &channel,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let err = require_local_conv_serving(&state, &channel)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.conversations.pure_backup_destination");
    }

    /// Gate 1 wired into `channel.fetch`: a User reading a pure-backup channel is
    /// refused before the roster auto-register / segment read.
    #[tokio::test]
    async fn channel_fetch_refuses_on_pure_backup_destination() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let reader = [0x6Au8; 32]; // plain actor → CallerClass::User
        let channel = [0x6Bu8; 32];
        db.create_user(&reader, "free", "test").await.unwrap();

        let name = format!("__conv/{}", hex::encode(channel));
        db.create_folder_with_options(
            &name,
            &channel,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let req = ChannelFetchRequest {
            channel_id: hex::encode(channel),
            after: 0,
            limit: 100,
            nest_url: None,
            extra: std::collections::BTreeMap::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = channel_fetch_handler()(state, reader, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.conversations.pure_backup_destination");
    }

    /// A same-nest `welcome.deliver` on an unclaimed channel (a DM/group) still
    /// auto-registers the recipient — the gate must not touch ordinary
    /// conversation delivery, whose roster row depends on this register.
    #[tokio::test]
    async fn welcome_deliver_still_auto_registers_dm_recipient() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let sender = [0x51u8; 32];
        let recipient = [0x52u8; 32];
        let channel = [0x53u8; 32]; // unclaimed — an ordinary DM channel

        // The mode gate acts on NEW parties (direct-messages.md § Reach
        // policy); this test's subject is channel auto-registration, so
        // arrange an accepted contact and let it flow.
        db.upsert_contact(&recipient, &sender, "accepted")
            .await
            .unwrap();

        let req = WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(recipient),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![],
            kind: WelcomeKind::Dm,
            nest_url: None,
            extra: Default::default(),
        };
        welcome_deliver_core(&state, &sender, &req, WelcomeRail::User)
            .await
            .unwrap();
        assert!(
            db.is_actor_in_channel(&recipient, &channel).await.unwrap(),
            "DM welcome must still auto-register the recipient on an unclaimed channel"
        );
    }

    /// F1 eviction durability: an **evicted** folder member
    /// who still knows the stable `group_id` cannot re-insert themselves into a
    /// **claimed** channel's roster via a self-addressed `welcome.deliver` — only
    /// the channel's claimant (the owning sharer) may register a recipient. The
    /// attacker-chosen `kind` is irrelevant (the gate keys on the channel's claim,
    /// not the request kind).
    #[tokio::test]
    async fn welcome_deliver_gates_self_insert_into_claimed_folder_channel() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let owner = [0x41u8; 32];
        let evicted = [0x42u8; 32];
        let newbie = [0x44u8; 32];
        let channel = [0x43u8; 32];

        // Owner binds the folder channel (the claim registers the owner) and
        // shares with a member, who is then evicted (rotate-on-removal).
        db.claim_folder_channel(&owner, &channel).await.unwrap();
        db.register_actor_channel(&evicted, &channel).await.unwrap();
        assert!(
            db.evict_actor_from_channel(&evicted, &channel)
                .await
                .unwrap()
        );
        assert!(!db.is_actor_in_channel(&evicted, &channel).await.unwrap());

        // ABUSE: the evicted member self-addresses a Welcome to re-insert.
        let abuse = WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(evicted),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(channel),
            },
            nest_url: None,
            extra: Default::default(),
        };
        // Refused outright since: a non-claimant on a claimed
        // channel earns no exemption from the recipient's inbox mode, so the
        // self-addressed Welcome never reaches the roster write at all. The
        // register gate behind it is what this test pins, so assert the
        // invariant whichever way the call lands.
        let _ = welcome_deliver_core(&state, &evicted, &abuse, WelcomeRail::User).await;
        assert!(
            !db.is_actor_in_channel(&evicted, &channel).await.unwrap(),
            "evicted member re-inserted via self-addressed welcome.deliver (F1 eviction not durable)"
        );

        // LEGIT: the owner (claimant) can still register a fresh recipient.
        let share = WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(newbie),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(channel),
            },
            nest_url: None,
            extra: Default::default(),
        };
        welcome_deliver_core(&state, &owner, &share, WelcomeRail::User)
            .await
            .unwrap();
        assert!(
            db.is_actor_in_channel(&newbie, &channel).await.unwrap(),
            "owner's legit share failed to register the recipient"
        );
    }

    /// The `Scheduling` exemption from the recipient's inbox mode belongs to
    /// the MDA CalDAV gateway's RAIL — a nest-side call site no client can
    /// select — and not to the `Scheduling` label, which any User-class caller
    /// may write on a Welcome of its own.
    /// `direct-messages.md` § Reach policy → § Scope.
    ///
    /// The User-rail half of the same rule is pinned end-to-end through the
    /// dispatcher in `conformance_conversations_welcome.rs`.
    #[tokio::test]
    async fn the_scheduling_gateway_rail_is_exempt_from_the_inbox_mode() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let organizer = [0x81u8; 32];
        let attendee = [0x82u8; 32];
        // Two channels: an exemption is per-delivery, and a seat earned on the
        // first would make the second read as in-band traffic.
        let gateway_channel = [0x83u8; 32];
        let client_channel = [0x84u8; 32];

        // The strictest mode, and no contact edge — the organizer is a
        // stranger to the attendee.
        db.set_inbox_mode(&attendee, "closed").await.unwrap();

        let invite = |channel: [u8; 32]| WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(attendee),
            channel_id: hex::encode(channel),
            welcome_bytes: vec![],
            kind: WelcomeKind::Scheduling,
            nest_url: None,
            extra: Default::default(),
        };

        // The gateway's own rail delivers: a mailbox-less attendee still gets
        // their calendar invite under `closed` (caldav-server.md
        // § Server-side auto-schedule).
        welcome_deliver_core(
            &state,
            &organizer,
            &invite(gateway_channel),
            WelcomeRail::SchedulingGateway,
        )
        .await
        .expect("the CalDAV gateway's rail reaches a `closed` attendee");

        // The same bytes, the same label, on the User rail: refused. The label
        // is not the exemption.
        let err = welcome_deliver_core(
            &state,
            &organizer,
            &invite(client_channel),
            WelcomeRail::User,
        )
        .await
        .expect_err("a client's own `Scheduling` label must not skip `closed`");
        assert_eq!(err.code, "fauna.conversations.forbidden");
        assert!(
            !db.is_actor_in_channel(&attendee, &client_channel)
                .await
                .unwrap(),
            "a refused Welcome must buy no roster seat — the seat is what the \
             two-step chain was built on"
        );
    }

    /// F1 eviction durability: the `channel.fetch`
    /// "auto-register on read" cannot re-insert an evicted member into a claimed
    /// folder channel. (No legit folder flow reads via `channel.fetch` — set
    /// content flows through `content_key.get` + the sync daemon.)
    #[tokio::test]
    async fn channel_fetch_gates_self_insert_into_claimed_folder_channel() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let owner = [0x61u8; 32];
        let evicted = [0x62u8; 32]; // plain actor → CallerClass::User
        let channel = [0x63u8; 32];
        db.create_user(&evicted, "free", "test").await.unwrap();

        db.claim_folder_channel(&owner, &channel).await.unwrap();
        db.register_actor_channel(&evicted, &channel).await.unwrap();
        db.evict_actor_from_channel(&evicted, &channel)
            .await
            .unwrap();

        let req = ChannelFetchRequest {
            channel_id: hex::encode(channel),
            after: 0,
            limit: 100,
            nest_url: None,
            extra: std::collections::BTreeMap::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        channel_fetch_handler()(state, evicted, payload)
            .await
            .unwrap();
        assert!(
            !db.is_actor_in_channel(&evicted, &channel).await.unwrap(),
            "evicted member re-inserted via channel.fetch auto-register"
        );
    }

    /// White-box unit of the [`folder_channel_claim`] read +
    /// [`claim_permits`] gate — the primitives `register_actor_channel_gated`
    /// and the commit rate cap both share: unclaimed channels always
    /// permit auto-register; a claimed folder channel permits it only for the
    /// claimant (owning sharer), never a stranger / evicted member.
    #[tokio::test]
    async fn claim_permits_gates_claimed_folder_channels() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let owner = [0x11u8; 32];
        let stranger = [0x22u8; 32];
        let channel = [0x33u8; 32];

        // Unclaimed (a DM/group): anyone may auto-register.
        assert!(claim_permits(
            folder_channel_claim(&db, &channel).await,
            &owner
        ));
        assert!(claim_permits(
            folder_channel_claim(&db, &channel).await,
            &stranger
        ));

        // Claim to the owner (a fresh folder channel).
        db.claim_folder_channel(&owner, &channel).await.unwrap();

        // Only the claimant may auto-register now.
        let claim = folder_channel_claim(&db, &channel).await;
        assert_eq!(claim, FolderChannelClaim::Claimed(owner));
        assert!(claim_permits(claim, &owner));
        assert!(!claim_permits(claim, &stranger));
        assert!(is_the_claimant(claim, &owner));
        assert!(!is_the_claimant(claim, &stranger));
    }

    /// Row 433: `Unknown` (a claim-read error, or a malformed `claimed_by`
    /// blob) is NOT the same as `Unclaimed` — it must gate like a real
    /// claimant would, never permit an auto-register the way the old
    /// `Option<[u8; 32]>` collapse silently did.
    #[test]
    fn unknown_claim_never_permits_or_self_admits() {
        assert!(!claim_permits(FolderChannelClaim::Unknown, &[0x11u8; 32]));
        assert!(!is_the_claimant(FolderChannelClaim::Unknown, &[0x11u8; 32]));
        // Unclaimed is the one permissive state, and only for the permit gate
        // — nobody IS "the claimant" of an unclaimed channel.
        assert!(claim_permits(FolderChannelClaim::Unclaimed, &[0x11u8; 32]));
        assert!(!is_the_claimant(
            FolderChannelClaim::Unclaimed,
            &[0x11u8; 32]
        ));
    }

    /// Row 433 contract item 1: a claim-read error does not register a
    /// non-claimant onto a claimed folder channel's roster. Simulated by
    /// dropping the `folder_channel_claims` table out from under a live claim
    /// — `folder_channel_claimed_by` then returns `Err`, not `Ok(None)`.
    #[tokio::test]
    async fn claim_read_error_does_not_register_a_non_claimant() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let owner = [0x44u8; 32];
        let evicted = [0x45u8; 32];
        let channel = [0x46u8; 32];

        db.claim_folder_channel(&owner, &channel).await.unwrap();
        db.register_actor_channel(&evicted, &channel).await.unwrap();
        db.evict_actor_from_channel(&evicted, &channel)
            .await
            .unwrap();

        // Simulate the claim-read error the finding describes.
        {
            let conn = db.conn().await;
            conn.execute_batch("DROP TABLE folder_channel_claims")
                .unwrap();
        }
        assert!(
            db.folder_channel_claimed_by(&channel).await.is_err(),
            "test setup: the drop must turn the read into an Err"
        );

        register_actor_channel_gated(&db, &channel, &evicted, &evicted, "test").await;

        assert!(
            !db.list_channel_actors(&channel)
                .await
                .unwrap()
                .contains(&evicted),
            "an unresolvable claim state must not re-admit the evicted member \
             (the owner IS expected on the roster — claim_folder_channel seats them)"
        );
    }

    /// Row 433 contract item 1 (malformed-blob half): a `claimed_by` value
    /// that is not exactly 32 bytes is an `Err`, not a silent "unclaimed".
    #[tokio::test]
    async fn malformed_claim_blob_does_not_read_as_unclaimed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let owner = [0x47u8; 32];
        let channel = [0x48u8; 32];

        db.claim_folder_channel(&owner, &channel).await.unwrap();
        // Corrupt the stored claim to a non-32-byte value.
        {
            let conn = db.conn().await;
            conn.execute(
                "UPDATE folder_channel_claims SET claimed_by = X'AA' WHERE channel_id = ?1",
                rusqlite::params![channel.as_slice()],
            )
            .unwrap();
        }

        let err = db
            .folder_channel_claimed_by(&channel)
            .await
            .expect_err("a 1-byte claimed_by must not decode as Ok(None)");
        let msg = err.to_string();
        assert!(msg.contains("folder_channel_claims.claimed_by"), "{msg}");
        assert!(msg.contains("want 32"), "{msg}");
    }

    // ── device-owned-epoch commit gate (`expect_no_commit_since`) ──────

    /// Inner bytes must clear the AEAD-shape floor the strict ingest verifier
    /// applies to every channel envelope (length >= 28 = 12-byte nonce + 16-byte
    /// tag, and no plaintext-content magic prefix). Before Phase 4 these fixtures
    /// got away with 16 bytes because `AppState::for_test` installed the
    /// permissive plaintext arm; there is one (strict) arm now.
    fn commit_env(marker: u8) -> Vec<u8> {
        fauna_mls::types::ChannelEnvelope::Commit(vec![marker; 32])
            .to_bytes()
            .unwrap()
    }

    fn app_env(marker: u8) -> Vec<u8> {
        fauna_mls::types::ChannelEnvelope::Application(vec![marker; 32])
            .to_bytes()
            .unwrap()
    }

    /// A gated send whose `expect_no_commit_since` predates a landed `Commit` is
    /// rejected `fauna.conversations.channel.stale`; a caught-up retry (`since` ==
    /// the landed commit's seq) is accepted — the design's rebase-then-retry path
    /// (devices.md § Cross-device MLS group-state sync).
    #[tokio::test]
    async fn channel_send_gate_rejects_stale_commit_and_accepts_caught_up() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let actor = [0x71u8; 32];
        let channel = [0x72u8; 32];

        // A first commit lands ungated at seq 1 → commit high-water mark = 1.
        let first = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(1)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(first.seq, 1);

        // A gated commit whose precondition (0) predates the landed commit (1) is stale.
        let err = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(2)),
            false,
            Some(0),
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.conversations.channel.stale");

        // Caught up (`since` == the landed commit's seq) → the retry is accepted.
        let retry = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(2)),
            false,
            Some(1),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(retry.seq, 2, "the caught-up retry appends at the next seq");
    }

    /// An envelope too large to be served back in one WS frame is REFUSED at the
    /// door, never accepted-then-unservable: the serve page
    /// byte-budgets each record to its bytes + `RECORD_WIRE_OVERHEAD`, so a record
    /// over `SERVE_PAGE_BUDGET_BYTES - RECORD_WIRE_OVERHEAD` would freeze
    /// `take_page_within_budget` (empty page, head unmoved) and stall the drain
    /// for every member, silent to the client. The cap fires before ingest, so
    /// the reason is a distinct `invalid_params`, not the generic ingest failure a
    /// looser 2 MiB-frame-only cap left it to.
    #[tokio::test]
    async fn channel_send_refuses_an_over_serve_budget_envelope() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let actor = [0x7au8; 32];
        let channel = [0x7bu8; 32];

        let cap = crate::segments::SERVE_PAGE_BUDGET_BYTES - crate::segments::RECORD_WIRE_OVERHEAD;
        let err = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(vec![0u8; cap + 1]),
            false,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.conversations.invalid_params",
            "an over-serve-budget envelope must be refused at the door with a distinct \
             reason, never reach ingest to be accepted-then-unservable"
        );
    }

    /// `Application` records never raise the commit mark, so they never trip the
    /// gate — a busy channel accumulating chat between a device's last-processed
    /// seq and its commit must not livelock the commit. Only a `Commit`-after-seq
    /// blocks.
    #[tokio::test]
    async fn channel_send_gate_ignores_application_records() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let actor = [0x73u8; 32];
        let channel = [0x74u8; 32];

        // Commit at seq 1 (mark = 1), then two Applications at seq 2, 3 (mark stays 1).
        channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(0xA0)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(app_env(0xA1)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(app_env(0xA2)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();

        // A gated commit caught up to the last Commit's seq (1) is accepted even
        // though Applications landed after it.
        let ok = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(0xBB)),
            false,
            Some(1),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            ok.seq, 4,
            "Applications after the caller's seq must not block the commit"
        );
    }

    /// A commit's **sender** is recorded on BOTH append arms — the ungated
    /// blind append and the device-owned-epoch gated one — because the floor
    /// roster's authorship rule reads it (`conversation-rooms.md` § The floor
    /// roster). The report-door tests seed the mark straight into the table,
    /// so they cannot see this link; a device with a commit gate installed
    /// sends every commit through the gated arm, which no other test drives
    /// with a sender assertion.
    #[tokio::test]
    async fn a_commits_sender_is_recorded_on_both_append_arms() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let alice = [0x75u8; 32];
        let bob = [0x76u8; 32];
        let channel = [0x77u8; 32];

        // Ungated: alice's commit lands at seq 1 and names her.
        channel_send_core(
            &state,
            &alice,
            &channel,
            Bytes::from(commit_env(0xC1)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (1, Some(alice)),
            "the ungated arm records the commit's sender"
        );

        // An Application from bob moves neither half of the pair.
        channel_send_core(
            &state,
            &bob,
            &channel,
            Bytes::from(app_env(0xC2)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (1, Some(alice)),
            "an application record authors no commit position"
        );

        // Gated: bob's caught-up commit lands at seq 3 and names him.
        channel_send_core(
            &state,
            &bob,
            &channel,
            Bytes::from(commit_env(0xC3)),
            false,
            Some(1),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (3, Some(bob)),
            "the gated arm records the commit's sender"
        );
    }

    /// An omitted precondition (`None`) is today's blind append — a second commit
    /// lands unconditionally even though the first raised the mark. A send that
    /// never sets the field (`expect_no_commit_since: None`) must never be gated.
    #[tokio::test]
    async fn channel_send_ungated_is_blind_append() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let actor = [0x75u8; 32];
        let channel = [0x76u8; 32];

        let a = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(1)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        let b = channel_send_core(
            &state,
            &actor,
            &channel,
            Bytes::from(commit_env(2)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            (a.seq, b.seq),
            (1, 2),
            "ungated sends append unconditionally"
        );
    }

    /// Folder-channel Commit admission (`federation.md` § Cross-nest shared
    /// folders + channel append, re-ratified 2026-08-24 — supersedes the
    /// 2026-07-18 claimant-only refusal): on a **claimed** folder channel a
    /// `ChannelEnvelope::Commit` is admitted from any actor on the channel
    /// roster — the member's device-owned-epoch self-`Update` takeover
    /// (`devices.md` § Cross-device MLS group-state sync) is the legitimate
    /// shape, and owner-only roster management is enforced member-side
    /// (`MlsEngine::process_commit`'s folder commit policy), because commit
    /// content is ciphertext to the nest. An **off-roster** actor's Commit is
    /// still refused (the `ingest_channel_envelope` roster check, side-effect
    /// free — a claimed channel's auto-register is claimant-gated), so
    /// evicted members and outsiders stay out.
    #[tokio::test]
    async fn channel_send_admits_rostered_member_commit_on_claimed_folder_channel() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let owner = [0x91u8; 32];
        let member = [0x92u8; 32];
        let outsider = [0x95u8; 32];
        let fs_channel = [0x93u8; 32];

        // A claimed folder channel: the owner is the claimant (auto-registered by
        // the claim), and a same-nest member was auto-joined onto the roster at
        // Welcome-relay time.
        db.claim_folder_channel(&owner, &fs_channel).await.unwrap();
        db.register_actor_channel(&member, &fs_channel)
            .await
            .unwrap();

        // (1) The rostered member's Commit (the takeover shape) is ADMITTED.
        let takeover = channel_send_core(
            &state,
            &member,
            &fs_channel,
            Bytes::from(commit_env(0x01)),
            false,
            None,
            &[],
        )
        .await
        .expect("a rostered member's Commit is admitted on a claimed folder channel");
        assert_eq!(takeover.seq, 1, "the member's takeover lands at seq 1");

        // (2) An OFF-roster actor's Commit is refused — the roster check, with
        // zero side effects (no seq consumed, no auto-register on a claimed
        // channel) — proven by the owner's rotation landing at seq 2 below.
        let err = channel_send_core(
            &state,
            &outsider,
            &fs_channel,
            Bytes::from(commit_env(0x02)),
            false,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.conversations.ingest_failed",
            "an off-roster Commit is refused by the roster check"
        );

        // (3) The claimant's own rotation still lands, at seq 2 — the refused
        // outsider commit consumed no seq.
        let owner_commit = channel_send_core(
            &state,
            &owner,
            &fs_channel,
            Bytes::from(commit_env(0x03)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            owner_commit.seq, 2,
            "the claimant's rotation lands at seq 2 (the outsider refusal consumed no seq)"
        );

        // (4) An unclaimed conversation channel is unaffected — any member may
        // commit there (auto-register admits the first poster).
        let conv_channel = [0x94u8; 32];
        let conv_commit = channel_send_core(
            &state,
            &member,
            &conv_channel,
            Bytes::from(commit_env(0x04)),
            false,
            None,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            conv_commit.seq, 1,
            "a conversation-channel member's commit is unaffected by the folder rules"
        );
    }

    /// Non-claimant commit rate cap (`federation.md` § residual (a), row
    /// 428): a non-claimant rostered member's Commit stream past
    /// `CHANNEL_COMMIT_LIMITER_CONFIG.max_events` is refused with the typed
    /// retryable `fauna.conversations.rate_limited` code; a takeover under
    /// the cap lands; the claimant is NEVER capped, even on the very channel
    /// where the member just exhausted the shared limiter (a different
    /// bucket key — `(actor, channel, "commit")` is per-actor).
    #[tokio::test]
    async fn channel_send_caps_non_claimant_commit_rate_but_never_the_claimant() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let owner = [0xa1u8; 32];
        let member = [0xa2u8; 32];
        let fs_channel = [0xa3u8; 32];

        db.claim_folder_channel(&owner, &fs_channel).await.unwrap();
        db.register_actor_channel(&member, &fs_channel)
            .await
            .unwrap();

        let cap = crate::bridge_rate_limit::CHANNEL_COMMIT_LIMITER_CONFIG.max_events;

        // The non-claimant member's takeover commits land, up to the cap.
        for i in 0..cap {
            channel_send_core(
                &state,
                &member,
                &fs_channel,
                Bytes::from(commit_env(i as u8)),
                false,
                None,
                &[],
            )
            .await
            .unwrap_or_else(|e| panic!("commit {i} under the cap should land: {e:?}"));
        }

        // The next one is refused — typed and retryable, not permission_denied.
        let err = channel_send_core(
            &state,
            &member,
            &fs_channel,
            Bytes::from(commit_env(0xee)),
            false,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.conversations.rate_limited",
            "past-cap commit gets the typed retryable refusal, not a permanent one"
        );

        // The claimant's own rotation is never capped.
        channel_send_core(
            &state,
            &owner,
            &fs_channel,
            Bytes::from(commit_env(0xff)),
            false,
            None,
            &[],
        )
        .await
        .expect("the claimant's own commit is never rate-capped");
    }
}
